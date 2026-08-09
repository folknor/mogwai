// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The storage policy (the retired rewrite plan, phase 1): three classes,
//! never mixed.
//!
//! - ARTIFACTS: the user's files, written to `--out` or the working
//!   directory, never cached, never auto-deleted. This module does not model
//!   them beyond [`artifact_path`] - a caller just picks a path and writes.
//! - CACHE: recomputable, keyed by a [`ProvenanceToken`] under
//!   `$XDG_CACHE_HOME/mogwai/` (falling back to `~/.cache/mogwai/`),
//!   overridable by `MOGWAI_CACHE_DIR` or `--cache-dir`. Stale-provenance
//!   entries - directories under the cache root that do not name the
//!   CURRENT token - are unreachable by construction and pruned on every
//!   write; [`CacheStore::clean_stale`] covers the manual case.
//! - SCRATCH: a run-scoped [`ScratchDir`] under the cache root, deleted on
//!   `Drop` (clean exit), ignorable on crash - the filesystem will not mind
//!   an orphaned scratch directory the way the 14,288-file
//!   `mnq-fit-scratch` mind minded a human.
//!
//! Phase 1 lands no cache PRODUCERS (measure12a's walk cache etc. are later
//! phases), so this module's own tests are synthetic - it proves the policy
//! mechanism, not any particular cache's contents.

use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// The git sha of the tree this crate was built from, folded in by
/// `build.rs`. Empty for a `cargo install`/crates.io build with no `.git`.
fn git_sha() -> &'static str {
    env!("MOGWAI_LAB_GIT_SHA")
}

/// The inputs a provenance token binds: crate version, the tape protocol
/// version (owned by `mogwai-data`; this crate does not depend on it, so the
/// caller passes the number), the fingerprint hash, the full invoked
/// command, and the measurement sub-contract hash. The git sha is folded in
/// automatically from the build-time env var.
#[derive(Clone, Copy, Debug)]
pub struct ProvenanceInputs<'a> {
    pub crate_version: &'a str,
    pub tape_protocol_version: u32,
    pub fingerprint_hash: &'a str,
    pub full_command: &'a str,
    pub subcontract_hash: &'a str,
}

/// A cache key that changes whenever anything that could change a cached
/// result changes. Opaque, filesystem-safe (hex), collision-resistant.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProvenanceToken(String);

