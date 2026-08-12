// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The signed ordered-count extension of `count-curve`.

#![expect(
    clippy::collapsible_if,
    clippy::needless_range_loop,
    clippy::semicolon_if_nothing_returned,
    clippy::single_element_loop,
    clippy::type_complexity,
    clippy::useless_vec,
    reason = "the loops mirror the signed estimator and permutation formulas"
)]

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow, bail};
use mogwai_lab::aggregate::artifact::write_json_atomic;
use mogwai_lab::aggregate::bootstrap::bootstrap_multiplicities;
use mogwai_lab::kernel::{splitmix64, tuple_mix};
use mogwai_lab::measure12a::OrderedCount;
use serde_json::{Value, json};

use crate::measure::run_observed_ordered;

const CORPUS: &str = "research/market-data/databento/mnqv/2026-07.full.tbbo";
const LEDGER: &str = "analysis/databento-jobs.json";
const PREFLIGHT: &str = "analysis/out/mnq-fit-preflight.json";
const SEQUENCE: &str = "analysis/out/ordered-counts.jsonl";
const SUMMARY: &str = "analysis/out/ordered-counts-panels.json";
const PERM_SEED: u64 = 8_934_572_019_384_756_123;
const REPS: usize = 2_000;
const HOURS: usize = 23;
const LAGS: [usize; 11] = [1, 2, 5, 10, 30, 60, 120, 300, 600, 1200, 1800];
const BLOCKS: [usize; 7] = [1, 5, 15, 60, 300, 900, 1800];

#[derive(Clone, Default)]
struct LagSuff {
    n: u64,
    prod: f64,
    diff2: f64,
}

#[derive(Clone)]
struct HourSuff {
    n: usize,
    sq: f64,
    lags: Vec<LagSuff>,
    blocks: Vec<(u64, f64)>,
    runs: Vec<usize>,
}

#[derive(Clone)]
struct SessionSuff {
    date: String,
    hours: Vec<HourSuff>,
    rates: Vec<f64>,
}

#[derive(Clone)]
struct Tail {
    values: Vec<f64>,
    a: Option<f64>,
    tau: Option<f64>,
    clamped: bool,
    reason: Option<&'static str>,
}

pub fn run() -> anyhow::Result<()> {
    run_with(&OrderedCountsRun::july())
}

pub struct OrderedCountsRun {
    pub month: u64,
    pub corpus: PathBuf,
    pub ledger: PathBuf,
    pub preflight: PathBuf,
    pub sequence: PathBuf,
    pub summary: PathBuf,
    pub permutation_seed: u64,
    pub bootstrap: Vec<Vec<i64>>,
    pub require_july_backcheck: bool,
}

impl OrderedCountsRun {
    fn july() -> Self {
        Self {
            month: 202_607,
            corpus: CORPUS.into(),
            ledger: LEDGER.into(),
            preflight: PREFLIGHT.into(),
            sequence: SEQUENCE.into(),
            summary: SUMMARY.into(),
            permutation_seed: PERM_SEED,
            bootstrap: Vec::new(),
            require_july_backcheck: true,
        }
    }
}

pub fn run_with(config: &OrderedCountsRun) -> anyhow::Result<()> {
    let reference: Value =
        serde_json::from_str(include_str!("../../../analysis/mnq-measure-12a.json"))?;
    let (observed, mut rows) = if config.sequence.exists() {
        let prior: Value = if config.summary.exists() {
            serde_json::from_slice(&std::fs::read(&config.summary)?)?
        } else {
            serde_json::from_str(include_str!(
                "../../../analysis/out/ordered-counts-panels.json"
            ))?
        };
        (
            json!({"binding":prior["binding"]["observed"].clone()}),
            read_sequence(&config.sequence)?,
        )
    } else {
        let (observed, rows) =
            run_observed_ordered(&config.corpus, &config.ledger, &config.preflight)?;
        (observed, rows)
    };
    finish(config, &reference, &observed, &mut rows)
}

pub(crate) fn run_with_rows(
    config: &OrderedCountsRun,
    observed: &Value,
    mut rows: Vec<OrderedCount>,
) -> anyhow::Result<()> {
    let reference: Value =
        serde_json::from_str(include_str!("../../../analysis/mnq-measure-12a.json"))?;
    finish(config, &reference, observed, &mut rows)
}

