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
const POWER_RUNS: usize = 500;
const POWER_COMPONENT_KEY: u64 = 1;
const POWER_ALPHA: f64 = 0.05;

#[derive(Args)]
pub struct StageMArgs {
    #[command(subcommand)]
    pub command: StageMCommand,
}

#[derive(Subcommand)]
pub enum StageMCommand {
    /// Produce the calendar-bound preflight for one new-design month.
    Preflight(PreflightArgs),
    /// Run one new-design month. July is reserved for `backcheck`.
    Month(MonthArgs),
    /// Recompute July Tier 1a under every original seed path.
    Backcheck(BackcheckArgs),
    /// Run Tier 1b from completed per-month slow-geometry artifacts.
    Exchangeability(ExchangeabilityArgs),
    /// Run the preregistered Tier 1b calendar-only power analysis.
    Power(PowerArgs),
    /// Summarize numeric per-month statistics with and without July.
    Summarize(SummarizeArgs),
}

#[derive(Args)]
pub struct PreflightArgs {
    #[arg(long, value_parser = parse_month)]
    month: u64,
    #[arg(long)]
    corpus: PathBuf,
    #[arg(long)]
    ledger: PathBuf,
    #[arg(long)]
    ledger_key: String,
    #[arg(long, default_value = "analysis/databento-calendar.json")]
    calendar: PathBuf,
    /// Defaults to `<output-root>/<YYYYMM>/preflight.json`.
    #[arg(long)]
    output: Option<PathBuf>,
    #[arg(long, default_value = "analysis/out/stage-m")]
    output_root: PathBuf,
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
    /// Defaults to `<output-root>/<YYYYMM>/preflight.json`.
    #[arg(long)]
    preflight: Option<PathBuf>,
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
pub struct PowerArgs {
    #[arg(long, default_value = "analysis/databento-calendar.json")]
    calendar: PathBuf,
    #[arg(
        long,
        default_value = "analysis/out/stage-m/202607/slow-geometry.recomputed.json"
    )]
    july_scores: PathBuf,
    #[arg(long, default_value = "analysis/out/slow-geometry.json")]
    july_scores_alternate: PathBuf,
    #[arg(long, default_value = "analysis/out/stage-m/power.json")]
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
        StageMCommand::Preflight(x) => run_preflight(x),
        StageMCommand::Month(x) => run_month(x),
        StageMCommand::Backcheck(x) => run_backcheck(&x),
        StageMCommand::Exchangeability(x) => run_exchangeability(&x),
        StageMCommand::Power(x) => run_power(&x),
        StageMCommand::Summarize(x) => run_summarize(&x),
    }
}

fn run_preflight(args: PreflightArgs) -> anyhow::Result<()> {
    if args.month == JULY {
        bail!("July is frozen and must use `mogwai preflight`");
    }
    let output = args.output.unwrap_or_else(|| {
        args.output_root
            .join(args.month.to_string())
            .join("preflight.json")
    });
    let artifact = mogwai_lab::preflight::run_month_preflight(
        args.month,
        &args.corpus,
        &args.ledger,
        &args.ledger_key,
        &args.calendar,
    )
    .map_err(|e| anyhow!("stage-m preflight refused: {e}"))?;
    mogwai_lab::preflight::write_json_atomic(&output, &artifact)
        .map_err(|e| anyhow!("writing {}: {e}", output.display()))?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "month": args.month,
            "rows": artifact.rows,
            "usable_sessions": artifact.usable_sessions.len(),
            "excluded_sessions": artifact.excluded_sessions.len(),
            "inventory_provenance": artifact.inventory_provenance,
        }))?
    );
    println!("stage-m preflight PASS -> {}", output.display());
    Ok(())
}

