// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Stage M Tier 1 orchestration. Each corpus month is deliberately one
//! invocation and one artifact directory.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow, bail};
use clap::{Args, Subcommand};
use mogwai_lab::aggregate::artifact::write_json_atomic;
use mogwai_lab::aggregate::bootstrap::{STAGE_M_SEED, stage_m_bootstrap_multiplicities};
use mogwai_lab::kernel::{splitmix64, tuple_mix};
use serde_json::{Value, json};

use crate::count_curve::{self, CountCurveMonthRun};
use crate::measure::run_observed_with_count_windows_ordered;
use crate::ordered_counts::{self, OrderedCountsRun};
use crate::slow_geometry::{self, SlowGeometryRun};

const JULY: u64 = 202_607;
const REPS: usize = 2_000;

#[derive(Args)]
pub struct StageMArgs {
    #[command(subcommand)]
    pub command: StageMCommand,
}

#[derive(Subcommand)]
pub enum StageMCommand {
    /// Run one new-design month. July is reserved for `backcheck`.
    Month(MonthArgs),
    /// Recompute July Tier 1a under every original seed path.
    Backcheck(BackcheckArgs),
    /// Run Tier 1b from completed per-month slow-geometry artifacts.
    Exchangeability(ExchangeabilityArgs),
    /// Summarize numeric per-month statistics with and without July.
    Summarize(SummarizeArgs),
}

#[derive(Args)]
pub struct MonthArgs {
    #[arg(long, value_parser = parse_month)]
    month: u64,
    #[arg(long)]
    corpus: PathBuf,
    #[arg(long)]
    ledger: PathBuf,
    /// Exact seal-ledger job key for this month.
    #[arg(long)]
    ledger_key: String,
    #[arg(long)]
    preflight: PathBuf,
    #[arg(long, default_value = "analysis/out/stage-m")]
    output_root: PathBuf,
}

#[derive(Args)]
pub struct BackcheckArgs {
    #[arg(long, default_value = "analysis/out/stage-m")]
    output_root: PathBuf,
}

#[derive(Args)]
pub struct ExchangeabilityArgs {
    /// JSON array of per-month slow-geometry artifact paths.
    #[arg(long)]
    manifest: PathBuf,
    #[arg(long, default_value = "analysis/out/stage-m/exchangeability.json")]
    output: PathBuf,
}

#[derive(Args)]
pub struct SummarizeArgs {
    /// JSON array of per-month artifact paths of one artifact kind.
    #[arg(long)]
    manifest: PathBuf,
    #[arg(long)]
    output: PathBuf,
}

pub fn run(args: StageMArgs) -> anyhow::Result<()> {
    match args.command {
        StageMCommand::Month(x) => run_month(x),
        StageMCommand::Backcheck(x) => run_backcheck(&x),
        StageMCommand::Exchangeability(x) => run_exchangeability(&x),
        StageMCommand::Summarize(x) => run_summarize(&x),
    }
}

fn run_summarize(args: &SummarizeArgs) -> anyhow::Result<()> {
    let manifest = read_json(&args.manifest)?;
    let paths = manifest
        .as_array()
        .ok_or_else(|| anyhow!("manifest must be a JSON array of paths"))?;
    let mut months = Vec::new();
    for p in paths {
        let path = p
            .as_str()
            .ok_or_else(|| anyhow!("manifest paths must be strings"))?;
        let v = read_json(path)?;
        let month = v["binding"]["month"]
            .as_u64()
            .ok_or_else(|| anyhow!("{path} has no month"))?;
        let mut flat = BTreeMap::new();
        flatten_numeric("", &v, &mut flat);
        months.push((month, flat));
    }
    months.sort_by_key(|x| x.0);
    let design_months = months.iter().filter(|x| x.0 != JULY).count();
    let outcome = if design_months < 6 {
        "design_population_insufficient"
    } else {
        "completed"
    };
    let artifact = json!({"outcome":outcome,"design_month_count":design_months,"per_month":months.iter().map(|(m,v)|json!({"month":m,"statistics":v})).collect::<Vec<_>>(),
        "combined":{"with_july":summaries(&months),"without_july":summaries(&months.iter().filter(|x|x.0!=JULY).cloned().collect::<Vec<_>>())}});
    write_json_atomic(&args.output, &artifact).map_err(|e| anyhow!(e.to_string()))?;
    if design_months < 6 {
        bail!("design_population_insufficient");
    }
    Ok(())
}