fn finish(
    config: &OrderedCountsRun,
    reference: &Value,
    observed: &Value,
    rows: &mut [OrderedCount],
) -> anyhow::Result<()> {
    rows.sort_by_key(|r| (r.session_date.clone(), r.segment_index, r.window_start_ns));
    if !config.sequence.exists() {
        write_sequence(&config.sequence, rows)?;
    }
    let sequence_sha256 =
        mogwai_lab::ledger::sha256_file(&config.sequence).map_err(|e| anyhow!(e.to_string()))?;

    if config.require_july_backcheck
        && let Some(mismatch) = backcheck(rows, reference)?
    {
        let out = json!({
            "binding": {"observed": observed["binding"].clone(), "sequence_sha256": sequence_sha256},
            "panel_a": {"outcome": "backcheck_mismatch"},
            "panel_b": {"outcome": "backcheck_mismatch"},
            "backcheck": {"matched": false, "first_mismatch": mismatch}
        });
        write_json_atomic(&config.summary, &out).map_err(|e| anyhow!(e.to_string()))?;
        println!("ordered-count outcome: backcheck_mismatch");
        return Ok(());
    }
    println!("ordered-count reconstruction backcheck matched exactly");

    let sessions = prepare(rows)?;
    let mults = if config.bootstrap.is_empty() {
        bootstrap_multiplicities(sessions.len())
    } else {
        config.bootstrap.clone()
    };
    let unit = vec![1_i64; sessions.len()];
    let panel_a = panel_a(&sessions, &unit, Some(&mults[..REPS]))?;
    let panel_b = panel_b(
        &sessions,
        &unit,
        Some(&mults[..REPS]),
        config.month,
        config.permutation_seed,
    )?;
    let out = json!({
        "binding": {
            "observed": observed["binding"].clone(),
            "month": config.month,
            "usable_sessions": sessions.len(),
            "thin": sessions.len() < 15,
            "sequence_path": config.sequence,
            "sequence_sha256": sequence_sha256,
            "ordered_counts_perm_seed": config.permutation_seed,
            "permutation_replicates": REPS,
            "bootstrap_replicates": REPS
        },
        "backcheck": {"matched": true, "horizons_s": [1, 5, 60], "fields": ["scheduled_windows", "zero_windows", "count_hist"]},
        "panel_a": panel_a,
        "panel_b": panel_b
    });
    write_json_atomic(&config.summary, &out).map_err(|e| anyhow!(e.to_string()))?;
    println!("ordered-count sequence -> {}", config.sequence.display());
    println!("ordered-count panels -> {}", config.summary.display());
    println!("ordered-count outcome: completed");
    Ok(())
}

fn read_sequence(path: &Path) -> anyhow::Result<Vec<OrderedCount>> {
    BufReader::new(File::open(path)?)
        .lines()
        .map(|line| Ok(serde_json::from_str(&line?)?))
        .collect()
}

/// Reconstruct the per-session Block 2 count histograms needed by the count
/// curve from the retained canonical one-second sequence.
pub(crate) fn count_curve_sessions_from_sequence(path: &Path) -> anyhow::Result<Vec<Value>> {
    let rows = read_sequence(path)?;
    let mut sessions: BTreeMap<String, BTreeMap<(u64, usize), BTreeMap<u32, u64>>> =
        BTreeMap::new();
    let mut at = 0;
    while at < rows.len() {
        let end = rows[at..]
            .iter()
            .position(|r| {
                r.session_date != rows[at].session_date || r.segment_index != rows[at].segment_index
            })
            .map_or(rows.len(), |x| at + x);
        let seg = &rows[at..end];
        for w in [1_usize, 5, 15, 60, 300] {
            for chunk in seg.chunks_exact(w) {
                let first = &chunk[0];
                let last = &chunk[w - 1];
                if first.window_start_ns / 3_600_000_000_000
                    != last.window_end_ns / 3_600_000_000_000
                {
                    continue;
                }
                let count = chunk.iter().map(|r| r.parent_count).sum::<u32>();
                *sessions
                    .entry(first.session_date.clone())
                    .or_default()
                    .entry((last.endpoint_hour, w))
                    .or_default()
                    .entry(count)
                    .or_default() += 1;
            }
        }
        at = end;
    }
    Ok(sessions
        .into_iter()
        .map(|(date, cells)| {
            let mut block2 = serde_json::Map::new();
            for hour in hour_values() {
                let mut windows = serde_json::Map::new();
                for w in [1_usize, 5, 15, 60, 300] {
                    let hist = cells.get(&(hour, w)).cloned().unwrap_or_default();
                    windows.insert(w.to_string(), json!({"count_hist":hist}));
                }
                block2.insert(hour.to_string(), Value::Object(windows));
            }
            json!({"session":date,"block2":block2})
        })
        .collect())
}

fn write_sequence(path: &Path, rows: &[OrderedCount]) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("sequence has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let staged = parent.join(format!(
        ".{}.staged",
        path.file_name()
            .and_then(|x| x.to_str())
            .unwrap_or("ordered-counts.jsonl")
    ));
    let mut out = BufWriter::new(File::create(&staged).context("creating staged sequence")?);
    for row in rows {
        serde_json::to_writer(&mut out, row)?;
        out.write_all(b"\n")?;
    }
    out.flush()?;
    drop(out);
    std::fs::rename(staged, path)?;
    Ok(())
}

fn backcheck(rows: &[OrderedCount], reference: &Value) -> anyhow::Result<Option<Value>> {
    let mut got: BTreeMap<(u64, u64), (u64, u64, BTreeMap<u32, u64>)> = BTreeMap::new();
    let mut at = 0;
    while at < rows.len() {
        let end = rows[at..]
            .iter()
            .position(|r| {
                r.session_date != rows[at].session_date || r.segment_index != rows[at].segment_index
            })
            .map_or(rows.len(), |x| at + x);
        let seg = &rows[at..end];
        for w in [1_usize, 5, 60] {
            for chunk in seg.chunks_exact(w) {
                let first = &chunk[0];
                let last = &chunk[w - 1];
                if first.window_start_ns / 3_600_000_000_000
                    != last.window_end_ns / 3_600_000_000_000
                {
                    continue;
                }
                let count = chunk.iter().map(|r| r.parent_count).sum::<u32>();
                let cell = got.entry((last.endpoint_hour, w as u64)).or_default();
                cell.0 += 1;
                cell.1 += u64::from(count == 0);
                *cell.2.entry(count).or_default() += 1;
            }
        }
        at = end;
    }
    for hour in hour_values() {
        for w in [1_u64, 5, 60] {
            let (scheduled, zero, hist) = got.get(&(hour, w)).cloned().unwrap_or_default();
            let expected =
                &reference["observed"]["monthly"]["block2"][hour.to_string()][w.to_string()];
            for (field, actual) in [
                ("scheduled_windows", json!(scheduled)),
                ("zero_windows", json!(zero)),
                ("count_hist", json!(hist)),
            ] {
                if expected[field] != actual {
                    return Ok(Some(
                        json!({"hour":hour,"horizon_s":w,"field":field,"expected":expected[field],"actual":actual}),
                    ));
                }
            }
        }
    }
    Ok(None)
}

