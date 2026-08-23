// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! `run_preflight`/`mode_preflight` (`analysis/mnq_fit.py` spec 4.1): the
//! fail-closed input-contract pass over the delivered TBBO corpus, and the
//! artifact it persists. The parity gate for phase 1 is this module
//! reproducing `analysis/out/mnq-fit-preflight.json` value-identically.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

use serde::Serialize;
use serde_json::{Map, Value};

use crate::error::{LabError, LabResult};
use crate::session::{MinuteFieldsCache, ScheduleFrame, SessionBounds};
use crate::stream::{data_files, parse_stream};
use crate::subcontract::{
    self, EXPECTED_FULL_SESSIONS, JOB_ID, MAX_EXCLUDED_SESSIONS, MAX_INVALID_WIDTH_SHARE,
    MAX_UNSIDED_SHARE, MIN_USABLE_SESSIONS, MIN_VALID_PARENT_QUOTE_SHARE, SESSION_INVENTORY,
};

#[derive(Default)]
struct SessionState {
    rows: u64,
    ids: std::collections::HashSet<String>,
    invalid_books: u64,
}

#[derive(Serialize)]
pub struct PreflightArtifact {
    pub job_id: String,
    /// The serialized spelling every preflight artifact on disk already
    /// carries. Nothing committed pins it - no committed artifact and no hash
    /// holds the key - so this comment is the whole of the pin, and the reader
    /// of those artifacts is a person rather than a gate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ledger_key: Option<String>,
    pub file_hashes: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inventory_provenance: Option<InventoryProvenance>,
    pub subcontract_hash: String,
    pub rows: u64,
    pub unsided: u64,
    pub unsided_share: f64,
    pub book_counts: BTreeMap<String, u64>,
    pub invalid_width_share: f64,
    pub parents_seen: u64,
    pub valid_parent_quote_share: f64,
    pub rows_outside_declared_sessions: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule_frame: Option<ScheduleFrameRecord>,
    pub sessions: BTreeMap<String, SessionRecord>,
    /// `[label, reason]` pairs, matching the Python side's list-of-2-tuples.
    pub excluded_sessions: Vec<(String, String)>,
    pub usable_sessions: Vec<String>,
}

#[derive(Serialize)]
pub struct ScheduleFrameRecord {
    pub rule: String,
    pub timezone_authority: String,
    pub timezone_authority_sha256: String,
    pub calendar_inventory_schedule_independence: String,
    pub scheduled_exposure_seconds: i64,
    pub per_session_bounds: Vec<SessionBounds>,
}

#[derive(Serialize)]
pub struct InventoryProvenance {
    pub calendar_artifact: String,
    pub calendar_artifact_hash: String,
    pub month: String,
}

#[derive(Serialize)]
pub struct SessionRecord {
    pub rows: u64,
    pub invalid_books: u64,
    pub status: String,
}

/// `run_preflight`: `verify_input` then the single streaming pass building
/// row/book/session/parent tallies, then the fail-closed contract checks in
/// the same order as the Python reference.
pub fn run_preflight(directory: &Path, jobs_manifest: &Path) -> LabResult<PreflightArtifact> {
    let hashes = crate::delivery::verify_input(directory, jobs_manifest)?;
    let inventory: Vec<(String, String)> = SESSION_INVENTORY
        .iter()
        .map(|(date, status)| ((*date).to_string(), (*status).to_string()))
        .collect();

    run_preflight_with_inventory(
        directory,
        hashes,
        JOB_ID.to_string(),
        None,
        &inventory,
        true,
        None,
        None,
    )
}

