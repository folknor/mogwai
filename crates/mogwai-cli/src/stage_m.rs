// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Stage M Tier 1 orchestration. Each corpus month is deliberately one
//! invocation and one artifact directory.

use std::collections::{BTreeMap, BTreeSet};
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
    /// Run the signed Amendment 2 slow-geometry re-verification ladder.
    ReverifyAmendment2(BackcheckArgs),
    /// Run Amendment 4's September-October mechanical schedule gate.
    ScheduleEquivalence(BackcheckArgs),
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
        StageMCommand::ReverifyAmendment2(x) => run_amendment2_reverification(&x),
        StageMCommand::ScheduleEquivalence(x) => run_schedule_equivalence(&x),
        StageMCommand::Exchangeability(x) => run_exchangeability(&x),
        StageMCommand::Power(x) => run_power(&x),
        StageMCommand::Summarize(x) => run_summarize(&x),
    }
}

fn run_schedule_equivalence(args: &BackcheckArgs) -> anyhow::Result<()> {
    let frame = mogwai_lab::session::ScheduleFrame::stage_m(Path::new("analysis/tz-america-chicago-2026c.json"))
        .map_err(|e| anyhow!("Amendment 4 authority refused: {e}"))?;
    let fixed = mogwai_lab::session::ScheduleFrame::JulyFixed;
    let mut months = Vec::new();
    let mut all_identical = true;
    for month in [202_509_u64, 202_510] {
        let preflight = read_json(args.output_root.join(month.to_string()).join("preflight.json"))?;
        let sessions = preflight["sessions"].as_object().ok_or_else(|| anyhow!("{month} preflight has no session inventory"))?;
        let mut comparisons = Vec::new();
        for session in sessions.keys() {
            let old = fixed.bounds(session).map_err(|e| anyhow!("old bound for {session}: {e}"))?;
            let new = frame.bounds(session).map_err(|e| anyhow!("new bound for {session}: {e}"))?;
            let identical = old.open_ns == new.open_ns && old.halt_start_ns == new.halt_start_ns
                && old.halt_end_ns == new.halt_end_ns && old.close_ns == new.close_ns;
            all_identical &= identical;
            comparisons.push(json!({"session":session,"identical":identical,"measured_frame":old,"amendment4_frame":new}));
        }
        months.push(json!({"month":month,"identical":comparisons.iter().all(|x|x["identical"]==true),"sessions":comparisons}));
    }
    let artifact = json!({
        "outcome":if all_identical {"completed"} else {"stopped_nonidentity"},
        "amendment":"Stage M Amendment 4",
        "authority":{"artifact":"analysis/tz-america-chicago-2026c.json","sha256":mogwai_lab::session::STAGE_M_TZ_AUTHORITY_SHA256},
        "rule":"September and October derived Amendment 4 bounds must be identical to their original fixed UTC-5 measurement frame",
        "months":months
    });
    let output = args.output_root.join("amendment4-schedule-equivalence.json");
    write_json_atomic(&output, &artifact).map_err(|e| anyhow!(e.to_string()))?;
    if !all_identical { bail!("Amendment 4 September-October schedule equivalence gate failed"); }
    println!("Amendment 4 September-October schedule equivalence PASS -> {}", output.display());
    Ok(())
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
    if matches!(args.month, 202_511 | 202_512 | 202_601 | 202_602 | 202_603) {
        snapshot_superseded(&output, &args.output_root.join(args.month.to_string()))?;
    }
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
    let affected = matches!(args.month, 202_511 | 202_512 | 202_601 | 202_602 | 202_603);
    if affected {
        for path in [&count_path, &sequence_path, &panels_path, &slow_path] {
            snapshot_superseded(path, &dir)?;
        }
    }

    let count_config = CountCurveMonthRun {
        month: args.month,
        corpus: args.corpus.clone(),
        ledger: args.ledger.clone(),
        preflight: preflight.clone(),
        output: count_path,
    };
    let pass = run_observed_with_count_windows_ordered(
        args.month,
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
    if let Err(error) = count_curve::write_month_from_observed(&count_config, &observed) {
        if args.month != 202_603 { return Err(error); }
        let refusal = json!({
            "outcome":"recorded_refusal","month":args.month,"scope":"extended count curve",
            "reason":error.to_string(),
            "authority":"Stage M month-generic refusal rule: mixed standard/daylight UTC endpoint-hour support cannot be adapted silently"
        });
        write_json_atomic(&count_config.output, &refusal).map_err(|e| anyhow!(e.to_string()))?;
    }
    let usable = usable_count(&preflight)?;
    let ordered_result = ordered_counts::run_with_rows(
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
    );
    if let Err(error) = ordered_result {
        if args.month != 202_603 { return Err(error); }
        let refusal = json!({
            "outcome":"recorded_refusal",
            "month":args.month,
            "scope":["Panel A","Panel B"],
            "reason":error.to_string(),
            "authority":"Stage M month-generic refusal rule: a frozen document that cannot be applied month-generically refuses and is never adapted silently",
            "sequence":sequence_path,
            "sequence_sha256":mogwai_lab::ledger::sha256_file(&sequence_path).map_err(|e| anyhow!(e.to_string()))?
        });
        write_json_atomic(&dir.join("ordered-counts-panels.json"), &refusal).map_err(|e| anyhow!(e.to_string()))?;
    }
    let hash =
        mogwai_lab::ledger::sha256_file(&sequence_path).map_err(|e| anyhow!(e.to_string()))?;
    slow_geometry::run_with(&SlowGeometryRun {
        month: args.month,
        input: sequence_path,
        output: slow_path,
        expected_sha256: hash,
        exclude_exact_close_for_comparison: false,
    })?;
    if affected { write_amendment4_invalidation(args.month, &dir)?; }
    Ok(())
}

fn snapshot_superseded(path: &Path, month_dir: &Path) -> anyhow::Result<()> {
    if !path.exists() { return Ok(()); }
    let archive = month_dir.join("superseded-invalid-schedule-frame");
    std::fs::create_dir_all(&archive)?;
    let destination = archive.join(path.file_name().ok_or_else(|| anyhow!("artifact has no filename"))?);
    if !destination.exists() { std::fs::copy(path, destination)?; }
    Ok(())
}

fn write_amendment4_invalidation(month: u64, dir: &Path) -> anyhow::Result<()> {
    let archive = dir.join("superseded-invalid-schedule-frame");
    let old_preflight = read_json(archive.join("preflight.json"))?;
    let new_preflight = read_json(dir.join("preflight.json"))?;
    let artifact_names = ["preflight.json", "count-curve.json", "ordered-counts.jsonl", "ordered-counts-panels.json", "slow-geometry.json"];
    let mut artifacts = Vec::new();
    for name in artifact_names {
        let old = archive.join(name);
        let replacement = dir.join(name);
        if !old.exists() || !replacement.exists() { bail!("missing supersession pair for {month} {name}"); }
        let old_hash = mogwai_lab::ledger::sha256_file(&old).map_err(|e| anyhow!(e.to_string()))?;
        let replacement_hash = mogwai_lab::ledger::sha256_file(&replacement).map_err(|e| anyhow!(e.to_string()))?;
        let former_outcome = if name.ends_with(".json") { read_json(&old).ok().and_then(|x| x["outcome"].as_str().map(str::to_string)).unwrap_or_else(|| "completed".to_string()) } else { "completed".to_string() };
        artifacts.push(json!({"artifact":old,"sha256":old_hash,"former_outcome":former_outcome,"status":"superseded_invalid_schedule_frame","replacement":replacement,"replacement_sha256":replacement_hash}));
    }
    let fixed = mogwai_lab::session::ScheduleFrame::JulyFixed;
    let old_bounds = old_preflight["sessions"].as_object().ok_or_else(|| anyhow!("old preflight has no sessions"))?.keys()
        .map(|date| fixed.bounds(date).map_err(|e| anyhow!(e.to_string()))).collect::<anyhow::Result<Vec<_>>>()?;
    let implementing_commit = String::from_utf8(std::process::Command::new("git").args(["rev-parse", "HEAD"]).output()?.stdout)?.trim().to_string();
    let record = json!({
        "outcome":"completed",
        "status":"superseded_invalid_schedule_frame",
        "former_completed_outcomes_preserved":true,
        "month":month,
        "defect":"the fixed UTC-5 July subcontract scheduled a dead pre-open hour and excluded the real final trading hour in standard time",
        "affected_months":[202511,202512,202601,202602,202603],
        "authority":{"amendment":"notes/stage-m-preregistration.md Amendment 4, signed 2026-08-12","timezone_artifact":"analysis/tz-america-chicago-2026c.json","timezone_sha256":mogwai_lab::session::STAGE_M_TZ_AUTHORITY_SHA256,"implementing_commit":implementing_commit},
        "artifacts":artifacts,
        "population_diagnostics":{
            "old":{"outside_session_rows":old_preflight["rows_outside_declared_sessions"],"scheduled_exposure_seconds":old_bounds.iter().map(|x|x.scheduled_open_seconds).sum::<i64>(),"usable_sessions":old_preflight["usable_sessions"].as_array().map_or(0,Vec::len),"per_session_bounds":old_bounds},
            "new":{"outside_session_rows":new_preflight["rows_outside_declared_sessions"],"scheduled_exposure_seconds":new_preflight["schedule_frame"]["scheduled_exposure_seconds"],"usable_sessions":new_preflight["usable_sessions"].as_array().map_or(0,Vec::len),"per_session_bounds":new_preflight["schedule_frame"]["per_session_bounds"]}
        },
        "calendar_inventory":"analysis/databento-calendar.json is graded per DATE from record counts and is schedule-independent; preflight inventory derivation is unaffected"
    });
    write_json_atomic(&dir.join("amendment4-invalidation.json"), &record).map_err(|e| anyhow!(e.to_string()))
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
        exclude_exact_close_for_comparison: false,
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

fn run_amendment2_reverification(args: &BackcheckArgs) -> anyhow::Result<()> {
    let months = [JULY, 202_509, 202_510, 202_511, 202_512, 202_601, 202_602];
    let mut reports = Vec::new();
    let mut candidates = Vec::new();
    for month in months {
        let (sequence, old_path, candidate) = if month == JULY {
            (
                PathBuf::from("analysis/out/ordered-counts.jsonl"),
                PathBuf::from("analysis/out/slow-geometry.json"),
                args.output_root.join(month.to_string()).join("slow-geometry.amendment2.json"),
            )
        } else {
            let dir = args.output_root.join(month.to_string());
            (dir.join("ordered-counts.jsonl"), dir.join("slow-geometry.json"), dir.join("slow-geometry.amendment2.json"))
        };
        let hash = mogwai_lab::ledger::sha256_file(&sequence).map_err(|e| anyhow!(e.to_string()))?;
        slow_geometry::run_with(&SlowGeometryRun { month, input: sequence.clone(), output: candidate.clone(), expected_sha256: hash.clone(), exclude_exact_close_for_comparison: false })?;
        let old = read_json(&old_path)?;
        let new = read_json(&candidate)?;
        let report = if month == JULY {
            let excluded = args.output_root.join(month.to_string()).join("slow-geometry.exact-close-excluded.json");
            slow_geometry::run_with(&SlowGeometryRun { month, input: sequence.clone(), output: excluded.clone(), expected_sha256: hash, exclude_exact_close_for_comparison: true })?;
            let run2 = read_json(&excluded)?;
            compare_amendment3_july(&old, &run2, &new, &sequence)?
        } else if month <= 202_510 {
            compare_amendment2_month(month, &old, &new)
        } else {
            verify_winter_month(month, &new)
        };
        let passed = report["passed"].as_bool().unwrap_or(false);
        reports.push(report);
        candidates.push((month, candidate, old_path));
        if !passed {
            let artifact = json!({"outcome":"stopped","amendment":"Stage M Amendment 3","reports":reports});
            write_json_atomic(&args.output_root.join("amendment2-reverification.json"), &artifact).map_err(|e| anyhow!(e.to_string()))?;
            bail!("Amendment 3 re-verification stopped at {month}");
        }
    }
    for (_, candidate, destination) in &candidates {
        std::fs::rename(candidate, destination)?;
    }
    let artifact = json!({
        "outcome":"completed",
        "amendment":"Stage M Amendment 3",
        "diagnostic":"abs(a-b) <= max(1e-9, 1e-12 * max(abs(a), abs(b)))",
        "reports":reports,
        "promoted_months":candidates.iter().map(|x|x.0).collect::<Vec<_>>()
    });
    write_json_atomic(&args.output_root.join("amendment2-reverification.json"), &artifact).map_err(|e| anyhow!(e.to_string()))?;
    Ok(())
}

fn compare_amendment2_month(month: u64, old: &Value, new: &Value) -> Value {
    let groups = [
        ("parent_totals", "/detail/residual_matrix/cells", Some(["parents", "exposure_s", "log_rate", "residual"].as_slice())),
        ("scores_and_loadings", "/detail/cross_fitted_factor", None),
        ("S_g", "/detail/statistic_1/result/bins", None),
        ("C", "/detail/statistic_2/covariance", None),
        ("C_star", "/detail/statistic_3/covariance", None),
        ("D", "/detail/statistic_2/boundary_contrasts", None),
        ("D_star", "/detail/statistic_3/boundary_contrasts", None),
        ("pair_and_supported_bin_counts", "/detail/statistic_1/result", None),
        ("stratum_assignments", "/detail/statistic_2/cells_used", None),
        ("permutation", "/detail/statistic_1/result/permutation", None),
    ];
    let mut comparisons = Vec::new();
    let mut passed = true;
    for (name, pointer, fields) in groups {
        let mut a = normalize_utc_slow_value(old.pointer(pointer).unwrap_or(&Value::Null).clone());
        let mut b = new.pointer(pointer).unwrap_or(&Value::Null).clone();
        if let Some(fields) = fields {
            retain_cell_fields(&mut a, fields);
            retain_cell_fields(&mut b, fields);
        }
        if name == "scores_and_loadings" {
            strip_amendment1_training_provenance(&mut a);
            strip_amendment1_training_provenance(&mut b);
        }
        let exact = a == b;
        let mut differences = Vec::new();
        collect_differences("$", &a, &b, &mut differences);
        let numeric_differences = differences.iter().filter(|x| x["kind"] == "numeric").count();
        let diagnostic_differences = differences.iter().filter(|x| x["within_diagnostic"] == true).count();
        let structural_differences = differences.iter().filter(|x| x["kind"] == "structural").count();
        let diagnostic = structural_differences == 0 && differences.iter().all(|x| x["kind"] == "numeric" && x["within_diagnostic"] == true);
        // Integer and other non-floating leaves already make `diagnostic`
        // false when they differ. Mixed objects may use the diagnostic for
        // their floating statistics while their counts remain exact.
        let integer_exact_required = name == "permutation";
        let ok = exact || (diagnostic && !integer_exact_required);
        passed &= ok;
        comparisons.push(json!({"statistic":name,"exact":exact,"diagnostic_passed":diagnostic,"numeric_differences":numeric_differences,"structural_differences":structural_differences,"differences_within_diagnostic":diagnostic_differences,"differences":differences,"comparison":if exact{"exact"}else if ok{"frozen_diagnostic"}else{"failed"}}));
    }
    let corrections = month == 202_509 || month == 202_510;
    if corrections {
        passed = comparisons.iter().all(|x| x["structural_differences"] == 0);
    }
    json!({"month":month,"phase":if month==JULY{"July bijection and moved reduction"}else{"exposed-score correction comparison"},"passed":passed,"changes_are_corrections":corrections,"correction_value_count":if corrections {comparisons.iter().map(|x|x["differences"].as_array().map_or(0, Vec::len)).sum::<usize>()} else {0},"comparisons":comparisons})
}

fn strip_amendment1_training_provenance(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.remove("training_dropped");
            map.remove("training_count");
            map.remove("training_retained");
            for child in map.values_mut() { strip_amendment1_training_provenance(child); }
        }
        Value::Array(items) => for child in items { strip_amendment1_training_provenance(child); },
        _ => {}
    }
}