fn prepare(rows: &[OrderedCount]) -> anyhow::Result<Vec<SessionSuff>> {
    let mut by_session: BTreeMap<String, Vec<&OrderedCount>> = BTreeMap::new();
    for row in rows {
        by_session
            .entry(row.session_date.clone())
            .or_default()
            .push(row);
    }
    let mut out = Vec::new();
    for (date, session_rows) in by_session {
        let mut hours = Vec::new();
        let mut rates = Vec::new();
        for hour in hour_values() {
            let xs = session_rows
                .iter()
                .copied()
                .filter(|r| r.endpoint_hour == hour)
                .collect::<Vec<_>>();
            if xs.is_empty() {
                bail!("session {date} hour {hour} is empty");
            }
            let mean = xs.iter().map(|r| r.parent_count as f64).sum::<f64>() / xs.len() as f64;
            rates.push(mean);
            let mut lags = Vec::new();
            for lag in LAGS {
                let mut s = LagSuff::default();
                for pair in xs.windows(lag + 1) {
                    let a = pair[0];
                    let b = pair[lag];
                    if a.segment_index == b.segment_index
                        && b.window_start_ns - a.window_start_ns == lag as u64 * 1_000_000_000
                    {
                        let ra = a.parent_count as f64 - mean;
                        let rb = b.parent_count as f64 - mean;
                        s.n += 1;
                        s.prod += ra * rb;
                        s.diff2 += (rb - ra).powi(2);
                    }
                }
                lags.push(s);
            }
            let sq = xs
                .iter()
                .map(|r| (r.parent_count as f64 - mean).powi(2))
                .sum();
            let mut blocks = Vec::new();
            for b in BLOCKS {
                let mut n = 0;
                let mut sum = 0.0;
                let mut pos = 0;
                while pos < session_rows.len() {
                    let stop = session_rows[pos..]
                        .iter()
                        .position(|r| r.segment_index != session_rows[pos].segment_index)
                        .map_or(session_rows.len(), |x| pos + x);
                    for chunk in session_rows[pos..stop].chunks_exact(b) {
                        if chunk[0].window_start_ns / 3_600_000_000_000
                            == chunk[b - 1].window_end_ns / 3_600_000_000_000
                            && chunk[b - 1].endpoint_hour == hour
                        {
                            let m =
                                chunk.iter().map(|r| r.parent_count as f64).sum::<f64>() / b as f64;
                            n += 1;
                            sum += (m - mean).powi(2);
                        }
                    }
                    pos = stop;
                }
                blocks.push((n, sum));
            }
            let mut runs = Vec::new();
            let mut run = 0;
            let mut prev: Option<&OrderedCount> = None;
            for x in &xs {
                if prev.is_some_and(|p| {
                    p.segment_index == x.segment_index && p.window_end_ns == x.window_start_ns
                }) {
                    run += 1;
                } else {
                    if run > 0 {
                        runs.push(run);
                    }
                    run = 1;
                }
                prev = Some(x);
            }
            if run > 0 {
                runs.push(run);
            }
            hours.push(HourSuff {
                n: xs.len(),
                sq,
                lags,
                blocks,
                runs,
            });
        }
        out.push(SessionSuff { date, hours, rates });
    }
    if out.len() != 22 {
        bail!(
            "ordered sequence contains {} sessions, expected 22",
            out.len()
        );
    }
    Ok(out)
}

fn panel_a(
    sessions: &[SessionSuff],
    mult: &[i64],
    boot: Option<&[Vec<i64>]>,
) -> anyhow::Result<Value> {
    let point = panel_a_point(sessions, mult)?;
    let uncertainties = if let Some(ms) = boot {
        let reps = ms
            .iter()
            .map(|m| panel_a_point(sessions, m))
            .collect::<anyhow::Result<Vec<_>>>()?;
        panel_a_uncertainty(&point, &reps)
    } else {
        Value::Null
    };
    Ok(json!({
        "outcome":panel_a_outcome(&point),
        "point":point,
        "uncertainty":uncertainties
    }))
}

fn panel_a_outcome(point: &Value) -> &'static str {
    if has_uncovered_block_failure(point) {
        "insufficient_support"
    } else {
        "completed"
    }
}

fn has_uncovered_block_failure(point: &Value) -> bool {
    hour_values().iter().any(|hour| {
        let hour = &point[hour.to_string()];
        hour["block_mean_variance"]
            .as_array()
            .is_some_and(|values| {
                values.iter().enumerate().any(|(i, value)| {
                    value.is_null()
                        && hour["block_reasons"][i].as_str() != Some("structurally_inapplicable")
                })
            })
    })
}

fn block_statistic(count: u64, sum: f64) -> (Option<f64>, Option<&'static str>) {
    if count == 0 {
        (None, Some("structurally_inapplicable"))
    } else {
        (Some(sum / count as f64), None)
    }
}

