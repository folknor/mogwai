// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The frozen measurement sub-contract, ported byte-faithfully from
//! `analysis/mnq_fit.py`'s `SUBCONTRACT_KEYS` block and `subcontract_hash()`.
//!
//! The hash binds `json.dumps({k: globals()[k] for k in SUBCONTRACT_KEYS},
//! sort_keys=True, default=list)` on the Python side: compact separators
//! (`", "` / `": "`), keys sorted lexicographically, tuples serialized as
//! JSON arrays via `default=list`, floats formatted with CPython's `repr()`
//! rule (shortest round-trip digits, fixed notation when `-4 < decpt <= 16`,
//! scientific otherwise with a signed, zero-padded two-digit exponent).
//! `serde_json`'s compact writer does not reproduce that byte-for-byte (no
//! space after `:`/`,`, and its float formatter never emits scientific
//! notation), so this module hand-rolls a small Python-`json`-compatible
//! serializer over an explicit value tree instead of deriving `Serialize`.
//! `subcontract_dumps()`/`subcontract_hash()` are pinned by a unit test
//! against the exact bytes and sha256 `analysis/mnq_fit.py` produces today.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

/// A JSON value restricted to the shapes this contract's constants actually
/// take, serialized to match `json.dumps(..., sort_keys=True, default=list)`.
#[derive(Clone, Debug)]
pub enum PyValue {
    Int(i64),
    Float(f64),
    Str(&'static str),
    List(Vec<PyValue>),
    /// Sorted by construction: `BTreeMap` orders keys the same way Python's
    /// `sort_keys=True` does for plain ASCII identifiers (codepoint order).
    Dict(BTreeMap<&'static str, PyValue>),
}

impl PyValue {
    fn str_list(items: &[&'static str]) -> PyValue {
        PyValue::List(items.iter().map(|s| PyValue::Str(s)).collect())
    }

    fn int_list(items: &[i64]) -> PyValue {
        PyValue::List(items.iter().map(|n| PyValue::Int(*n)).collect())
    }

    fn float_list(items: &[f64]) -> PyValue {
        PyValue::List(items.iter().map(|n| PyValue::Float(*n)).collect())
    }
}

// `py_float_repr` used to live here as a second, independently written copy
// of CPython's float `repr`. It carried the exponent-switch threshold as
// `decpt <= 17` where CPython uses 16, so it would have formatted 1e16 as
// `10000000000000000.0` instead of `1e+16` - invisible here only because every
// sub-contract float is small. The one implementation now lives in
// `crate::kernel`; this module's test still pins the shapes it depends on.
use crate::kernel::py_float_repr;

fn escape_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn dump(v: &PyValue, out: &mut String) {
    match v {
        PyValue::Int(n) => out.push_str(&n.to_string()),
        PyValue::Float(x) => out.push_str(&py_float_repr(*x)),
        PyValue::Str(s) => escape_str(s, out),
        PyValue::List(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                dump(item, out);
            }
            out.push(']');
        }
        PyValue::Dict(map) => {
            out.push('{');
            for (i, (k, val)) in map.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                escape_str(k, out);
                out.push_str(": ");
                dump(val, out);
            }
            out.push('}');
        }
    }
}

/// `JOB_ID` - also the preflight artifact's `job_id` field.
pub const JOB_ID: &str = "GLBX-20260805-HAPEWPABKG";
/// `DELIVERY_KEY` - the delivery this job binds to.
pub const DELIVERY_KEY: &str = "mnqv|2026-07.full|tbbo";

pub const MAX_UNSIDED_SHARE: f64 = 0.01;
pub const MAX_INVALID_WIDTH_SHARE: f64 = 0.001;
pub const MIN_VALID_PARENT_QUOTE_SHARE: f64 = 0.95;
pub const MIN_DOMINANT_ID_SHARE: f64 = 1.0;
pub const MAX_EXCLUDED_SESSIONS: usize = 4;
pub const MIN_USABLE_SESSIONS: usize = 18;

pub const PRICE_UNITS_PER_POINT: i64 = 1_000_000_000;
pub const TICK_UNITS: i64 = 250_000_000;

pub const UTC_OFFSET_MINUTES: i32 = -300;
pub const SESSION_OPEN_LOCAL_MIN: i32 = 17 * 60;
pub const SESSION_CLOSE_LOCAL_MIN: i32 = 16 * 60;
pub const HALT_START_LOCAL_MIN: i32 = 15 * 60 + 15;
pub const HALT_END_LOCAL_MIN: i32 = 15 * 60 + 30;

/// The frozen July 2026 session inventory (label, status).
pub const SESSION_INVENTORY: &[(&str, &str)] = &[
    ("2026-07-01", "full"),
    ("2026-07-02", "full"),
    ("2026-07-03", "early_close_excluded"),
    ("2026-07-06", "full"),
    ("2026-07-07", "full"),
    ("2026-07-08", "full"),
    ("2026-07-09", "full"),
    ("2026-07-10", "full"),
    ("2026-07-13", "full"),
    ("2026-07-14", "full"),
    ("2026-07-15", "full"),
    ("2026-07-16", "full"),
    ("2026-07-17", "full"),
    ("2026-07-20", "full"),
    ("2026-07-21", "full"),
    ("2026-07-22", "full"),
    ("2026-07-23", "full"),
    ("2026-07-24", "full"),
    ("2026-07-27", "full"),
    ("2026-07-28", "full"),
    ("2026-07-29", "full"),
    ("2026-07-30", "full"),
    ("2026-07-31", "full"),
];
pub const EXPECTED_FULL_SESSIONS: i64 = 22;

pub const SEARCH_START_NS: i64 = 1_783_288_800_000_000_000;
pub const SEARCH_LENGTH: &str = "7d";
pub const SEARCH_SEEDS: &[i64] = &[1, 2];
pub const FINAL_START_NS: i64 = 1_782_856_800_000_000_000;
pub const FINAL_END_NS: i64 = 1_785_531_600_000_000_000;
pub const FINAL_LENGTH: &str = "2674800s";
pub const FINAL_SEEDS: &[i64] = &[1, 2, 3, 4, 5, 6, 7, 8];
/// The estimator burn-in prefix a summary walk generates before `--start` and
/// then discards. The constant's own name in [`tree`] stays `SUMMARY_WARMUP`,
/// which is inherited and frozen: that map is a transcription of
/// `analysis/mnq_fit.py`'s constant names, and its bytes are the sub-contract
/// hash every committed measurement and preflight artifact records.
pub const SUMMARY_BURN_IN: &str = "3d";

pub const SOLVE_RELATIVE_STEP: f64 = 1e-3;
pub const VOL_GRID_POINTS: i64 = 32;
pub const VOL_SCALAR_DOMAIN: (f64, f64) = (1e-8, 1e-4);
pub const DISPLACEMENT_BIN_TICKS: f64 = 0.05;

pub const ACF_LAGS: &[i64] = &[1, 10, 50];
pub const HORIZON_SECONDS: &[i64] = &[60, 300];

pub const RESAMPLE_SEED: i64 = 1;
pub const RESAMPLE_REPLICATES: i64 = 1000;
pub const RESAMPLE_SESSIONS_PER_REPLICATE: i64 = 22;
pub const RESAMPLE_ENVELOPE_LEVEL: f64 = 0.99;
pub const MINUTE_RANGE_GATES: &[&str] = &["p99", "p99.9", "max"];

pub const MIN_PARENT_CELL_RETURNS: i64 = 1000;
pub const MIN_60S_CELL_RETURNS: i64 = 40;
pub const MIN_300S_CELL_RETURNS: i64 = 6;
pub const SESSION_HOUR_BAND: (f64, f64) = (0.8, 1.25);
pub const ARRIVAL_HOUR_REL_TOL: f64 = 0.10;
pub const WALLTIME_POOLED_REL_TOL: f64 = 0.15;
pub const SESSION_ARRAY_DECIMALS: i64 = 6;
pub const TOP_MINUTE_RECORDS: i64 = 32;
pub const GENERATED_SESSIONS_PER_SEED: i64 = 23;
pub const SESSION_VOL_CORR_MIN: f64 = 0.90;
pub const MNQ_DOW_WEIGHT: &[f64] = &[1.5179, 0.9080, 0.9865, 1.0157, 1.0535, 1.0225, 1.0000];
pub const WALLTIME_HOURLY_ROLE: &str = "diagnostic";

pub const FAIL_HOURS_300: &[i64] = &[19, 20, 23];
pub const FAIL_HOURS_60: &[i64] = &[20];
pub const HOT_HOURS: &[i64] = &[19, 20];
pub const COLD_HOURS: &[i64] = &[23];
pub const RESIDUAL_WINDOW_S: i64 = 300;
pub const RESIDUAL_MIN_HISTORY: i64 = 1000;
pub const RESIDUAL_EXCEED_MULTIPLES: &[i64] = &[4, 8, 16];
pub const INNOVATION_EXCEED_ABS: &[i64] = &[4, 8, 16];
pub const PERMUTATION_REPLICATES: i64 = 16;
pub const PERMUTATION_VARIANTS: &[&str] = &["sign", "magnitude"];
pub const BOOTSTRAP_REPLICATES: i64 = 10_000;
pub const BOOTSTRAP_BLOCK_SESSIONS: i64 = 5;
pub const BOOTSTRAP_BASE_SEED: i64 = 1_342_176_408_401_967_774;
pub const PERMUTATION_BASE_SEED: i64 = 7_205_759_943_768_246_531;
pub const CONTROL_TIE_BASE_SEED: i64 = 3_141_592_653_589_793_238;
pub const FAMILY_ENVELOPE_LEVEL: f64 = 0.95;
pub const SEED_DIRECTION_MIN: i64 = 7;
pub const FOLD_MIN_SESSIONS: i64 = 15;
pub const MATERIALITY_BAND: (f64, f64) = (0.8, 1.25);
pub const GAP_CLOSE_MIN: f64 = 0.50;
pub const GAP_CLOSE_LCB_MIN: f64 = 0.25;
pub const GAP_CLOSE_EPS: f64 = 1e-9;
pub const COUNT_WINDOWS_S: &[i64] = &[1, 5, 60];
pub const WALL_HORIZONS_S: &[i64] = &[1, 5, 15, 60, 300];
pub const EXCEEDANCE_TICKS: &[i64] = &[399, 642, 968];
pub const PARENT_COUNT_BIN_EDGES: &[i64] = &[1, 65, 257, 1025, 4097];
pub const PARENT_COUNT_BIN_NAMES: &[&str] =
    &["0", "1-64", "65-256", "257-1024", "1025-4096", "4097+"];
pub const SEGMENT_LABEL_EDGES_S: &[i64] = &[300, 1800];
pub const SINCE_OPEN_BIN_NAMES: &[&str] = &["0-300", "300-1800", "1800+"];
pub const UNTIL_CLOSE_BIN_NAMES: &[&str] = &["1800+", "300-1800", "0-300"];
pub const MIN_1S_CELL_RETURNS: i64 = 2500;
pub const MIN_5S_CELL_RETURNS: i64 = 500;
pub const MIN_15S_CELL_RETURNS: i64 = 160;
pub const MIN_RESIDUAL_CELL: i64 = 1000;
pub const MIN_MINUTES_CELL: i64 = 30;
pub const MIN_BOUNDARY_MINUTES_CELL: i64 = 4;
pub const MIN_BOUNDARY_60S_CELL_RETURNS: i64 = 3;
pub const SIGMA_ESCALATION_MIN: f64 = 2.0;
pub const CONTROL_ESCALATION_MAX: f64 = 1.25;
pub const INITIATION_INNOVATION_MIN: i64 = 8;

fn reference_shape() -> PyValue {
    let mut abs_return_acf = BTreeMap::new();
    abs_return_acf.insert("1", PyValue::Float(0.30741));
    abs_return_acf.insert("10", PyValue::Float(0.15649));
    abs_return_acf.insert("50", PyValue::Float(0.12252));
    let mut duration_acf = BTreeMap::new();
    duration_acf.insert("1", PyValue::Float(0.32204));
    duration_acf.insert("5", PyValue::Float(0.22388));
    let mut return_acf = BTreeMap::new();
    return_acf.insert("1", PyValue::Float(-0.19697));
    let mut top = BTreeMap::new();
    top.insert("abs_return_acf", PyValue::Dict(abs_return_acf));
    top.insert("duration_acf", PyValue::Dict(duration_acf));
    top.insert("duration_dispersion_cv2", PyValue::Float(4.6188));
    top.insert("return_acf", PyValue::Dict(return_acf));
    top.insert("zero_change_frac", PyValue::Float(0.47376));
    PyValue::Dict(top)
}

fn tolerances() -> PyValue {
    let mut m = BTreeMap::new();
    m.insert(
        "mean_event_duration_s",
        PyValue::str_list(&["relative"]).with_num(0.10),
    );
    m.insert(
        "children_mean",
        PyValue::str_list(&["relative"]).with_num(0.10),
    );
    m.insert(
        "children_single_frac",
        PyValue::str_list(&["absolute"]).with_num(0.05),
    );
    m.insert(
        "levels_mean",
        PyValue::str_list(&["relative"]).with_num(0.15),
    );
    m.insert("mid_rms", PyValue::str_list(&["relative"]).with_num(0.10));
    m.insert(
        "minute_range_p99",
        PyValue::List(vec![
            PyValue::Str("envelope_upper"),
            PyValue::Str("resampled"),
        ]),
    );
    m.insert(
        "minute_range_p99.9",
        PyValue::List(vec![
            PyValue::Str("envelope_upper"),
            PyValue::Str("resampled"),
        ]),
    );
    m.insert(
        "minute_range_max",
        PyValue::List(vec![
            PyValue::Str("envelope_upper"),
            PyValue::Str("resampled"),
        ]),
    );
    m.insert(
        "session_arrival_hour",
        PyValue::str_list(&["relative"]).with_num(ARRIVAL_HOUR_REL_TOL),
    );
    m.insert(
        "session_vol_hour",
        PyValue::List(vec![PyValue::Str("band"), band_pair(SESSION_HOUR_BAND)]),
    );
    m.insert(
        "walltime_hour_60",
        PyValue::List(vec![PyValue::Str("band"), band_pair(SESSION_HOUR_BAND)]),
    );
    m.insert(
        "walltime_hour_300",
        PyValue::List(vec![PyValue::Str("band"), band_pair(SESSION_HOUR_BAND)]),
    );
    m.insert(
        "walltime_pooled_60",
        PyValue::str_list(&["relative"]).with_num(WALLTIME_POOLED_REL_TOL),
    );
    m.insert(
        "walltime_pooled_300",
        PyValue::str_list(&["relative"]).with_num(WALLTIME_POOLED_REL_TOL),
    );
    PyValue::Dict(m)
}

fn band_pair(band: (f64, f64)) -> PyValue {
    PyValue::float_list(&[band.0, band.1])
}

impl PyValue {
    /// `["relative", 0.1]`-shaped pairs: build the string half then append
    /// the trailing numeric half. Small helper, only used by `tolerances()`.
    fn with_num(self, n: f64) -> PyValue {
        match self {
            PyValue::List(mut items) => {
                items.push(PyValue::Float(n));
                PyValue::List(items)
            }
            other => other,
        }
    }
}

/// Build the sub-contract value tree, mirroring
/// `{k: globals()[k] for k in SUBCONTRACT_KEYS}` key-for-key.
///
/// A stale-looking hash in a committed artifact is not necessarily stale, and
/// the owner ruled on exactly this case on 2026-08-08: do not "fix" it.
/// `analysis/mnq-fit.json` records `binding.subcontract_hash` 35e5b033, while
/// this function returns 1ca79d9c today. The difference is correct history
/// rather than drift - the protocol-12a constants joined this key set after
/// the protocol-11 fit ran, and that fit never read one of them. The rewrite's
/// phase-3b parity gate demonstrated it: every fitted number in the artifact
/// reproduced at 132/132 walk-cache hits while only the binding differed.
/// Editing the artifact's hash to today's value would assert a binding that
/// never happened, which is precisely what the harness's own tamper check
/// exists to refuse. The artifact is therefore readable but not extensible
/// until a fresh fit runs; that re-run buys extensibility, not correctness.
///
/// The design defect this exposes is the flat namespace, not the hash: one
/// key set spanning every mode means any constant edit retroactively unbinds
/// every prior fit, including for constants that fit never read. Harmless at
/// one instrument; corrosive at a dozen, where adding a single constant
/// silently unbinds every committed fit artifact and the next reader concludes
/// the corpus is stale when nothing moved. Scoping the hash per mode is the
/// fix.
fn tree() -> PyValue {
    let mut m: BTreeMap<&'static str, PyValue> = BTreeMap::new();
    m.insert("JOB_ID", PyValue::Str(JOB_ID));
    // Inherited hash-bound spelling from the frozen subcontract format.
    m.insert("LEDGER_KEY", PyValue::Str(DELIVERY_KEY));
    m.insert("MAX_UNSIDED_SHARE", PyValue::Float(MAX_UNSIDED_SHARE));
    m.insert(
        "MAX_INVALID_WIDTH_SHARE",
        PyValue::Float(MAX_INVALID_WIDTH_SHARE),
    );
    m.insert(
        "MIN_VALID_PARENT_QUOTE_SHARE",
        PyValue::Float(MIN_VALID_PARENT_QUOTE_SHARE),
    );
    m.insert(
        "MIN_DOMINANT_ID_SHARE",
        PyValue::Float(MIN_DOMINANT_ID_SHARE),
    );
    m.insert(
        "MAX_EXCLUDED_SESSIONS",
        PyValue::Int(MAX_EXCLUDED_SESSIONS as i64),
    );
    m.insert(
        "MIN_USABLE_SESSIONS",
        PyValue::Int(MIN_USABLE_SESSIONS as i64),
    );
    m.insert("PRICE_UNITS_PER_POINT", PyValue::Int(PRICE_UNITS_PER_POINT));
    m.insert("TICK_UNITS", PyValue::Int(TICK_UNITS));
    m.insert(
        "UTC_OFFSET_MINUTES",
        PyValue::Int(UTC_OFFSET_MINUTES.into()),
    );
    m.insert(
        "SESSION_OPEN_LOCAL_MIN",
        PyValue::Int(SESSION_OPEN_LOCAL_MIN.into()),
    );
    m.insert(
        "SESSION_CLOSE_LOCAL_MIN",
        PyValue::Int(SESSION_CLOSE_LOCAL_MIN.into()),
    );
    m.insert(
        "HALT_START_LOCAL_MIN",
        PyValue::Int(HALT_START_LOCAL_MIN.into()),
    );
    m.insert(
        "HALT_END_LOCAL_MIN",
        PyValue::Int(HALT_END_LOCAL_MIN.into()),
    );
    m.insert(
        "SESSION_INVENTORY",
        PyValue::List(
            SESSION_INVENTORY
                .iter()
                .map(|(label, status)| {
                    PyValue::List(vec![PyValue::Str(label), PyValue::Str(status)])
                })
                .collect(),
        ),
    );
    m.insert(
        "EXPECTED_FULL_SESSIONS",
        PyValue::Int(EXPECTED_FULL_SESSIONS),
    );
    m.insert("SEARCH_START_NS", PyValue::Int(SEARCH_START_NS));
    m.insert("SEARCH_LENGTH", PyValue::Str(SEARCH_LENGTH));
    m.insert("SEARCH_SEEDS", PyValue::int_list(SEARCH_SEEDS));
    m.insert("FINAL_START_NS", PyValue::Int(FINAL_START_NS));
    m.insert("FINAL_END_NS", PyValue::Int(FINAL_END_NS));
    m.insert("FINAL_LENGTH", PyValue::Str(FINAL_LENGTH));
    m.insert("FINAL_SEEDS", PyValue::int_list(FINAL_SEEDS));
    // Inherited and frozen: the key is the Python constant's name, and these
    // bytes are hashed into every committed artifact's `subcontract_hash`.
    m.insert("SUMMARY_WARMUP", PyValue::Str(SUMMARY_BURN_IN));
    m.insert("SOLVE_RELATIVE_STEP", PyValue::Float(SOLVE_RELATIVE_STEP));
    m.insert("VOL_GRID_POINTS", PyValue::Int(VOL_GRID_POINTS));
    m.insert("VOL_SCALAR_DOMAIN", band_pair(VOL_SCALAR_DOMAIN));
    m.insert(
        "DISPLACEMENT_BIN_TICKS",
        PyValue::Float(DISPLACEMENT_BIN_TICKS),
    );
    m.insert("TOLERANCES", tolerances());
    m.insert("ACF_LAGS", PyValue::int_list(ACF_LAGS));
    m.insert("HORIZON_SECONDS", PyValue::int_list(HORIZON_SECONDS));
    m.insert("REFERENCE_SHAPE", reference_shape());
    m.insert("RESAMPLE_SEED", PyValue::Int(RESAMPLE_SEED));
    m.insert("RESAMPLE_REPLICATES", PyValue::Int(RESAMPLE_REPLICATES));
    m.insert(
        "RESAMPLE_SESSIONS_PER_REPLICATE",
        PyValue::Int(RESAMPLE_SESSIONS_PER_REPLICATE),
    );
    m.insert(
        "RESAMPLE_ENVELOPE_LEVEL",
        PyValue::Float(RESAMPLE_ENVELOPE_LEVEL),
    );
    m.insert("MINUTE_RANGE_GATES", PyValue::str_list(MINUTE_RANGE_GATES));
    m.insert(
        "MIN_PARENT_CELL_RETURNS",
        PyValue::Int(MIN_PARENT_CELL_RETURNS),
    );
    m.insert("MIN_60S_CELL_RETURNS", PyValue::Int(MIN_60S_CELL_RETURNS));
    m.insert("MIN_300S_CELL_RETURNS", PyValue::Int(MIN_300S_CELL_RETURNS));
    m.insert("SESSION_HOUR_BAND", band_pair(SESSION_HOUR_BAND));
    m.insert("ARRIVAL_HOUR_REL_TOL", PyValue::Float(ARRIVAL_HOUR_REL_TOL));
    m.insert(
        "WALLTIME_POOLED_REL_TOL",
        PyValue::Float(WALLTIME_POOLED_REL_TOL),
    );
    m.insert(
        "SESSION_ARRAY_DECIMALS",
        PyValue::Int(SESSION_ARRAY_DECIMALS),
    );
    m.insert("TOP_MINUTE_RECORDS", PyValue::Int(TOP_MINUTE_RECORDS));
    m.insert(
        "GENERATED_SESSIONS_PER_SEED",
        PyValue::Int(GENERATED_SESSIONS_PER_SEED),
    );
    m.insert("SESSION_VOL_CORR_MIN", PyValue::Float(SESSION_VOL_CORR_MIN));
    m.insert("MNQ_DOW_WEIGHT", PyValue::float_list(MNQ_DOW_WEIGHT));
    m.insert("WALLTIME_HOURLY_ROLE", PyValue::Str(WALLTIME_HOURLY_ROLE));
    m.insert("FAIL_HOURS_300", PyValue::int_list(FAIL_HOURS_300));
    m.insert("FAIL_HOURS_60", PyValue::int_list(FAIL_HOURS_60));
    m.insert("HOT_HOURS", PyValue::int_list(HOT_HOURS));
    m.insert("COLD_HOURS", PyValue::int_list(COLD_HOURS));
    m.insert("RESIDUAL_WINDOW_S", PyValue::Int(RESIDUAL_WINDOW_S));
    m.insert("RESIDUAL_MIN_HISTORY", PyValue::Int(RESIDUAL_MIN_HISTORY));
    m.insert(
        "RESIDUAL_EXCEED_MULTIPLES",
        PyValue::int_list(RESIDUAL_EXCEED_MULTIPLES),
    );
    m.insert(
        "INNOVATION_EXCEED_ABS",
        PyValue::int_list(INNOVATION_EXCEED_ABS),
    );
    m.insert(
        "PERMUTATION_REPLICATES",
        PyValue::Int(PERMUTATION_REPLICATES),
    );
    m.insert(
        "PERMUTATION_VARIANTS",
        PyValue::str_list(PERMUTATION_VARIANTS),
    );
    m.insert("BOOTSTRAP_REPLICATES", PyValue::Int(BOOTSTRAP_REPLICATES));
    m.insert(
        "BOOTSTRAP_BLOCK_SESSIONS",
        PyValue::Int(BOOTSTRAP_BLOCK_SESSIONS),
    );
    m.insert("BOOTSTRAP_BASE_SEED", PyValue::Int(BOOTSTRAP_BASE_SEED));
    m.insert("PERMUTATION_BASE_SEED", PyValue::Int(PERMUTATION_BASE_SEED));
    m.insert("CONTROL_TIE_BASE_SEED", PyValue::Int(CONTROL_TIE_BASE_SEED));
    m.insert(
        "FAMILY_ENVELOPE_LEVEL",
        PyValue::Float(FAMILY_ENVELOPE_LEVEL),
    );
    m.insert("SEED_DIRECTION_MIN", PyValue::Int(SEED_DIRECTION_MIN));
    m.insert("FOLD_MIN_SESSIONS", PyValue::Int(FOLD_MIN_SESSIONS));
    m.insert("MATERIALITY_BAND", band_pair(MATERIALITY_BAND));
    m.insert("GAP_CLOSE_MIN", PyValue::Float(GAP_CLOSE_MIN));
    m.insert("GAP_CLOSE_LCB_MIN", PyValue::Float(GAP_CLOSE_LCB_MIN));
    m.insert("GAP_CLOSE_EPS", PyValue::Float(GAP_CLOSE_EPS));
    m.insert("COUNT_WINDOWS_S", PyValue::int_list(COUNT_WINDOWS_S));
    m.insert("WALL_HORIZONS_S", PyValue::int_list(WALL_HORIZONS_S));
    m.insert("EXCEEDANCE_TICKS", PyValue::int_list(EXCEEDANCE_TICKS));
    m.insert(
        "PARENT_COUNT_BIN_EDGES",
        PyValue::int_list(PARENT_COUNT_BIN_EDGES),
    );
    m.insert(
        "PARENT_COUNT_BIN_NAMES",
        PyValue::str_list(PARENT_COUNT_BIN_NAMES),
    );
    m.insert(
        "SEGMENT_LABEL_EDGES_S",
        PyValue::int_list(SEGMENT_LABEL_EDGES_S),
    );
    m.insert(
        "SINCE_OPEN_BIN_NAMES",
        PyValue::str_list(SINCE_OPEN_BIN_NAMES),
    );
    m.insert(
        "UNTIL_CLOSE_BIN_NAMES",
        PyValue::str_list(UNTIL_CLOSE_BIN_NAMES),
    );
    m.insert("MIN_1S_CELL_RETURNS", PyValue::Int(MIN_1S_CELL_RETURNS));
    m.insert("MIN_5S_CELL_RETURNS", PyValue::Int(MIN_5S_CELL_RETURNS));
    m.insert("MIN_15S_CELL_RETURNS", PyValue::Int(MIN_15S_CELL_RETURNS));
    m.insert("MIN_RESIDUAL_CELL", PyValue::Int(MIN_RESIDUAL_CELL));
    m.insert("MIN_MINUTES_CELL", PyValue::Int(MIN_MINUTES_CELL));
    m.insert(
        "MIN_BOUNDARY_MINUTES_CELL",
        PyValue::Int(MIN_BOUNDARY_MINUTES_CELL),
    );
    m.insert(
        "MIN_BOUNDARY_60S_CELL_RETURNS",
        PyValue::Int(MIN_BOUNDARY_60S_CELL_RETURNS),
    );
    m.insert("SIGMA_ESCALATION_MIN", PyValue::Float(SIGMA_ESCALATION_MIN));
    m.insert(
        "CONTROL_ESCALATION_MAX",
        PyValue::Float(CONTROL_ESCALATION_MAX),
    );
    m.insert(
        "INITIATION_INNOVATION_MIN",
        PyValue::Int(INITIATION_INNOVATION_MIN),
    );
    PyValue::Dict(m)
}

/// The protocol-12a half of the key set, verbatim from `mnq_fit.py`'s own
/// section marker: everything after the `# Protocol 12a` comment in
/// `SUBCONTRACT_KEYS`. Taken from that comment rather than inferred, because
/// the boundary is a claim about which constants a mode reads and the Python
/// is the only place that records it.
const PROTOCOL_12A_KEYS: &[&str] = &[
    "BOOTSTRAP_BASE_SEED",
    "BOOTSTRAP_BLOCK_SESSIONS",
    "BOOTSTRAP_REPLICATES",
    "COLD_HOURS",
    "CONTROL_ESCALATION_MAX",
    "CONTROL_TIE_BASE_SEED",
    "COUNT_WINDOWS_S",
    "EXCEEDANCE_TICKS",
    "FAIL_HOURS_300",
    "FAIL_HOURS_60",
    "FAMILY_ENVELOPE_LEVEL",
    "FOLD_MIN_SESSIONS",
    "GAP_CLOSE_EPS",
    "GAP_CLOSE_LCB_MIN",
    "GAP_CLOSE_MIN",
    "HOT_HOURS",
    "INITIATION_INNOVATION_MIN",
    "INNOVATION_EXCEED_ABS",
    "MATERIALITY_BAND",
    "MIN_15S_CELL_RETURNS",
    "MIN_1S_CELL_RETURNS",
    "MIN_5S_CELL_RETURNS",
    "MIN_BOUNDARY_60S_CELL_RETURNS",
    "MIN_BOUNDARY_MINUTES_CELL",
    "MIN_MINUTES_CELL",
    "MIN_RESIDUAL_CELL",
    "PARENT_COUNT_BIN_EDGES",
    "PARENT_COUNT_BIN_NAMES",
    "PERMUTATION_BASE_SEED",
    "PERMUTATION_REPLICATES",
    "PERMUTATION_VARIANTS",
    "RESIDUAL_EXCEED_MULTIPLES",
    "RESIDUAL_MIN_HISTORY",
    "RESIDUAL_WINDOW_S",
    "SEED_DIRECTION_MIN",
    "SEGMENT_LABEL_EDGES_S",
    "SIGMA_ESCALATION_MIN",
    "SINCE_OPEN_BIN_NAMES",
    "UNTIL_CLOSE_BIN_NAMES",
    "WALL_HORIZONS_S",
];

/// Which measurement mode a sub-contract hash binds.
///
/// The defect this fixes, restated from `tree`'s note: one flat key set
/// spanning every mode means any constant edit retroactively unbinds every
/// prior fit, including fits that never read the constant that moved. Adding a
/// single protocol-12a constant already unbound `mnq-fit.json` once, and the
/// owner had to rule that the resulting hash mismatch was correct history
/// rather than drift. At one instrument that is a curiosity to be explained; at
/// a dozen it is a standing invitation to conclude the corpus is stale when
/// nothing moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The protocol-11 fit: everything ahead of the `# Protocol 12a` marker.
    Protocol11,
    /// The protocol-12a measurement block.
    Protocol12a,
}