/// Stage M's month-generic preflight. The calendar is an input artifact: its
/// date grades become the inventory without changing the frozen 12a
/// subcontract or its July producer.
pub fn run_month_preflight(
    month: u64,
    directory: &Path,
    jobs_manifest: &Path,
    delivery_key: &str,
    calendar_path: &Path,
) -> LabResult<PreflightArtifact> {
    let hashes = crate::delivery::verify_input_entry(directory, jobs_manifest, delivery_key)?;
    let job_id = crate::delivery::input_entry_job_id(jobs_manifest, delivery_key)?;
    let calendar_bytes = std::fs::read(calendar_path)?;
    let calendar: Value = serde_json::from_slice(&calendar_bytes)?;
    let month_label = format!("{:04}-{:02}", month / 100, month % 100);
    let month_value = calendar["months"].get(&month_label).ok_or_else(|| {
        LabError::refusal(format!("calendar artifact carries no month {month_label}"))
    })?;
    if month_value["role"] != "new-design" {
        return Err(LabError::refusal(format!(
            "calendar month {month_label} has role {:?}, not new-design",
            month_value["role"]
        )));
    }
    if !calendar["classification"].is_object() {
        return Err(LabError::refusal(
            "calendar artifact carries no declared classification thresholds",
        ));
    }
    let dates = month_value["dates"].as_object().ok_or_else(|| {
        LabError::refusal(format!(
            "calendar month {month_label} carries no dates object"
        ))
    })?;
    let mut inventory = Vec::new();
    for (date, row) in dates {
        match row["grade"].as_str() {
            Some("full_session") => inventory.push((date.clone(), "full".to_string())),
            Some("half_session") => inventory.push((date.clone(), "half".to_string())),
            Some("closure") => {}
            Some(grade) => {
                return Err(LabError::refusal(format!(
                    "calendar date {date} has unknown grade {grade}"
                )));
            }
            None => {
                return Err(LabError::refusal(format!(
                    "calendar date {date} carries no grade"
                )));
            }
        }
    }
    let frame = ScheduleFrame::stage_m(Path::new("analysis/tz-america-chicago-2026c.json"))?;
    let per_session_bounds = inventory
        .iter()
        .map(|(date, _)| frame.bounds(date))
        .collect::<LabResult<Vec<_>>>()?;
    let schedule_record = ScheduleFrameRecord {
        rule: "Stage M Amendment 4 DST-aware America/Chicago civil-boundary conversion".to_string(),
        timezone_authority: "analysis/tz-america-chicago-2026c.json".to_string(),
        timezone_authority_sha256: crate::session::STAGE_M_TZ_AUTHORITY_SHA256.to_string(),
        calendar_inventory_schedule_independence: "analysis/databento-calendar.json grades each DATE from record counts and is schedule-independent; its inventory derivation is unaffected by the schedule-frame correction".to_string(),
        scheduled_exposure_seconds: per_session_bounds.iter().map(|x| x.scheduled_open_seconds).sum(),
        per_session_bounds,
    };
    run_preflight_with_inventory(
        directory,
        hashes,
        job_id,
        Some(delivery_key.to_string()),
        &inventory,
        false,
        Some(InventoryProvenance {
            calendar_artifact: calendar_path.display().to_string(),
            calendar_artifact_hash: crate::delivery::sha256_bytes(&calendar_bytes),
            month: month_label,
        }),
        Some((frame, schedule_record)),
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "the July parity path keeps its explicit frozen inputs"
)]
fn run_preflight_with_inventory(
    directory: &Path,
    hashes: BTreeMap<String, String>,
    job_id: String,
    delivery_key: Option<String>,
    inventory: &[(String, String)],
    enforce_july_session_floors: bool,
    inventory_provenance: Option<InventoryProvenance>,
    schedule: Option<(ScheduleFrame, ScheduleFrameRecord)>,
) -> LabResult<PreflightArtifact> {
    let inventory_status: BTreeMap<&str, &str> = inventory
        .iter()
        .map(|(label, status)| (label.as_str(), status.as_str()))
        .collect();

    let mut rows: u64 = 0;
    let mut unsided: u64 = 0;
    let mut book_counts: BTreeMap<String, u64> = BTreeMap::from([
        ("normal".to_string(), 0),
        ("locked".to_string(), 0),
        ("crossed".to_string(), 0),
        ("nonpositive".to_string(), 0),
    ]);
    let mut outside_sessions: u64 = 0;
    let mut per_session: BTreeMap<String, SessionState> = BTreeMap::new();
    let mut parent_total: u64 = 0;
    let mut parent_valid_quote: u64 = 0;
    let mut prev_key: Option<(i64, char)> = None;
    let mut minute_cache = schedule.as_ref().map_or_else(MinuteFieldsCache::new, |x| {
        MinuteFieldsCache::with_frame(x.0.clone())
    });

    for row in parse_stream(data_files(directory)?) {
        let row = row?;
        rows += 1;
        if row.side == 'N' {
            unsided += 1;
        }
        *book_counts
            .get_mut(row.book)
            .expect("classify_book only emits the four known labels") += 1;
        let (session, _segment, _hour) = minute_cache.minute_fields(row.ts as u64);
        match &session {
            Some(label) if inventory_status.contains_key(label.as_str()) => {
                let state = per_session.entry(label.clone()).or_default();
                state.rows += 1;
                state.ids.insert(row.instrument_id.clone());
                if row.book != "normal" {
                    state.invalid_books += 1;
                }
            }
            _ => outside_sessions += 1,
        }
        if row.side == 'N' {
            prev_key = None;
        } else {
            let key = (row.ts, row.side);
            if Some(key) != prev_key {
                parent_total += 1;
                if row.book == "normal" {
                    parent_valid_quote += 1;
                }
                prev_key = Some(key);
            }
        }
    }

    if rows == 0 {
        return Err(LabError::refusal("the stream carried no rows"));
    }
    let unsided_share = unsided as f64 / rows as f64;
    if unsided_share > MAX_UNSIDED_SHARE {
        return Err(LabError::refusal(format!(
            "unsided share {unsided_share:.6} exceeds {MAX_UNSIDED_SHARE}"
        )));
    }
    let invalid = rows - book_counts["normal"];
    let invalid_share = invalid as f64 / rows as f64;
    if invalid_share > MAX_INVALID_WIDTH_SHARE {
        return Err(LabError::refusal(format!(
            "invalid-width share {invalid_share:.6} (locked+crossed+nonpositive) exceeds \
             {MAX_INVALID_WIDTH_SHARE}"
        )));
    }
    if parent_total == 0 {
        return Err(LabError::refusal("no sided parents in the stream"));
    }
    let quote_share = parent_valid_quote as f64 / parent_total as f64;
    if quote_share < MIN_VALID_PARENT_QUOTE_SHARE {
        return Err(LabError::refusal(format!(
            "valid parent-quote coverage {quote_share:.6} is below {MIN_VALID_PARENT_QUOTE_SHARE}"
        )));
    }

    let mut excluded: Vec<(String, String)> = Vec::new();
    let mut usable: Vec<String> = Vec::new();
    for (label, status) in inventory {
        if status != "full" {
            continue;
        }
        match per_session.get(label) {
            None => excluded.push((label.clone(), "absent".to_string())),
            Some(state) if state.rows == 0 => {
                excluded.push((label.clone(), "absent".to_string()));
            }
            Some(state) if state.ids.len() > 1 => {
                let mut ids: Vec<&String> = state.ids.iter().collect();
                ids.sort();
                excluded.push((label.clone(), format!("impure: ids {ids:?}")));
            }
            Some(_) => usable.push(label.clone()),
        }
    }
    if enforce_july_session_floors && excluded.len() > MAX_EXCLUDED_SESSIONS {
        return Err(LabError::refusal(format!(
            "{} sessions excluded ({excluded:?}); more than {MAX_EXCLUDED_SESSIONS}",
            excluded.len()
        )));
    }
    if enforce_july_session_floors && usable.len() < MIN_USABLE_SESSIONS {
        return Err(LabError::refusal(format!(
            "only {} usable sessions of the expected {EXPECTED_FULL_SESSIONS}; fewer than \
             {MIN_USABLE_SESSIONS}",
            usable.len()
        )));
    }

    let mut sessions: BTreeMap<String, SessionRecord> = BTreeMap::new();
    let usable_set: std::collections::HashSet<&str> = usable.iter().map(String::as_str).collect();
    for (label, status) in inventory {
        let state = per_session.get(label);
        let record_status = if status != "full" {
            "early_close_excluded".to_string()
        } else if usable_set.contains(label.as_str()) {
            "usable".to_string()
        } else {
            "excluded".to_string()
        };
        sessions.insert(
            label.clone(),
            SessionRecord {
                rows: state.map_or(0, |s| s.rows),
                invalid_books: state.map_or(0, |s| s.invalid_books),
                status: record_status,
            },
        );
    }

    Ok(PreflightArtifact {
        job_id,
        ledger_key: delivery_key,
        file_hashes: hashes,
        inventory_provenance,
        subcontract_hash: subcontract::subcontract_hash(),
        rows,
        unsided,
        unsided_share,
        book_counts,
        invalid_width_share: invalid_share,
        parents_seen: parent_total,
        valid_parent_quote_share: quote_share,
        rows_outside_declared_sessions: outside_sessions,
        schedule_frame: schedule.map(|x| x.1),
        sessions,
        excluded_sessions: excluded,
        usable_sessions: usable,
    })
}