fn flatten_numeric(prefix: &str, v: &Value, out: &mut BTreeMap<String, f64>) {
    match v {
        Value::Number(n) => {
            if let Some(x) = n.as_f64() {
                out.insert(prefix.to_string(), x);
            }
        }
        Value::Object(m) => {
            for (k, v) in m {
                let p = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten_numeric(&p, v, out);
            }
        }
        Value::Array(xs) => {
            for (i, v) in xs.iter().enumerate() {
                flatten_numeric(&format!("{prefix}[{i}]"), v, out);
            }
        }
        _ => {}
    }
}

fn summaries(months: &[(u64, BTreeMap<String, f64>)]) -> Value {
    let mut fields: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
    for (_, m) in months {
        for (k, v) in m {
            fields.entry(k).or_default().push(*v);
        }
    }
    Value::Object(fields.into_iter().map(|(k,x)| {
        let mean = x.iter().sum::<f64>()/x.len() as f64;
        let sd = if x.len() > 1 {(x.iter().map(|v|(v-mean)*(v-mean)).sum::<f64>()/(x.len()-1) as f64).sqrt()} else {0.0};
        (k.to_string(),json!({"month_count":x.len(),"mean":mean,"standard_deviation":sd,"min":x.iter().copied().fold(f64::INFINITY,f64::min),"max":x.iter().copied().fold(f64::NEG_INFINITY,f64::max)}))
    }).collect())
}

fn run_month(args: MonthArgs) -> anyhow::Result<()> {
    if args.month == JULY {
        bail!("July is spent-design and must use `stage-m backcheck`");
    }
    let dir = args.output_root.join(args.month.to_string());
    std::fs::create_dir_all(&dir)?;
    let count_path = dir.join("count-curve.json");
    let sequence_path = dir.join("ordered-counts.jsonl");
    let panels_path = dir.join("ordered-counts-panels.json");
    let slow_path = dir.join("slow-geometry.json");

    let count_config = CountCurveMonthRun {
        month: args.month,
        corpus: args.corpus.clone(),
        ledger: args.ledger.clone(),
        preflight: args.preflight.clone(),
        output: count_path,
    };
    let pass = run_observed_with_count_windows_ordered(
        &args.corpus,
        &args.ledger,
        &args.preflight,
        &[1, 5, 15, 60, 300],
        &args.ledger_key,
    );
    let (observed, rows) = match pass {
        Ok(x) => x,
        Err(error) => {
            let reason = error.to_string();
            let outcome = if reason.starts_with("verifying the delivered corpus") {
                "input_mismatch"
            } else {
                "recorded_refusal"
            };
            let artifact = json!({"outcome":outcome,"month":args.month,"ledger_key":args.ledger_key,"reason":reason});
            write_json_atomic(&dir.join("refusal.json"), &artifact)
                .map_err(|e| anyhow!(e.to_string()))?;
            return Err(error);
        }
    };
    count_curve::write_month_from_observed(&count_config, &observed)?;
    let usable = usable_count(&args.preflight)?;
    ordered_counts::run_with_rows(
        &OrderedCountsRun {
            month: args.month,
            corpus: args.corpus,
            ledger: args.ledger,
            preflight: args.preflight,
            sequence: sequence_path.clone(),
            summary: panels_path,
            permutation_seed: STAGE_M_SEED,
            bootstrap: stage_m_bootstrap_multiplicities(args.month, usable),
            require_july_backcheck: false,
        },
        &observed,
        rows,
    )?;
    let hash =
        mogwai_lab::ledger::sha256_file(&sequence_path).map_err(|e| anyhow!(e.to_string()))?;
    slow_geometry::run_with(&SlowGeometryRun {
        month: args.month,
        input: sequence_path,
        output: slow_path,
        expected_sha256: hash,
    })
}

fn usable_count(path: &Path) -> anyhow::Result<usize> {
    let v: Value = serde_json::from_slice(&std::fs::read(path)?)?;
    v["usable_sessions"]
        .as_array()
        .map(Vec::len)
        .ok_or_else(|| anyhow!("preflight artifact carries no usable_sessions array"))
}

