// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! `mogwai cache`: the manual-case cover for the storage policy's
//! stale-provenance pruning (the retired rewrite plan, phase 1). Phase 1
//! lands no cache PRODUCERS - `stats`/`clean` operate on whatever a later
//! phase has written under the cache root, and are honest about reporting
//! zero when nothing has.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use mogwai_lab::storage::{
    CacheStore, ProvenanceToken, cache_clean_all, cache_entry_tokens, cache_root, cache_stats,
};

#[derive(Args)]
pub(crate) struct CacheArgs {
    #[command(subcommand)]
    command: CacheCommand,
    /// Overrides `MOGWAI_CACHE_DIR` and the XDG default.
    #[arg(long, global = true, value_name = "PATH")]
    cache_dir: Option<PathBuf>,
}

#[derive(Subcommand)]
enum CacheCommand {
    /// Entry, file and byte counts under the cache root.
    Stats {
        /// Also print each provenance token under the cache root, which is
        /// what `clean --stale --keep` takes.
        #[arg(long)]
        entries: bool,
    },
    /// Removes provenance directories. Bare `clean` clears everything;
    /// `--stale --keep <TOKEN>` clears every directory except the named one.
    ///
    /// `--keep` IS REQUIRED WITH `--stale`, AND THAT IS THE FIX FOR A REAL
    /// DEFECT rather than an ergonomic choice. `--stale` used to synthesize
    /// "the current token" here, folding `std::env::args()` into it - so the
    /// token it computed was one for the literal string
    /// `".../mogwai cache clean --stale"`, which no producer has ever written
    /// under. A cache PRODUCER's token binds the command that produced the
    /// entries (`arrival-screen:kernel-version=...:start=...`) and that
    /// command's own sub-contract hash; nothing about a `cache` invocation
    /// determines it. The mismatch was total, so `clean --stale` deleted
    /// EVERYTHING, identically to bare `clean`, on the invocation an operator
    /// reaches for precisely because it is supposed to be the safe one. A
    /// token this command cannot derive is one the operator must name, and
    /// `cache stats --entries` prints the candidates.
    Clean {
        #[arg(long)]
        stale: bool,
        /// The provenance token to keep, as printed by
        /// `mogwai cache stats --entries`.
        #[arg(long, value_name = "TOKEN", requires = "stale")]
        keep: Option<String>,
    },
}