/// `require_preflight`: `(preflight artifact JSON, sha256 of the artifact
/// file bytes)`. Refuses an absent artifact or one whose `file_hashes` do
/// not match the hashes just computed over the delivered corpus - the
/// artifact and the bytes on disk must agree before anything downstream
/// trusts either.
pub fn require_preflight(
    hashes: &BTreeMap<String, String>,
    artifact_path: &Path,
) -> LabResult<(Value, String)> {
    if !artifact_path.exists() {
        return Err(LabError::refusal(
            "no preflight artifact; run preflight first",
        ));
    }
    let bytes = std::fs::read(artifact_path)?;
    let artifact_hash = crate::delivery::sha256_bytes(&bytes);
    let artifact: Value = serde_json::from_slice(&bytes)?;
    let got_hashes = artifact.get("file_hashes").cloned().unwrap_or(Value::Null);
    let want_hashes = serde_json::to_value(hashes)?;
    if got_hashes != want_hashes {
        return Err(LabError::refusal(
            "preflight artifact hashes do not match the bytes on disk; re-run preflight \
             against the current delivery",
        ));
    }
    Ok((artifact, artifact_hash))
}

/// `json_safe` + `write_json_atomic`: non-finite floats become the strings
/// `"nan"`/`"inf"`/`"-inf"` (a strict JSON consumer would refuse the
/// non-standard tokens `json.dump` would otherwise emit), written via a
/// `.tmp` + rename so a reader never observes a partial file.
pub fn write_json_atomic(path: &Path, artifact: &PreflightArtifact) -> LabResult<()> {
    let value = serde_json::to_value(artifact)?;
    let safe = json_safe(value);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    {
        let mut file = std::fs::File::create(&tmp)?;
        serde_json::to_writer_pretty(&file, &safe)?;
        file.write_all(b"\n")?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn json_safe(v: Value) -> Value {
    match v {
        Value::Number(n) => {
            if let Some(f) = n.as_f64()
                && !f.is_finite()
            {
                return Value::String(
                    if f.is_nan() {
                        "nan"
                    } else if f > 0.0 {
                        "inf"
                    } else {
                        "-inf"
                    }
                    .to_string(),
                );
            }
            Value::Number(n)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(json_safe).collect()),
        Value::Object(map) => {
            let mut out = Map::new();
            for (k, val) in map {
                out.insert(k, json_safe(val));
            }
            Value::Object(out)
        }
        other => other,
    }
}