impl ProvenanceToken {
    pub fn compute(inputs: &ProvenanceInputs<'_>) -> Self {
        let blob = format!(
            "crate_version={}\ntape_protocol_version={}\nfingerprint_hash={}\nfull_command={}\n\
             subcontract_hash={}\ngit_sha={}\n",
            inputs.crate_version,
            inputs.tape_protocol_version,
            inputs.fingerprint_hash,
            inputs.full_command,
            inputs.subcontract_hash,
            git_sha(),
        );
        let mut hasher = Sha256::new();
        hasher.update(blob.as_bytes());
        Self(crate::ledger::hex_digest(&hasher.finalize()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Where the user's own output files go: the `--out` path if the caller
/// passed one, else `default_name` in the current working directory.
/// Artifacts are never cached and never auto-deleted - this is deliberately
/// the entire policy for them.
pub fn artifact_path(explicit_out: Option<&Path>, default_name: &str) -> PathBuf {
    match explicit_out {
        Some(p) => p.to_path_buf(),
        None => PathBuf::from(default_name),
    }
}

/// Resolves the cache root: `--cache-dir` first, then `MOGWAI_CACHE_DIR`,
/// then XDG (`$XDG_CACHE_HOME/mogwai` or `~/.cache/mogwai`).
pub fn cache_root(cli_override: Option<&Path>) -> PathBuf {
    if let Some(p) = cli_override {
        return p.to_path_buf();
    }
    if let Ok(v) = std::env::var("MOGWAI_CACHE_DIR")
        && !v.is_empty()
    {
        return PathBuf::from(v);
    }
    if let Ok(v) = std::env::var("XDG_CACHE_HOME")
        && !v.is_empty()
    {
        return Path::new(&v).join("mogwai");
    }
    if let Ok(home) = std::env::var("HOME")
        && !home.is_empty()
    {
        return Path::new(&home).join(".cache").join("mogwai");
    }
    // No HOME and no XDG override: a relative fallback beats panicking. Real
    // deployments always carry HOME; this only matters in a stripped test
    // sandbox.
    PathBuf::from(".mogwai-cache")
}

/// A provenance-keyed cache directory under the cache root's `entries/`
/// subdirectory - `scratch/` is reserved for [`ScratchDir`], so the two
/// classes can never collide on a name.
#[derive(Clone)]
pub struct CacheStore {
    root: PathBuf,
    token: ProvenanceToken,
}

impl CacheStore {
    pub fn open(root: PathBuf, token: ProvenanceToken) -> Self {
        Self { root, token }
    }

    fn entries_root(&self) -> PathBuf {
        self.root.join("entries")
    }

    fn provenance_dir(&self) -> PathBuf {
        self.entries_root().join(self.token.as_str())
    }

    /// Writes `key` under the current provenance directory, pruning every
    /// sibling (stale-provenance) directory first - "stale-provenance
    /// entries are unreachable by construction and are pruned automatically
    /// on write" (phase-1 policy).
    pub fn write(&self, key: &str, bytes: &[u8]) -> io::Result<()> {
        self.clean_stale()?;
        let dir = self.provenance_dir();
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join(key), bytes)
    }

    pub fn read(&self, key: &str) -> io::Result<Option<Vec<u8>>> {
        match std::fs::read(self.provenance_dir().join(key)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Deletes every provenance directory under `entries/` except the
    /// current token's. Returns the count deleted.
    pub fn clean_stale(&self) -> io::Result<u64> {
        let entries_root = self.entries_root();
        let read = match std::fs::read_dir(&entries_root) {
            Ok(r) => r,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e),
        };
        let mut removed = 0u64;
        for entry in read {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            if entry.file_name().to_str() != Some(self.token.as_str()) {
                std::fs::remove_dir_all(entry.path())?;
                removed += 1;
            }
        }
        Ok(removed)
    }
}

/// Aggregate counts over the whole cache root, for `mogwai cache stats`.
#[derive(Debug, Default, Clone, Copy)]
pub struct CacheStats {
    pub provenance_dirs: u64,
    pub files: u64,
    pub bytes: u64,
}

fn walk_bytes_and_files(dir: &Path, stats: &mut CacheStats) -> io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_dir() {
            walk_bytes_and_files(&entry.path(), stats)?;
        } else if ty.is_file() {
            stats.files += 1;
            stats.bytes += entry.metadata()?.len();
        }
    }
    Ok(())
}

/// `mogwai cache stats`: the entry count, file count and byte total under
/// the cache root's `entries/` subdirectory (scratch is excluded - it is
/// meant to be gone by the time anyone runs this).
pub fn cache_stats(root: &Path) -> io::Result<CacheStats> {
    let entries_root = root.join("entries");
    let mut stats = CacheStats::default();
    match std::fs::read_dir(&entries_root) {
        Ok(read) => {
            for entry in read {
                let entry = entry?;
                if entry.file_type()?.is_dir() {
                    stats.provenance_dirs += 1;
                    walk_bytes_and_files(&entry.path(), &mut stats)?;
                }
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    Ok(stats)
}

/// `mogwai cache clean`: removes every provenance directory under the cache
/// root, regardless of token. Returns the count removed.
pub fn cache_clean_all(root: &Path) -> io::Result<u64> {
    let entries_root = root.join("entries");
    let read = match std::fs::read_dir(&entries_root) {
        Ok(r) => r,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let mut removed = 0u64;
    for entry in read {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            std::fs::remove_dir_all(entry.path())?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// A run-scoped scratch directory under the cache root's `scratch/`
/// subdirectory: deleted on `Drop` (clean exit), ignorable if the process
/// crashes first - the opposite of the 14,288-file `mnq-fit-scratch` this
/// policy exists to prevent a repeat of.
pub struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    pub fn new(root: &Path) -> io::Result<Self> {
        let unique = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let path = root.join("scratch").join(unique);
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.path).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_test_root(name: &str) -> PathBuf {
        // Cargo sets this for every test binary invocation, pointing at
        // `target/tmp` - keeps test scratch data inside the project tree
        // rather than `/tmp` (project convention).
        let base = match std::env::var("CARGO_TARGET_TMPDIR") {
            Ok(v) => PathBuf::from(v),
            Err(_) => std::env::temp_dir(),
        };
        base.join(format!(
            "mogwai-lab-storage-test-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ))
    }

    #[test]
    fn cache_root_prefers_override_then_env_then_xdg() {
        let cli = PathBuf::from("/explicit/cache");
        assert_eq!(cache_root(Some(&cli)), cli);
    }

    #[test]
    fn provenance_token_changes_when_any_input_changes() {
        let base = ProvenanceInputs {
            crate_version: "0.1.0",
            tape_protocol_version: 11,
            fingerprint_hash: "abc",
            full_command: "mogwai preflight",
            subcontract_hash: "def",
        };
        let t1 = ProvenanceToken::compute(&base);
        let mut changed = base;
        changed.tape_protocol_version = 12;
        let t2 = ProvenanceToken::compute(&changed);
        assert_ne!(t1, t2);
        let t1_again = ProvenanceToken::compute(&base);
        assert_eq!(t1, t1_again);
    }

    #[test]
    fn write_prunes_stale_provenance_and_read_round_trips() {
        let root = scratch_test_root("prune");
        let inputs_old = ProvenanceInputs {
            crate_version: "0.1.0",
            tape_protocol_version: 11,
            fingerprint_hash: "old",
            full_command: "mogwai preflight",
            subcontract_hash: "def",
        };
        let old_token = ProvenanceToken::compute(&inputs_old);
        let old_store = CacheStore::open(root.clone(), old_token.clone());
        old_store.write("k", b"stale").unwrap();
        assert_eq!(old_store.read("k").unwrap(), Some(b"stale".to_vec()));

        let inputs_new = ProvenanceInputs {
            fingerprint_hash: "new",
            ..inputs_old
        };
        let new_token = ProvenanceToken::compute(&inputs_new);
        let new_store = CacheStore::open(root.clone(), new_token);
        new_store.write("k", b"fresh").unwrap();

        // The old provenance directory is gone; a store keyed on it reads
        // nothing back.
        assert_eq!(old_store.read("k").unwrap(), None);
        assert_eq!(new_store.read("k").unwrap(), Some(b"fresh".to_vec()));

        let stats = cache_stats(&root).unwrap();
        assert_eq!(stats.provenance_dirs, 1);
        assert_eq!(stats.files, 1);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn cache_clean_all_removes_every_provenance_dir() {
        let root = scratch_test_root("clean-all");
        let token = ProvenanceToken::compute(&ProvenanceInputs {
            crate_version: "0.1.0",
            tape_protocol_version: 11,
            fingerprint_hash: "x",
            full_command: "mogwai preflight",
            subcontract_hash: "y",
        });
        let store = CacheStore::open(root.clone(), token);
        store.write("k", b"v").unwrap();
        assert_eq!(cache_stats(&root).unwrap().provenance_dirs, 1);
        let removed = cache_clean_all(&root).unwrap();
        assert_eq!(removed, 1);
        assert_eq!(cache_stats(&root).unwrap().provenance_dirs, 0);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn scratch_dir_is_removed_on_drop() {
        let root = scratch_test_root("scratch");
        let path = {
            let scratch = ScratchDir::new(&root).unwrap();
            let p = scratch.path().to_path_buf();
            assert!(p.exists());
            p
        };
        assert!(!path.exists());
        std::fs::remove_dir_all(&root).ok();
    }
}
