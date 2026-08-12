// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Artifact-only implementation of the signed slow-geometry preregistration.

#![expect(
    clippy::needless_pass_by_value,
    clippy::needless_range_loop,
    clippy::semicolon_if_nothing_returned,
    reason = "the artifact constructor consumes JSON conceptually and the eigensolver mirrors indexed matrix formulas"
)]

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, anyhow, bail};
use mogwai_lab::aggregate::artifact::write_json_atomic;
use mogwai_lab::kernel::{splitmix64, tuple_mix};
use mogwai_lab::measure12a::OrderedCount;
use serde::Serialize;
use serde_json::{Value, json};

const INPUT: &str = "analysis/out/ordered-counts.jsonl";
const OUTPUT: &str = "analysis/out/slow-geometry.json";
const INPUT_SHA256: &str = "33aaf2c11d70a68c2ef91da88b69ad01dc9dd8ec1f3cbb0f9324593707e68a70";
const PERM_SEED: u64 = 5_177_340_928_461_523_719;
const REPS: usize = 2_000;
const MIN_PAIRS: usize = 8;
const HOURS: [u64; 23] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 22, 23,
];
const EDGES: [f64; 10] = [
    1.0,
    2.0,
    3.0,
    6.0,
    12.0,
    24.0,
    48.0,
    72.0,
    96.0,
    f64::INFINITY,
];

#[derive(Clone, Serialize)]
struct Cell {
    session_date: String,
    hour: u64,
    parents: u64,
    exposure_s: u64,
    timestamp_ns: u64,
    log_rate: f64,
    residual: f64,
}

#[derive(Serialize)]
struct Exclusion {
    session_date: String,
    hour: u64,
    reason: &'static str,
}

#[derive(Clone, Serialize)]
struct Score {
    session_date: String,
    factor_normalization: &'static str,
    sign_alignment: &'static str,
    loading_sum_before_alignment: f64,
    score: f64,
    loading: Vec<Loading>,
    #[serde(skip)]
    z_star: Vec<f64>,
}

#[derive(Clone, Serialize)]
struct Loading {
    hour: u64,
    value: f64,
}

#[derive(Serialize)]
struct Refusal {
    session_date: String,
    reason: String,
}

#[derive(Clone, Copy)]
enum Field {
    Residual,
    Standardized,
}

pub fn run() -> anyhow::Result<()> {
    run_with(&SlowGeometryRun::july())
}

pub struct SlowGeometryRun {
    pub month: u64,
    pub input: PathBuf,
    pub output: PathBuf,
    pub expected_sha256: String,
}

impl SlowGeometryRun {
    fn july() -> Self {
        Self {
            month: 202_607,
            input: INPUT.into(),
            output: OUTPUT.into(),
            expected_sha256: INPUT_SHA256.into(),
        }
    }
}

pub fn run_with(config: &SlowGeometryRun) -> anyhow::Result<()> {
    let actual =
        mogwai_lab::ledger::sha256_file(&config.input).map_err(|e| anyhow!(e.to_string()))?;
    let commit = executing_commit()?;
    if actual != config.expected_sha256 {
        let artifact = base(
            "input_mismatch",
            &actual,
            &commit,
            json!({"reason":"sequence_sha256_mismatch", "expected_sha256":config.expected_sha256}),
        );
        write(&config.output, &artifact)?;
        println!("slow-geometry outcome: input_mismatch");
        return Ok(());
    }

    let rows = read_sequence(&config.input)?;
    let (cells, exclusions, dates) = construct_cells(&rows)?;
    let (scores, refusals) = cross_fit(&cells, &exclusions, &dates);
    let score_stat = score_statistic(&scores, &dates, config.month);
    let elapsed = elapsed_statistics(&cells, None, Field::Residual);
    let residualized = elapsed_statistics(&cells, Some(&scores), Field::Standardized);
    let boundary_contrasts = contrasts(&elapsed);
    let residualized_contrasts = contrasts(&residualized);
    let unsupported = score_stat["bins"].as_array().is_none();
    let outcome = if unsupported {
        "insufficient_support"
    } else {
        "completed"
    };
    let detail = json!({
        "residual_matrix": {"construction":"log(parents/exposure_s), centered on per-hour mean of logs", "cells":cells, "excluded_cells":exclusions},
        "statistic_1":{"cells_used":cells.iter().map(|c|json!({"session_date":c.session_date,"hour":c.hour})).collect::<Vec<_>>(),"result":score_stat},
        "cross_fitted_factor":{"normalization":"unit_euclidean_length", "sign_rule":"positive_loading_sum; exact tie uses lowest-numbered-hour positive", "scores":scores, "refusals":refusals},
        "statistic_2":{"units":"log_rate_squared", "cells_used":cells.iter().map(|c|json!({"session_date":c.session_date,"hour":c.hour})).collect::<Vec<_>>(), "covariance":elapsed, "boundary_contrasts":boundary_contrasts},
        "statistic_3":{"units":"standardized_squared", "recomputed_statistic":"elapsed_separation_covariance_only", "cells_used":cells.iter().filter(|c|scores.iter().any(|s|s.session_date==c.session_date)).map(|c|json!({"session_date":c.session_date,"hour":c.hour})).collect::<Vec<_>>(), "covariance":residualized, "boundary_contrasts":residualized_contrasts},
        "uncertainty":{"score_autocovariance":"shared-permutation max statistic", "elapsed_covariance":"descriptive point estimates plus pair counts", "boundary_contrasts":"descriptive point estimates plus pair counts", "factor":"point estimates only", "bootstrap":false}
    });
    let mut artifact = base(outcome, &actual, &commit, detail);
    artifact["binding"]["month"] = json!(config.month);
    artifact["binding"]["sequence_path"] = json!(config.input);
    artifact["binding"]["expected_sequence_sha256"] = json!(config.expected_sha256);
    write(&config.output, &artifact)?;
    println!("slow-geometry artifact -> {}", config.output.display());
    println!("slow-geometry outcome: {outcome}");
    Ok(())
}