fn compare_amendment3_july(old: &Value, excluded: &Value, signed: &Value, sequence: &Path) -> anyhow::Result<Value> {
    let coordinate_only = compare_amendment2_month(JULY, old, excluded);
    let signed_comparison = compare_amendment2_month(JULY, old, signed);
    let input_gate = verify_july_input_gate(old, excluded, signed, sequence)?;
    let passed = coordinate_only["passed"] == true && input_gate["passed"] == true;
    Ok(json!({
        "month":JULY,
        "phase":"Amendment 3 revised July gate and three-way attribution",
        "passed":passed,
        "input_gate":input_gate,
        "three_way_attribution":{
            "run_1":"original UTC reduction",
            "run_2":"session-local reduction with exact-close seconds excluded",
            "run_3":"signed session-local reduction",
            "run_1_to_run_2":coordinate_only,
            "run_2_to_run_3_attribution":"same deterministic reduction binary and input, differing only by the mechanically enumerated exact-close row filter",
            "attributable_solely_to_enumerated_seconds":input_gate["passed"]
        },
        "complete_old_to_new_comparison":signed_comparison
    }))
}

fn verify_july_input_gate(old: &Value, excluded: &Value, signed: &Value, sequence: &Path) -> anyhow::Result<Value> {
    let old_cells = normalized_cells(old)?;
    let excluded_cells = cells_by_key(excluded)?;
    let signed_cells = cells_by_key(signed)?;
    let rows = std::fs::read_to_string(sequence)?;
    let mut exact_close_parents = BTreeMap::new();
    let mut opens = BTreeMap::<String, u64>::new();
    let parsed = rows.lines().map(serde_json::from_str::<Value>).collect::<Result<Vec<_>, _>>()?;
    for row in &parsed {
        if row["segment_index"] == 0 {
            let date = row["session_date"].as_str().unwrap().to_string();
            let start = row["window_start_ns"].as_u64().unwrap();
            opens.entry(date).and_modify(|x| *x = (*x).min(start)).or_insert(start);
        }
    }
    for row in &parsed {
        let date = row["session_date"].as_str().unwrap();
        let start = row["window_start_ns"].as_u64().unwrap();
        let end = row["window_end_ns"].as_u64().unwrap();
        if end == opens[date] + 23 * 3_600_000_000_000 && start + 1_000_000_000 == end {
            exact_close_parents.insert(date.to_string(), row["parent_count"].as_u64().unwrap());
        }
    }
    let aligned = old_cells.len() == 506 && old_cells.keys().eq(excluded_cells.keys()) && old_cells.keys().eq(signed_cells.keys());
    let mut deltas = Vec::new();
    let mut unauthorized = Vec::new();
    for (key, old_cell) in &old_cells {
        let Some(run2) = excluded_cells.get(key) else { continue };
        let Some(run3) = signed_cells.get(key) else { continue };
        if old_cell != run2 {
            unauthorized.push(json!({"key":{"session_date":key.0,"local_hour":key.1},"reason":"run_2_does_not_reproduce_aligned_run_1","run_1":old_cell,"run_2":run2}));
        }
        if run2 != run3 {
            let parent_delta = run3["parents"].as_u64().unwrap() - run2["parents"].as_u64().unwrap();
            let exposure_before = run2["exposure_s"].as_u64().unwrap();
            let exposure_after = run3["exposure_s"].as_u64().unwrap();
            let recorded = exact_close_parents.get(&key.0).copied();
            let authorized = key.1 == 22 && exposure_before == 2700 && exposure_after == 2701 && recorded == Some(parent_delta);
            let delta = json!({"session_date":key.0,"local_hour":key.1,"exposure_s":{"old":exposure_before,"new":exposure_after,"delta":exposure_after-exposure_before},"parents":{"old":run2["parents"],"new":run3["parents"],"delta":parent_delta},"exact_close_window_recorded_parent_count":recorded,"authorized":authorized});
            if !authorized { unauthorized.push(delta.clone()); }
            deltas.push(delta);
        }
    }
    let passed = aligned && exact_close_parents.len() == 22 && deltas.len() == 22 && unauthorized.is_empty();
    Ok(json!({"passed":passed,"aligned_cell_count":if aligned {506} else {0},"expected_aligned_cell_count":506,"enumerated_exact_close_seconds":exact_close_parents.len(),"differing_input_cells":deltas.len(),"expected_differing_input_cells":22,"deltas":deltas,"unauthorized_differences":unauthorized}))
}