fn run_backcheck(args: &BackcheckArgs) -> anyhow::Result<()> {
    let out_dir = args.output_root.join(JULY.to_string());
    std::fs::create_dir_all(&out_dir)?;
    let sequence = PathBuf::from("analysis/out/ordered-counts.jsonl");
    let sequence_hash =
        mogwai_lab::ledger::sha256_file(&sequence).map_err(|e| anyhow!(e.to_string()))?;
    let panels = out_dir.join("ordered-counts-panels.recomputed.json");
    ordered_counts::run_with(&OrderedCountsRun {
        month: JULY,
        corpus: "research/market-data/databento/mnqv/2026-07.full.tbbo".into(),
        ledger: "analysis/databento-jobs.json".into(),
        preflight: "analysis/out/mnq-fit-preflight.json".into(),
        sequence: sequence.clone(),
        summary: panels.clone(),
        permutation_seed: 8_934_572_019_384_756_123,
        bootstrap: Vec::new(),
        require_july_backcheck: true,
    })?;
    let slow = out_dir.join("slow-geometry.recomputed.json");
    slow_geometry::run_with(&SlowGeometryRun {
        month: JULY,
        input: sequence,
        output: slow.clone(),
        expected_sha256: sequence_hash,
    })?;

    let old_panels = read_json("analysis/out/ordered-counts-panels.json")?;
    let new_panels = read_json(&panels)?;
    let old_slow = read_json("analysis/out/slow-geometry.json")?;
    let new_slow = read_json(&slow)?;
    let reference = read_json("analysis/mnq-measure-12a.json")?;
    let sessions = reference["observed"]["per_session"]
        .as_array()
        .ok_or_else(|| anyhow!("July 12a artifact has no observed sessions"))?;
    let curve_sessions = ordered_counts::count_curve_sessions_from_sequence(Path::new(
        "analysis/out/ordered-counts.jsonl",
    ))?;
    let count_recomputed = count_curve::july_observed_statistics(&curve_sessions)?;
    let old_count = read_json("analysis/out/count-curve-measurement.json")?;
    let monthly_recomputed = mogwai_lab::aggregate::monthly::blocks_from_sessions(sessions)
        .map_err(|e| anyhow!(e.to_string()))?;
    let checks = vec![
        check(
            "measure12a.observed.monthly",
            "point_estimate",
            &reference["observed"]["monthly"],
            &monthly_recomputed,
        ),
        check(
            "count_curve.records",
            "point_and_retained_bootstrap_outputs",
            &old_count["observed"],
            &count_recomputed,
        ),
        check(
            "ordered.panel_a.point",
            "point_estimate",
            &old_panels["panel_a"]["point"],
            &new_panels["panel_a"]["point"],
        ),
        check(
            "ordered.panel_b.point",
            "point_estimate",
            &old_panels["panel_b"]["point"],
            &new_panels["panel_b"]["point"],
        ),
        check(
            "ordered.panel_a.bootstrap",
            "null_distribution",
            &old_panels["panel_a"]["uncertainty"],
            &new_panels["panel_a"]["uncertainty"],
        ),
        check(
            "ordered.panel_b.bootstrap",
            "null_distribution",
            &old_panels["panel_b"]["uncertainty"],
            &new_panels["panel_b"]["uncertainty"],
        ),
        check(
            "ordered.panel_b.permutation",
            "null_distribution",
            &old_panels["panel_b"]["permutation"],
            &new_panels["panel_b"]["permutation"],
        ),
        check(
            "slow.scores",
            "point_estimate",
            &old_slow["detail"]["cross_fitted_factor"]["scores"],
            &new_slow["detail"]["cross_fitted_factor"]["scores"],
        ),
        check(
            "slow.S_g",
            "point_estimate",
            &old_slow["detail"]["statistic_1"]["result"]["bins"],
            &new_slow["detail"]["statistic_1"]["result"]["bins"],
        ),
        check(
            "slow.permutation",
            "null_distribution",
            &old_slow["detail"]["statistic_1"]["result"]["permutation"],
            &new_slow["detail"]["statistic_1"]["result"]["permutation"],
        ),
    ];
    let matched = checks.iter().all(|x| x["equal"] == true);
    let artifact = json!({
        "outcome":if matched {"completed"} else {"method_mismatch"},
        "month":JULY,
        "seed_domain":"original",
        "checks":checks,
        "comparison_rule":"point estimates compare exactly; permutation/bootstrap outputs compare exactly only where committed artifacts retain comparable values"
    });
    write_json_atomic(&out_dir.join("backcheck.json"), &artifact)
        .map_err(|e| anyhow!(e.to_string()))?;
    if matched {
        Ok(())
    } else {
        bail!("method_mismatch")
    }
}

