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
//!   CURRENT token - are unreachable by construction and pruned before a
//!   normal write or once before a prepared parallel batch;
//!   [`CacheStore::clean_stale`] covers the manual case.
//! - SCRATCH: a run-scoped [`ScratchDir`] under the cache root, deleted on
//!   `Drop` (clean exit), ignorable on crash - the filesystem will not mind
//!   an orphaned scratch directory the way the 14,288-file
//!   `mnq-fit-scratch` mind minded a human.
//!
//! Phase 1 lands no cache PRODUCERS (measure12a's walk cache etc. are later
//! phases), so this module's own tests are synthetic - it proves the policy
//! mechanism, not any particular cache's contents.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

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

    /// A token an OPERATOR named, as printed by `mogwai cache stats
    /// --entries`. It is not computed from inputs and is not validated
    /// against any: it is a directory name to keep, and the only caller is
    /// `cache clean --stale --keep`. A token this process could compute for
    /// itself must go through [`Self::compute`] instead - the whole reason
    /// this exists is that `cache` has no producer's inputs to compute one
    /// from, and synthesizing one anyway is what made `clean --stale` delete
    /// the entire cache.
    #[must_use]
    pub fn named(token: &str) -> Self {
        Self(token.to_string())
    }
}

/// The provenance directory names under the cache root, sorted - the
/// candidates for `cache clean --stale --keep`.
pub fn cache_entry_tokens(root: &Path) -> io::Result<Vec<String>> {
    let entries_root = root.join("entries");
    let read = match std::fs::read_dir(&entries_root) {
        Ok(r) => r,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut tokens = Vec::new();
    for entry in read {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            tokens.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    tokens.sort();
    Ok(tokens)
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

/// The three environment readings a cache-root decision consults, ALREADY
/// READ. Taking them as data rather than reading them inside the precedence
/// rule is what makes that rule testable: every limb below the first needs a
/// different environment to exercise, and mutating the process environment is
/// unsound in a multi-threaded test binary, so a rule that reads for itself
/// can only ever have its `--cache-dir` limb covered.
///
/// NOT `pub`: it exists so the rule can be exercised, and the only exerciser is
/// `mod tests` in this file. A testability helper that leaves the crate is a
/// seam anyone can reach for in production.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CacheEnv<'a> {
    pub mogwai_cache_dir: Option<&'a str>,
    pub xdg_cache_home: Option<&'a str>,
    pub home: Option<&'a str>,
}

/// The owned form of [`CacheEnv`]: the three ambient readings, each bound to
/// the field it feeds BY NAME.
///
/// The naming is the point. This was a bare `(Option<String>, Option<String>,
/// Option<String>)` for one round, with `cache_root` doing the positional
/// unpack - three same-typed values in an unnamed tuple, and nothing in the
/// suite could see a transposition, because the only wrapper assertion returns
/// on the `--cache-dir` limb before any reading is consulted. Swapping the
/// `HOME` and `XDG_CACHE_HOME` slots left every test green while resolving a
/// real run's cache to `~/mogwai`. Making the rule pure moved the impurity
/// here rather than removing it; a named struct at both ends removes the hop
/// instead of relocating it.
struct AmbientCacheEnv {
    mogwai_cache_dir: Option<String>,
    xdg_cache_home: Option<String>,
    home: Option<String>,
}

impl AmbientCacheEnv {
    /// The ambient reading, taken once at the edge.
    fn read() -> Self {
        Self {
            mogwai_cache_dir: std::env::var("MOGWAI_CACHE_DIR").ok(),
            xdg_cache_home: std::env::var("XDG_CACHE_HOME").ok(),
            home: std::env::var("HOME").ok(),
        }
    }

    fn borrowed(&self) -> CacheEnv<'_> {
        CacheEnv {
            mogwai_cache_dir: self.mogwai_cache_dir.as_deref(),
            xdg_cache_home: self.xdg_cache_home.as_deref(),
            home: self.home.as_deref(),
        }
    }
}

/// The precedence rule, pure: `--cache-dir` first, then `MOGWAI_CACHE_DIR`,
/// then XDG (`$XDG_CACHE_HOME/mogwai` or `~/.cache/mogwai`). An EMPTY variable
/// counts as unset at every limb - an exported-but-blank variable is a
/// configuration accident, not a request to cache at the filesystem root.
///
/// `pub(crate)` for the same reason [`CacheEnv`] is: `cache_root` is the whole
/// public surface, and nothing outside this file has ever named either.
#[must_use]
pub(crate) fn cache_root_from(cli_override: Option<&Path>, env: CacheEnv<'_>) -> PathBuf {
    if let Some(p) = cli_override {
        return p.to_path_buf();
    }
    if let Some(v) = env.mogwai_cache_dir.filter(|v| !v.is_empty()) {
        return PathBuf::from(v);
    }
    if let Some(v) = env.xdg_cache_home.filter(|v| !v.is_empty()) {
        return Path::new(v).join("mogwai");
    }
    if let Some(home) = env.home.filter(|v| !v.is_empty()) {
        return Path::new(home).join(".cache").join("mogwai");
    }
    // No HOME and no XDG override: a relative fallback beats panicking. Real
    // deployments always carry HOME; this only matters in a stripped test
    // sandbox.
    PathBuf::from(".mogwai-cache")
}

/// [`cache_root_from`] against the ambient environment. This wrapper is the
/// only thing here that reads it, and it holds no logic of its own - not even
/// a field mapping, which is why [`AmbientCacheEnv`] carries the names.
pub fn cache_root(cli_override: Option<&Path>) -> PathBuf {
    let ambient = AmbientCacheEnv::read();
    cache_root_from(cli_override, ambient.borrowed())
}

/// A provenance-keyed cache directory under the cache root's `entries/`
/// subdirectory - `scratch/` is reserved for [`ScratchDir`], so the two
/// classes can never collide on a name.
#[derive(Clone)]
pub struct CacheStore {
    root: PathBuf,
    token: ProvenanceToken,
}

static CACHE_WRITE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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
    /// sibling (stale-provenance) directory first.
    pub fn write(&self, key: &str, bytes: &[u8]) -> io::Result<()> {
        self.prepare_for_writes()?;
        self.write_prepared(key, bytes)
    }

    /// Prunes stale provenance and creates the current directory before a
    /// batch of writes. Parallel producers call this once on the coordinator,
    /// rather than racing a recursive stale-directory scan on every entry.
    pub fn prepare_for_writes(&self) -> io::Result<()> {
        self.clean_stale()?;
        let dir = self.provenance_dir();
        std::fs::create_dir_all(dir)
    }

    /// Atomically publishes one entry after [`Self::prepare_for_writes`].
    /// Unique staging names make distinct concurrent writes independent; if
    /// two processes compute the same key, either complete identical entry is
    /// visible and a reader can never observe a partial JSON document.
    pub fn write_prepared(&self, key: &str, bytes: &[u8]) -> io::Result<()> {
        let dir = self.provenance_dir();
        let sequence = CACHE_WRITE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let staged = dir.join(format!(".{key}.{}.{}.tmp", std::process::id(), sequence));
        let result = (|| {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&staged)?;
            file.write_all(bytes)?;
            drop(file);
            std::fs::rename(&staged, dir.join(key))
        })();
        if result.is_err() {
            drop(std::fs::remove_file(staged));
        }
        result
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

/// The scratch root for this CRATE'S LIB UNIT TESTS, and the only shape they
/// may use: a `ScratchDir` under the workspace `target/`, whose UNIQUE LEAF is
/// removed on drop. Everything written by the test goes in that leaf, so the
/// drop removes all of it; what survives is the empty
/// `target/lab-unit-scratch/<name>/scratch/` spine, which is a fixed set of
/// empty directories rather than an accumulation. Stated exactly because the
/// obvious reading - "the scratch is removed on drop" - would have a reader
/// expect `target/lab-unit-scratch` to disappear, and it does not.
///
/// `CARGO_TARGET_TMPDIR` is NOT the answer here, and reaching for it is how
/// both hand-rolled scratch shapes in this crate - `cadence.rs`'s probe
/// fixture and this module's own four-call-site `scratch_test_root` - ended up
/// writing to the system temp directory, against the project convention that
/// all data lives in the tree.
/// Cargo defines it for INTEGRATION test targets only, so in a lib unit test
/// `env::var` always fails and any `env::temp_dir()` fallback beside it is
/// the branch that always runs - silently, since the fallback reads as the
/// exceptional case. `CARGO_MANIFEST_DIR` is defined for every compilation,
/// which is why the path is derived from it instead.
///
/// The unique leaf carries the pid and a nanosecond stamp (`ScratchDir::new`),
/// so a second test under the same `name` is unrepresentable rather than
/// merely unlikely. HOLD THE GUARD for the test's lifetime: it is what
/// removes the directory, including on the panic path, which a trailing
/// `remove_dir_all` at the end of a test body never does.
#[cfg(test)]
pub(crate) fn unit_test_scratch(name: &str) -> ScratchDir {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/lab-unit-scratch")
        .join(name);
    ScratchDir::new(&base).expect("creating a unit-test scratch directory")
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.path).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All four limbs, in every ordering that matters, against readings
    /// supplied as data - so the three below `--cache-dir` are covered
    /// without touching the process environment.
    #[test]
    fn cache_root_prefers_override_then_env_then_xdg() {
        let cli = PathBuf::from("/explicit/cache");
        let full = CacheEnv {
            mogwai_cache_dir: Some("/from/mogwai-var"),
            xdg_cache_home: Some("/from/xdg"),
            home: Some("/from/home"),
        };

        // The override wins over every reading, not merely over an empty one.
        assert_eq!(cache_root_from(Some(&cli), full), cli);
        // Each limb wins once the ones above it are gone, and each ADDS its
        // own suffix - `MOGWAI_CACHE_DIR` is the root itself, XDG gets
        // `mogwai`, HOME gets `.cache/mogwai`.
        assert_eq!(
            cache_root_from(None, full),
            PathBuf::from("/from/mogwai-var")
        );
        assert_eq!(
            cache_root_from(
                None,
                CacheEnv {
                    mogwai_cache_dir: None,
                    ..full
                }
            ),
            PathBuf::from("/from/xdg/mogwai")
        );
        assert_eq!(
            cache_root_from(
                None,
                CacheEnv {
                    home: Some("/from/home"),
                    ..CacheEnv::default()
                }
            ),
            PathBuf::from("/from/home/.cache/mogwai")
        );
        assert_eq!(
            cache_root_from(None, CacheEnv::default()),
            PathBuf::from(".mogwai-cache")
        );

        // An exported-but-empty variable is unset at every limb, and falls
        // through to the next one rather than resolving to a bare suffix.
        assert_eq!(
            cache_root_from(
                None,
                CacheEnv {
                    mogwai_cache_dir: Some(""),
                    ..full
                }
            ),
            PathBuf::from("/from/xdg/mogwai")
        );
        assert_eq!(
            cache_root_from(
                None,
                CacheEnv {
                    mogwai_cache_dir: Some(""),
                    xdg_cache_home: Some(""),
                    home: Some("/from/home"),
                }
            ),
            PathBuf::from("/from/home/.cache/mogwai")
        );
        assert_eq!(
            cache_root_from(
                None,
                CacheEnv {
                    mogwai_cache_dir: Some(""),
                    xdg_cache_home: Some(""),
                    home: Some(""),
                }
            ),
            PathBuf::from(".mogwai-cache")
        );

        // And the ambient wrapper still routes the override, which is the one
        // limb it can be asked about without mutating the environment.
        assert_eq!(cache_root(Some(&cli)), cli);
    }

    /// The READ SITE, which the test above cannot see: `cache_root(Some(..))`
    /// returns on its first line, so every assertion up there is satisfied
    /// whatever `AmbientCacheEnv::read` puts in which field.
    ///
    /// The variable names are written out as literals here rather than taken
    /// from the reader, because taking them from the reader is what pins an
    /// implementation against itself. A transposed read site - `home` filled
    /// from `XDG_CACHE_HOME`, say - fails this the moment the two readings
    /// differ, which on any real machine they do.
    #[test]
    fn the_ambient_reading_binds_each_variable_to_its_own_limb() {
        let ambient = AmbientCacheEnv::read();
        assert_eq!(
            ambient.mogwai_cache_dir,
            std::env::var("MOGWAI_CACHE_DIR").ok(),
            "the MOGWAI_CACHE_DIR limb is not fed from MOGWAI_CACHE_DIR"
        );
        assert_eq!(
            ambient.xdg_cache_home,
            std::env::var("XDG_CACHE_HOME").ok(),
            "the XDG limb is not fed from XDG_CACHE_HOME"
        );
        assert_eq!(
            ambient.home,
            std::env::var("HOME").ok(),
            "the HOME limb is not fed from HOME"
        );
        // Say so when the ambient environment cannot tell the three apart,
        // rather than letting a green result imply a check that did not
        // happen. A stripped sandbox with none of the three set is the case.
        if ambient.mogwai_cache_dir == ambient.xdg_cache_home
            && ambient.xdg_cache_home == ambient.home
        {
            eprintln!(
                "note: all three cache-root readings are identical here, so this \
                 assertion cannot discriminate a transposed read site"
            );
        }
        // The borrow hop is by name too, and it is the other half of the path
        // `cache_root` walks - checked against the owned fields it came from.
        let borrowed = ambient.borrowed();
        assert_eq!(
            borrowed.mogwai_cache_dir,
            ambient.mogwai_cache_dir.as_deref()
        );
        assert_eq!(borrowed.xdg_cache_home, ambient.xdg_cache_home.as_deref());
        assert_eq!(borrowed.home, ambient.home.as_deref());
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
        let scratch = unit_test_scratch("prune");
        let root = scratch.path().to_path_buf();
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
    }

    #[test]
    fn prepared_cache_writes_publish_complete_entries_concurrently() {
        let scratch = unit_test_scratch("parallel-writes");
        let root = scratch.path().to_path_buf();
        let token = ProvenanceToken::compute(&ProvenanceInputs {
            crate_version: "0.1.0",
            tape_protocol_version: 12,
            fingerprint_hash: "parallel",
            full_command: "arrival-screen",
            subcontract_hash: "measure",
        });
        let store = CacheStore::open(root.clone(), token);
        store.prepare_for_writes().expect("prepared cache");
        std::thread::scope(|scope| {
            for index in 0..8 {
                let store = store.clone();
                scope.spawn(move || {
                    let key = format!("seed-{index}.json");
                    let bytes = format!("{{\"seed\":{index}}}");
                    store
                        .write_prepared(&key, bytes.as_bytes())
                        .expect("atomic cache write");
                });
            }
        });
        for index in 0..8 {
            let key = format!("seed-{index}.json");
            let bytes = store.read(&key).expect("cache read").expect("entry");
            assert_eq!(bytes, format!("{{\"seed\":{index}}}").as_bytes());
        }
        let staged: Vec<_> = std::fs::read_dir(store.provenance_dir())
            .expect("cache directory")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(staged.is_empty(), "staged cache files remain: {staged:?}");
    }

    #[test]
    fn cache_clean_all_removes_every_provenance_dir() {
        let scratch = unit_test_scratch("clean-all");
        let root = scratch.path().to_path_buf();
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
    }

    #[test]
    fn scratch_dir_is_removed_on_drop() {
        let outer = unit_test_scratch("scratch");
        let root = outer.path().to_path_buf();
        let path = {
            let scratch = ScratchDir::new(&root).unwrap();
            let p = scratch.path().to_path_buf();
            assert!(p.exists());
            p
        };
        assert!(!path.exists());
    }
}