fn run_power(args: &PowerArgs) -> anyhow::Result<()> {
    let calendar = read_json(&args.calendar)?;
    let mut populations = Vec::new();
    let months = calendar["months"]
        .as_object()
        .ok_or_else(|| anyhow!("calendar artifact has no months object"))?;
    for (month_text, value) in months {
        if value["role"] != "new-design" {
            continue;
        }
        let month = month_text.replace('-', "").parse::<u64>()?;
        let dates = value["dates"]
            .as_object()
            .ok_or_else(|| anyhow!("calendar month {month_text} has no dates"))?;
        let mut days = Vec::new();
        for (date, row) in dates {
            match row["grade"].as_str() {
                Some("full_session" | "half_session") => {
                    days.push(mogwai_lab::session::days_from_iso(date));
                }
                Some("closure") => {}
                Some(grade) => bail!("calendar date {date} has unknown grade {grade}"),
                None => bail!("calendar date {date} has no grade"),
            }
        }
        days.sort_unstable();
        if days.is_empty() {
            bail!("calendar month {month_text} has no scheduled sessions");
        }
        populations.push((month, days));
    }
    populations.sort_by_key(|x| x.0);
    if populations.len() < 5 {
        bail!("calendar supplies fewer than five new-design months");
    }

    let july = read_json(&args.july_scores)?;
    let alternate = read_json(&args.july_scores_alternate)?;
    let july_rows = july["detail"]["cross_fitted_factor"]["scores"]
        .as_array()
        .ok_or_else(|| anyhow!("canonical July artifact has no cross-fitted scores"))?;
    let alternate_rows = alternate["detail"]["cross_fitted_factor"]["scores"]
        .as_array()
        .ok_or_else(|| anyhow!("alternate July artifact has no cross-fitted scores"))?;
    if alternate_rows != july_rows {
        bail!("the two candidate July score sources disagree");
    }
    let july_values = july_rows
        .iter()
        .map(|row| {
            row["score"]
                .as_f64()
                .ok_or_else(|| anyhow!("July score is not finite"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    if july_values.len() < 2 {
        bail!("July has fewer than two scores");
    }
    let july_mean = july_values.iter().sum::<f64>() / july_values.len() as f64;
    let july_variance = july_values
        .iter()
        .map(|x| (x - july_mean) * (x - july_mean))
        .sum::<f64>()
        / (july_values.len() - 1) as f64;
    if !july_variance.is_finite() || july_variance <= 0.0 {
        bail!("July sample score variance is not positive and finite");
    }

    let rhos = [0.3, 0.5, 0.7, 0.9];
    let lambdas = [0.25, 0.5, 0.75];
    let mut surface = Vec::new();
    for (lambda_index, lambda) in lambdas.into_iter().enumerate() {
        for (rho_index, rho) in rhos.into_iter().enumerate() {
            let mut rejected = 0usize;
            for simulation_index in 0..POWER_RUNS {
                let simulation_seed = tuple_mix(
                    STAGE_M_SEED,
                    &[
                        POWER_COMPONENT_KEY,
                        rho_index as u64,
                        lambda_index as u64,
                        simulation_index as u64,
                    ],
                );
                let simulated =
                    simulate_scores(&populations, july_variance, rho, lambda, simulation_seed);
                let p_value = exchangeability_p_value(&simulated)?;
                rejected += usize::from(p_value <= POWER_ALPHA);
            }
            let power = rejected as f64 / POWER_RUNS as f64;
            let (low, high) = wilson_95(rejected, POWER_RUNS);
            surface.push(json!({
                "rho":rho,"lambda":lambda,"simulations":POWER_RUNS,
                "rejections":rejected,"power":power,
                "binomial_95_percent_interval":{"method":"Wilson score, two-sided, z=1.959963984540054","low":low,"high":high}
            }));
        }
    }
    let minimum_rho = lambdas
        .into_iter()
        .map(|lambda| {
            let found = surface.iter().find_map(|cell| {
                (cell["lambda"].as_f64() == Some(lambda)
                    && cell["power"].as_f64().is_some_and(|x| x >= 0.8))
                .then(|| cell["rho"].as_f64().unwrap())
            });
            json!({"lambda":lambda,"minimum_rho_reaching_80_percent_power":found})
        })
        .collect::<Vec<_>>();
    let focal_power = surface
        .iter()
        .find(|cell| cell["rho"] == 0.5 && cell["lambda"] == 0.5)
        .and_then(|cell| cell["power"].as_f64())
        .expect("focal cell exists");
    let artifact = json!({
        "outcome":"completed",
        "contract":"notes/stage-m-preregistration.md Tier 1b",
        "declared_simplification":"scalar scores only; factor directions and cross-fitting are not simulated; this calibrates sensitivity to score-level persistence, not the full pipeline",
        "population":{
            "calendar":args.calendar,
            "role":"new-design",
            "months":populations.iter().map(|(month,days)|json!({"month":month,"scheduled_sessions":days.len()})).collect::<Vec<_>>(),
            "session_grade_treatment":"closures contribute no session; full_session and half_session each contribute one scheduled trading session",
            "index":"trading-session index within each month; calendar gaps add no decay"
        },
        "calibration":{
            "canonical_july_score_source":args.july_scores,
            "alternate_candidate_source":args.july_scores_alternate,
            "candidate_sources_equal":true,
            "choice":"the Stage M recomputation is canonical because it is the method backcheck output; equality with the original committed source is required",
            "score_count":july_values.len(),"sample_score_variance":july_variance,"sample_variance_denominator":"n-1"
        },
        "simulation_law":{
            "process":"Gaussian AR(1) factor over trading-session index, stationary initialization independently per month, plus independent Gaussian noise",
            "total_variance":july_variance,"factor_shares":lambdas,"persistence":rhos,
            "weekday_effects":"none","simulations_per_cell":POWER_RUNS
        },
        "test":{
            "replicates":REPS,"rejection_rule":"p_value <= 0.05",
            "p_value":"(1 + null max absolute month-equal statistics at least observed) / (1 + 2000)",
            "implementation_choice":"the preregistration specifies the p-value but not an alpha; the conventional two-sided 0.05 level is declared here before results"
        },
        "seed_derivation":{
            "implementation_choice":"the preregistration did not name simulation-draw seeds",
            "simulation":"tuple_mix(STAGE_M_SEED, [1, rho_index, lambda_index, simulation_index]); reserved component key 1 distinguishes power draws from Tier 1b permutation pseudo-month key 0",
            "month":"tuple_mix(simulation_seed, [month]); each month has its own draw stream, making draws independent of month and cell iteration order",
            "normal_draw":"Box-Muller from consecutive splitmix64 states",
            "stage_m_seed":STAGE_M_SEED,"reserved_component_key":POWER_COMPONENT_KEY
        },
        "interval_implementation_choice":"the preregistration requires a binomial 95 percent interval but names no construction; two-sided Wilson score intervals are used",
        "surface":surface,"minimum_rho_by_lambda":minimum_rho,
        "predeclared_interpretation":{
            "cell":{"rho":0.5,"lambda":0.5,"power":focal_power},
            "rule":"if power is below 50 percent, Tier 1b still runs but non-rejection is uninformative and may not be cited as evidence against persistence; power above 50 percent is not thereby adequate",
            "outcome":if focal_power < 0.5 {"below_50_percent_non_rejection_uninformative"} else {"at_or_above_50_percent_not_thereby_adequate"}
        }
    });
    write_json_atomic(&args.output, &artifact).map_err(|e| anyhow!(e.to_string()))
}

fn simulate_scores(
    populations: &[(u64, Vec<i64>)],
    total_variance: f64,
    rho: f64,
    lambda: f64,
    simulation_seed: u64,
) -> Vec<MonthScores> {
    let factor_variance = lambda * total_variance;
    let noise_sd = ((1.0 - lambda) * total_variance).sqrt();
    let innovation_sd = (factor_variance * (1.0 - rho * rho)).sqrt();
    populations
        .iter()
        .map(|(month, dates)| {
            let mut normal = NormalStream::new(tuple_mix(simulation_seed, &[*month]));
            let mut factor = factor_variance.sqrt() * normal.draw();
            let mut scores = Vec::with_capacity(dates.len());
            for i in 0..dates.len() {
                if i > 0 {
                    factor = rho.mul_add(factor, innovation_sd * normal.draw());
                }
                scores.push(factor + noise_sd * normal.draw());
            }
            MonthScores {
                month: *month,
                dates: dates.clone(),
                weekdays: dates
                    .iter()
                    .map(|day| (day + 3).rem_euclid(7) as usize)
                    .collect(),
                scores,
            }
        })
        .collect()
}

struct NormalStream {
    state: u64,
    spare: Option<f64>,
}

impl NormalStream {
    fn new(state: u64) -> Self {
        Self { state, spare: None }
    }

    fn draw(&mut self) -> f64 {
        if let Some(x) = self.spare.take() {
            return x;
        }
        self.state = splitmix64(self.state);
        let u1 = ((self.state >> 11) as f64 + 0.5) * (1.0 / (1u64 << 53) as f64);
        self.state = splitmix64(self.state);
        let u2 = ((self.state >> 11) as f64 + 0.5) * (1.0 / (1u64 << 53) as f64);
        let radius = (-2.0 * u1.ln()).sqrt();
        let angle = std::f64::consts::TAU * u2;
        self.spare = Some(radius * angle.sin());
        radius * angle.cos()
    }
}

fn wilson_95(successes: usize, runs: usize) -> (f64, f64) {
    let z = 1.959_963_984_540_054;
    let n = runs as f64;
    let p = successes as f64 / n;
    let denominator = 1.0 + z * z / n;
    let center = (p + z * z / (2.0 * n)) / denominator;
    let half = z * (p.mul_add(1.0 - p, z * z / (4.0 * n)) / n).sqrt() / denominator;
    (center - half, center + half)
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
    let preflight = args.preflight.unwrap_or_else(|| dir.join("preflight.json"));
    let count_path = dir.join("count-curve.json");
    let sequence_path = dir.join("ordered-counts.jsonl");
    let panels_path = dir.join("ordered-counts-panels.json");
    let slow_path = dir.join("slow-geometry.json");

    let count_config = CountCurveMonthRun {
        month: args.month,
        corpus: args.corpus.clone(),
        ledger: args.ledger.clone(),
        preflight: preflight.clone(),
        output: count_path,
    };
    let pass = run_observed_with_count_windows_ordered(
        &args.corpus,
        &args.ledger,
        &preflight,
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
    let usable = usable_count(&preflight)?;
    ordered_counts::run_with_rows(
        &OrderedCountsRun {
            month: args.month,
            corpus: args.corpus,
            ledger: args.ledger,
            preflight,
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

    // Stage M Amendment 1 added per-fold training provenance to every score
    // (training_dropped, training_count, training_retained). The committed
    // July reference predates it; the backcheck equality is on statistical
    // content, and provenance fields are projected away on BOTH sides.
    fn strip_amendment1_provenance(scores: &serde_json::Value) -> serde_json::Value {
        let mut scores = scores.clone();
        if let Some(items) = scores.as_array_mut() {
            for item in items {
                if let Some(map) = item.as_object_mut() {
                    map.remove("training_dropped");
                    map.remove("training_count");
                    map.remove("training_retained");
                }
            }
        }
        scores
    }

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
            &strip_amendment1_provenance(&old_slow["detail"]["cross_fitted_factor"]["scores"]),
            &strip_amendment1_provenance(&new_slow["detail"]["cross_fitted_factor"]["scores"]),
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
    let (exceed, p_value) = exchangeability_null(months, observed_max)?;
    Ok(json!({
        "months":months.iter().map(|m|m.month).collect::<Vec<_>>(),
        "observed":observed,"max_abs_month_equal":observed_max,
        "null_exceedance_count":exceed,"p_value":p_value
    }))
}

fn exchangeability_p_value(months: &[MonthScores]) -> anyhow::Result<f64> {
    let observed_max = supported_max(&pooled_bins(months))?;
    exchangeability_null(months, observed_max).map(|x| x.1)
}

fn exchangeability_null(months: &[MonthScores], observed_max: f64) -> anyhow::Result<(usize, f64)> {
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
    Ok((exceed, (1.0 + exceed as f64) / (1.0 + REPS as f64)))
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

    #[test]
    fn power_draws_are_deterministic_and_month_order_independent() {
        let a = vec![(202_509, vec![1, 2, 3]), (202_510, vec![4, 5, 6])];
        let b = vec![(202_510, vec![4, 5, 6]), (202_509, vec![1, 2, 3])];
        let seed = tuple_mix(STAGE_M_SEED, &[POWER_COMPONENT_KEY, 1, 1, 7]);
        let first = simulate_scores(&a, 2.0, 0.5, 0.5, seed);
        let again = simulate_scores(&a, 2.0, 0.5, 0.5, seed);
        let reordered = simulate_scores(&b, 2.0, 0.5, 0.5, seed);
        assert_eq!(first[0].scores, again[0].scores);
        assert_eq!(first[0].scores, reordered[1].scores);
        assert_eq!(first[1].scores, reordered[0].scores);
    }

    #[test]
    fn wilson_interval_contains_observed_power() {
        let (low, high) = wilson_95(250, 500);
        assert!(low < 0.5 && high > 0.5);
        assert!(wilson_95(0, 500).0.abs() < f64::EPSILON);
        assert!((wilson_95(500, 500).1 - 1.0).abs() < f64::EPSILON);
    }
}