fn check(name: &str, kind: &str, old: &Value, new: &Value) -> Value {
    json!({"statistic":name,"comparison":kind,"comparable":true,"equal":old == new})
}

fn read_json(path: impl AsRef<Path>) -> anyhow::Result<Value> {
    serde_json::from_slice(
        &std::fs::read(path.as_ref())
            .with_context(|| format!("reading {}", path.as_ref().display()))?,
    )
    .map_err(Into::into)
}

#[derive(Clone)]
struct MonthScores {
    month: u64,
    dates: Vec<i64>,
    weekdays: Vec<usize>,
    scores: Vec<f64>,
}

fn run_exchangeability(args: &ExchangeabilityArgs) -> anyhow::Result<()> {
    let manifest: Value = read_json(&args.manifest)?;
    let paths = manifest
        .as_array()
        .ok_or_else(|| anyhow!("manifest must be a JSON array of paths"))?;
    let mut months = Vec::new();
    for path in paths {
        let path = path
            .as_str()
            .ok_or_else(|| anyhow!("manifest paths must be strings"))?;
        let v = read_json(path)?;
        let month = v["binding"]["month"]
            .as_u64()
            .ok_or_else(|| anyhow!("{path} has no canonical month"))?;
        let rows = v["detail"]["cross_fitted_factor"]["scores"]
            .as_array()
            .ok_or_else(|| anyhow!("{path} has no scores"))?;
        let mut dates = Vec::new();
        let mut scores = Vec::new();
        let mut weekdays = Vec::new();
        for row in rows {
            let d = row["session_date"]
                .as_str()
                .ok_or_else(|| anyhow!("score has no date"))?;
            let day = mogwai_lab::session::days_from_iso(d);
            dates.push(day);
            weekdays.push((day + 3).rem_euclid(7) as usize);
            scores.push(
                row["score"]
                    .as_f64()
                    .ok_or_else(|| anyhow!("score is not finite"))?,
            );
        }
        months.push(MonthScores {
            month,
            dates,
            weekdays,
            scores,
        });
    }
    months.sort_by_key(|x| x.month);
    let without_july = months
        .iter()
        .filter(|m| m.month != JULY)
        .cloned()
        .collect::<Vec<_>>();
    let primary = exchangeability_test(&without_july)?;
    let with_july = if without_july.len() == months.len() {
        primary.clone()
    } else {
        exchangeability_test(&months)?
    };
    let artifact = json!({
        "outcome":"completed","binding":{"stage_m_seed":STAGE_M_SEED,"pseudo_month_key":0,"replicates":REPS,"months":months.iter().map(|m|m.month).collect::<Vec<_>>()},
        "null":"computed cross-fitted scores exchangeable within month-weekday cells",
        "primary_without_july":primary,"descriptive_with_july":with_july,
        "interpretation":"conditional non-exchangeability given month and weekday only; no serial mechanism or architecture class is identified"
    });
    write_json_atomic(&args.output, &artifact).map_err(|e| anyhow!(e.to_string()))
}

