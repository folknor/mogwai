// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Delivery identity binding (the retired protocol-10 fit spec, 4.1): jobs manifest +
//! delivered manifest + rehash, before a byte of CSV. The lab reads
//! `analysis/databento-jobs.json` read-only (per AGENTS.md/the phase-1 brief) -
//! nothing here ever writes it.

use std::collections::{BTreeMap, HashSet};
use std::io::Read;
use std::path::Path;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::error::{LabError, LabResult};
use crate::subcontract::{DELIVERY_KEY, JOB_ID};

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
struct JobsManifest {
    #[serde(default)]
    jobs: BTreeMap<String, DeliveryEntry>,
}

#[derive(Deserialize)]
struct DeliveryEntry {
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

/// `verify_input`: delivery state and job-id checks, agreement between the jobs
/// manifest's inventory and the delivered manifest's, on-disk presence (a
/// regular file, not a directory or dangling symlink), then a rehash of every
/// `.csv.zst` against the jobs manifest. Returns the verified
/// `{filename: sha256}` map.
pub fn verify_input(directory: &Path, jobs_manifest: &Path) -> LabResult<BTreeMap<String, String>> {
    verify_input_bound(directory, jobs_manifest, DELIVERY_KEY, Some(JOB_ID))
}

/// Month-generic delivery verification for Stage M. The caller supplies the
/// exact jobs-manifest delivery key - the Stage M preregistration calls it the
/// seal-ledger entry - and the delivery's job id is then bound to the delivered
/// manifest rather than to July's sub-contract constant.
pub fn verify_input_entry(
    directory: &Path,
    jobs_manifest: &Path,
    delivery_key: &str,
) -> LabResult<BTreeMap<String, String>> {
    verify_input_bound(directory, jobs_manifest, delivery_key, None)
}

/// Return the job id carried by a verified delivery. Call this only after
/// [`verify_input_entry`] has established the manifest binding.
pub fn input_entry_job_id(jobs_manifest: &Path, delivery_key: &str) -> LabResult<String> {
    let jobs: JobsManifest = serde_json::from_str(&std::fs::read_to_string(jobs_manifest)?)?;
    jobs.jobs
        .get(delivery_key)
        .and_then(|entry| entry.job_id.clone())
        .ok_or_else(|| LabError::refusal(format!("delivery {delivery_key} carries no job id")))
}

fn verify_input_bound(
    directory: &Path,
    jobs_manifest: &Path,
    delivery_key: &str,
    expected_job: Option<&str>,
) -> LabResult<BTreeMap<String, String>> {
    let jobs_text = std::fs::read_to_string(jobs_manifest)?;
    let jobs: JobsManifest = serde_json::from_str(&jobs_text)?;
    let entry = jobs.jobs.get(delivery_key).ok_or_else(|| {
        LabError::refusal(format!(
            "jobs manifest carries no delivery for {delivery_key}"
        ))
    })?;
    if entry.state.as_deref() != Some("downloaded") {
        return Err(LabError::refusal(format!(
            "delivery state is {:?}, not downloaded",
            entry.state
        )));
    }
    if expected_job.is_some_and(|job| entry.job_id.as_deref() != Some(job)) {
        return Err(LabError::refusal(format!(
            "jobs manifest names job {:?}, the sub-contract binds {}",
            entry.job_id,
            expected_job.unwrap_or_default()
        )));
    }
    let manifest_path = directory.join("manifest.json");
    let manifest_text = std::fs::read_to_string(&manifest_path)?;
    let manifest: Manifest = serde_json::from_str(&manifest_text)?;
    if manifest.job_id != entry.job_id {
        return Err(LabError::refusal(format!(
            "delivered manifest names job {:?}, not jobs-manifest job {:?}",
            manifest.job_id, entry.job_id
        )));
    }
    let delivery_files = &entry.files;
    let mut manifest_files = manifest.files.normalized();
    if manifest.files.is_vendor_list() {
        manifest_files.insert("manifest.json".to_string(), sha256_file(&manifest_path)?);
    }
    if delivery_files.is_empty() {
        return Err(LabError::refusal("the delivery carries no file inventory"));
    }
    if delivery_files != &manifest_files {
        let delivery_keys: HashSet<&String> = delivery_files.keys().collect();
        let manifest_keys: HashSet<&String> = manifest_files.keys().collect();
        let only_jobs_manifest: Vec<&&String> = {
            let mut v: Vec<&&String> = delivery_keys.difference(&manifest_keys).collect();
            v.sort();
            v
        };
        let only_manifest: Vec<&&String> = {
            let mut v: Vec<&&String> = manifest_keys.difference(&delivery_keys).collect();
            v.sort();
            v
        };
        let mut moved: Vec<&String> = delivery_keys
            .intersection(&manifest_keys)
            .filter(|n| delivery_files.get(**n) != manifest_files.get(**n))
            .copied()
            .collect();
        moved.sort();
        return Err(LabError::refusal(format!(
            "jobs and delivered manifests disagree (only jobs manifest: {only_jobs_manifest:?}; only \
             delivered manifest: {only_manifest:?}; hash mismatch: {moved:?}); the landing is \
             not the delivery the jobs manifest recorded"
        )));
    }
    let on_disk: HashSet<String> = std::fs::read_dir(directory)?
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    let mut absent: Vec<&String> = delivery_files
        .keys()
        .filter(|n| !on_disk.contains(n.as_str()))
        .collect();
    absent.sort();
    if !absent.is_empty() {
        return Err(LabError::refusal(format!(
            "delivery inventory file(s) missing from disk: {absent:?}; the delivery is incomplete \
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
        let expected = delivery_files.get(&name).ok_or_else(|| {
            LabError::refusal(format!("{name} is on disk but not in the jobs manifest"))
        })?;
        let actual = sha256_file(&path)?;
        if &actual != expected {
            return Err(LabError::refusal(format!(
                "{name}: sha256 {actual} does not match the jobs manifest's {expected}; the bytes on \
                 disk are not the delivery"
            )));
        }
        hashes.insert(name, actual);
    }
    Ok(hashes)
}