fn normalized_cells(artifact: &Value) -> anyhow::Result<BTreeMap<(String, u64), Value>> {
    let cells = normalize_utc_slow_value(artifact.pointer("/detail/residual_matrix/cells").ok_or_else(|| anyhow!("missing cells"))?.clone());
    cells_by_key_value(&cells)
}

fn cells_by_key(artifact: &Value) -> anyhow::Result<BTreeMap<(String, u64), Value>> {
    cells_by_key_value(artifact.pointer("/detail/residual_matrix/cells").ok_or_else(|| anyhow!("missing cells"))?)
}

fn cells_by_key_value(cells: &Value) -> anyhow::Result<BTreeMap<(String, u64), Value>> {
    cells.as_array().ok_or_else(|| anyhow!("cells are not an array"))?.iter().map(|cell| {
        let key = (cell["session_date"].as_str().ok_or_else(|| anyhow!("cell missing session_date"))?.to_string(), cell["local_hour"].as_u64().ok_or_else(|| anyhow!("cell missing local_hour"))?);
        let mut input = cell.clone();
        input.as_object_mut().unwrap().retain(|k, _| matches!(k.as_str(), "session_date" | "local_hour" | "parents" | "exposure_s"));
        Ok((key, input))
    }).collect()
}

fn normalize_utc_slow_value(mut value: Value) -> Value {
    fn visit(value: &mut Value) {
        match value {
            Value::Object(map) => {
                if let Some(v) = map.remove("hour") {
                    let old = v.as_u64().unwrap();
                    map.insert("local_hour".into(), json!(match old { 22 => 0, 23 => 1, x => x + 2 }));
                }
                if let Some(v) = map.remove("hour20") { map.insert("local_hour_22".into(), v); }
                if let Some(v) = map.remove("D_hour20") { map.insert("D_local_hour_22".into(), v); }
                for v in map.values_mut() { visit(v); }
            }
            Value::Array(items) => {
                for item in items.iter_mut() { visit(item); }
                if items.iter().all(|x| x.get("local_hour").and_then(Value::as_u64).is_some()) {
                    items.sort_by_key(|x| (x.get("session_date").and_then(Value::as_str).unwrap_or("" ).to_string(), x["local_hour"].as_u64().unwrap()));
                }
            }
            _ => {}
        }
    }
    visit(&mut value);
    value
}

