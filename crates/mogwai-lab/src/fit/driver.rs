// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! `run_fit` from `analysis/mnq_fit.py`: the protocol-11 session calibration.
//!
//! Scope, frozen: the two session arrays and `vol_scalar`, nothing else. No
//! protocol-10 solve is executed - every other preset value resolves from
//! the shipped MNQ preset byte-for-byte through the scratch config's
//! `preset = "MNQ"` inheritance.
//!
//! The Python fanned its walks out as generator subprocesses under a
//! thread pool and replayed the SOLVE serially from the cache, so evaluation
//! order, tie-breaks and determinism were untouched by the parallelism - the
//! cache was the synchronization point. This port drops the pool and walks
//! in-process; because the cache is still the synchronization point and the
//! generator is CRN-deterministic, the solve sees exactly the same scores in
//! exactly the same order.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::error::{LabError, LabResult};
use crate::fit::curves::{
    cell_scale, exposed_utc_hours, fit_intensity_hour, fit_vol_hour, hour_exposure_weights,
    normalize_hour_curve, observed_walltime_curves,
};
use crate::fit::observe::{nearest_rank_of, observe};
use crate::fit::solve::{SLACK, solve_scalar};
use crate::fit::walk::{OverrideValue, Overrides, WalkCache, run_summary_walk};
use crate::fit::{
    Bound, CADENCE_NAMES, FAMILIES, MINUTE_RANGE_GATES, targets, tolerance, tolerance_json, within,
};
use crate::kernel::nearest_rank_list;
use crate::stream::{data_files, parse_stream};
use crate::subcontract::{
    FINAL_LENGTH, FINAL_SEEDS, FINAL_START_NS, GENERATED_SESSIONS_PER_SEED, JOB_ID,
    MIN_60S_CELL_RETURNS, MIN_300S_CELL_RETURNS, MIN_PARENT_CELL_RETURNS, MNQ_DOW_WEIGHT,
    SEARCH_LENGTH, SEARCH_SEEDS, SEARCH_START_NS, SESSION_ARRAY_DECIMALS, SESSION_HOUR_BAND,
    SESSION_VOL_CORR_MIN, SUMMARY_WARMUP, TOP_MINUTE_RECORDS, VOL_GRID_POINTS, VOL_SCALAR_DOMAIN,
    subcontract_hash,
};

/// Everything `run_fit` needs from its caller.
pub struct FitConfig {
    pub corpus: PathBuf,
    pub ledger: PathBuf,
    pub preflight: PathBuf,
    /// The Python-era `mnq-fit-scratch` directory, read-only.
    pub python_cache_dir: Option<PathBuf>,
    /// The harness commit the Python-era cache entries were keyed under.
    pub python_cache_commit: String,
    /// Where fresh walks may write scratch configs.
    pub scratch_dir: PathBuf,
    /// The commit this artifact binds. `None` means the caller has not
    /// established one; the driver refuses rather than inventing it.
    pub harness_commit: String,
    pub native_cache: Option<crate::storage::CacheStore>,
}

/// `pooled`: fold a seed's summaries into the one record the gates read.
#[must_use]
pub fn pooled(summaries: &[Value]) -> Value {
    let s_i64 = |key: &str| -> i64 { summaries.iter().map(|s| s[key].as_i64().unwrap_or(0)).sum() };
    let parents = s_i64("parents");
    let sided = s_i64("sided_rows");
    let singles = s_i64("single_parents");
    let levels = s_i64("level_count_sum");
    let gaps = s_i64("eligible_gaps");
    let gap_ns = s_i64("gap_sum_ns");

    let mut size_hist: BTreeMap<i64, i64> = BTreeMap::new();
    let mut bid_hist: BTreeMap<i64, i64> = BTreeMap::new();
    let mut ask_hist: BTreeMap<i64, i64> = BTreeMap::new();
    for s in summaries {
        for (src, dst) in [
            ("size_histogram", &mut size_hist),
            ("bid_size_histogram", &mut bid_hist),
            ("ask_size_histogram", &mut ask_hist),
        ] {
            if let Some(map) = s[src].as_object() {
                for (k, v) in map {
                    let key = k.parse::<f64>().unwrap_or(0.0) as i64;
                    *dst.entry(key).or_insert(0) += v.as_i64().unwrap_or(0);
                }
            }
        }
    }
    let mid_n = s_i64("mid_return_count");
    // `sum(...)` over floats: the compensated builtin (phase-2b pin).
    let mid_ss: f64 = crate::kernel::py_sum(
        summaries
            .iter()
            .map(|s| s["mid_return_sumsq"].as_f64().unwrap_or(0.0)),
    );
    let mut buyer: BTreeMap<i64, i64> = BTreeMap::new();
    let mut seller: BTreeMap<i64, i64> = BTreeMap::new();
    for s in summaries {
        for (src, dst) in [
            ("buyer_displacement_hist", &mut buyer),
            ("seller_displacement_hist", &mut seller),
        ] {
            if let Some(map) = s[src].as_object() {
                for (k, v) in map {
                    // The keys are bin LEFT EDGES the generator printed from
                    // index * 0.05; round(), not floor(), recovers the index
                    // because 0.05 is not exactly representable.
                    let key = (k.parse::<f64>().unwrap_or(0.0)
                        / crate::subcontract::DISPLACEMENT_BIN_TICKS)
                        .round() as i64;
                    *dst.entry(key).or_insert(0) += v.as_i64().unwrap_or(0);
                }
            }
        }
    }
    let mut width: BTreeMap<i64, i64> = BTreeMap::new();
    for s in summaries {
        if let Some(map) = s["width_ticks_histogram"].as_object() {
            for (k, v) in map {
                *width.entry(k.parse::<i64>().unwrap_or(0)).or_insert(0) += v.as_i64().unwrap_or(0);
            }
        }
    }
    let mut horizon: BTreeMap<String, (i64, f64, f64)> = BTreeMap::new();
    for s in summaries {
        if let Some(map) = s["horizon_vol"].as_object() {
            for (h, rec) in map {
                let e = horizon.entry(h.clone()).or_insert((0, 0.0, 0.0));
                e.0 += rec["count"].as_i64().unwrap_or(0);
                e.1 += rec["sum"].as_f64().unwrap_or(0.0);
                e.2 += rec["sumsq"].as_f64().unwrap_or(0.0);
            }
        }
    }
    let horizon_out: Map<String, Value> = horizon
        .into_iter()
        .map(|(h, (c, s, sq))| {
            (
                h,
                json!({
                    "count": c, "sum": s, "sumsq": sq,
                    "rms": if c > 0 { (sq / c as f64).sqrt() } else { f64::NAN },
                }),
            )
        })
        .collect();
    let hist_json = |h: &BTreeMap<i64, i64>| -> Value {
        Value::Object(h.iter().map(|(k, v)| (k.to_string(), json!(v))).collect())
    };
    json!({
        "parents": parents,
        // Python int/int: the pooled nanosecond gap sum passes 2^53 over
        // eight FINAL seeds, so pre-rounding the numerator to binary64 lands
        // one ulp off the committed artifact.
        "mean_event_duration_s": if gaps > 0 {
            crate::kernel::py_int_div(gap_ns, gaps) / 1e9
        } else { f64::NAN },
        "children_mean": if parents > 0 { sided as f64 / parents as f64 } else { f64::NAN },
        "children_single_frac": if parents > 0 { singles as f64 / parents as f64 } else { f64::NAN },
        "levels_mean": if parents > 0 { levels as f64 / parents as f64 } else { f64::NAN },
        "size_histogram": hist_json(&size_hist),
        "bid_size_histogram": hist_json(&bid_hist),
        "ask_size_histogram": hist_json(&ask_hist),
        "mid_rms": if mid_n > 0 { (mid_ss / mid_n as f64).sqrt() } else { f64::NAN },
        "displacement_hist": {"B": hist_json(&buyer), "A": hist_json(&seller)},
        "width_histogram": hist_json(&width),
        "horizon_vol": Value::Object(horizon_out),
        "first_book_mid": summaries.first().map_or(Value::Null, |s| s["first_book_mid"].clone()),
    })
}