fn panel_a_point(sessions: &[SessionSuff], mult: &[i64]) -> anyhow::Result<Value> {
    let mut out = serde_json::Map::new();
    for hi in 0..HOURS {
        let mut n = 0_u64;
        let mut sq = 0.0;
        for (s, &m) in sessions.iter().zip(mult) {
            n += s.hours[hi].n as u64 * m as u64;
            sq += s.hours[hi].sq * m as f64;
        }
        let gamma0 = sq / n as f64;
        let mut gamma = Vec::new();
        let mut variogram = Vec::new();
        let mut pair_counts = Vec::new();
        for li in 0..LAGS.len() {
            let mut pn = 0;
            let mut prod = 0.0;
            let mut diff = 0.0;
            for (s, &m) in sessions.iter().zip(mult) {
                pn += s.hours[hi].lags[li].n * m as u64;
                prod += s.hours[hi].lags[li].prod * m as f64;
                diff += s.hours[hi].lags[li].diff2 * m as f64;
            }
            pair_counts.push(pn);
            gamma.push((pn >= 30).then_some(prod / pn as f64));
            variogram.push((pn >= 30).then_some(diff / pn as f64));
        }
        let mut block_values = Vec::new();
        let mut block_counts = Vec::new();
        let mut block_reasons = Vec::new();
        for bi in 0..BLOCKS.len() {
            let mut bn = 0;
            let mut sum = 0.0;
            for (s, &m) in sessions.iter().zip(mult) {
                bn += s.hours[hi].blocks[bi].0 * m as u64;
                sum += s.hours[hi].blocks[bi].1 * m as f64;
            }
            block_counts.push(bn);
            let (value, reason) = block_statistic(bn, sum);
            block_values.push(value);
            block_reasons.push(reason);
        }
        let measured = gamma.iter().copied().collect::<Option<Vec<_>>>();
        let (scenarios, tail) = if let Some(g) = measured {
            scenarios(sessions, mult, hi, gamma0, &g)?
        } else {
            (
                json!({"truncated":null,"held":null,"fitted":null,"scenario_envelope":null}),
                Tail {
                    values: Vec::new(),
                    a: None,
                    tau: None,
                    clamped: false,
                    reason: Some("refused_lag"),
                },
            )
        };
        out.insert(hour_values()[hi].to_string(), json!({
            "gamma_0":gamma0,
            "lags_s":LAGS,"gamma":gamma,"variogram":variogram,"pair_counts":pair_counts,
            "blocks_s":BLOCKS,"block_mean_variance":block_values,"block_counts":block_counts,
            "block_reasons":block_reasons,
            "hour_mean_variance":scenarios,
            "fitted_tail":{"a":tail.a,"tau_s":tail.tau,"clamped":tail.clamped,"reason":tail.reason,
                "splice":{"measured_gamma_1800":gamma[10],"fitted_gamma_1801":tail.values.first(),
                    "discontinuity":tail.values.first().map(|x|*x-gamma[10].unwrap())}}
        }));
    }
    Ok(Value::Object(out))
}

fn scenarios(
    sessions: &[SessionSuff],
    mult: &[i64],
    hi: usize,
    gamma0: f64,
    gamma: &[f64],
) -> anyhow::Result<(Value, Tail)> {
    let mut base = vec![0.0; 3600];
    for k in 1..=1800 {
        base[k] = interp(k, gamma);
    }
    let tail = fit_tail(gamma);
    let mut vals = [0.0; 3];
    let mut counts = [0_u64; 3];
    for (s, &m) in sessions.iter().zip(mult) {
        if m == 0 {
            continue;
        }
        let h = &s.hours[hi];
        for scenario in 0..3 {
            if scenario == 2 && tail.reason.is_some() {
                continue;
            }
            let mut numer = 0.0;
            for &l in &h.runs {
                let mut v = l as f64 * gamma0;
                for k in 1..l {
                    let g = if k <= 1800 {
                        base[k]
                    } else {
                        match scenario {
                            0 => 0.0,
                            1 => gamma[10],
                            _ => tail.a.unwrap() * (-(k as f64) / tail.tau.unwrap()).exp(),
                        }
                    };
                    v += 2.0 * (l - k) as f64 * g;
                }
                numer += v;
            }
            vals[scenario] += m as f64 * numer / (h.n * h.n) as f64;
            counts[scenario] += m as u64;
        }
    }
    let result = [
        Some(vals[0] / counts[0] as f64),
        Some(vals[1] / counts[1] as f64),
        (counts[2] > 0).then_some(vals[2] / counts[2] as f64),
    ];
    let finite = result.iter().flatten().copied().collect::<Vec<_>>();
    Ok((
        json!({"truncated":result[0],"held":result[1],"fitted":result[2],"eligible_sessions":[counts[0],counts[1],counts[2]],"scenario_envelope":{"min":finite.iter().copied().fold(f64::INFINITY,f64::min),"max":finite.iter().copied().fold(f64::NEG_INFINITY,f64::max)}}),
        tail,
    ))
}

fn interp(k: usize, g: &[f64]) -> f64 {
    let p = LAGS.partition_point(|&x| x < k);
    if p == 0 {
        return g[0];
    }
    if p == LAGS.len() {
        return g[g.len() - 1];
    }
    let lo = LAGS[p - 1];
    let hi = LAGS[p];
    g[p - 1] + (g[p] - g[p - 1]) * (k - lo) as f64 / (hi - lo) as f64
}

