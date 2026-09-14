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

/// `analysis/databento-jobs.json`. Every key the ledger carries is named, the
/// ones this module never consults under underscore bindings, so a renamed
/// `state` or `files` refuses instead of decoding to `None` or an empty map.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JobsManifest {
    #[serde(rename = "_version")]
    _version: u32,
    jobs: BTreeMap<String, DeliveryEntry>,
}

/// One ledger entry. Two entry generations exist: the submitted form
/// (`compression`, `encoding`, `planned_quote`, `split_duration`,
/// `submitted_at`) and the reconciled form (`intent_at`,
/// `live_quote_at_intent`, `reconciled_at`), so those keys are optional.
/// Every entry of both generations carries `state`, `job_id`, `files`,
/// `schema`, `scope`, `window` and `live_quote_at_submit`, so those are
/// required: an absent one refuses at decode, naming the key, rather than
/// reaching the verifier as `None` or an empty inventory.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeliveryEntry {
    state: String,
    job_id: String,
    files: BTreeMap<String, String>,
    #[serde(rename = "schema")]
    _schema: String,
    #[serde(rename = "scope")]
    _scope: String,
    #[serde(rename = "window")]
    _window: String,
    #[serde(rename = "live_quote_at_submit")]
    _live_quote_at_submit: f64,
    #[serde(rename = "compression", default)]
    _compression: Option<String>,
    #[serde(rename = "encoding", default)]
    _encoding: Option<String>,
    #[serde(rename = "planned_quote", default)]
    _planned_quote: Option<f64>,
    #[serde(rename = "split_duration", default)]
    _split_duration: Option<String>,
    #[serde(rename = "submitted_at", default)]
    _submitted_at: Option<String>,
    #[serde(rename = "intent_at", default)]
    _intent_at: Option<String>,
    #[serde(rename = "live_quote_at_intent", default)]
    _live_quote_at_intent: Option<f64>,
    #[serde(rename = "reconciled_at", default)]
    _reconciled_at: Option<String>,
}

/// The vendor's delivered `manifest.json`. Deliberately tolerant (no
/// `deny_unknown_fields`, optional `job_id`, defaulted `files`): it is a
/// third-party document, and no test, fixture or document in this tree
/// records its full shape, so there is nothing to type it strictly against.
/// The tolerance is bounded by what follows: a missing `job_id` cannot equal
/// the ledger's required one, and missing `files` cannot equal a non-empty
/// inventory.
#[derive(Deserialize)]
struct Manifest {
    job_id: Option<String>,
    #[serde(default)]
    files: ManifestFiles,
}

/// Two delivered-manifest generations exist on disk: the July-era map of
/// `filename -> sha256`, and the vendor-native batch list of objects whose
/// `hash` carries a `sha256:` prefix. Both normalize to the same map.
///
/// The generation is chosen by the JSON shape rather than by `untagged`: an
/// untagged enum tries each variant in turn and reports only that none
/// matched, so a malformed entry in either generation would surface as an
/// opaque "did not match any variant" instead of the field that is wrong.
#[derive(Default)]
enum ManifestFiles {
    #[default]
    Empty,
    Map(BTreeMap<String, String>),
    List(Vec<ManifestFileEntry>),
}

impl<'de> Deserialize<'de> for ManifestFiles {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        match serde_json::Value::deserialize(deserializer)? {
            map @ serde_json::Value::Object(_) => serde_json::from_value(map)
                .map(Self::Map)
                .map_err(D::Error::custom),
            list @ serde_json::Value::Array(_) => serde_json::from_value(list)
                .map(Self::List)
                .map_err(D::Error::custom),
            other => Err(D::Error::custom(format!(
                "manifest files must be a filename map or a vendor file list, not {other}"
            ))),
        }
    }
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
        .map(|entry| entry.job_id.clone())
        .ok_or_else(|| {
            LabError::refusal(format!(
                "jobs manifest carries no delivery for {delivery_key}"
            ))
        })
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
    if entry.state != "downloaded" {
        return Err(LabError::refusal(format!(
            "delivery state is {:?}, not downloaded",
            entry.state
        )));
    }
    if expected_job.is_some_and(|job| entry.job_id != job) {
        return Err(LabError::refusal(format!(
            "jobs manifest names job {:?}, the sub-contract binds {}",
            entry.job_id,
            expected_job.unwrap_or_default()
        )));
    }
    let manifest_path = directory.join("manifest.json");
    let manifest_text = std::fs::read_to_string(&manifest_path)?;
    let manifest: Manifest = serde_json::from_str(&manifest_text)?;
    if manifest.job_id.as_deref() != Some(entry.job_id.as_str()) {
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

#[cfg(test)]
mod tests {
    use super::*;

    const LEDGER: &str = include_str!("../../../analysis/databento-jobs.json");

    /// A reconciled-generation entry with every key present, as a JSON object
    /// a test can remove one key from.
    fn entry() -> serde_json::Value {
        serde_json::json!({
            "files": {"manifest.json": "00"},
            "intent_at": "2026-08-05T09:22:34+00:00",
            "job_id": "GLBX-20260805-JUBCRPRLG8",
            "live_quote_at_intent": 24.0,
            "live_quote_at_submit": 24.0,
            "reconciled_at": "2026-08-05T09:23:54+00:00",
            "schema": "trades",
            "scope": "pairv",
            "state": "downloaded",
            "window": "2026-07.2wk"
        })
    }

    fn decode(entry: &serde_json::Value) -> Result<JobsManifest, serde_json::Error> {
        serde_json::from_value(serde_json::json!({"_version": 1, "jobs": {"k": entry}}))
    }

    #[test]
    fn the_committed_ledger_decodes_under_the_strict_entry() {
        let jobs: JobsManifest =
            serde_json::from_str(LEDGER).expect("the committed ledger decodes");
        assert!(!jobs.jobs.is_empty());
        assert!(
            decode(&entry()).is_ok(),
            "the test entry is itself well formed"
        );
    }

    #[test]
    fn a_ledger_entry_missing_a_required_key_refuses_naming_it() {
        for key in ["state", "job_id", "files"] {
            let mut stripped = entry();
            stripped.as_object_mut().expect("an object").remove(key);
            let Err(error) = decode(&stripped) else {
                panic!("an entry without {key} must refuse at decode");
            };
            assert!(
                error
                    .to_string()
                    .contains(&format!("missing field `{key}`")),
                "the refusal must name {key}: {error}"
            );
        }
    }

    #[test]
    fn a_ledger_without_a_jobs_map_refuses() {
        let error = serde_json::from_str::<JobsManifest>(r#"{"_version": 1}"#)
            .err()
            .expect("a ledger without jobs must refuse");
        assert!(
            error.to_string().contains("missing field `jobs`"),
            "{error}"
        );
    }
}