// --- generated-side evidence (4.5-4.6) -------------------------------------

/// `(raw curve, normalized curve)` per horizon - the pair `seed_curves`
/// returns for each of the two wall-time horizons.
type HourCurvePair = (Option<BTreeMap<usize, f64>>, Option<BTreeMap<usize, f64>>);

struct SeedRec {
    seed: Value,
    parent_vol: Option<BTreeMap<usize, f64>>,
    parent_vol_raw: Option<BTreeMap<usize, f64>>,
    walltime: BTreeMap<i64, HourCurvePair>,
    arrival_count_by_hour: Vec<i64>,
    walltime_pooled: BTreeMap<i64, (i64, f64)>,
    shortfalls: Vec<Value>,
    session_cells: Value,
    top_minutes: Value,
}

fn seed_curves(summary: &Value) -> LabResult<SeedRec> {
    let cells = summary["session_cells"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let complete: Vec<&Value> = cells
        .iter()
        .filter(|c| c["complete"].as_bool().unwrap_or(false))
        .collect();
    if complete.len() as i64 != GENERATED_SESSIONS_PER_SEED {
        return Err(LabError::refusal(format!(
            "seed {}: {} complete generated sessions against the required {}",
            summary["seed"],
            complete.len(),
            GENERATED_SESSIONS_PER_SEED
        )));
    }
    let mut shortfalls: Vec<Value> = Vec::new();

    // EVERY failing seed/session/hour/count is recorded before the family
    // fails - the diagnostic trail is the point, so the scan never stops at
    // the first miss.
    let curve_from =
        |kind: &str, floor: i64, shortfalls: &mut Vec<Value>| -> LabResult<HourCurvePair> {
            let mut raw: BTreeMap<usize, f64> = BTreeMap::new();
            let mut qualified = true;
            for hour in exposed_utc_hours() {
                let mut scales: Vec<f64> = Vec::new();
                for sess in &complete {
                    let cell = if kind == "parent" {
                        &sess["mid_abs_by_hour"][hour]
                    } else {
                        &sess[format!("horizon_{kind}_by_hour")][hour]
                    };
                    let count = cell["count"].as_i64().unwrap_or(0);
                    if count < floor {
                        shortfalls.push(json!({
                            "kind": kind,
                            "hour": hour,
                            "seed": summary["seed"].clone(),
                            "session_start_ns": sess["session_start_ns"].clone(),
                            "count": count,
                            "floor": floor,
                        }));
                        qualified = false;
                        continue;
                    }
                    scales.push(cell_scale(
                        count,
                        cell["sum_abs"].as_f64().unwrap_or(f64::NAN),
                        cell["max_abs"].as_f64().unwrap_or(f64::NAN),
                    ));
                }
                if qualified {
                    scales.sort_by(f64::total_cmp);
                    raw.insert(
                        hour,
                        nearest_rank_list(&scales, 0.5)
                            .ok_or_else(|| LabError::refusal("empty list has no quantiles"))?,
                    );
                }
            }
            if !qualified {
                return Ok((None, None));
            }
            // Evidence curves carry exposed hours only; the normalizer's
            // conventional hour-21 value belongs to installed arrays.
            let full = normalize_hour_curve(&raw)?;
            let normalized: BTreeMap<usize, f64> =
                (0..24).filter(|h| *h != 21).map(|h| (h, full[h])).collect();
            Ok((Some(raw), Some(normalized)))
        };

    let (parent_vol_raw, parent_vol) =
        curve_from("parent", MIN_PARENT_CELL_RETURNS, &mut shortfalls)?;
    let (wt60_raw, wt60) = curve_from("60", MIN_60S_CELL_RETURNS, &mut shortfalls)?;
    let (wt300_raw, wt300) = curve_from("300", MIN_300S_CELL_RETURNS, &mut shortfalls)?;

    let mut counts = vec![0i64; 24];
    let mut pooled_wt: BTreeMap<i64, (i64, f64)> = [(60i64, (0i64, 0.0f64)), (300, (0, 0.0))]
        .into_iter()
        .collect();
    for sess in &complete {
        for hour in 0..24usize {
            counts[hour] += sess["parent_count_by_hour"][hour].as_i64().unwrap_or(0);
            for h in [60i64, 300] {
                let cell = &sess[format!("horizon_{h}_by_hour")][hour];
                let e = pooled_wt.get_mut(&h).expect("seeded");
                e.0 += cell["count"].as_i64().unwrap_or(0);
                e.1 += cell["sumsq"].as_f64().unwrap_or(0.0);
            }
        }
    }
    let mut walltime = BTreeMap::new();
    walltime.insert(60i64, (wt60_raw, wt60));
    walltime.insert(300i64, (wt300_raw, wt300));
    Ok(SeedRec {
        seed: summary["seed"].clone(),
        parent_vol,
        parent_vol_raw,
        walltime,
        arrival_count_by_hour: counts,
        walltime_pooled: pooled_wt,
        shortfalls,
        session_cells: summary["session_cells"].clone(),
        top_minutes: summary["top_minutes"].clone(),
    })
}

struct Evidence {
    per_seed: Vec<SeedRec>,
    arrival_marginal: BTreeMap<usize, f64>,
    central_parent_vol: Option<BTreeMap<usize, f64>>,
    central_wt: BTreeMap<i64, Option<BTreeMap<usize, f64>>>,
    walltime_pooled_rms: BTreeMap<i64, Option<f64>>,
    shortfalls: Vec<Value>,
}

fn generated_evidence(per_seed_summaries: &[Value]) -> LabResult<Evidence> {
    let seeds: Vec<SeedRec> = per_seed_summaries
        .iter()
        .map(seed_curves)
        .collect::<LabResult<_>>()?;
    let n_seeds = seeds.len() as i64;
    let weights = hour_exposure_weights();
    let mut rate_raw: BTreeMap<usize, f64> = BTreeMap::new();
    for hour in exposed_utc_hours() {
        let total: i64 = seeds.iter().map(|s| s.arrival_count_by_hour[hour]).sum();
        let denom = weights[hour] * GENERATED_SESSIONS_PER_SEED * n_seeds;
        rate_raw.insert(hour, total as f64 / denom as f64);
    }
    let full = normalize_hour_curve(&rate_raw)?;
    let arrival_marginal: BTreeMap<usize, f64> =
        (0..24).filter(|h| *h != 21).map(|h| (h, full[h])).collect();

    // Per-seed normalized curves, nearest-rank median across seeds per hour;
    // the across-seed curve is NOT renormalized.
    let central =
        |pick: &dyn Fn(&SeedRec) -> &Option<BTreeMap<usize, f64>>| -> Option<BTreeMap<usize, f64>> {
            if seeds.iter().any(|s| pick(s).is_none()) {
                return None;
            }
            let mut out = BTreeMap::new();
            for hour in exposed_utc_hours() {
                let mut vals: Vec<f64> = seeds
                    .iter()
                    .map(|s| pick(s).as_ref().expect("checked")[&hour])
                    .collect();
                vals.sort_by(f64::total_cmp);
                out.insert(hour, nearest_rank_list(&vals, 0.5).expect("nonempty"));
            }
            Some(out)
        };
    let central_parent_vol = central(&|s: &SeedRec| &s.parent_vol);
    let mut central_wt = BTreeMap::new();
    central_wt.insert(60i64, central(&|s: &SeedRec| &s.walltime[&60].1));
    central_wt.insert(300i64, central(&|s: &SeedRec| &s.walltime[&300].1));

    let mut walltime_pooled_rms = BTreeMap::new();
    for h in [60i64, 300] {
        let count: i64 = seeds.iter().map(|s| s.walltime_pooled[&h].0).sum();
        // `sum(...)` over floats: the compensated builtin (phase-2b pin).
        let sumsq: f64 = crate::kernel::py_sum(seeds.iter().map(|s| s.walltime_pooled[&h].1));
        // A zero-return horizon is a deliberate FAILED measurement,
        // represented as null - never NaN.
        walltime_pooled_rms.insert(
            h,
            if count > 0 {
                Some((sumsq / count as f64).sqrt())
            } else {
                None
            },
        );
    }
    let shortfalls: Vec<Value> = seeds.iter().flat_map(|s| s.shortfalls.clone()).collect();
    Ok(Evidence {
        per_seed: seeds,
        arrival_marginal,
        central_parent_vol,
        central_wt,
        walltime_pooled_rms,
        shortfalls,
    })
}

#[derive(Default)]
struct Judged {
    checks: BTreeMap<String, bool>,
    measured: Map<String, Value>,
    targets: Map<String, Value>,
}

impl Judged {
    fn passes(&self) -> bool {
        !self.checks.is_empty() && self.checks.values().all(|v| *v)
    }
}

fn as_list24(curve: Option<&BTreeMap<usize, f64>>) -> Value {
    match curve {
        None => Value::Null,
        Some(c) => Value::Array(
            (0..24)
                .map(|h| c.get(&h).map_or(Value::Null, |v| json!(v)))
                .collect(),
        ),
    }
}

/// The whole protocol-11 fit.
#[allow(
    clippy::too_many_lines,
    reason = "a faithful port of one 740-line Python driver; the artifact's \
              record shapes are its structure"
)]
pub fn run_fit(cfg: &FitConfig) -> LabResult<Value> {
    let hashes = crate::ledger::verify_input(&cfg.corpus, &cfg.ledger)?;
    let (preflight, preflight_hash) = crate::preflight::require_preflight(&hashes, &cfg.preflight)?;
    let usable: Vec<String> = preflight["usable_sessions"]
        .as_array()
        .ok_or_else(|| LabError::refusal("preflight carries no usable_sessions"))?
        .iter()
        .map(|v| v.as_str().unwrap_or_default().to_string())
        .collect();

    let observed = observe(parse_stream(data_files(&cfg.corpus)?), &usable)?;

    // --- closed-form session refits (4.2, 4.3, 4.6) ---
    let vol_hour_fit = fit_vol_hour(&observed, &usable)?;
    let intensity_fit = fit_intensity_hour(&observed, &usable)?;
    let walltime_obs = observed_walltime_curves(&observed, &usable)?;
    let candidate_vol_hour: Vec<f64> = vol_hour_fit["materialized"]
        .as_array()
        .expect("a 24-array")
        .iter()
        .map(|v| v.as_f64().expect("a float"))
        .collect();
    let candidate_intensity: Vec<f64> = intensity_fit["materialized"]
        .as_array()
        .expect("a 24-array")
        .iter()
        .map(|v| v.as_f64().expect("a float"))
        .collect();
    let mut session_overrides: Overrides = Overrides::new();
    session_overrides.insert(
        "session.vol_hour".into(),
        OverrideValue::Floats(candidate_vol_hour.clone()),
    );
    session_overrides.insert(
        "session.intensity_hour".into(),
        OverrideValue::Floats(candidate_intensity.clone()),
    );

    let mut cache = WalkCache::new(
        cfg.python_cache_dir.clone(),
        cfg.python_cache_commit.clone(),
        cfg.native_cache.clone(),
    );

    let obs_mid_rms = observed["mid_rms"].as_f64().unwrap_or(f64::NAN);

    // --- the vol_scalar re-solve (4.4) ---
    let vol_overrides = |scalar: f64| -> Overrides {
        let mut o = session_overrides.clone();
        o.insert("generator.vol_scalar".into(), OverrideValue::Float(scalar));
        o
    };
    let scratch = cfg.scratch_dir.clone();
    let vol_solve = {
        let cache = &mut cache;
        let scratch = &scratch;
        let mut vol_eval = |scalar: f64| -> f64 {
            let ov = vol_overrides(scalar);
            let mut raw = Vec::new();
            for seed in SEARCH_SEEDS.iter().copied() {
                match summary_for(
                    cache,
                    scratch,
                    &ov,
                    seed,
                    SEARCH_START_NS,
                    SEARCH_LENGTH,
                    SUMMARY_WARMUP,
                ) {
                    Ok(s) => raw.push(s),
                    // A failed walk is an infinite objective, exactly as the
                    // Python's Refusal propagation left it unfit.
                    Err(_) => return f64::INFINITY,
                }
            }
            let gen_pooled = pooled(&raw);
            let g = gen_pooled["mid_rms"].as_f64().unwrap_or(f64::NAN);
            if !g.is_finite() || g <= 0.0 {
                return f64::INFINITY;
            }
            (g - obs_mid_rms).abs() / obs_mid_rms
        };
        // The Python PREWARMED exactly `coarse_grid(*VOL_SCALAR_DOMAIN,
        // VOL_GRID_POINTS, log_domain=True)` in parallel before solving,
        // then replayed the solve serially from the cache. In-process there
        // is no pool to warm: `solve_scalar` regenerates the identical grid
        // and walks it serially, so the evaluation ORDER the Python's serial
        // replay had is exactly what happens here.
        solve_scalar(
            &mut vol_eval,
            VOL_SCALAR_DOMAIN.0,
            VOL_SCALAR_DOMAIN.1,
            VOL_GRID_POINTS as usize,
            true,
            None,
            Some(0.001),
        )
    };
    let candidate_vol_scalar = vol_solve["best_candidate"].as_f64().expect("a float");
    let mut solves = Map::new();
    let mut vol_record = vol_solve.as_object().expect("an object").clone();
    vol_record.insert("target".into(), json!(obs_mid_rms));

    // --- gate references ---
    let obs_marginal: Vec<Value> = intensity_fit["marginal_target"]["normalized"]
        .as_array()
        .expect("a 24-array")
        .clone();
    let obs_vol_curve = candidate_vol_hour.clone();

    let judge = |family: &str,
                 gen_pooled: &Value,
                 evidence: Option<&Evidence>,
                 per_seed: Option<&[Value]>|
     -> Judged {
        let mut out = Judged::default();
        let hour_gate = |out: &mut Judged,
                         name: &str,
                         curve: Option<&BTreeMap<usize, f64>>,
                         reference: &dyn Fn(usize) -> Option<f64>| {
            let (kind, bound) = tolerance(name);
            let mut per_hour = Map::new();
            let mut ok = curve.is_some();
            for hour in exposed_utc_hours() {
                match curve {
                    None => {
                        per_hour.insert(hour.to_string(), Value::Null);
                    }
                    Some(c) => {
                        let good =
                            within(kind, bound, c[&hour], reference(hour).unwrap_or(f64::NAN));
                        per_hour.insert(hour.to_string(), json!(good));
                        ok = ok && good;
                    }
                }
            }
            out.checks.insert(name.to_string(), ok);
            out.measured.insert(
                name.to_string(),
                match curve {
                    None => Value::Null,
                    Some(c) => Value::Array(
                        (0..24)
                            .map(|h| if h == 21 { Value::Null } else { json!(c[&h]) })
                            .collect(),
                    ),
                },
            );
            out.targets.insert(
                name.to_string(),
                Value::Array(
                    (0..24)
                        .map(|h| {
                            if h == 21 {
                                Value::Null
                            } else {
                                reference(h).map_or(Value::Null, |v| json!(v))
                            }
                        })
                        .collect(),
                ),
            );
            out.measured
                .insert(format!("{name}_per_hour"), Value::Object(per_hour));
        };

        if family == "session_arrival" {
            let reference = |h: usize| obs_marginal[h].as_f64();
            hour_gate(
                &mut out,
                "session_arrival_hour",
                evidence.map(|e| &e.arrival_marginal),
                &reference,
            );
        }
        if family == "session_parent_vol" {
            let reference = |h: usize| Some(obs_vol_curve[h]);
            hour_gate(
                &mut out,
                "session_vol_hour",
                evidence.and_then(|e| e.central_parent_vol.as_ref()),
                &reference,
            );
        }
        if family == "session_walltime" {
            for h in [60i64, 300] {
                let obs_hourly = walltime_obs[h.to_string()]["hourly"]["normalized"]
                    .as_array()
                    .expect("a 24-array")
                    .clone();
                let reference = |hh: usize| obs_hourly[hh].as_f64();
                hour_gate(
                    &mut out,
                    &format!("walltime_hour_{h}"),
                    evidence.and_then(|e| e.central_wt[&h].as_ref()),
                    &reference,
                );
                let name = format!("walltime_pooled_{h}");
                let (kind, bound) = tolerance(&name);
                let obs_pooled = walltime_obs[h.to_string()]["pooled_rms"]
                    .as_f64()
                    .unwrap_or(f64::NAN);
                let gen_rms = evidence.and_then(|e| e.walltime_pooled_rms[&h]);
                out.checks.insert(
                    name.clone(),
                    gen_rms.is_some_and(|g| within(kind, bound, g, obs_pooled)),
                );
                out.measured
                    .insert(name.clone(), gen_rms.map_or(Value::Null, |v| json!(v)));
                out.targets.insert(name, json!(obs_pooled));
            }
        }
        if family == "base_volatility" {
            for name in CADENCE_NAMES {
                let (kind, bound) = tolerance(name);
                let g = gen_pooled[name].as_f64().unwrap_or(f64::NAN);
                let o = observed[name].as_f64().unwrap_or(f64::NAN);
                out.checks
                    .insert(name.to_string(), within(kind, bound, g, o));
                out.measured.insert(name.to_string(), json!(g));
                out.targets.insert(name.to_string(), json!(o));
            }
            let g = gen_pooled["mid_rms"].as_f64().unwrap_or(f64::NAN);
            out.checks.insert(
                "mid_rms".into(),
                within("relative", Bound::Scalar(0.10), g, obs_mid_rms),
            );
            out.measured.insert("mid_rms".into(), json!(g));
            out.targets.insert("mid_rms".into(), json!(obs_mid_rms));

            let envelope = &observed["minute_range_envelope"];
            let mut seed_stats: Vec<BTreeMap<&str, i64>> = Vec::new();
            for summary in per_seed.unwrap_or(&[]) {
                let hist: BTreeMap<i64, i64> = summary["minute_range_ticks_hist"]
                    .as_object()
                    .map(|m| {
                        m.iter()
                            .map(|(k, v)| (k.parse::<i64>().unwrap_or(0), v.as_i64().unwrap_or(0)))
                            .collect()
                    })
                    .unwrap_or_default();
                let mut rec = BTreeMap::new();
                rec.insert(
                    "p99",
                    if hist.is_empty() {
                        0
                    } else {
                        nearest_rank_of(&hist, 0.99).unwrap_or(0)
                    },
                );
                rec.insert(
                    "p99.9",
                    if hist.is_empty() {
                        0
                    } else {
                        nearest_rank_of(&hist, 0.999).unwrap_or(0)
                    },
                );
                rec.insert(
                    "max",
                    summary["minute_range_max_ticks"].as_i64().unwrap_or(0),
                );
                seed_stats.push(rec);
            }
            for stat in MINUTE_RANGE_GATES {
                let name = format!("minute_range_{stat}");
                let bound = envelope[stat].as_f64().unwrap_or(f64::NAN);
                let values: Vec<i64> = seed_stats.iter().map(|s| s[stat]).collect();
                out.checks.insert(
                    name.clone(),
                    !values.is_empty() && values.iter().all(|v| (*v as f64) <= bound + SLACK),
                );
                out.measured.insert(name.clone(), json!(values));
                out.targets.insert(name, envelope[stat].clone());
            }
        }
        if let Some(e) = evidence
            && !e.shortfalls.is_empty()
        {
            out.measured
                .insert("generated_cell_shortfalls".into(), json!(e.shortfalls));
        }
        out
    };

    // --- family probes then the final combined run (4.5) ---
    let mut combined_overrides = session_overrides.clone();
    combined_overrides.insert(
        "generator.vol_scalar".into(),
        OverrideValue::Float(candidate_vol_scalar),
    );
    let mut arrival_only: Overrides = Overrides::new();
    arrival_only.insert(
        "session.intensity_hour".into(),
        OverrideValue::Floats(candidate_intensity.clone()),
    );
    let probe_defs: BTreeMap<&str, Overrides> = [
        ("session_arrival", arrival_only),
        ("session_parent_vol", combined_overrides.clone()),
        ("session_walltime", combined_overrides.clone()),
        ("base_volatility", combined_overrides.clone()),
    ]
    .into_iter()
    .collect();

    let mut probe_results: BTreeMap<&str, Judged> = BTreeMap::new();
    let mut probe_errors: BTreeMap<&str, String> = BTreeMap::new();
    for family in FAMILIES {
        let ov = &probe_defs[family];
        let run = (|| -> LabResult<(Value, Vec<Value>, Evidence)> {
            let mut raw = Vec::new();
            for seed in FINAL_SEEDS.iter().copied() {
                raw.push(summary_for(
                    &mut cache,
                    &scratch,
                    ov,
                    seed,
                    FINAL_START_NS,
                    FINAL_LENGTH,
                    SUMMARY_WARMUP,
                )?);
            }
            let p = pooled(&raw);
            let e = generated_evidence(&raw)?;
            Ok((p, raw, e))
        })();
        match run {
            Ok((p, raw, e)) => {
                probe_results.insert(family, judge(family, &p, Some(&e), Some(&raw)));
            }
            Err(err) => {
                let mut j = Judged::default();
                j.checks.insert("probe_run".into(), false);
                probe_results.insert(family, j);
                probe_errors.insert(family, err.to_string());
            }
        }
    }

    // The final combined run is attempted REGARDLESS of individual probe
    // misses, so the artifact records interactions.
    let mut combined_results: BTreeMap<&str, Judged> = BTreeMap::new();
    let mut combined_evidence: Option<Evidence> = None;
    let mut combined_error: Option<String> = None;
    let combined = (|| -> LabResult<(Value, Vec<Value>, Evidence)> {
        let mut raw = Vec::new();
        for seed in FINAL_SEEDS.iter().copied() {
            raw.push(summary_for(
                &mut cache,
                &scratch,
                &combined_overrides,
                seed,
                FINAL_START_NS,
                FINAL_LENGTH,
                SUMMARY_WARMUP,
            )?);
        }
        let p = pooled(&raw);
        let e = generated_evidence(&raw)?;
        Ok((p, raw, e))
    })();
    match combined {
        Ok((p, raw, e)) => {
            for family in FAMILIES {
                combined_results.insert(family, judge(family, &p, Some(&e), Some(&raw)));
            }
            let gen_rms = p["mid_rms"].as_f64().unwrap_or(f64::NAN);
            vol_record.insert(
                "final_score".into(),
                if gen_rms.is_finite() && obs_mid_rms > 0.0 {
                    json!((gen_rms - obs_mid_rms).abs() / obs_mid_rms)
                } else {
                    Value::Null
                },
            );
            combined_evidence = Some(e);
        }
        Err(err) => combined_error = Some(err.to_string()),
    }
    solves.insert("vol_scalar".into(), Value::Object(vol_record));

    let family_ok = |family: &str| -> bool {
        probe_results[family].passes()
            && combined_error.is_none()
            && combined_results.get(family).is_some_and(Judged::passes)
    };
    let stage_checks = |family: &str, names: &[&str]| -> bool {
        let probe = probe_results.get(family);
        let combined = combined_results.get(family);
        combined_error.is_none()
            && probe.is_some()
            && combined.is_some()
            && names
                .iter()
                .all(|n| probe.expect("checked").checks.get(*n) == Some(&true))
            && names
                .iter()
                .all(|n| combined.expect("checked").checks.get(*n) == Some(&true))
    };

    // The Brick V amendment: the wall-time family splits by role. The pooled
    // gates land; the hourly contour is RECORDED and never gates protocol 11.
    let walltime_pooled_ok = stage_checks(
        "session_walltime",
        &["walltime_pooled_60", "walltime_pooled_300"],
    );
    let walltime_hourly_ok = stage_checks(
        "session_walltime",
        &["walltime_hour_60", "walltime_hour_300"],
    );
    let session_ok =
        family_ok("session_arrival") && family_ok("session_parent_vol") && walltime_pooled_ok;
    let cadence_ok = stage_checks("base_volatility", &CADENCE_NAMES);
    let pooled_rms_ok = stage_checks("base_volatility", &["mid_rms"]);
    let envelope_names: Vec<String> = MINUTE_RANGE_GATES
        .iter()
        .map(|s| format!("minute_range_{s}"))
        .collect();
    let envelope_ok = stage_checks(
        "base_volatility",
        &envelope_names
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    );

    let arrays_land = session_ok && cadence_ok && pooled_rms_ok;
    let vol_fitted = arrays_land && envelope_ok;

    let failure_reason = |family: &str| -> Option<String> {
        if let Some(e) = probe_errors.get(family) {
            return Some(e.clone());
        }
        if let Some(e) = &combined_error {
            return Some(format!("the combined run failed: {e}"));
        }
        if !cadence_ok {
            return Some(
                "cadence regressed under the candidate arrays; a cadence regression refuses \
                 protocol 11 outright"
                    .into(),
            );
        }
        if !pooled_rms_ok {
            return Some(
                "the pooled parent RMS missed its target; protocol 11 refuses rather than \
                 landing arrays with a known-wrong scale"
                    .into(),
            );
        }
        if !session_ok {
            return Some("a session landing gate failed; the atomic group does not land".into());
        }
        None
    };

    let mut verdicts = Map::new();
    for (target, family, metrics) in targets() {
        let probe = &probe_results[family];
        let combined = combined_results.get(family);
        let (status, reason) = if target == "intensity_hour" || target == "vol_hour" {
            if arrays_land {
                ("fitted", None)
            } else {
                ("declared-misrepresented", failure_reason(family))
            }
        } else if vol_fitted {
            ("fitted", None)
        } else if arrays_land {
            (
                "declared-best-candidate",
                Some(
                    "the minute-range envelope failed; the best candidate is carried under \
                     declared provenance as protocol 12's motivating evidence"
                        .to_string(),
                ),
            )
        } else {
            ("declared-misrepresented", failure_reason(family))
        };
        let pick = |j: &Judged, from_measured: bool| -> Value {
            Value::Object(
                metrics
                    .iter()
                    .map(|m| {
                        let src = if from_measured {
                            &j.measured
                        } else {
                            &j.targets
                        };
                        (
                            (*m).to_string(),
                            src.get(*m).cloned().unwrap_or(Value::Null),
                        )
                    })
                    .collect(),
            )
        };
        let pick_checks = |j: &Judged| -> Value {
            Value::Object(
                metrics
                    .iter()
                    .map(|m| {
                        (
                            (*m).to_string(),
                            j.checks.get(*m).map_or(Value::Null, |b| json!(b)),
                        )
                    })
                    .collect(),
            )
        };
        let mut record = Map::new();
        record.insert("family".into(), json!(family));
        record.insert("status".into(), json!(status));
        record.insert(
            "tolerance".into(),
            Value::Object(
                metrics
                    .iter()
                    .map(|m| ((*m).to_string(), tolerance_json(m)))
                    .collect(),
            ),
        );
        record.insert(
            "measured".into(),
            json!({
                "probe": pick(probe, true),
                "combined": combined.map_or(Value::Null, |c| pick(c, true)),
            }),
        );
        record.insert("observed".into(), pick(probe, false));
        record.insert(
            "checks".into(),
            json!({
                "probe": pick_checks(probe),
                "combined": combined.map_or(Value::Null, pick_checks),
            }),
        );
        if let Some(r) = reason {
            record.insert("reason".into(), json!(r));
        }
        verdicts.insert(target.to_string(), Value::Object(record));
    }

    let mut landing: Vec<String> = Vec::new();
    if arrays_land {
        for (target, verdict) in &verdicts {
            if verdict["status"] == "fitted" {
                landing.push(target.clone());
            }
        }
        landing.sort();
    }

    let mut diagnostics = observed["diagnostics"]
        .as_object()
        .expect("an object")
        .clone();
    diagnostics.insert(
        "generated_cell_shortfalls".into(),
        combined_evidence
            .as_ref()
            .map_or(Value::Null, |e| json!(e.shortfalls)),
    );
    diagnostics.insert(
        "sqrt_decomposition".into(),
        json!({
            "note": "retired per-minute vol_hour vs sqrt of the July parent marginal; \
                     lineage diagnostic, not an exponent estimate",
            "retired_vol_hour_peak_to_trough": 1.8702 / 0.5533,
            "fitted_parent_vol_curve": candidate_vol_hour.clone(),
            "fitted_curve_inverted": (0..=8usize)
                .filter(|h| *h != 21)
                .map(|h| candidate_vol_hour[h])
                .fold(f64::NEG_INFINITY, f64::max)
                > 1.0,
        }),
    );

    // --- the frozen session_refit artifact block (spec Brick H) ---
    let observed_cell_records = |cells_map: &Value, horizon: bool| -> Value {
        let zero = if horizon {
            json!({"count": 0, "sum": 0.0, "sumsq": 0.0, "sum_abs": 0.0, "max_abs": 0.0})
        } else {
            json!({"count": 0, "sum_abs": 0.0, "max_abs": 0.0})
        };
        let mut labels: Vec<&String> = cells_map
            .as_object()
            .map(|m| m.keys().collect())
            .unwrap_or_default();
        labels.sort();
        Value::Array(
            labels
                .into_iter()
                .map(|label| {
                    json!({
                        "session": label,
                        "cells": (0..24).map(|h| {
                            cells_map[label].get(h.to_string()).cloned()
                                .unwrap_or_else(|| zero.clone())
                        }).collect::<Vec<Value>>(),
                    })
                })
                .collect(),
        )
    };

    let per_seed_record = |rec: &SeedRec| -> Value {
        let mut walltime = Map::new();
        for h in [60i64, 300] {
            let (count, sumsq) = rec.walltime_pooled[&h];
            let rms = if count > 0 {
                json!((sumsq / count as f64).sqrt())
            } else {
                Value::Null
            };
            walltime.insert(
                h.to_string(),
                json!({
                    "hourly": {
                        "raw": as_list24(rec.walltime[&h].0.as_ref()),
                        "normalized": as_list24(rec.walltime[&h].1.as_ref()),
                    },
                    "pooled_rms": rms,
                    "return_count": count,
                }),
            );
        }
        json!({
            "seed": rec.seed.clone(),
            "session_cells": rec.session_cells.clone(),
            "parent_vol_curve": {
                "raw": as_list24(rec.parent_vol_raw.as_ref()),
                "normalized": as_list24(rec.parent_vol.as_ref()),
            },
            "walltime_curves": Value::Object(walltime),
            "arrival_count_by_hour": rec.arrival_count_by_hour.clone(),
            "top_minutes": rec.top_minutes.clone(),
        })
    };

    // BOTH stage records always exist (frozen schema): a stage whose run
    // never happened carries an all-null record with pass null.
    let session_gate_records = |metric: &str, family: &str| -> Value {
        let mut records = Vec::new();
        for (stage, results) in [
            ("probe", probe_results.get(family)),
            ("combined", combined_results.get(family)),
        ] {
            let per_hour_map = results
                .and_then(|r| r.measured.get(&format!("{metric}_per_hour")))
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let gen_curve = results.and_then(|r| r.measured.get(metric));
            let obs_curve = results.and_then(|r| r.targets.get(metric));
            let mut worst_hour = Value::Null;
            let mut worst_ratio: Option<f64> = None;
            if let (Some(g), Some(o)) = (gen_curve, obs_curve)
                && g.is_array()
                && o.is_array()
            {
                for h in exposed_utc_hours() {
                    let (Some(gv), Some(ov)) = (g[h].as_f64(), o[h].as_f64()) else {
                        continue;
                    };
                    if ov == 0.0 {
                        continue;
                    }
                    let ratio = (gv / ov - 1.0).abs();
                    if worst_ratio.is_none_or(|w| ratio > w) {
                        worst_ratio = Some(ratio);
                        worst_hour = json!(h);
                    }
                }
            }
            records.push(json!({
                "family": family,
                "stage": stage,
                "per_hour": (0..24).map(|h| if h == 21 { Value::Null } else {
                    per_hour_map.get(&h.to_string()).cloned().unwrap_or(Value::Null)
                }).collect::<Vec<Value>>(),
                "worst_hour": worst_hour,
                "worst_ratio": worst_ratio.map_or(Value::Null, |v| json!(v)),
                "pass": results.and_then(|r| r.checks.get(metric))
                    .map_or(Value::Null, |b| json!(b)),
            }));
        }
        Value::Array(records)
    };

    // Status vocabulary: passed iff both stage checks read true, not-run when
    // either stage never produced a check, failed otherwise.
    let metric_verdict_record = |metric: &str, family: &str| -> Value {
        let probe = probe_results.get(family);
        let combined = combined_results.get(family);
        let probe_check = probe.and_then(|p| p.checks.get(metric)).copied();
        let combined_check = combined.and_then(|c| c.checks.get(metric)).copied();
        let status = match (probe_check, combined_check) {
            (Some(true), Some(true)) => "passed",
            (Some(_), Some(_)) => "failed",
            _ => "not-run",
        };
        json!({
            "family": family,
            "status": status,
            "tolerance": tolerance_json(metric),
            "measured": {
                "probe": probe.and_then(|p| p.measured.get(metric).cloned())
                    .unwrap_or(Value::Null),
                "combined": combined.and_then(|c| c.measured.get(metric).cloned())
                    .unwrap_or(Value::Null),
            },
            "observed": probe.and_then(|p| p.targets.get(metric).cloned())
                .unwrap_or(Value::Null),
            "checks": {
                "probe": probe_check.map_or(Value::Null, |b| json!(b)),
                "combined": combined_check.map_or(Value::Null, |b| json!(b)),
            },
        })
    };

    let raw_obs = &observed["session_refit_raw"];
    let session_refit = json!({
        "constants": {
            "MIN_PARENT_CELL_RETURNS": MIN_PARENT_CELL_RETURNS,
            "MIN_60S_CELL_RETURNS": MIN_60S_CELL_RETURNS,
            "MIN_300S_CELL_RETURNS": MIN_300S_CELL_RETURNS,
            "SESSION_HOUR_BAND": [SESSION_HOUR_BAND.0, SESSION_HOUR_BAND.1],
            "ARRIVAL_HOUR_REL_TOL": crate::subcontract::ARRIVAL_HOUR_REL_TOL,
            "WALLTIME_POOLED_REL_TOL": crate::subcontract::WALLTIME_POOLED_REL_TOL,
            "SESSION_ARRAY_DECIMALS": SESSION_ARRAY_DECIMALS,
            "TOP_MINUTE_RECORDS": TOP_MINUTE_RECORDS,
            "SESSION_VOL_CORR_MIN": SESSION_VOL_CORR_MIN,
        },
        "observed": {
            "session_count": usable.len(),
            "parent_count_by_hour": raw_obs["parent_count_by_hour_dow"].as_array()
                .expect("a 24x7 table").iter()
                .map(|row| row.as_array().expect("a row").iter()
                    .map(|v| v.as_i64().unwrap_or(0)).sum::<i64>())
                .collect::<Vec<i64>>(),
            "parent_count_by_hour_dow": raw_obs["parent_count_by_hour_dow"].clone(),
            "open_minutes_by_hour_dow": intensity_fit["open_minutes_by_hour_dow"].clone(),
            "parent_rate_target": intensity_fit["marginal_target"].clone(),
            "parent_vol_cells": observed_cell_records(&raw_obs["parent_vol_cells"], false),
            "parent_vol_curve": vol_hour_fit.clone(),
            "horizon_60_cells": observed_cell_records(&raw_obs["horizon_cells"]["60"], true),
            "horizon_300_cells": observed_cell_records(&raw_obs["horizon_cells"]["300"], true),
            "walltime_curves": walltime_obs.clone(),
        },
        "candidate": {
            "intensity_hour": {
                "raw": intensity_fit["raw"].clone(),
                "normalized_unrounded": intensity_fit["normalized_unrounded"].clone(),
                "materialized": intensity_fit["materialized"].clone(),
            },
            "vol_hour": vol_hour_fit.clone(),
            "dow_weight": MNQ_DOW_WEIGHT,
            "vol_scalar": candidate_vol_scalar,
        },
        "generated": {
            "final_seeds": FINAL_SEEDS,
            "per_seed": combined_evidence.as_ref().map_or_else(Vec::new, |e| {
                e.per_seed.iter().map(per_seed_record).collect()
            }),
            "central_curves": {
                "parent_vol": combined_evidence.as_ref()
                    .map_or(Value::Null, |e| as_list24(e.central_parent_vol.as_ref())),
                "walltime_60": combined_evidence.as_ref()
                    .map_or(Value::Null, |e| as_list24(e.central_wt[&60].as_ref())),
                "walltime_300": combined_evidence.as_ref()
                    .map_or(Value::Null, |e| as_list24(e.central_wt[&300].as_ref())),
                "arrival_marginal": combined_evidence.as_ref()
                    .map_or(Value::Null, |e| as_list24(Some(&e.arrival_marginal))),
            },
        },
        "verdicts": {
            "session_arrival": session_gate_records("session_arrival_hour", "session_arrival"),
            "session_parent_vol": session_gate_records("session_vol_hour", "session_parent_vol"),
            "session_walltime_60": session_gate_records("walltime_hour_60", "session_walltime"),
            "session_walltime_300": session_gate_records("walltime_hour_300", "session_walltime"),
            "walltime_pooled_60": metric_verdict_record("walltime_pooled_60", "session_walltime"),
            "walltime_pooled_300": metric_verdict_record("walltime_pooled_300", "session_walltime"),
            "mid_rms": metric_verdict_record("mid_rms", "base_volatility"),
            "minute_range_p99": metric_verdict_record("minute_range_p99", "base_volatility"),
            "minute_range_p99.9": metric_verdict_record("minute_range_p99.9", "base_volatility"),
            "minute_range_max": metric_verdict_record("minute_range_max", "base_volatility"),
        },
    });

    let mut landing_rule = Map::new();
    landing_rule.insert("session_ok".into(), json!(session_ok));
    landing_rule.insert("walltime_pooled_ok".into(), json!(walltime_pooled_ok));
    landing_rule.insert("walltime_hourly_ok".into(), json!(walltime_hourly_ok));
    landing_rule.insert("cadence_ok".into(), json!(cadence_ok));
    landing_rule.insert("pooled_rms_ok".into(), json!(pooled_rms_ok));
    landing_rule.insert("envelope_ok".into(), json!(envelope_ok));
    landing_rule.insert("arrays_land".into(), json!(arrays_land));
    if let Some(e) = &combined_error {
        landing_rule.insert("combined_error".into(), json!(e));
    }

    let artifact = json!({
        "binding": {
            "job_id": JOB_ID,
            "file_hashes": hashes,
            "preflight_artifact_hash": preflight_hash,
            "subcontract_hash": subcontract_hash(),
            "harness_tree_commit": cfg.harness_commit,
        },
        "sessions": {
            "inventory": preflight["sessions"].clone(),
            "usable_count": usable.len(),
        },
        "preflight": {
            "rows": preflight["rows"].clone(),
            "unsided_share": preflight["unsided_share"].clone(),
            "invalid_width_share": preflight["invalid_width_share"].clone(),
            "book_counts": preflight["book_counts"].clone(),
            "valid_parent_quote_share": preflight["valid_parent_quote_share"].clone(),
        },
        "observed": observed,
        "solves": Value::Object(solves),
        "session_refit": session_refit,
        "landing_rule": Value::Object(landing_rule),
        "verdicts": Value::Object(verdicts),
        "diagnostics": Value::Object(diagnostics),
        "fitted_candidates": {
            "intensity_hour": candidate_intensity,
            "vol_hour": candidate_vol_hour,
            "vol_scalar": candidate_vol_scalar,
        },
        "landing_set": landing,
    });
    eprintln!(
        "[fit] walk cache: {} python hits, {} native hits, {} misses",
        cache.stats.python_hits, cache.stats.native_hits, cache.stats.misses
    );
    Ok(artifact)
}

