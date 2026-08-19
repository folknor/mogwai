// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Identity binding (`analysis/mnq_fit.py` spec 4.1): ledger + manifest +
//! rehash, before a byte of CSV. The lab reads `analysis/databento-jobs.json`
//! READ-ONLY (per AGENTS.md/the phase-1 brief) - nothing here ever writes it.

#[cfg(any(test, feature = "test-seam"))]
use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::io::Read;
use std::path::Path;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::error::{LabError, LabResult};
use crate::subcontract::{JOB_ID, LEDGER_KEY};

/// Which of the two git reads the tree gate performs. The gate's whole
/// contract is expressed in the ORDER of these: a run that reads `Head`
/// without having read `Status` first would bind a commit it never checked,
/// and a run that reads neither never consulted the tree at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeQuery {
    /// `git status --porcelain`.
    Status,
    /// `git rev-parse HEAD`.
    Head,
}

/// One git command's result, in the terms the gate actually reasons about:
/// did the command succeed, and what did it print. Modelling the RAW output
/// rather than a `clean: bool` verdict is deliberate - a double that returned
/// the verdict would move the gate's own logic into the double and leave the
/// production decision untested.
#[derive(Debug, Clone)]
pub struct TreeReading {
    pub success: bool,
    pub stdout: String,
}

/// The seam the tree gate reads through. Production is [`GitTreeOracle`];
/// tests install a [`ScriptedTree`] so a refusal can be provoked in either
/// direction without a git process and without depending on whatever state
/// the developer's working tree happens to be in.
pub trait TreeOracle {
    fn read(&self, query: TreeQuery) -> LabResult<TreeReading>;
}

/// The real reader: the two git commands this gate has always run.
pub struct GitTreeOracle;

impl TreeOracle for GitTreeOracle {
    fn read(&self, query: TreeQuery) -> LabResult<TreeReading> {
        let args: &[&str] = match query {
            TreeQuery::Status => &["status", "--porcelain"],
            TreeQuery::Head => &["rev-parse", "HEAD"],
        };
        let output = std::process::Command::new("git").args(args).output()?;
        Ok(TreeReading {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        })
    }
}

/// THE SEAM DOES NOT SHIP, and that is the point of the cfg rather than a
/// tidiness preference. `require_clean_tree` is an INTEGRITY GATE: its return
/// value is written into an artifact's `binding.harness_tree_commit` beside
/// `"clean_tree": true`, and `fresh_tree_state` is the pre-write
/// re-attestation of the same claim. An injectable reader exported
/// unconditionally would let ANY caller of this library install a double that
/// answers "clean, at deadbeef" and have both ends of that fail-closed
/// contract fabricate a binding from one install. `mogwai-cli` depends on
/// `mogwai-lab` normally, so an unconditional export lands in
/// `target/release/mogwai`.
///
/// So the whole module below is behind `test-seam`, which `mogwai-cli`
/// enables ONLY in `[dev-dependencies]`. Under resolver 3 a dev-dependency's
/// features are not unified into a build that compiles no test target, so
/// `cargo build --release -p mogwai-cli` gets a `read_tree` that is a direct
/// call to `GitTreeOracle` with no thread-local and no installation point at
/// all. `test` is in the cfg as well so this crate's own unit tests need no
/// self-dependency to reach it.
///
/// The gate is belt AND braces: `--all-features` would switch this on in a
/// release build too, so the two artifact writers in `mogwai-cli` also refuse
/// outright when [`tree_readings_are_production`] is false. Neither guard
/// alone is enough - the cfg can be defeated by a feature flag, and the
/// runtime check is only as good as the call sites that remember it.
#[cfg(any(test, feature = "test-seam"))]
mod seam {
    use super::{GitTreeOracle, LabResult, TreeOracle, TreeQuery, TreeReading};
    use std::cell::RefCell;
    use std::rc::Rc;