fn base(outcome: &str, hash: &str, commit: &str, detail: Value) -> Value {
    json!({
        "outcome":outcome,
        "binding":{"sequence_path":INPUT,"sequence_sha256":hash,"expected_sequence_sha256":INPUT_SHA256,
            "preregistration":{"path":"notes/slow-geometry-preregistration.md","frozen":"2026-08-11","signed_by":"codex session 019fefe4-b680-7e70-8a8e-9df36e0beecf","landed_commit":"69aa132"},
            "implementing_commit":commit,"permutation_seed":PERM_SEED,"permutation_replicates":REPS},
        "classes_advanced":["A-only","B-only","A+B mixed"],
        "detail":detail
    })
}

fn read_sequence(path: &Path) -> anyhow::Result<Vec<OrderedCount>> {
    BufReader::new(File::open(path).context("opening bound ordered sequence")?)
        .lines()
        .map(|line| Ok(serde_json::from_str(&line?)?))
        .collect()
}

fn construct_cells(
    rows: &[OrderedCount],
) -> anyhow::Result<(Vec<Cell>, Vec<Exclusion>, Vec<String>)> {
    let mut dates = rows
        .iter()
        .map(|r| r.session_date.clone())
        .collect::<Vec<_>>();
    dates.sort();
    dates.dedup();
    let mut grouped: BTreeMap<(String, u64), Vec<&OrderedCount>> = BTreeMap::new();
    for row in rows {
        grouped
            .entry((row.session_date.clone(), row.endpoint_hour))
            .or_default()
            .push(row);
    }
    let mut raw = Vec::new();
    let mut excluded = Vec::new();
    for date in &dates {
        for &hour in &HOURS {
            let Some(xs) = grouped.get(&(date.clone(), hour)) else {
                excluded.push(Exclusion {
                    session_date: date.clone(),
                    hour,
                    reason: "missing_cell",
                });
                continue;
            };
            let parents = xs.iter().map(|x| u64::from(x.parent_count)).sum::<u64>();
            let exposure_ns = xs
                .iter()
                .map(|x| x.window_end_ns - x.window_start_ns)
                .sum::<u64>();
            if exposure_ns == 0 {
                excluded.push(Exclusion {
                    session_date: date.clone(),
                    hour,
                    reason: "zero_exposure",
                });
                continue;
            }
            if parents == 0 {
                excluded.push(Exclusion {
                    session_date: date.clone(),
                    hour,
                    reason: "zero_parents",
                });
                continue;
            }
            let weighted_mid = xs
                .iter()
                .map(|x| {
                    u128::from(x.window_start_ns + x.window_end_ns)
                        * u128::from(x.window_end_ns - x.window_start_ns)
                })
                .sum::<u128>()
                / (2 * u128::from(exposure_ns));
            raw.push((
                date.clone(),
                hour,
                parents,
                exposure_ns / 1_000_000_000,
                weighted_mid as u64,
                (parents as f64 / (exposure_ns as f64 / 1e9)).ln(),
            ));
        }
    }
    let mut means = BTreeMap::new();
    for &hour in &HOURS {
        let x = raw
            .iter()
            .filter(|x| x.1 == hour)
            .map(|x| x.5)
            .collect::<Vec<_>>();
        if !x.is_empty() {
            means.insert(hour, mean(&x));
        }
    }
    let cells = raw
        .into_iter()
        .map(|x| Cell {
            session_date: x.0,
            hour: x.1,
            parents: x.2,
            exposure_s: x.3,
            timestamp_ns: x.4,
            log_rate: x.5,
            residual: x.5 - means[&x.1],
        })
        .collect();
    Ok((cells, excluded, dates))
}