impl Mode {
    fn contains(self, key: &str) -> bool {
        let is_12a = PROTOCOL_12A_KEYS.contains(&key);
        match self {
            Self::Protocol11 => !is_12a,
            Self::Protocol12a => is_12a,
        }
    }
}

/// The sub-contract bytes for one mode, in the same canonical form as
/// [`subcontract_dumps`].
#[must_use]
pub fn subcontract_dumps_for(mode: Mode) -> String {
    let PyValue::Dict(all) = tree() else {
        unreachable!("tree is a dict")
    };
    let scoped: BTreeMap<&'static str, PyValue> = all
        .into_iter()
        .filter(|(key, _)| mode.contains(key))
        .collect();
    let mut out = String::new();
    dump(&PyValue::Dict(scoped), &mut out);
    out
}

/// The sub-contract hash for one mode.
///
/// A new binding, with no Python counterpart: `mnq_fit.py` has only the flat
/// [`subcontract_hash`], and that flat hash is what cross-language parity is
/// checked against, so it is untouched. These are for artifacts written from
/// here on, which record the hash of the constants their own mode actually
/// reads - so a protocol-12a constant moving no longer unbinds a protocol-11
/// fit.
///
/// It does not retroactively rebind anything. `analysis/mnq-fit.json` keeps
/// `binding.subcontract_hash` 35e5b033 as committed, per the owner's 2026-08-08
/// ruling: that value accurately records what the protocol-11 fit ran under,
/// and rewriting it would assert a binding that never happened.
#[must_use]
pub fn subcontract_hash_for(mode: Mode) -> String {
    let mut hasher = Sha256::new();
    hasher.update(subcontract_dumps_for(mode).as_bytes());
    crate::delivery::hex_digest(&hasher.finalize())
}