    thread_local! {
        /// THREAD-LOCAL, and the scope is exactly right: libtest runs every
        /// test on its own thread, so an installed double cannot leak into a
        /// sibling test running in parallel, and the artifact commands read
        /// the tree on the thread that entered `run` rather than on a worker.
        static INSTALLED: RefCell<Option<Rc<dyn TreeOracle>>> = const { RefCell::new(None) };
    }

    /// Restores whatever oracle was installed before, so an installation nests
    /// and cannot strand a double on the thread for later tests.
    pub struct TreeOracleGuard {
        previous: Option<Rc<dyn TreeOracle>>,
    }

    impl Drop for TreeOracleGuard {
        fn drop(&mut self) {
            let previous = self.previous.take();
            // `try_with`, not `with`: a guard dropped during thread-local
            // teardown would otherwise panic with "access after destruction"
            // while unwinding. Nothing reaches that today - the guards are
            // stack-locals inside test bodies - but a destructor that can
            // panic on a path nobody chose is not worth keeping for one
            // saved call.
            let _teardown: Result<(), std::thread::AccessError> =
                INSTALLED.try_with(|slot| *slot.borrow_mut() = previous);
        }
    }

    /// Install a tree-state reader for this thread until the returned guard
    /// drops. Generic over the concrete oracle so a caller can keep its own
    /// handle - which the scripted double's query log is read through - and
    /// still hand one in here.
    #[must_use]
    pub fn install_tree_oracle(oracle: Rc<impl TreeOracle + 'static>) -> TreeOracleGuard {
        let erased: Rc<dyn TreeOracle> = oracle;
        let previous = INSTALLED.with(|slot| slot.borrow_mut().replace(erased));
        TreeOracleGuard { previous }
    }

    /// Whether the tree readings this thread would get come from `git` itself.
    /// The two artifact writers refuse when this is false: an artifact whose
    /// binding was attested by a scripted reader is a forged provenance
    /// record, and it must not be possible to produce one even in a build that
    /// compiled the seam in.
    #[must_use]
    pub fn tree_readings_are_production() -> bool {
        INSTALLED.with(|slot| slot.borrow().is_none())
    }

    pub(super) fn read_tree(query: TreeQuery) -> LabResult<TreeReading> {
        let installed = INSTALLED.with(|slot| slot.borrow().clone());
        match installed {
            Some(oracle) => oracle.read(query),
            None => GitTreeOracle.read(query),
        }
    }
}

#[cfg(any(test, feature = "test-seam"))]
use seam::read_tree;
#[cfg(any(test, feature = "test-seam"))]
pub use seam::{TreeOracleGuard, install_tree_oracle, tree_readings_are_production};

/// Without the seam there is nothing to install and nothing to ask: the gate
/// is the two git commands, as it was before the seam existed.
#[cfg(not(any(test, feature = "test-seam")))]
fn read_tree(query: TreeQuery) -> LabResult<TreeReading> {
    GitTreeOracle.read(query)
}

/// The production build has no way to install anything, so the answer is a
/// constant. It exists in both configurations so the call sites that refuse on
/// it need no cfg of their own.
#[cfg(not(any(test, feature = "test-seam")))]
#[must_use]
pub fn tree_readings_are_production() -> bool {
    true
}

/// A scripted tree state, serving the two git commands' REAL output shape and
/// RECORDING every query it was asked, in order.
///
/// BOTH READINGS ARE THE CALLER'S TO STATE. An earlier cut hardcoded the head
/// reading as a success, which made `unreadable()` model a failing
/// `git status` ONLY - so `require_clean_tree`'s fourth outcome, the
/// `rev-parse` failure, was unreachable through the double and untested, and
/// `fresh_tree_state`'s `head.success` term could be deleted with every test
/// still green. That is not a hypothetical shape: a repository with no commits
/// or an unborn HEAD prints nothing from `git status --porcelain` and SUCCEEDS
/// there while `git rev-parse HEAD` fails.
#[cfg(any(test, feature = "test-seam"))]
pub struct ScriptedTree {
    status: TreeReading,
    head: TreeReading,
    log: RefCell<Vec<TreeQuery>>,
}