fn cross_fit(
    cells: &[Cell],
    exclusions: &[Exclusion],
    dates: &[String],
) -> (Vec<Score>, Vec<Refusal>) {
    let map = cells
        .iter()
        .map(|c| ((c.session_date.clone(), c.hour), c.residual))
        .collect::<BTreeMap<_, _>>();
    let mut scores = Vec::new();
    let mut refusals = Vec::new();
    for held in dates {
        if let Some(x) = exclusions.iter().find(|x| &x.session_date == held) {
            refusals.push(Refusal {
                session_date: held.clone(),
                reason: format!("excluded_cell_hour_{}:{}", x.hour, x.reason),
            });
            continue;
        }
        let train = dates.iter().filter(|d| *d != held).collect::<Vec<_>>();
        let mut mu = Vec::new();
        let mut sigma = Vec::new();
        let mut refused = None;
        for &h in &HOURS {
            let x = train
                .iter()
                .filter_map(|d| map.get(&((**d).clone(), h)))
                .copied()
                .collect::<Vec<_>>();
            let m = mean(&x);
            let sd = (x.iter().map(|v| (v - m).powi(2)).sum::<f64>() / x.len() as f64).sqrt();
            if sd == 0.0 {
                refused = Some(format!("zero_training_variance_hour_{h}"));
                break;
            }
            mu.push(m);
            sigma.push(sd);
        }
        if let Some(reason) = refused {
            refusals.push(Refusal {
                session_date: held.clone(),
                reason,
            });
            continue;
        }
        let ztrain = train
            .iter()
            .map(|d| {
                HOURS
                    .iter()
                    .enumerate()
                    .map(|(i, h)| (map[&((**d).clone(), *h)] - mu[i]) / sigma[i])
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let corr = (0..HOURS.len())
            .map(|i| {
                (0..HOURS.len())
                    .map(|j| ztrain.iter().map(|z| z[i] * z[j]).sum::<f64>() / ztrain.len() as f64)
                    .collect()
            })
            .collect::<Vec<Vec<f64>>>();
        let Some(mut v) = leading_eigenvector(corr) else {
            refusals.push(Refusal {
                session_date: held.clone(),
                reason: "failed_eigensolve".into(),
            });
            continue;
        };
        let before = v.iter().sum::<f64>();
        let alignment = if before < 0.0 || (before == 0.0 && v[0] < 0.0) {
            for x in &mut v {
                *x = -*x
            }
            if before == 0.0 {
                "tie_flipped_for_lowest_numbered_hour"
            } else {
                "flipped_for_positive_loading_sum"
            }
        } else if before == 0.0 {
            "tie_already_lowest_numbered_hour_positive"
        } else {
            "already_positive_loading_sum"
        };
        let z = HOURS
            .iter()
            .enumerate()
            .map(|(i, h)| (map[&(held.clone(), *h)] - mu[i]) / sigma[i])
            .collect::<Vec<_>>();
        let f = z.iter().zip(&v).map(|(a, b)| a * b).sum::<f64>();
        let z_star = z.iter().zip(&v).map(|(a, b)| a - f * b).collect();
        scores.push(Score {
            session_date: held.clone(),
            factor_normalization: "unit_euclidean_length",
            sign_alignment: alignment,
            loading_sum_before_alignment: before,
            score: f,
            loading: HOURS
                .iter()
                .zip(v)
                .map(|(h, value)| Loading { hour: *h, value })
                .collect(),
            z_star,
        });
    }
    (scores, refusals)
}

fn leading_eigenvector(mut a: Vec<Vec<f64>>) -> Option<Vec<f64>> {
    let n = a.len();
    let mut v = vec![vec![0.0; n]; n];
    for i in 0..n {
        v[i][i] = 1.0;
    }
    for _ in 0..(100 * n * n) {
        let mut p = 0;
        let mut q = 1;
        let mut best = 0.0;
        for i in 0..n {
            for j in i + 1..n {
                if a[i][j].abs() > best {
                    best = a[i][j].abs();
                    p = i;
                    q = j;
                }
            }
        }
        if best < 1e-12 {
            break;
        }
        let phi = 0.5 * (2.0 * a[p][q]).atan2(a[q][q] - a[p][p]);
        let (s, c) = phi.sin_cos();
        for k in 0..n {
            let ap = a[k][p];
            let aq = a[k][q];
            a[k][p] = c * ap - s * aq;
            a[k][q] = s * ap + c * aq;
        }
        for k in 0..n {
            let ap = a[p][k];
            let aq = a[q][k];
            a[p][k] = c * ap - s * aq;
            a[q][k] = s * ap + c * aq;
            let vp = v[k][p];
            let vq = v[k][q];
            v[k][p] = c * vp - s * vq;
            v[k][q] = s * vp + c * vq;
        }
    }
    let idx = (0..n).max_by(|i, j| a[*i][*i].total_cmp(&a[*j][*j]))?;
    let mut out = (0..n).map(|i| v[i][idx]).collect::<Vec<_>>();
    let norm = out.iter().map(|x| x * x).sum::<f64>().sqrt();
    if !norm.is_finite() || norm == 0.0 {
        return None;
    }
    for x in &mut out {
        *x /= norm
    }
    Some(out)
}

fn score_statistic(scores: &[Score], dates: &[String], month: u64) -> Value {
    let centered = mean(&scores.iter().map(|s| s.score).collect::<Vec<_>>());
    let observed = score_bins(scores, dates, centered);
    let supported = observed
        .iter()
        .filter_map(|x| x.1)
        .map(f64::abs)
        .collect::<Vec<_>>();
    if supported.is_empty() {
        return json!({"bins":render_score_bins(&observed),"centered_score_mean":centered,"max_abs":null,"permutation":{"replicates":REPS,"null_exceedance_count":null,"p_value":null,"reason":"no_supported_gap_bin"}});
    }
    let obsmax = supported.into_iter().fold(0.0, f64::max);
    let mut exceed = 0;
    let values = scores.iter().map(|s| s.score).collect::<Vec<_>>();
    for rep in 0..REPS {
        let mut perm = values.clone();
        let mut state = if month == 202_607 {
            tuple_mix(PERM_SEED, &[rep as u64])
        } else {
            tuple_mix(
                mogwai_lab::aggregate::bootstrap::STAGE_M_SEED,
                &[month, rep as u64],
            )
        };
        for i in (1..perm.len()).rev() {
            state = splitmix64(state);
            let j = (state % (i as u64 + 1)) as usize;
            perm.swap(i, j)
        }
        let ps = scores
            .iter()
            .zip(&perm)
            .map(|(s, x)| Score {
                score: *x,
                ..s.clone()
            })
            .collect::<Vec<_>>();
        let b = score_bins(&ps, dates, centered);
        let m = b
            .iter()
            .filter_map(|x| x.1)
            .map(f64::abs)
            .fold(0.0, f64::max);
        if m >= obsmax {
            exceed += 1
        }
    }
    json!({"scores_centered_before_autocovariance":true,"centered_score_mean":centered,"bins":render_score_bins(&observed),"max_abs":obsmax,"permutation":{"scope":"S(g)_shared_max_only","replicates":REPS,"null_exceedance_count":exceed,"p_value":(1.0+exceed as f64)/(1.0+REPS as f64)}})
}

fn score_bins(
    scores: &[Score],
    _dates: &[String],
    center: f64,
) -> Vec<(&'static str, Option<f64>, usize)> {
    let mut x = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
    for i in 0..scores.len() {
        for j in i + 1..scores.len() {
            let g = (civil_day(&scores[j].session_date) - civil_day(&scores[i].session_date))
                .unsigned_abs() as usize;
            let b = if g >= 4 { 3 } else { g.saturating_sub(1) };
            x[b].push((scores[i].score - center) * (scores[j].score - center));
        }
    }
    ["1", "2", "3", "4_or_more"]
        .into_iter()
        .zip(x)
        .map(|(name, v)| (name, (v.len() >= MIN_PAIRS).then(|| mean(&v)), v.len()))
        .collect()
}
fn render_score_bins(x: &[(&str, Option<f64>, usize)]) -> Value {
    Value::Array(x.iter().map(|(g,v,n)|json!({"calendar_gap_days":g,"pair_count":n,"S":v,"reason":if v.is_none(){Some("fewer_than_8_pairs")}else{None}})).collect())
}

fn elapsed_statistics(cells: &[Cell], scores: Option<&[Score]>, field: Field) -> Value {
    let mut bins = vec![vec![vec![Vec::<f64>::new(); 2]; 2]; 9];
    let score_map = scores.map(|ss| {
        ss.iter()
            .map(|s| (s.session_date.clone(), s))
            .collect::<BTreeMap<_, _>>()
    });
    let usable = cells
        .iter()
        .filter(|c| {
            score_map
                .as_ref()
                .is_none_or(|m| m.contains_key(&c.session_date))
        })
        .collect::<Vec<_>>();
    for i in 0..usable.len() {
        for j in i + 1..usable.len() {
            let a = usable[i];
            let b = usable[j];
            let sep = a.timestamp_ns.abs_diff(b.timestamp_ns) as f64 / 3.6e12;
            if sep < 1.0 {
                continue;
            }
            let bi = (0..9)
                .find(|k| sep >= EDGES[*k] && sep < EDGES[*k + 1])
                .unwrap();
            let class = usize::from(a.session_date != b.session_date);
            let stratum = usize::from(a.hour == 20 || b.hour == 20);
            let product = match field {
                Field::Residual => a.residual * b.residual,
                Field::Standardized => {
                    let m = score_map.as_ref().unwrap();
                    let sa = m[&a.session_date];
                    let sb = m[&b.session_date];
                    let ai = HOURS.iter().position(|h| *h == a.hour).unwrap();
                    let bj = HOURS.iter().position(|h| *h == b.hour).unwrap();
                    sa.z_star[ai] * sb.z_star[bj]
                }
            };
            bins[bi][class][stratum].push(product)
        }
    }
    let names = [
        "[1,2)", "[2,3)", "[3,6)", "[6,12)", "[12,24)", "[24,48)", "[48,72)", "[72,96)", "[96,inf)",
    ];
    Value::Array((0..9).map(|bi|json!({"bin_hours":names[bi],"WITHIN":{"ordinary":estimate(&bins[bi][0][0]),"hour20":estimate(&bins[bi][0][1])},"CROSS":{"ordinary":estimate(&bins[bi][1][0]),"hour20":estimate(&bins[bi][1][1])}})).collect())
}
fn estimate(x: &[f64]) -> Value {
    json!({"pair_count":x.len(),"value":if x.len()>=MIN_PAIRS{Some(mean(x))}else{None},"reason":if x.len()<MIN_PAIRS{Some("fewer_than_8_pairs")}else{None}})
}
fn contrasts(c: &Value) -> Value {
    Value::Array(c.as_array().unwrap().iter().map(|b|{let calc=|s:&str|{let w=&b["WITHIN"][s];let x=&b["CROSS"][s];let value=match(w["value"].as_f64(),x["value"].as_f64()){(Some(a),Some(z))=>Some(a-z),_=>None};json!({"within_pair_count":w["pair_count"],"cross_pair_count":x["pair_count"],"value":value,"reason":if value.is_none(){Some("one_or_both_classes_below_support")}else{None}})};json!({"bin_hours":b["bin_hours"],"D_ordinary":calc("ordinary"),"D_hour20":calc("hour20")})}).collect())
}

fn civil_day(s: &str) -> i64 {
    let y = s[0..4].parse::<i64>().unwrap();
    let m = s[5..7].parse::<i64>().unwrap();
    let d = s[8..10].parse::<i64>().unwrap();
    let y = y - i64::from(m <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = m + if m > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy
}
fn mean(x: &[f64]) -> f64 {
    x.iter().sum::<f64>() / x.len() as f64
}
fn executing_commit() -> anyhow::Result<String> {
    let out = Command::new("git").args(["rev-parse", "HEAD"]).output()?;
    if !out.status.success() {
        bail!("git rev-parse HEAD failed")
    }
    Ok(String::from_utf8(out.stdout)?.trim().into())
}
fn write(path: &Path, v: &Value) -> anyhow::Result<()> {
    write_json_atomic(path, v).map_err(|e| anyhow!(e.to_string()))
}