fn exchangeability_test(months: &[MonthScores]) -> anyhow::Result<Value> {
    let observed = pooled_bins(months);
    let observed_max = supported_max(&observed)?;
    let mut exceed = 0;
    for rep in 0..REPS {
        let mut permuted = months.to_vec();
        let mut state = tuple_mix(STAGE_M_SEED, &[0, rep as u64]);
        for month in &mut permuted {
            for weekday in 0..7 {
                let idx = month
                    .weekdays
                    .iter()
                    .enumerate()
                    .filter_map(|(i, w)| (*w == weekday).then_some(i))
                    .collect::<Vec<_>>();
                let mut values = idx.iter().map(|i| month.scores[*i]).collect::<Vec<_>>();
                for i in (1..values.len()).rev() {
                    state = splitmix64(state);
                    let j = (state % (i as u64 + 1)) as usize;
                    values.swap(i, j);
                }
                for (i, v) in idx.into_iter().zip(values) {
                    month.scores[i] = v;
                }
            }
        }
        if supported_max(&pooled_bins(&permuted))? >= observed_max {
            exceed += 1;
        }
    }
    Ok(json!({
        "months":months.iter().map(|m|m.month).collect::<Vec<_>>(),
        "observed":observed,"max_abs_month_equal":observed_max,
        "null_exceedance_count":exceed,"p_value":(1.0+exceed as f64)/(1.0+REPS as f64)
    }))
}

fn pooled_bins(months: &[MonthScores]) -> Value {
    let mut rows = Vec::new();
    for bin in 1..=4 {
        let mut month_values = Vec::new();
        let mut weighted_sum = 0.0;
        let mut pairs = 0usize;
        let mut cells = Vec::new();
        for m in months {
            let center = m.scores.iter().sum::<f64>() / m.scores.len() as f64;
            let mut products = Vec::new();
            for i in 0..m.scores.len() {
                for j in i + 1..m.scores.len() {
                    let gap = (m.dates[j] - m.dates[i]).unsigned_abs() as usize;
                    let b = if gap >= 4 { 4 } else { gap };
                    if b == bin {
                        products.push((m.scores[i] - center) * (m.scores[j] - center));
                    }
                }
            }
            let value =
                (products.len() >= 4).then(|| products.iter().sum::<f64>() / products.len() as f64);
            if let Some(x) = value {
                month_values.push(x);
                weighted_sum += x * products.len() as f64;
                pairs += products.len();
            }
            cells.push(json!({"month":m.month,"pair_count":products.len(),"S":value,"reason":if value.is_none(){Some("fewer_than_4_pairs")}else{None}}));
        }
        let supported = month_values.len() >= 5;
        rows.push(json!({"gap_days":if bin == 4 {"4_or_more".into()} else {bin.to_string()},"contributing_months":month_values.len(),"month_cells":cells,
            "month_equal_S":if supported {Some(month_values.iter().sum::<f64>()/month_values.len() as f64)} else {None},
            "pair_weighted_descriptive_S":if pairs > 0 {Some(weighted_sum/pairs as f64)} else {None},"supported":supported}));
    }
    Value::Array(rows)
}

fn supported_max(rows: &Value) -> anyhow::Result<f64> {
    let values = rows
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|x| x["month_equal_S"].as_f64())
        .map(f64::abs)
        .collect::<Vec<_>>();
    if values.is_empty() {
        bail!("no pooled bin has the frozen five-month support minimum");
    }
    Ok(values.into_iter().fold(0.0, f64::max))
}

fn parse_month(s: &str) -> Result<u64, String> {
    let x = s
        .parse::<u64>()
        .map_err(|_| "month must be YYYYMM".to_string())?;
    if x / 100 < 1 || !(1..=12).contains(&(x % 100)) {
        return Err("month must be canonical YYYYMM".into());
    }
    Ok(x)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_months_enforce_month_cell_and_pooled_support() {
        let months = (0..5)
            .map(|m| MonthScores {
                month: 202_508 + m,
                dates: vec![0, 1, 2, 3, 4],
                weekdays: vec![3, 4, 5, 6, 0],
                scores: vec![0.0, 1.0, 2.0, 3.0, 4.0],
            })
            .collect::<Vec<_>>();
        let bins = pooled_bins(&months);
        assert_eq!(bins[0]["contributing_months"], 5);
        assert_eq!(bins[0]["supported"], true);
        assert!(supported_max(&bins).is_ok());
        assert_eq!(bins[1]["month_cells"][0]["reason"], "fewer_than_4_pairs");
    }

    #[test]
    fn canonical_month_parser_rejects_ordinals_and_bad_months() {
        assert_eq!(parse_month("202508"), Ok(202_508));
        assert!(parse_month("8").is_err());
        assert!(parse_month("202513").is_err());
    }
}