#[cfg(any(test, feature = "test-seam"))]
impl ScriptedTree {
    /// A clean tree at `head`: `git status --porcelain` prints nothing.
    #[must_use]
    pub fn clean(head: &str) -> Self {
        Self::new(status_ok(""), head_ok(head))
    }

    /// A dirty tree: one porcelain line, the shape `git status --porcelain`
    /// really prints for a modified tracked file.
    #[must_use]
    pub fn dirty(head: &str) -> Self {
        Self::new(
            status_ok(" M crates/mogwai-lab/src/ledger.rs\n"),
            head_ok(head),
        )
    }

    /// A tree `git status` cannot report on at all - no repository, or a
    /// broken index. Both commands fail and print nothing.
    #[must_use]
    pub fn unreadable() -> Self {
        Self::new(failed(), failed())
    }

    /// A tree whose STATUS reads clean but whose HEAD does not resolve: an
    /// unborn branch, a repository with no commits, a broken HEAD ref. This is
    /// the only constructor that reaches `require_clean_tree`'s fourth
    /// outcome.
    #[must_use]
    pub fn head_unreadable() -> Self {
        Self::new(status_ok(""), failed())
    }

    /// The general constructor, for a caller replaying readings it obtained
    /// some other way.
    #[must_use]
    pub fn new(status: TreeReading, head: TreeReading) -> Self {
        Self {
            status,
            head,
            log: RefCell::new(Vec::new()),
        }
    }

    /// Every query this oracle was asked, in the order it was asked.
    #[must_use]
    pub fn queries(&self) -> Vec<TreeQuery> {
        self.log.borrow().clone()
    }
}

#[cfg(any(test, feature = "test-seam"))]
fn status_ok(stdout: &str) -> TreeReading {
    TreeReading {
        success: true,
        stdout: stdout.to_string(),
    }
}

#[cfg(any(test, feature = "test-seam"))]
fn head_ok(head: &str) -> TreeReading {
    TreeReading {
        success: true,
        stdout: format!("{head}\n"),
    }
}

#[cfg(any(test, feature = "test-seam"))]
fn failed() -> TreeReading {
    TreeReading {
        success: false,
        stdout: String::new(),
    }
}

#[cfg(any(test, feature = "test-seam"))]
impl TreeOracle for ScriptedTree {
    fn read(&self, query: TreeQuery) -> LabResult<TreeReading> {
        self.log.borrow_mut().push(query);
        Ok(match query {
            TreeQuery::Status => self.status.clone(),
            TreeQuery::Head => self.head.clone(),
        })
    }
}

/// Return the commit that exactly identifies the code about to produce an
/// artifact. Dirty or unidentifiable trees fail closed.
pub fn require_clean_tree() -> LabResult<String> {
    let status = read_tree(TreeQuery::Status)?;
    if !status.success {
        return Err(LabError::Harness(
            "git status failed; the harness tree is unidentifiable".to_string(),
        ));
    }
    if !status.stdout.trim().is_empty() {
        return Err(LabError::Harness(
            "the working tree is dirty; an artifact may only bind a commit that is exactly the code that ran - commit first".to_string(),
        ));
    }
    let head = read_tree(TreeQuery::Head)?;
    if !head.success {
        return Err(LabError::Harness(
            "git rev-parse failed; the harness tree is unidentifiable".to_string(),
        ));
    }
    Ok(head.stdout.trim().to_string())
}

/// Read HEAD and cleanliness afresh immediately before an artifact write.
pub fn fresh_tree_state() -> LabResult<(String, bool)> {
    let status = read_tree(TreeQuery::Status)?;
    let head = read_tree(TreeQuery::Head)?;
    let clean = status.success && status.stdout.trim().is_empty() && head.success;
    Ok((head.stdout.trim().to_string(), clean))
}

/// Lowercase hex of a digest. sha2 0.11 finalizes to a `hybrid_array::Array`,
/// which - unlike the 0.10 `GenericArray` - implements no `LowerHex`, so the
/// `{:x}` formatting every hash site used has to be spelled out once here.
pub fn hex_digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[usize::from(b >> 4)] as char);
        out.push(HEX[usize::from(b & 0x0f)] as char);
    }
    out
}