/// One cached-or-fresh walk. The cache is the synchronization point the
/// solve's determinism rests on.
fn summary_for(
    cache: &mut WalkCache,
    scratch: &Path,
    overrides: &Overrides,
    seed: i64,
    start_ns: i64,
    length: &str,
    warmup: &str,
) -> LabResult<Value> {
    if let Some(hit) = cache.get(overrides, seed, start_ns, length, warmup) {
        return Ok(hit);
    }
    let summary = run_summary_walk(scratch, overrides, seed, start_ns, length, warmup)?;
    cache.put(overrides, seed, start_ns, length, warmup, &summary)?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pooled_recovers_the_displacement_bin_index_by_rounding() {
        // "0.15" parses to 2.9999.../0.05 - floor would be off by one, so
        // the Python rounds. Same input, same recovery.
        let s = json!({
            "parents": 2, "sided_rows": 2, "single_parents": 2,
            "level_count_sum": 2, "eligible_gaps": 1, "gap_sum_ns": 1_000_000_000,
            "size_histogram": {}, "bid_size_histogram": {}, "ask_size_histogram": {},
            "buyer_displacement_hist": {"0.15": 3}, "seller_displacement_hist": {},
            "width_ticks_histogram": {}, "mid_return_count": 1, "mid_return_sumsq": 4.0,
            "horizon_vol": {}, "first_book_mid": "1.0",
        });
        let p = pooled(&[s]);
        assert_eq!(p["displacement_hist"]["B"]["3"], 3);
        assert_eq!(p["mid_rms"], 2.0);
        assert_eq!(p["mean_event_duration_s"], 1.0);
    }

    #[test]
    fn pooled_over_no_gaps_reads_nan_rather_than_dividing_by_zero() {
        let s = json!({
            "parents": 0, "sided_rows": 0, "single_parents": 0,
            "level_count_sum": 0, "eligible_gaps": 0, "gap_sum_ns": 0,
            "size_histogram": {}, "bid_size_histogram": {}, "ask_size_histogram": {},
            "buyer_displacement_hist": {}, "seller_displacement_hist": {},
            "width_ticks_histogram": {}, "mid_return_count": 0, "mid_return_sumsq": 0.0,
            "horizon_vol": {}, "first_book_mid": Value::Null,
        });
        let p = pooled(&[s]);
        // `serde_json` renders a NaN as `null`, where CPython's `json.dumps`
        // writes the bare token `NaN`. The difference is unreachable in a
        // real fit - every one of these denominators is populated, which the
        // parity gate demonstrates over the delivered corpus - and the
        // artifact's strict writer refuses non-finites anyway. What matters
        // here is that the empty case NEVER divides by zero.
        assert!(p["mean_event_duration_s"].is_null());
        assert!(p["children_mean"].is_null());
    }
}