fn fit_tail(g: &[f64]) -> Tail {
    let idx = [5, 6, 7, 8, 9, 10];
    let pts = idx
        .into_iter()
        .filter(|&i| g[i] > 0.0)
        .map(|i| (LAGS[i] as f64, g[i].ln()))
        .collect::<Vec<_>>();
    if pts.len() < 3 {
        return Tail {
            values: Vec::new(),
            a: None,
            tau: None,
            clamped: false,
            reason: Some("fewer_than_three_positive_tail_lags"),
        };
    }
    let mx = pts.iter().map(|p| p.0).sum::<f64>() / pts.len() as f64;
    let my = pts.iter().map(|p| p.1).sum::<f64>() / pts.len() as f64;
    let slope = pts.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum::<f64>()
        / pts.iter().map(|p| (p.0 - mx).powi(2)).sum::<f64>();
    if slope >= 0.0 {
        return Tail {
            values: Vec::new(),
            a: None,
            tau: None,
            clamped: false,
            reason: Some("non_decaying_tail"),
        };
    }
    let raw = -1.0 / slope;
    let tau = raw.clamp(1.0, 86400.0);
    let clamped = tau != raw;
    let loga = if clamped {
        pts.iter().map(|p| p.1 + p.0 / tau).sum::<f64>() / pts.len() as f64
    } else {
        my - slope * mx
    };
    let a = loga.exp();
    Tail {
        values: (1801..3600)
            .map(|k| a * (-(k as f64) / tau).exp())
            .collect(),
        a: Some(a),
        tau: Some(tau),
        clamped,
        reason: None,
    }
}

fn panel_a_uncertainty(point: &Value, reps: &[Value]) -> Value {
    let mut out = serde_json::Map::new();
    for hour in hour_values() {
        let key = hour.to_string();
        let mut h = serde_json::Map::new();
        for field in ["gamma_0"] {
            h.insert(
                field.into(),
                uncertainty(
                    point[&key][field].as_f64(),
                    reps.iter().map(|r| r[&key][field].as_f64()),
                ),
            );
        }
        for field in ["gamma", "variogram", "block_mean_variance"] {
            let len = point[&key][field].as_array().map_or(0, Vec::len);
            h.insert(
                field.into(),
                Value::Array(
                    (0..len)
                        .map(|i| {
                            let key = key.clone();
                            let propagated_reason = (field == "block_mean_variance")
                                .then(|| point[&key]["block_reasons"][i].as_str())
                                .flatten();
                            uncertainty_with_reason(
                                point[&key][field][i].as_f64(),
                                reps.iter().map(move |r| r[&key][field][i].as_f64()),
                                propagated_reason,
                            )
                        })
                        .collect(),
                ),
            );
        }
        let mut scenarios_out = serde_json::Map::new();
        for field in ["truncated", "held", "fitted"] {
            let key_for_reps = key.clone();
            scenarios_out.insert(
                field.into(),
                uncertainty(
                    point[&key]["hour_mean_variance"][field].as_f64(),
                    reps.iter()
                        .map(|r| r[&key_for_reps]["hour_mean_variance"][field].as_f64()),
                ),
            );
        }
        for field in ["min", "max"] {
            let key_for_reps = key.clone();
            scenarios_out.insert(
                format!("scenario_envelope_{field}"),
                uncertainty(
                    point[&key]["hour_mean_variance"]["scenario_envelope"][field].as_f64(),
                    reps.iter().map(|r| {
                        r[&key_for_reps]["hour_mean_variance"]["scenario_envelope"][field].as_f64()
                    }),
                ),
            );
        }
        h.insert("hour_mean_variance".into(), Value::Object(scenarios_out));
        let mut tail_out = serde_json::Map::new();
        for field in ["a", "tau_s"] {
            let key_for_reps = key.clone();
            tail_out.insert(
                field.into(),
                uncertainty(
                    point[&key]["fitted_tail"][field].as_f64(),
                    reps.iter()
                        .map(|r| r[&key_for_reps]["fitted_tail"][field].as_f64()),
                ),
            );
        }
        for field in ["fitted_gamma_1801", "discontinuity"] {
            let key_for_reps = key.clone();
            tail_out.insert(
                field.into(),
                uncertainty(
                    point[&key]["fitted_tail"]["splice"][field].as_f64(),
                    reps.iter()
                        .map(|r| r[&key_for_reps]["fitted_tail"]["splice"][field].as_f64()),
                ),
            );
        }
        h.insert("fitted_tail".into(), Value::Object(tail_out));
        out.insert(key, h.into());
    }
    out.into()
}

fn panel_b(
    sessions: &[SessionSuff],
    mult: &[i64],
    boot: Option<&[Vec<i64>]>,
    month: u64,
    permutation_seed: u64,
) -> anyhow::Result<Value> {
    let point = panel_b_point(sessions, mult)?;
    let uncertainty = if let Some(ms) = boot {
        let reps = ms
            .iter()
            .map(|m| panel_b_point(sessions, m).ok())
            .collect::<Vec<_>>();
        panel_b_uncertainty(&point, &reps)
    } else {
        Value::Null
    };
    let permutation = permutation_nulls(sessions, &point, month, permutation_seed)?;
    let loo = leave_one_out(sessions, &point)?;
    Ok(
        json!({"outcome":"completed","point":point,"uncertainty":uncertainty,"permutation":permutation,"leave_one_session_out":loo}),
    )
}