/// sha256 hex digest of an in-memory byte slice - the provenance token's
/// fingerprint-hash component hashes `analysis/fingerprint.json`'s bytes
/// this way rather than reading it as a whole file with [`sha256_file`].
pub fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex_digest(&hasher.finalize())
}

pub fn sha256_file(path: &Path) -> LabResult<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex_digest(&hasher.finalize()))
}

#[derive(Deserialize)]
struct LedgerFile {
    #[serde(default)]
    jobs: BTreeMap<String, LedgerJobEntry>,
}

#[derive(Deserialize)]
struct LedgerJobEntry {
    state: Option<String>,
    job_id: Option<String>,
    #[serde(default)]
    files: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct Manifest {
    job_id: Option<String>,
    #[serde(default)]
    files: ManifestFiles,
}

/// Two delivered-manifest generations exist on disk: the July-era map of
/// `filename -> sha256`, and the vendor-native batch list of objects whose
/// `hash` carries a `sha256:` prefix. Both normalize to the same map.
#[derive(Deserialize, Default)]
#[serde(untagged)]
enum ManifestFiles {
    #[default]
    #[serde(skip)]
    Empty,
    Map(BTreeMap<String, String>),
    List(Vec<ManifestFileEntry>),
}

#[derive(Deserialize)]
struct ManifestFileEntry {
    filename: String,
    hash: String,
}

impl ManifestFiles {
    fn normalized(&self) -> BTreeMap<String, String> {
        match self {
            ManifestFiles::Empty => BTreeMap::new(),
            ManifestFiles::Map(map) => map.clone(),
            ManifestFiles::List(entries) => entries
                .iter()
                .map(|entry| {
                    let hash = entry
                        .hash
                        .strip_prefix("sha256:")
                        .unwrap_or(&entry.hash)
                        .to_string();
                    (entry.filename.clone(), hash)
                })
                .collect(),
        }
    }