pub(crate) fn run(args: CacheArgs) -> anyhow::Result<()> {
    let CacheArgs { command, cache_dir } = args;
    let root = cache_root(cache_dir.as_deref());
    match command {
        CacheCommand::Stats { entries } => {
            let stats = cache_stats(&root)?;
            println!("cache root: {}", root.display());
            println!("provenance dirs: {}", stats.provenance_dirs);
            println!("files: {}", stats.files);
            println!("bytes: {}", stats.bytes);
            if entries {
                for token in cache_entry_tokens(&root)? {
                    println!("entry: {token}");
                }
            }
        }
        CacheCommand::Clean {
            stale: true,
            keep: Some(token),
        } => {
            // AN UNKNOWN TOKEN IS A REFUSAL, because `clean_stale` only ever
            // COMPARES names: a token matching no directory keeps no directory,
            // so `--keep bbb` fat-fingered for `bbbb` removes the entire cache
            // including the one entry the operator named. That is the very data
            // loss `--keep` was introduced to prevent, reduced from unconditional
            // to one keystroke away. `ProvenanceToken::named` validates nothing
            // and cannot - a token is whatever a producer wrote - so the check
            // belongs here, against what is on disk.
            let present = cache_entry_tokens(&root)?;
            anyhow::ensure!(
                present.contains(&token),
                "no cache entry under {} carries the provenance token {token}. Keeping a token \
                 that matches nothing prunes EVERYTHING, which is what `--keep` exists to \
                 prevent, so this refuses instead. Present tokens: {}",
                root.display(),
                if present.is_empty() {
                    "none - the cache is empty".to_string()
                } else {
                    present.join(", ")
                }
            );
            let store = CacheStore::open(root, ProvenanceToken::named(&token));
            let removed = store.clean_stale()?;
            println!(
                "pruned {removed} provenance director{} other than {token}",
                if removed == 1 { "y" } else { "ies" }
            );
        }
        CacheCommand::Clean {
            stale: true,
            keep: None,
        } => {
            // `requires = "stale"` binds the other direction only, so this
            // arm is reachable. Refusing is the point: the previous code
            // guessed a token here and the guess deleted everything.
            anyhow::bail!(
                "`cache clean --stale` needs `--keep <TOKEN>`: a cache entry's provenance token \
                 binds the command that PRODUCED it, which this invocation cannot derive. Run \
                 `mogwai cache stats --entries` for the tokens present, or `mogwai cache clean` \
                 to clear the lot deliberately."
            );
        }
        CacheCommand::Clean {
            stale: false,
            keep: _,
        } => {
            let removed = cache_clean_all(&root)?;
            println!(
                "removed {removed} provenance director{}",
                if removed == 1 { "y" } else { "ies" }
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use mogwai_lab::storage::ScratchDir;

    use super::*;

    /// Two provenance directories under a private cache root, so a prune can
    /// be observed rather than argued about.
    ///
    /// The root is under the WORKSPACE target directory, not a bare relative
    /// `target/`. A unit test's working directory is its crate, so a relative
    /// path lands in `crates/mogwai-cli/target/`, which the root `.gitignore`
    /// hides (`target` matches at any depth) and which `cargo clean` never
    /// touches, because cargo's target dir is at the workspace root. That is
    /// an unswept, permanently growing scratch area wearing a build
    /// directory's name. `CARGO_TARGET_TMPDIR` is not available here - cargo
    /// sets it for integration tests and benches only - so the workspace root
    /// is resolved from `CARGO_MANIFEST_DIR` instead.
    ///
    /// THE GUARD IS RETURNED, not the path. Its leaf is unique per process, so
    /// two concurrent test sweeps, which the full gate profile runs by design,
    /// cannot race each other's seeding, and dropping it removes what the test
    /// wrote.
    fn seeded_root(name: &str) -> ScratchDir {
        let scratch = crate::test_paths::scratch_dir(name);
        let root = scratch.path();
        for token in ["aaaa", "bbbb"] {
            std::fs::create_dir_all(root.join("entries").join(token)).expect("seeding the cache");
            std::fs::write(root.join("entries").join(token).join("k"), b"{}").expect("an entry");
        }
        scratch
    }

    fn args(root: &Path, command: CacheCommand) -> CacheArgs {
        CacheArgs {
            command,
            cache_dir: Some(root.to_path_buf()),
        }
    }

    /// THE FINDING, ASSERTED ON THE RESOURCE RATHER THAN ON AN ERROR. The old
    /// `--stale` synthesized a token from its own argv, matched nothing, and
    /// removed both directories. A test observing only the exit status could
    /// not tell that apart from a correct prune, so this one counts what
    /// survives on disk.
    #[test]
    fn stale_without_a_named_token_refuses_and_deletes_nothing() {
        let scratch = seeded_root("cache-stale-refuses");
        let root = scratch.path();
        let result = run(args(
            root,
            CacheCommand::Clean {
                stale: true,
                keep: None,
            },
        ));
        // THE RESOURCE BEFORE THE VERDICT, deliberately. Asserted the other way
        // round, the `expect_err` fires first and the test goes red without
        // ever looking at the disk - so a bite-check reads as proof for an
        // assertion that never ran, which this arc has paid for before.
        assert!(root.join("entries/aaaa/k").exists(), "aaaa was deleted");
        assert!(root.join("entries/bbbb/k").exists(), "bbbb was deleted");
        let err = result.expect_err("a prune with no token to keep must refuse");
        assert!(err.to_string().contains("--keep <TOKEN>"), "{err}");
    }

    /// And the named token is the one kept - a prune that keeps nothing is
    /// bare `clean` under another name, which is exactly what the defect made
    /// it.
    #[test]
    fn stale_keeps_the_named_token_and_only_that_one() {
        let scratch = seeded_root("cache-stale-keeps");
        let root = scratch.path();
        run(args(
            root,
            CacheCommand::Clean {
                stale: true,
                keep: Some("bbbb".to_string()),
            },
        ))
        .expect("the prune runs");
        assert!(!root.join("entries/aaaa").exists(), "aaaa survived");
        assert!(root.join("entries/bbbb/k").exists(), "bbbb was pruned");
    }

    /// THE TYPO, WHICH IS THE SAME DATA LOSS ONE KEYSTROKE AWAY. `clean_stale`
    /// only compares names, so a `--keep` naming nothing keeps nothing and the
    /// whole cache goes - the exact outcome the required `--keep` was added to
    /// stop. Asserted on the RESOURCE first, for the same reason its sibling
    /// above is: an `expect_err` fired before any disk check proves the command
    /// refused, not that it refused BEFORE deleting.
    #[test]
    fn an_unknown_token_refuses_rather_than_pruning_everything() {
        let scratch = seeded_root("cache-stale-typo");
        let root = scratch.path();
        let result = run(args(
            root,
            CacheCommand::Clean {
                stale: true,
                // One character short of `bbbb`.
                keep: Some("bbb".to_string()),
            },
        ));
        assert!(root.join("entries/aaaa/k").exists(), "aaaa was deleted");
        assert!(root.join("entries/bbbb/k").exists(), "bbbb was deleted");
        let err = result.expect_err("a token matching no entry must refuse");
        let shown = err.to_string();
        assert!(shown.contains("provenance token bbb"), "{shown}");
        // The candidates are named, so the refusal is actionable.
        assert!(shown.contains("aaaa, bbbb"), "{shown}");
    }

    /// `--keep` is only reachable if the operator can read the tokens off the
    /// cache, so the listing is part of the fix rather than a nicety.
    #[test]
    fn the_entry_listing_names_every_provenance_directory() {
        let scratch = seeded_root("cache-entry-listing");
        assert_eq!(
            cache_entry_tokens(scratch.path()).expect("listing the cache"),
            vec!["aaaa".to_string(), "bbbb".to_string()]
        );
    }
}