fn retain_cell_fields(value: &mut Value, fields: &[&str]) {
    if let Some(items) = value.as_array_mut() {
        for item in items {
            if let Some(map) = item.as_object_mut() {
                map.retain(|key, _| key == "session_date" || key == "local_hour" || fields.contains(&key.as_str()));
            }
        }
    }
}

fn collect_differences(path: &str, a: &Value, b: &Value, out: &mut Vec<Value>) {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) if x.is_f64() || y.is_f64() => {
            if x == y { return; }
            let (x, y) = (x.as_f64().unwrap(), y.as_f64().unwrap());
            let ok = (x-y).abs() <= 1e-9_f64.max(1e-12 * x.abs().max(y.abs()));
            out.push(json!({"path":path,"kind":"numeric","old":x,"new":y,"absolute_difference":(x-y).abs(),"within_diagnostic":ok}));
        }
        (Value::Array(x), Value::Array(y)) => {
            if x.len() != y.len() { out.push(json!({"path":path,"kind":"structural","old_length":x.len(),"new_length":y.len()})); }
            for (i, (x, y)) in x.iter().zip(y).enumerate() { collect_differences(&format!("{path}[{i}]"), x, y, out); }
        }
        (Value::Object(x), Value::Object(y)) => {
            let keys = x.keys().chain(y.keys()).collect::<BTreeSet<_>>();
            for key in keys {
                match (x.get(key), y.get(key)) {
                    (Some(x), Some(y)) => collect_differences(&format!("{path}.{key}"), x, y, out),
                    (old, new) => out.push(json!({"path":format!("{path}.{key}"),"kind":"structural","old":old,"new":new})),
                }
            }
        }
        _ if a != b => out.push(json!({"path":path,"kind":"value","old":a,"new":b})),
        _ => {}
    }
}

fn verify_winter_month(month: u64, artifact: &Value) -> Value {
    let cells = artifact.pointer("/detail/residual_matrix/cells").and_then(Value::as_array);
    let scores = artifact.pointer("/detail/cross_fitted_factor/scores").and_then(Value::as_array);
    let complete = cells.is_some_and(|xs| {
        let mut per = BTreeMap::<&str, usize>::new();
        for x in xs { *per.entry(x["session_date"].as_str().unwrap_or("")).or_default() += 1; }
        !per.is_empty() && per.values().all(|n| *n == 23)
    });
    let scored = scores.is_some_and(|xs| !xs.is_empty());
    json!({"month":month,"phase":"winter completeness and scoring","passed":complete&&scored,"complete_local_coordinate_cells":complete,"score_count":scores.map_or(0,Vec::len),"refusal_rules":"Amendment 1 complete-case training with floor 12"})
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