    fn is_vendor_list(&self) -> bool {
        matches!(self, Self::List(_))
    }
}

/// `verify_input`: ledger-entry state and job-id checks, ledger/manifest
/// inventory agreement, on-disk presence (a REGULAR file, not a directory or
/// dangling symlink), then a rehash of every `.csv.zst` against the ledger.
/// Returns the verified `{filename: sha256}` map.
pub fn verify_input(directory: &Path, ledger_path: &Path) -> LabResult<BTreeMap<String, String>> {
    verify_input_bound(directory, ledger_path, LEDGER_KEY, Some(JOB_ID))
}

/// Month-generic ledger verification for Stage M. The caller supplies the
/// exact seal-ledger entry key; the entry's job id is then bound to the
/// delivered manifest rather than to July's subcontract constant.
pub fn verify_input_entry(
    directory: &Path,
    ledger_path: &Path,
    ledger_key: &str,
) -> LabResult<BTreeMap<String, String>> {
    verify_input_bound(directory, ledger_path, ledger_key, None)
}

/// Return the job id carried by a verified ledger entry. Call this only after
/// [`verify_input_entry`] has established the ledger/manifest binding.
pub fn input_entry_job_id(ledger_path: &Path, ledger_key: &str) -> LabResult<String> {
    let ledger: LedgerFile = serde_json::from_str(&std::fs::read_to_string(ledger_path)?)?;
    ledger
        .jobs
        .get(ledger_key)
        .and_then(|entry| entry.job_id.clone())
        .ok_or_else(|| LabError::refusal(format!("ledger entry {ledger_key} carries no job id")))
}

fn verify_input_bound(
    directory: &Path,
    ledger_path: &Path,
    ledger_key: &str,
    expected_job: Option<&str>,
) -> LabResult<BTreeMap<String, String>> {
    let ledger_text = std::fs::read_to_string(ledger_path)?;
    let ledger: LedgerFile = serde_json::from_str(&ledger_text)?;
    let entry = ledger
        .jobs
        .get(ledger_key)
        .ok_or_else(|| LabError::refusal(format!("ledger carries no entry for {ledger_key}")))?;
    if entry.state.as_deref() != Some("downloaded") {
        return Err(LabError::refusal(format!(
            "ledger entry state is {:?}, not downloaded",
            entry.state
        )));
    }
    if expected_job.is_some_and(|job| entry.job_id.as_deref() != Some(job)) {
        return Err(LabError::refusal(format!(
            "ledger names job {:?}, the sub-contract binds {}",
            entry.job_id,
            expected_job.unwrap_or_default()
        )));
    }
    let manifest_path = directory.join("manifest.json");
    let manifest_text = std::fs::read_to_string(&manifest_path)?;
    let manifest: Manifest = serde_json::from_str(&manifest_text)?;
    if manifest.job_id != entry.job_id {
        return Err(LabError::refusal(format!(
            "manifest names job {:?}, not ledger job {:?}",
            manifest.job_id, entry.job_id
        )));
    }
    let ledger_files = &entry.files;
    let mut manifest_files = manifest.files.normalized();
    if manifest.files.is_vendor_list() {
        manifest_files.insert("manifest.json".to_string(), sha256_file(&manifest_path)?);
    }
    if ledger_files.is_empty() {
        return Err(LabError::refusal(
            "the ledger entry carries no file inventory",
        ));
    }
    if ledger_files != &manifest_files {
        let ledger_keys: HashSet<&String> = ledger_files.keys().collect();
        let manifest_keys: HashSet<&String> = manifest_files.keys().collect();
        let only_ledger: Vec<&&String> = {
            let mut v: Vec<&&String> = ledger_keys.difference(&manifest_keys).collect();
            v.sort();
            v
        };
        let only_manifest: Vec<&&String> = {
            let mut v: Vec<&&String> = manifest_keys.difference(&ledger_keys).collect();
            v.sort();
            v
        };
        let mut moved: Vec<&String> = ledger_keys
            .intersection(&manifest_keys)
            .filter(|n| ledger_files.get(**n) != manifest_files.get(**n))
            .copied()
            .collect();
        moved.sort();
        return Err(LabError::refusal(format!(
            "ledger and manifest inventories disagree (only ledger: {only_ledger:?}; only \
             manifest: {only_manifest:?}; hash mismatch: {moved:?}); the landing is not the \
             delivery the ledger recorded"
        )));
    }
    let on_disk: HashSet<String> = std::fs::read_dir(directory)?
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    let mut absent: Vec<&String> = ledger_files
        .keys()
        .filter(|n| !on_disk.contains(n.as_str()))
        .collect();
    absent.sort();
    if !absent.is_empty() {
        return Err(LabError::refusal(format!(
            "ledger inventory file(s) missing from disk: {absent:?}; the delivery is incomplete \
             and hashing the remainder proves nothing"
        )));
    }
    let mut hashes = BTreeMap::new();
    for path in crate::stream::data_files(directory)? {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        let expected = ledger_files.get(&name).ok_or_else(|| {
            LabError::refusal(format!("{name} is on disk but not in the ledger inventory"))
        })?;
        let actual = sha256_file(&path)?;
        if &actual != expected {
            return Err(LabError::refusal(format!(
                "{name}: sha256 {actual} does not match the ledger's {expected}; the bytes on \
                 disk are not the delivery"
            )));
        }
        hashes.insert(name, actual);
    }
    Ok(hashes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    /// Replays two readings verbatim, whatever they are - the vehicle for
    /// holding the scripted double against the REAL endpoint below.
    struct Replay {
        status: TreeReading,
        head: TreeReading,
    }

    impl TreeOracle for Replay {
        fn read(&self, query: TreeQuery) -> LabResult<TreeReading> {
            Ok(match query {
                TreeQuery::Status => self.status.clone(),
                TreeQuery::Head => self.head.clone(),
            })
        }
    }

    fn verdict() -> Result<String, String> {
        require_clean_tree().map_err(|e| e.to_string())
    }

    /// The three verdicts the gate can reach, and the QUERY SEQUENCE each one
    /// costs. The sequence is the ordering claim in its smallest form: a dirty
    /// or unreadable tree is refused on the status read alone, so nothing
    /// downstream of it - not even `rev-parse` - is reached.
    #[test]
    fn the_tree_gate_refuses_on_the_status_read_and_binds_only_after_it() {
        let dirty = Rc::new(ScriptedTree::dirty("cafebabe"));
        {
            let _guard = install_tree_oracle(Rc::clone(&dirty));
            let err = verdict().expect_err("a dirty tree is refused");
            assert!(err.contains("the working tree is dirty"), "{err}");
        }
        assert_eq!(dirty.queries(), vec![TreeQuery::Status]);

        let broken = Rc::new(ScriptedTree::unreadable());
        {
            let _guard = install_tree_oracle(Rc::clone(&broken));
            let err = verdict().expect_err("an unreadable tree is refused");
            assert!(err.contains("git status failed"), "{err}");
        }
        assert_eq!(broken.queries(), vec![TreeQuery::Status]);

        // The fourth outcome, and the one the double could not model at all
        // until it took both readings: `git status --porcelain` succeeds and
        // prints nothing while `git rev-parse HEAD` fails. A repository with
        // no commits is exactly this. The gate must refuse rather than bind
        // the empty string it would otherwise have trimmed out of an empty
        // stdout - which is what makes this branch worth reaching: the failure
        // it guards is a BINDING TO NOTHING, not an error.
        let unborn = Rc::new(ScriptedTree::head_unreadable());
        {
            let _guard = install_tree_oracle(Rc::clone(&unborn));
            let err = verdict().expect_err("an unresolvable HEAD is refused");
            assert!(err.contains("git rev-parse failed"), "{err}");
        }
        assert_eq!(unborn.queries(), vec![TreeQuery::Status, TreeQuery::Head]);

        let clean = Rc::new(ScriptedTree::clean("cafebabe"));
        {
            let _guard = install_tree_oracle(Rc::clone(&clean));
            assert_eq!(verdict().expect("a clean tree binds"), "cafebabe");
        }
        assert_eq!(clean.queries(), vec![TreeQuery::Status, TreeQuery::Head]);
    }

    /// The `head.success` term of `fresh_tree_state`'s conjunction, which is
    /// the mid-run re-attestation's half of the same fourth outcome. Deleting
    /// `&& head.success` there left every other test in this module green.
    #[test]
    fn a_clean_status_over_an_unresolvable_head_is_not_a_clean_reading() {
        let unborn = Rc::new(ScriptedTree::head_unreadable());
        let _guard = install_tree_oracle(Rc::clone(&unborn));
        let (head, clean) = fresh_tree_state().expect("a scripted reading");
        assert_eq!(head, "", "an unresolvable HEAD prints nothing");
        assert!(
            !clean,
            "a reading that cannot name the commit is not a clean reading, whatever status said"
        );
    }

    /// `fresh_tree_state` reads BOTH regardless, because its caller needs the
    /// HEAD it would have bound in order to say the tree moved.
    #[test]
    fn the_fresh_reading_reports_head_and_cleanliness_together() {
        let dirty = Rc::new(ScriptedTree::dirty("cafebabe"));
        let _guard = install_tree_oracle(Rc::clone(&dirty));
        let (head, clean) = fresh_tree_state().expect("a scripted reading");
        assert_eq!(head, "cafebabe");
        assert!(!clean);
        assert_eq!(dirty.queries(), vec![TreeQuery::Status, TreeQuery::Head]);
    }

    /// THE DOUBLE IS AUDITED AGAINST THE REAL ENDPOINT, not against what the
    /// tests want: the actual `git` readings for THIS tree are replayed
    /// through the seam and the gate must reach the same verdict it reaches
    /// against `GitTreeOracle` directly. Passes in EVERY tree state - clean,
    /// dirty, no repository, unborn HEAD - which is the whole point of the
    /// seam. It is the one test here allowed to consult the ambient tree,
    /// because comparing against it is its job.
    ///
    /// IT IS THE ONLY TEST IN THIS CRATE THAT SPAWNS A SUBPROCESS, which is a
    /// conscious cost against the fast, sandbox-safe framing of this crate's
    /// sweep. `git` NOT BEING ON PATH IS NOT A FAILURE - there is nothing to
    /// audit the double against, so the test says so and returns rather than
    /// reporting a defect in this workspace. Every OTHER outcome of the two
    /// commands, including both of them failing inside a directory that is not
    /// a repository, is a state this test compares in.
    #[test]
    fn the_scripted_double_reproduces_the_real_git_readings() {
        let (Ok(status), Ok(head)) = (
            GitTreeOracle.read(TreeQuery::Status),
            GitTreeOracle.read(TreeQuery::Head),
        ) else {
            eprintln!("git is not on PATH; the double has no real endpoint to be audited against");
            return;
        };
        let ambient = verdict();
        let replayed = {
            let _guard = install_tree_oracle(Rc::new(Replay {
                status: status.clone(),
                head: head.clone(),
            }));
            verdict()
        };
        assert_eq!(ambient, replayed, "the seam changed the gate's verdict");

        // And the scripted constructors stand in for those readings: whichever
        // state this tree is in, the matching constructor reaches the same
        // verdict as the replay of the real bytes.
        //
        // THE SELECTION IS ON BOTH READINGS' SUCCESS, not on the status bytes
        // alone. Selecting on the bytes made this test FALSELY RED on a
        // repository with no commits: `ambient` and `replayed` both refuse on
        // `rev-parse`, but `clean("")` binds the empty string, and the
        // mismatch would have been reported as the double failing to model the
        // tree when it was the selection failing to read it.
        let scripted = match (
            status.success,
            status.stdout.trim().is_empty(),
            head.success,
        ) {
            (false, _, _) => ScriptedTree::unreadable(),
            (true, true, false) => ScriptedTree::head_unreadable(),
            (true, true, true) => ScriptedTree::clean(head.stdout.trim()),
            (true, false, _) => ScriptedTree::dirty(head.stdout.trim()),
        };
        let from_script = {
            let _guard = install_tree_oracle(Rc::new(scripted));
            verdict()
        };
        assert_eq!(
            replayed, from_script,
            "the scripted constructor does not model the real reading"
        );
        // WHERE THIS SECOND ASSERTION BITES, stated because on a clean tree it
        // does not: `clean()`'s status bytes are the empty string, which is
        // bit-identical to what real `git status` printed, so both sides are
        // fed the same input by construction and the comparison is near
        // tautological. It carries real signal in the other three states,
        // where the constructor INVENTS the reading - `dirty()`'s porcelain
        // line is its own text, and `unreadable()` / `head_unreadable()`
        // synthesize a failure the real command reported some other way. A
        // clean gate machine therefore exercises the weakest of the four, and
        // that is a limit of running this comparison against whatever tree is
        // there rather than a gap that a rewrite closes.
    }

    /// An installation nests and is undone, so a test that installs a double
    /// cannot leave one behind for whatever runs next on the thread.
    #[test]
    fn an_installed_oracle_is_removed_when_its_guard_drops() {
        {
            let _outer = install_tree_oracle(Rc::new(ScriptedTree::clean("outer")));
            {
                let _inner = install_tree_oracle(Rc::new(ScriptedTree::clean("inner")));
                assert_eq!(verdict().expect("clean"), "inner");
                assert!(
                    !tree_readings_are_production(),
                    "an installed double must be visible to the artifact writers' refusal"
                );
            }
            assert_eq!(verdict().expect("clean"), "outer");
        }
        // Read through the same predicate the artifact writers refuse on, so
        // this pins the guard AND the check they depend on in one place.
        assert!(tree_readings_are_production());
    }
}