fn residuals(sessions: &[SessionSuff], mult: &[i64]) -> Vec<Vec<f64>> {
    let mut gm = vec![0.0; HOURS];
    let n = mult.iter().sum::<i64>() as f64;
    for h in 0..HOURS {
        gm[h] = (sessions
            .iter()
            .zip(mult)
            .map(|(s, m)| s.rates[h].ln() * *m as f64)
            .sum::<f64>()
            / n)
            .exp();
    }
    sessions
        .iter()
        .map(|s| (0..HOURS).map(|h| (s.rates[h] / gm[h]).ln()).collect())
        .collect()
}

fn panel_b_point(sessions: &[SessionSuff], mult: &[i64]) -> anyhow::Result<Value> {
    let r = residuals(sessions, mult);
    let corr = corr_matrix(&r, mult)?;
    let (eigenvalue, loading) = leading(&corr);
    let factor_share = eigenvalue / HOURS as f64;
    let br = r
        .iter()
        .map(|x| {
            vec![
                mean(&x[0..7]),
                mean(&x[7..13]),
                mean(&x[13..20]),
                x[20],
                mean(&x[21..23]),
            ]
        })
        .collect::<Vec<_>>();
    let bc = corr_matrix(&br, mult)?;
    let mut vars = Vec::new();
    for b in 0..5 {
        vars.push(weighted_var(
            &br.iter().map(|x| x[b]).collect::<Vec<_>>(),
            mult,
        )?);
    }
    let vs = vars.iter().sum::<f64>();
    let consecutive = consecutive(sessions, &r)?;
    Ok(
        json!({"hour_correlation":corr,"leading_eigenvalue":eigenvalue,"leading_variance_share":factor_share,"leading_loading":loading,"block_names":["asia","london","cash","hour_20","post_close"],"block_correlation":bc,"block_variance_share":vars.iter().map(|x|x/vs).collect::<Vec<_>>(),"consecutive_session_covariance":consecutive}),
    )
}

fn corr_matrix(rows: &[Vec<f64>], mult: &[i64]) -> anyhow::Result<Vec<Vec<f64>>> {
    let d = rows[0].len();
    let mut out = vec![vec![0.0; d]; d];
    for i in 0..d {
        for j in i..d {
            let x = rows.iter().map(|r| r[i]).collect::<Vec<_>>();
            let y = rows.iter().map(|r| r[j]).collect::<Vec<_>>();
            let c = weighted_corr(&x, &y, mult)?;
            out[i][j] = c;
            out[j][i] = c;
        }
    }
    Ok(out)
}
fn weighted_corr(x: &[f64], y: &[f64], m: &[i64]) -> anyhow::Result<f64> {
    let n = m.iter().sum::<i64>() as f64;
    let mx = x.iter().zip(m).map(|(x, m)| x * *m as f64).sum::<f64>() / n;
    let my = y.iter().zip(m).map(|(x, m)| x * *m as f64).sum::<f64>() / n;
    let vx = x
        .iter()
        .zip(m)
        .map(|(x, m)| *m as f64 * (x - mx).powi(2))
        .sum::<f64>();
    let vy = y
        .iter()
        .zip(m)
        .map(|(y, m)| *m as f64 * (y - my).powi(2))
        .sum::<f64>();
    if vx == 0.0 || vy == 0.0 {
        bail!("null correlation")
    };
    Ok(x.iter()
        .zip(y)
        .zip(m)
        .map(|((x, y), m)| *m as f64 * (x - mx) * (y - my))
        .sum::<f64>()
        / (vx * vy).sqrt())
}
fn weighted_var(x: &[f64], m: &[i64]) -> anyhow::Result<f64> {
    let n = m.iter().sum::<i64>() as f64;
    let a = x.iter().zip(m).map(|(x, m)| x * *m as f64).sum::<f64>() / n;
    Ok(x.iter()
        .zip(m)
        .map(|(x, m)| *m as f64 * (x - a).powi(2))
        .sum::<f64>()
        / n)
}

fn leading(a: &[Vec<f64>]) -> (f64, Vec<f64>) {
    let n = a.len();
    let mut v = vec![1.0 / (n as f64).sqrt(); n];
    for _ in 0..10000 {
        let mut z = vec![0.0; n];
        for i in 0..n {
            z[i] = a[i].iter().zip(&v).map(|(x, y)| x * y).sum();
        }
        let norm = z.iter().map(|x| x * x).sum::<f64>().sqrt();
        for x in &mut z {
            *x /= norm
        }
        let delta = z
            .iter()
            .zip(&v)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0, f64::max);
        v = z;
        if delta < 1e-14 {
            break;
        }
    }
    if v.iter().filter(|x| **x > 0.0).count() < v.iter().filter(|x| **x < 0.0).count() {
        for x in &mut v {
            *x = -*x
        }
    }
    let av = (0..n)
        .map(|i| a[i].iter().zip(&v).map(|(x, y)| x * y).sum::<f64>())
        .collect::<Vec<_>>();
    (v.iter().zip(av).map(|(x, y)| x * y).sum(), v)
}

