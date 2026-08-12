// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Identity binding (`analysis/mnq_fit.py` spec 4.1): ledger + manifest +
//! rehash, before a byte of CSV. The lab reads `analysis/databento-jobs.json`
//! READ-ONLY (per AGENTS.md/the phase-1 brief) - nothing here ever writes it.

use std::collections::{BTreeMap, HashSet};
use std::io::Read;
use std::path::Path;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::error::{LabError, LabResult};
use crate::subcontract::{JOB_ID, LEDGER_KEY};

/// Return the commit that exactly identifies the code about to produce an
/// artifact. Dirty or unidentifiable trees fail closed.
pub fn require_clean_tree() -> LabResult<String> {
    let status = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .output()?;
    if !status.status.success() {
        return Err(LabError::Harness(
            "git status failed; the harness tree is unidentifiable".to_string(),
        ));
    }
    if !String::from_utf8_lossy(&status.stdout).trim().is_empty() {
        return Err(LabError::Harness(
            "the working tree is dirty; an artifact may only bind a commit that is exactly the code that ran - commit first".to_string(),
        ));
    }
    let head = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()?;
    if !head.status.success() {
        return Err(LabError::Harness(
            "git rev-parse failed; the harness tree is unidentifiable".to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&head.stdout).trim().to_string())
}

/// Read HEAD and cleanliness afresh immediately before an artifact write.
pub fn fresh_tree_state() -> LabResult<(String, bool)> {
    let status = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .output()?;
    let head = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()?;
    let clean = status.status.success()
        && String::from_utf8_lossy(&status.stdout).trim().is_empty()
        && head.status.success();
    Ok((
        String::from_utf8_lossy(&head.stdout).trim().to_string(),
        clean,
    ))
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
    files: BTreeMap<String, String>,
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
    let manifest_files = &manifest.files;
    if ledger_files.is_empty() {
        return Err(LabError::refusal(
            "the ledger entry carries no file inventory",
        ));
    }
    if ledger_files != manifest_files {
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
