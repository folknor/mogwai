// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! `mogwai cache`: the manual-case cover for the storage policy's
//! stale-provenance pruning (notes/rust-rewrite-phases.md phase 1). Phase 1
//! lands no cache PRODUCERS - `stats`/`clean` operate on whatever a later
//! phase has written under the cache root, and are honest about reporting
//! zero when nothing has.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use mogwai_lab::storage::{CacheStore, ProvenanceInputs, ProvenanceToken, cache_clean_all, cache_root, cache_stats};

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
    Stats,
    /// Removes provenance directories. Bare `clean` clears everything;
    /// `--stale` keeps only the CURRENT provenance token's entries (the
    /// pruning `CacheStore::write` already does automatically - this is the
    /// manual case).
    Clean {
        #[arg(long)]
        stale: bool,
    },
}

/// The current provenance token for a bare `mogwai cache` invocation: this
/// binary's own version, the tape protocol version it was built against,
/// the sub-contract hash, this command line, and (if present)
/// `analysis/fingerprint.json`'s hash. `clean --stale` prunes every
/// provenance directory that does not match this.
fn current_token() -> ProvenanceToken {
    let fingerprint_hash = std::fs::read(PathBuf::from("analysis").join("fingerprint.json"))
        .ok()
        .map(|bytes| mogwai_lab::ledger::sha256_bytes(&bytes))
        .unwrap_or_default();
    let full_command = std::env::args().collect::<Vec<_>>().join(" ");
    ProvenanceToken::compute(&ProvenanceInputs {
        crate_version: env!("CARGO_PKG_VERSION"),
        tape_protocol_version: mogwai_data::TAPE_PROTOCOL_VERSION,
        fingerprint_hash: &fingerprint_hash,
        full_command: &full_command,
        subcontract_hash: &mogwai_lab::subcontract::subcontract_hash(),
    })
}

pub(crate) fn run(args: CacheArgs) -> anyhow::Result<()> {
    let CacheArgs { command, cache_dir } = args;
    let root = cache_root(cache_dir.as_deref());
    match command {
        CacheCommand::Stats => {
            let stats = cache_stats(&root)?;
            println!("cache root: {}", root.display());
            println!("provenance dirs: {}", stats.provenance_dirs);
            println!("files: {}", stats.files);
            println!("bytes: {}", stats.bytes);
        }
        CacheCommand::Clean { stale: true } => {
            let store = CacheStore::open(root, current_token());
            let removed = store.clean_stale()?;
            println!("pruned {removed} stale provenance director{}", if removed == 1 { "y" } else { "ies" });
        }
        CacheCommand::Clean { stale: false } => {
            let removed = cache_clean_all(&root)?;
            println!("removed {removed} provenance director{}", if removed == 1 { "y" } else { "ies" });
        }
    }
    Ok(())
}