fn consecutive(sessions: &[SessionSuff], r: &[Vec<f64>]) -> anyhow::Result<Value> {
    let mut bins = serde_json::Map::new();
    for (gname, glo, ghi) in [
        ("1", 1, 1),
        ("2", 2, 2),
        ("3", 3, 3),
        ("4_or_more", 4, i64::MAX),
    ] {
        let mut hs = Vec::new();
        for h in 0..HOURS {
            let mut vals = Vec::new();
            for i in 0..sessions.len() - 1 {
                let gap = date_days(&sessions[i].date, &sessions[i + 1].date)?;
                if gap >= glo && gap <= ghi {
                    vals.push(r[i][h] * r[i + 1][h]);
                }
            }
            hs.push(if vals.len() >= 8 {
                json!({"value":mean(&vals),"pair_count":vals.len()})
            } else {
                json!({"value":null,"pair_count":vals.len(),"reason":"minimum_support_8"})
            });
        }
        let avail = hs
            .iter()
            .filter_map(|x| x["value"].as_f64())
            .collect::<Vec<_>>();
        bins.insert(gname.into(),json!({"hours":hs,"pooled":if avail.is_empty(){json!({"value":null,"contributing_hours":0})}else{json!({"value":mean(&avail),"contributing_hours":avail.len()})}}));
    }
    Ok(bins.into())
}
fn date_days(a: &str, b: &str) -> anyhow::Result<i64> {
    fn civil(s: &str) -> anyhow::Result<i64> {
        let p = s
            .split('-')
            .map(str::parse::<i64>)
            .collect::<Result<Vec<_>, _>>()?;
        let (y, m, d) = (p[0], p[1], p[2]);
        let y = y - i64::from(m <= 2);
        let era = if y >= 0 { y } else { y - 399 } / 400;
        let yoe = y - era * 400;
        let mp = m + if m > 2 { -3 } else { 9 };
        let doy = (153 * mp + 2) / 5 + d - 1;
        Ok(era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy)
    }
    Ok(civil(b)? - civil(a)?)
}

fn permutation_nulls(
    sessions: &[SessionSuff],
    point: &Value,
    month: u64,
    permutation_seed: u64,
) -> anyhow::Result<Value> {
    let base = residuals(sessions, &vec![1; sessions.len()]);
    let observed = point["leading_eigenvalue"].as_f64().unwrap();
    let observed_c = &point["consecutive_session_covariance"];
    let mut eig_ge = 0;
    let mut c_ge: BTreeMap<(String, usize), u64> = BTreeMap::new();
    let mut pooled_ge: BTreeMap<String, u64> = BTreeMap::new();
    for rep in 0..REPS {
        let mut r = base.clone();
        for h in 0..HOURS {
            let mut vals = r.iter().map(|x| x[h]).collect::<Vec<_>>();
            let mut state = if month == 202_607 {
                tuple_mix(permutation_seed, &[hour_values()[h], rep as u64])
            } else {
                tuple_mix(
                    mogwai_lab::aggregate::bootstrap::STAGE_M_SEED,
                    &[month, hour_values()[h], rep as u64],
                )
            };
            for i in (1..vals.len()).rev() {
                state = splitmix64(state);
                let j = state % (i as u64 + 1);
                vals.swap(i, j as usize);
            }
            for (i, x) in vals.into_iter().enumerate() {
                r[i][h] = x;
            }
        }
        let corr = corr_matrix(&r, &vec![1; sessions.len()])?;
        if leading(&corr).0 >= observed {
            eig_ge += 1
        }
        let pc = consecutive(sessions, &r)?;
        for (g, gv) in pc.as_object().unwrap() {
            for h in 0..HOURS {
                if let (Some(o), Some(n)) = (
                    observed_c[g]["hours"][h]["value"].as_f64(),
                    gv["hours"][h]["value"].as_f64(),
                ) {
                    if n >= o {
                        *c_ge.entry((g.clone(), h)).or_default() += 1
                    }
                }
            }
            if let (Some(o), Some(n)) = (
                observed_c[g]["pooled"]["value"].as_f64(),
                gv["pooled"]["value"].as_f64(),
            ) {
                if n >= o {
                    *pooled_ge.entry(g.clone()).or_default() += 1
                }
            }
        }
    }
    let mut cb = serde_json::Map::new();
    for (g, gv) in observed_c.as_object().unwrap() {
        let hs = (0..HOURS)
            .map(|h| {
                gv["hours"][h]["value"].as_f64().map(|_| {
                    (1 + c_ge.get(&(g.clone(), h)).copied().unwrap_or(0)) as f64 / (REPS + 1) as f64
                })
            })
            .collect::<Vec<_>>();
        let pooled = gv["pooled"]["value"]
            .as_f64()
            .map(|_| (1 + pooled_ge.get(g).copied().unwrap_or(0)) as f64 / (REPS + 1) as f64);
        cb.insert(
            g.clone(),
            json!({"hour_p_values":hs,"pooled_p_value":pooled}),
        );
    }
    Ok(
        json!({"leading_eigenvalue_p_value":(1+eig_ge)as f64/(REPS+1)as f64,"consecutive_session_covariance":cb}),
    )
}

fn leave_one_out(sessions: &[SessionSuff], point: &Value) -> anyhow::Result<Value> {
    let full = point["leading_loading"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_f64().unwrap())
        .collect::<Vec<_>>();
    let mut rows = Vec::new();
    for omit in 0..sessions.len() {
        let kept = sessions
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != omit)
            .map(|(_, s)| s.clone())
            .collect::<Vec<_>>();
        let r = residuals(&kept, &vec![1; kept.len()]);
        let corr = corr_matrix(&r, &vec![1; kept.len()])?;
        let (e, l) = leading(&corr);
        let flips = l
            .iter()
            .zip(&full)
            .filter(|(a, b)| a.signum() != b.signum())
            .count();
        rows.push(json!({"omitted_session":sessions[omit].date,"leading_variance_share":e/HOURS as f64,"sign_flips":flips}));
    }
    Ok(Value::Array(rows))
}