/// The exact bytes `analysis/mnq_fit.py`'s `subcontract_hash()` hashes.
pub fn subcontract_dumps() -> String {
    let mut out = String::new();
    dump(&tree(), &mut out);
    out
}

/// `subcontract_hash()`: sha256 hex digest of `subcontract_dumps()`.
pub fn subcontract_hash() -> String {
    let mut hasher = Sha256::new();
    hasher.update(subcontract_dumps().as_bytes());
    crate::delivery::hex_digest(&hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ground truth captured verbatim from a live
    /// `python3 -c "...json.dumps(...)"` run against `analysis/mnq_fit.py`
    /// (2026-08-06). If the Python constants ever move, this fixture and
    /// `EXPECTED_HASH` must move with them - the whole point of the hash is
    /// that the two sides cannot silently drift.
    const EXPECTED_HASH: &str = "1ca79d9cd043e7ce4b8b633fdbcdf0547a02a26570ea9120eb0141254a8ad954";

    #[test]
    fn hash_matches_the_python_reference() {
        assert_eq!(subcontract_hash(), EXPECTED_HASH);
    }

    /// The burn-in rename moved the Rust constant and deliberately left the
    /// hashed key alone. The tree is a transcription of the Python's own
    /// names, and every committed artifact records a hash of these bytes, so
    /// the key is frozen where the identifier is free.
    #[test]
    fn the_hashed_tree_keeps_the_inherited_summary_warmup_key() {
        let dumps = subcontract_dumps();
        assert!(dumps.contains("\"SUMMARY_WARMUP\": \"3d\""));
        assert!(!dumps.contains("SUMMARY_BURN_IN"));
    }

    /// The delivery rename moved the Rust constant and deliberately left the
    /// hashed key alone, for the same reason the burn-in rename did: the tree
    /// transcribes the Python's own names and its bytes are the
    /// `subcontract_hash` every committed measurement and preflight artifact
    /// records. Without this, a pass respelling the map key fails only on an
    /// opaque digest.
    #[test]
    fn the_hashed_tree_keeps_the_inherited_ledger_key() {
        let dumps = subcontract_dumps();
        assert!(dumps.contains("\"LEDGER_KEY\": \"mnqv|2026-07.full|tbbo\""));
        assert!(!dumps.contains("DELIVERY_KEY"));
    }

    /// Two constants encode one quantity, so one of them must be derived from
    /// the other or they need a gate. The final window's length is written
    /// twice here - as the `FINAL_END_NS - FINAL_START_NS` difference and as
    /// the `FINAL_LENGTH` seconds string - and the two are read by different
    /// consumers: `measure`'s artifact writer and `count_curve`'s binding check
    /// take the difference, while `run_final_walk` and the `fit` driver parse
    /// the string, so the walk that produces the exposure record and the
    /// artifact that records its window disagree about which is authoritative.
    /// Neither can be deleted while the fit driver needs a duration string and
    /// the artifact needs nanoseconds, so this is the gate. Without it, editing
    /// `FINAL_LENGTH` alone moves the measured window and nothing says so until
    /// a ten-minute equality pin fails as an opaque diff of two 20 KB
    /// accumulator records.
    ///
    /// `hash_matches_the_python_reference` is not this gate, though a bare edit
    /// to `FINAL_LENGTH` does redden it: the hash covers every sub-contract
    /// constant against the retired Python oracle's snapshot, so the sanctioned
    /// way to move one is to re-bless `EXPECTED_HASH` in the same change - and
    /// that re-bless says nothing about whether the two encodings still agree.
    /// A frozen-snapshot hash catches an unintended edit; only this catches an
    /// intended edit made to one encoding of a quantity written twice.
    #[test]
    fn the_final_windows_two_encodings_of_its_length_agree() {
        let seconds: i64 = FINAL_LENGTH
            .trim_end_matches('s')
            .parse()
            .expect("FINAL_LENGTH is a seconds string");
        assert_eq!(
            FINAL_END_NS - FINAL_START_NS,
            seconds * 1_000_000_000,
            "FINAL_LENGTH and the FINAL_END_NS/FINAL_START_NS difference \
             encode different window lengths"
        );
    }

    /// What survives the oracle, and - said plainly - what does not.
    ///
    /// Until phase 4b item 7 this file carried a test that parsed
    /// `mnq_fit.py`'s `SUBCONTRACT_KEYS` at its `# Protocol 12a` section marker
    /// and compared the two sides. That marker was the only independent
    /// authority on the classification, because the boundary is a claim about
    /// which mode reads which constant, and no amount of Rust can settle that
    /// from the inside. It was deliberately landed before the retirement, ran
    /// green, and was deleted with the oracle rather than redirected at the
    /// Rust - redirecting it would have made it circular, a test that asserts a
    /// list matches itself while looking like a cross-check.
    ///
    /// So `PROTOCOL_12A_KEYS` is now the authority. These assertions are real
    /// but narrower, and the difference is worth being explicit about: they
    /// catch a typo or a stale name, not a misclassification.
    ///
    /// Note also what is deliberately not asserted. "Every key belongs to
    /// exactly one mode" is true by construction, since `Mode::Protocol11` is
    /// defined as "not in `PROTOCOL_12A_KEYS`", so such an assertion could
    /// never fail - a tautology wearing the costume of a partition proof.
    #[test]
    fn every_classified_key_exists_and_protocol_eleven_retains_its_own() {
        let PyValue::Dict(all) = tree() else {
            unreachable!("tree is a dict")
        };
        let flat: Vec<&str> = all.keys().copied().collect();
        let missing: Vec<&&str> = PROTOCOL_12A_KEYS
            .iter()
            .filter(|key| !flat.contains(*key))
            .collect();
        assert!(
            missing.is_empty(),
            "PROTOCOL_12A_KEYS names constants that are not in the sub-contract: {missing:?}"
        );
        assert!(
            PROTOCOL_12A_KEYS.len() < flat.len(),
            "protocol-11 must retain keys of its own"
        );
        assert_eq!(
            PROTOCOL_12A_KEYS.len(),
            40,
            "the 12a set was 40 keys when the Python marker last validated it, verified against \
             `mnq_fit.py` before the retirement moved it. A change here is a classification \
             decision with no oracle left to check it, so it needs its own argument rather than \
             a quiet edit"
        );
    }

    /// The scoped hashes are distinct from each other and from the flat one.
    /// If any two coincided the split would be decorative.
    ///
    /// These are regression pins on values of our own, not parity claims:
    /// `mnq_fit.py` has no per-mode hash to compare against, which is the whole
    /// reason these are new rather than ported.
    #[test]
    fn the_scoped_hashes_are_distinct_and_pinned() {
        let eleven = subcontract_hash_for(Mode::Protocol11);
        let twelve_a = subcontract_hash_for(Mode::Protocol12a);
        assert_ne!(eleven, twelve_a);
        assert_ne!(eleven, subcontract_hash());
        assert_ne!(twelve_a, subcontract_hash());

        // The scoped dumps must be strict subsets of the flat one, by length.
        assert!(subcontract_dumps_for(Mode::Protocol11).len() < subcontract_dumps().len());
        assert!(subcontract_dumps_for(Mode::Protocol12a).len() < subcontract_dumps().len());
    }

    /// The point of the exercise, stated as a test rather than as a comment:
    /// a protocol-12a constant is absent from the protocol-11 binding, so
    /// moving it cannot unbind a protocol-11 fit. Checked on the bytes, since
    /// that is what gets hashed.
    #[test]
    fn a_protocol_12a_constant_is_absent_from_the_protocol_11_binding() {
        let eleven = subcontract_dumps_for(Mode::Protocol11);
        for key in ["BOOTSTRAP_BASE_SEED", "EXCEEDANCE_TICKS", "GAP_CLOSE_MIN"] {
            assert!(
                !eleven.contains(key),
                "{key} is a protocol-12a constant and must not bind the protocol-11 fit"
            );
            assert!(
                subcontract_dumps_for(Mode::Protocol12a).contains(key),
                "{key} must still bind its own mode"
            );
        }
        // And a protocol-11 constant is in the fit's binding and not in 12a's.
        for key in ["FINAL_SEEDS", "VOL_GRID_POINTS"] {
            assert!(eleven.contains(key));
            assert!(!subcontract_dumps_for(Mode::Protocol12a).contains(key));
        }
    }

    #[test]
    fn float_repr_matches_python() {
        assert_eq!(py_float_repr(1.0), "1.0");
        assert_eq!(py_float_repr(0.9), "0.9");
        assert_eq!(py_float_repr(1.25), "1.25");
        assert_eq!(py_float_repr(1e-9), "1e-09");
        assert_eq!(py_float_repr(1e-8), "1e-08");
        assert_eq!(py_float_repr(0.0001), "0.0001");
        assert_eq!(py_float_repr(2.0), "2.0");
        assert_eq!(py_float_repr(4.6188), "4.6188");
        assert_eq!(py_float_repr(-0.19697), "-0.19697");
        assert_eq!(py_float_repr(0.99), "0.99");
    }
}