fn panel_b_uncertainty(point: &Value, reps: &[Option<Value>]) -> Value {
    let mut out = serde_json::Map::new();
    for f in ["leading_eigenvalue", "leading_variance_share"] {
        out.insert(
            f.into(),
            uncertainty(
                point[f].as_f64(),
                reps.iter().map(|r| r.as_ref().and_then(|r| r[f].as_f64())),
            ),
        );
    }
    for (f, n) in [("leading_loading", HOURS), ("block_variance_share", 5)] {
        out.insert(
            f.into(),
            Value::Array(
                (0..n)
                    .map(|i| {
                        uncertainty(
                            point[f][i].as_f64(),
                            reps.iter()
                                .map(move |r| r.as_ref().and_then(|r| r[f][i].as_f64())),
                        )
                    })
                    .collect(),
            ),
        );
    }
    for (f, n) in [("hour_correlation", HOURS), ("block_correlation", 5)] {
        out.insert(
            f.into(),
            Value::Array(
                (0..n)
                    .map(|i| {
                        Value::Array(
                            (0..n)
                                .map(|j| {
                                    uncertainty(
                                        point[f][i][j].as_f64(),
                                        reps.iter().map(move |r| {
                                            r.as_ref().and_then(|r| r[f][i][j].as_f64())
                                        }),
                                    )
                                })
                                .collect(),
                        )
                    })
                    .collect(),
            ),
        );
    }
    out.into()
}

fn uncertainty(point: Option<f64>, values: impl Iterator<Item = Option<f64>>) -> Value {
    uncertainty_with_reason(point, values, None)
}

fn uncertainty_with_reason(
    point: Option<f64>,
    values: impl Iterator<Item = Option<f64>>,
    propagated_reason: Option<&str>,
) -> Value {
    let xs = values.collect::<Vec<_>>();
    let finite = xs.iter().flatten().count();
    if point.is_none() {
        return json!({"point":null,"uncertainty":null,"finite_replicates":finite,
            "reason":propagated_reason.unwrap_or("point_estimate_refused")});
    }
    if finite != REPS {
        return json!({"point":point,"uncertainty":null,"finite_replicates":finite,"reason":"bootstrap_replicate_failure"});
    }
    let mut ys = xs.into_iter().flatten().collect::<Vec<_>>();
    let a = mean(&ys);
    let se = (ys.iter().map(|x| (x - a).powi(2)).sum::<f64>() / (REPS - 1) as f64).sqrt();
    ys.sort_by(f64::total_cmp);
    json!({"point":point,"standard_error":se,"p2_5":ys[49],"p97_5":ys[1949],"finite_replicates":finite})
}
fn mean(x: &[f64]) -> f64 {
    x.iter().sum::<f64>() / x.len() as f64
}
fn hour_values() -> Vec<u64> {
    (0..24).filter(|h| *h != 21).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolation_is_linear_only_between_positive_lags() {
        let g = LAGS.iter().map(|x| *x as f64).collect::<Vec<_>>();
        assert_eq!(interp(1, &g), 1.0);
        assert_eq!(interp(4, &g), 4.0);
        assert_eq!(interp(1800, &g), 1800.0);
    }

    #[test]
    fn fitted_tail_recovers_an_exponential() {
        let g = LAGS
            .iter()
            .map(|k| 7.0 * (-(*k as f64) / 400.0).exp())
            .collect::<Vec<_>>();
        let fit = fit_tail(&g);
        assert!((fit.a.unwrap() - 7.0).abs() < 1e-10);
        assert!((fit.tau.unwrap() - 400.0).abs() < 1e-10);
        assert!(!fit.clamped);
    }

    #[test]
    fn calendar_gap_uses_elapsed_days() {
        assert_eq!(date_days("2026-07-31", "2026-08-03").unwrap(), 3);
    }

    #[test]
    fn leading_eigenpair_has_majority_positive_sign() {
        let (e, loading) = leading(&[vec![1.0, 0.5], vec![0.5, 1.0]]);
        assert!((e - 1.5).abs() < 1e-12);
        assert!(loading.iter().all(|x| *x > 0.0));
    }

    #[test]
    fn zero_block_support_is_structural_and_not_insufficient() {
        let (value, reason) = block_statistic(0, 0.0);
        assert_eq!(value, None);
        assert_eq!(reason, Some("structurally_inapplicable"));

        let mut point = serde_json::Map::new();
        for hour in hour_values() {
            point.insert(
                hour.to_string(),
                json!({
                    "block_mean_variance": [value],
                    "block_reasons": [reason],
                }),
            );
        }
        assert_eq!(panel_a_outcome(&Value::Object(point)), "completed");
    }

    #[test]
    fn uncovered_null_block_support_remains_insufficient() {
        let mut point = serde_json::Map::new();
        for hour in hour_values() {
            point.insert(
                hour.to_string(),
                json!({
                    "block_mean_variance": [null],
                    "block_reasons": [null],
                }),
            );
        }
        assert_eq!(
            panel_a_outcome(&Value::Object(point)),
            "insufficient_support"
        );
    }
}
