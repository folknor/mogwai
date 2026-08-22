// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Stage M Tier 2's append-only candidate ledger and executable hurdle.
//!
//! This module intentionally contains no candidate. A specification must be
//! appended by `commit` in one invocation and can only be evaluated by a later
//! invocation. That process boundary is the anti-peek boundary in the frozen
//! search-order rule.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow, bail};
use clap::{Args, Subcommand};
use mogwai_data::{TickEvent, TickSource};
use mogwai_lab::aggregate::bootstrap::STAGE_M_SEED;
use mogwai_lab::kernel::{splitmix64, tuple_mix};
use mogwai_protocol::AggressorSide;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const ALT_SEED: u64 = 3_958_267_140_192_837_465;
const INCUMBENT_SEED: u64 = 6_172_038_459_284_617_530;
const ALT_RUNS: usize = 200;
const DESIGN_LEVEL: f64 = 0.90;

#[derive(Args)]
pub struct Tier2Args {
    #[command(subcommand)]
    command: Tier2Command,
}

#[derive(Subcommand)]
enum Tier2Command {
    /// Append the adjudicated, bounded C1/C2 to C3 continuation record.
    Continue(ArtifactArgs),
    /// Append a complete unevaluated candidate specification.
    Commit(CommitArgs),
    /// Evaluate one previously committed candidate.
    Evaluate(EvaluateArgs),
    /// Close the bounded search after C3 has been evaluated.
    Close(ArtifactArgs),
    /// Apply the frozen mechanical designation rule.
    Designate(DesignateArgs),
    /// Compute and record the once-only EXCESS baseline W.
    Baseline(CommonArgs),
    /// Materialize the 24 shipped-generator control fields.
    Controls(ControlsArgs),
}

#[derive(Args)]
struct ArtifactArgs {
    #[arg(long, default_value = "analysis/out/stage-m/tier2.jsonl")]
    artifact: PathBuf,
}

#[derive(Args)]
struct CommonArgs {
    #[arg(
        long,
        default_value = "analysis/out/stage-m/manifest-slow-geometry.json"
    )]
    manifest: PathBuf,
    #[arg(long, default_value = "analysis/out/stage-m/tier2.jsonl")]
    artifact: PathBuf,
}

#[derive(Args)]
struct CommitArgs {
    #[arg(long)]
    specification: PathBuf,
    #[arg(long, default_value = "analysis/out/stage-m/tier2.jsonl")]
    artifact: PathBuf,
}

#[derive(Args)]
struct EvaluateArgs {
    #[arg(long)]
    id: String,
    #[arg(
        long,
        default_value = "analysis/out/stage-m/manifest-slow-geometry.json"
    )]
    manifest: PathBuf,
    #[arg(
        long,
        default_value = "analysis/out/stage-m/tier2-controls-manifest.json"
    )]
    controls: PathBuf,
    #[arg(long, default_value = "analysis/out/stage-m/tier2.jsonl")]
    artifact: PathBuf,
}

#[derive(Args)]
struct DesignateArgs {
    #[arg(long, default_value = "analysis/out/stage-m/tier2.jsonl")]
    artifact: PathBuf,
}

#[derive(Args)]
struct ControlsArgs {
    #[arg(
        long,
        default_value = "analysis/out/stage-m/manifest-slow-geometry.json"
    )]
    design_manifest: PathBuf,
    #[arg(long, default_value = "analysis/out/stage-m/tier2-controls")]
    output: PathBuf,
    #[arg(
        long,
        default_value = "analysis/out/stage-m/tier2-controls-manifest.json"
    )]
    manifest: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Candidate {
    id: String,
    coordinates: Vec<Coordinate>,
    joint: JointRule,
    refusals: RefusalSemantics,
    thin_months: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Coordinate {
    name: String,
    statistic: Statistic,
}

/// An executable, mechanism-neutral vocabulary over exactly the frozen field
/// and its cross-fitted scores. New vocabulary is an implementation change,
/// never an implicit interpretation of an already committed specification.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Statistic {
    ResidualVariance,
    SessionMeanVariance,
    SessionMeanOneDayCovariance,
    CrossHourCoherence,
    SessionMeanVarianceRatio,
    PermutationStandardizedLeadingCovarianceShare,
    ScoreVariance,
    ScoreGapCovariance {
        minimum_days: u64,
        maximum_days: Option<u64>,
    },
    LocalHourResidualVariance {
        local_hour: u64,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum JointRule {
    /// One Hotelling predictive statistic. The 90 percent cutoff is computed
    /// from its exact finite-sample F distribution, not supplied by a user.
    HotellingPredictive { ridge: f64 },
    /// Bonferroni Student predictive maximum at the declared joint level.
    StudentMax,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RefusalSemantics {
    missing_cells: Disposition,
    score_refusal: Disposition,
    nonfinite_statistic: Disposition,
    singular_predictive_fit: Disposition,
    thin_month: Disposition,
    finer_than_session_hour_input: Disposition,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Disposition {
    Include,
    ExcludeWithRecord,
    RejectMonth,
    RefuseCandidate,
}

#[derive(Clone)]
struct Cell {
    date: String,
    hour: u64,
    residual: f64,
}

#[derive(Clone)]
struct Month {
    key: u64,
    cells: Vec<Cell>,
    scores: BTreeMap<String, f64>,
    thin: bool,
}

#[derive(Clone, Serialize)]
struct Verdict {
    rejected: bool,
    statistic: f64,
    critical_value: f64,
}

pub fn run(args: Tier2Args) -> anyhow::Result<()> {
    match args.command {
        Tier2Command::Continue(x) => continuation(&x),
        Tier2Command::Commit(x) => commit(&x),
        Tier2Command::Evaluate(x) => evaluate(&x),
        Tier2Command::Close(x) => close_search(&x),
        Tier2Command::Designate(x) => designate(&x),
        Tier2Command::Baseline(x) => baseline(&x),
        Tier2Command::Controls(x) => controls(&x),
    }
}

fn continuation(args: &ArtifactArgs) -> anyhow::Result<()> {
    let records = ledger(&args.artifact)?;
    if records.iter().any(|x| x["record"] == "continuation") {
        bail!("the Tier 2 continuation was already recorded");
    }
    for (id, order) in [("C1", 1), ("C2", 2)] {
        if !records.iter().any(|x| {
            x["record"] == "candidate_evaluation"
                && x["candidate_id"] == id
                && x["search_order"] == order
        }) {
            bail!("continuation requires the preserved {id} evaluation at search order {order}");
        }
    }
    if !records.iter().any(|x| x["record"] == "designation") {
        bail!("continuation requires the preserved premature designation");
    }
    append(
        &args.artifact,
        &json!({
            "record":"continuation",
            "revision":4,
            "adjudication":"reviewer session 019ff6c9",
            "preserved_results":[{"candidate_id":"C1","search_order":1},{"candidate_id":"C2","search_order":2}],
            "preserved_premature_designation":true,
            "supersedes_terminal_interpretation_of_prior_designation":true,
            "reason":"designation was dispatched before search closure",
            "next_candidate":{"candidate_id":"C3","search_order":3,"committable":true},
            "bounded_search":{"final_candidate":"C3","no_c4":true},
            "search_closure":"append explicit search_closed after C3 evaluation whatever its result",
            "designation":"run over the complete candidate history after search closure"
        }),
    )?;
    println!("Tier 2 bounded continuation appended");
    Ok(())
}

fn close_search(args: &ArtifactArgs) -> anyhow::Result<()> {
    let records = ledger(&args.artifact)?;
    if records.iter().any(|x| x["record"] == "search_closed") {
        bail!("Tier 2 search is already closed");
    }
    if !records.iter().any(|x| x["record"] == "continuation") {
        bail!("search closure requires the reviewed continuation record");
    }
    if !records.iter().any(|x| {
        x["record"] == "candidate_evaluation" && x["candidate_id"] == "C3" && x["search_order"] == 3
    }) {
        bail!("search closure requires the C3 evaluation at search order 3");
    }
    if records
        .iter()
        .filter(|x| x["record"] == "candidate_committed")
        .count()
        != 3
    {
        bail!("bounded search closure requires exactly C1, C2, and C3");
    }
    append(
        &args.artifact,
        &json!({
            "record":"search_closed",
            "after_candidate":"C3",
            "last_search_order":3,
            "no_further_candidate":"no C4 under the revision 4 adjudicated boundary",
            "designation_scope":"complete candidate history"
        }),
    )?;
    println!("Tier 2 search closed after C3");
    Ok(())
}

fn commit(args: &CommitArgs) -> anyhow::Result<()> {
    let bytes = std::fs::read(&args.specification)?;
    let spec: Candidate =
        serde_json::from_slice(&bytes).context("parsing complete candidate specification")?;
    validate_spec(&spec)?;
    let records = ledger(&args.artifact)?;
    if records.iter().any(|x| x["candidate_id"] == spec.id) {
        bail!("candidate id {} is already committed", spec.id);
    }
    append(
        &args.artifact,
        &json!({
            "record":"candidate_committed", "candidate_id":spec.id,
            "search_order":records.iter().filter(|x|x["record"]=="candidate_committed").count()+1,
            "specification":spec, "specification_sha256":mogwai_lab::ledger::sha256_file(&args.specification).map_err(|e|anyhow!(e.to_string()))?,
            "state":"committed_before_evaluation"
        }),
    )?;
    println!("Tier 2 candidate committed; evaluation requires a separate invocation");
    Ok(())
}

fn validate_spec(x: &Candidate) -> anyhow::Result<()> {
    if x.id.trim().is_empty() {
        bail!("candidate id is empty");
    }
    if x.coordinates.is_empty() || x.coordinates.len() > 4 {
        bail!("H1: coordinate count must be 1..=4");
    }
    let names = x
        .coordinates
        .iter()
        .map(|c| c.name.as_str())
        .collect::<BTreeSet<_>>();
    if names.len() != x.coordinates.len() || names.iter().any(|x| x.trim().is_empty()) {
        bail!("coordinate names must be nonempty and unique");
    }
    if x.thin_months.trim().is_empty() {
        bail!("H6: thin-month fitting treatment is unspecified");
    }
    if let JointRule::HotellingPredictive { ridge } = x.joint
        && (!ridge.is_finite() || ridge < 0.0)
    {
        bail!("H2: Hotelling ridge must be finite and nonnegative");
    }
    for c in &x.coordinates {
        match c.statistic {
            Statistic::LocalHourResidualVariance { local_hour } if local_hour > 22 => {
                bail!("session-local hour is outside 0..22")
            }
            Statistic::ScoreGapCovariance {
                minimum_days,
                maximum_days: Some(maximum_days),
            } if minimum_days > maximum_days => bail!("gap bounds are reversed"),
            _ => {}
        }
    }
    Ok(())
}

fn baseline(args: &CommonArgs) -> anyhow::Result<()> {
    if ledger(&args.artifact)?
        .iter()
        .any(|x| x["record"] == "excess_baseline")
    {
        bail!("the once-only EXCESS baseline is already recorded");
    }
    let months = load_manifest(&args.manifest)?;
    let (w, table) = excess_baseline(&months);
    let contributing = table
        .iter()
        .filter(|x| x["W_m"].is_number() && x["month"] != 202_607)
        .count();
    let outcome = if contributing >= 4 {
        "completed"
    } else {
        "excess_baseline_unavailable"
    };
    append(
        &args.artifact,
        &json!({"record":"excess_baseline","outcome":outcome,"W":w,"contributing_new_design_months":contributing,"months":table,
        "definition":"unweighted mean of population variances of complete-session unweighted residual means; new-design months only"}),
    )?;
    println!("Tier 2 EXCESS baseline {outcome}");
    Ok(())
}

fn evaluate(args: &EvaluateArgs) -> anyhow::Result<()> {
    let records = ledger(&args.artifact)?;
    if records
        .iter()
        .any(|x| x["record"] == "candidate_evaluation" && x["candidate_id"] == args.id)
    {
        bail!("candidate {} was already evaluated", args.id);
    }
    let committed = records
        .iter()
        .find(|x| x["record"] == "candidate_committed" && x["candidate_id"] == args.id)
        .ok_or_else(|| anyhow!("candidate {} has no prior committed specification", args.id))?;
    let spec: Candidate = serde_json::from_value(committed["specification"].clone())?;
    validate_spec(&spec)?;
    let population = load_manifest(&args.manifest)?;
    let months = population
        .iter()
        .filter(|m| m.key != 202_607)
        .cloned()
        .collect::<Vec<_>>();
    if months.len() != 8 {
        bail!(
            "H3/H5 require exactly eight new-design months; found {}",
            months.len()
        );
    }
    let (w, w_table) = excess_baseline(&population);
    let w = w.ok_or_else(|| anyhow!("excess_baseline_unavailable"))?;
    if w_table
        .iter()
        .filter(|x| x["W_m"].is_number() && x["month"] != 202_607)
        .count()
        < 4
    {
        bail!("excess_baseline_unavailable");
    }

    let projected = project_all(&spec, &months)?;
    let mut h3 = Vec::new();
    for held in 0..months.len() {
        let train = projected
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != held)
            .map(|(_, x)| x.clone())
            .collect::<Vec<_>>();
        let verdict = joint(&spec.joint, &train, &projected[held])?;
        h3.push(json!({"month":months[held].key,"thin":months[held].thin,"verdict":verdict}));
    }
    let h3_out = h3
        .iter()
        .filter(|x| x["verdict"]["rejected"] == true)
        .count();

    let controls = load_manifest(&args.controls)?;
    if controls.len() != 24 {
        bail!(
            "H4 requires exactly 24 incumbent controls; found {}",
            controls.len()
        );
    }
    let fit_all = projected.clone();
    let mut h4 = Vec::new();
    let mut refusals = Vec::new();
    for control in &controls {
        match project(&spec, control).and_then(|value| joint(&spec.joint, &fit_all, &value)) {
            Ok(verdict) => h4.push(json!({"walk":control.key,"verdict":verdict})),
            Err(error) => {
                let refusal = json!({"hurdle":"H4","walk":control.key,"reason":error.to_string()});
                refusals.push(refusal.clone());
                h4.push(json!({"walk":control.key,"refusal":refusal}));
            }
        }
    }
    let h4_count = h4
        .iter()
        .filter(|x| x["verdict"]["rejected"] == true)
        .count();

    let (h5_no_slow, no_slow_detail, no_slow_refusals) = h5(&spec, &months, &projected, 1, w)?;
    let (h5_excess, excess_detail, excess_refusals) = h5(&spec, &months, &projected, 2, w)?;
    refusals.extend(no_slow_refusals);
    refusals.extend(excess_refusals);
    let h1 = spec.coordinates.len() <= 4;
    let h2 = true;
    let h3_pass = h3_out <= 1;
    let h4_pass = h4_count >= 23;
    let h5_pass = h5_no_slow >= 160 && h5_excess >= 160;
    let h6 = refusals.is_empty();
    let admissible = h1 && h2 && h3_pass && h4_pass && h5_pass && h6;
    append(
        &args.artifact,
        &json!({
            "record":"candidate_evaluation","candidate_id":args.id,"search_order":committed["search_order"],"admissible":admissible,
            "hurdle":{"H1":{"passed":h1,"coordinates":spec.coordinates.len()},"H2":{"passed":h2,"joint_rule":spec.joint,"level":DESIGN_LEVEL},
            "H3":{"passed":h3_pass,"outside_count":h3_out,"months":h3},"H4":{"passed":h4_pass,"rejection_count":h4_count,"controls":h4},
            "H5":{"passed":h5_pass,"required_each":160,"no_slow":{"rejection_count":h5_no_slow,"simulations":no_slow_detail},"excess":{"rejection_count":h5_excess,"W":w,"W_months":w_table,"simulations":excess_detail}},
            "H6":{"passed":h6,"refusal_count":refusals.len(),"refusals":refusals,"refusal_semantics":spec.refusals,"thin_month_treatment":spec.thin_months}},
            "stage_f_handoff":if admissible{"eligible_for_mechanical_designation"}else{"not_eligible"}
        }),
    )?;
    println!(
        "Tier 2 candidate {} evaluation appended: admissible={admissible}",
        args.id
    );
    Ok(())
}

fn h5(
    spec: &Candidate,
    months: &[Month],
    projected: &[Vec<f64>],
    alternative: u64,
    w: f64,
) -> anyhow::Result<(usize, Vec<Value>, Vec<Value>)> {
    let mut rejected = 0;
    let mut detail = Vec::with_capacity(ALT_RUNS);
    let mut refusals = Vec::new();
    for simulation in 1..=ALT_RUNS {
        let held = (simulation - 1) % months.len();
        let mut changed = months[held].clone();
        let excluded = if alternative == 1 {
            no_slow(&mut changed, simulation as u64);
            0
        } else {
            excess(&mut changed, simulation as u64, w)?
        };
        // Scores are a derived input. Recompute them under exactly the same
        // complete-case LOMO session folds after perturbing the residual field.
        changed.scores = crate::slow_geometry::tier2_scores(
            changed
                .cells
                .iter()
                .map(|c| (c.date.clone(), c.hour, c.residual))
                .collect(),
        );
        let train = projected
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != held)
            .map(|(_, x)| x.clone())
            .collect::<Vec<_>>();
        let common = json!({"simulation":simulation,"held_month":months[held].key,"seed":tuple_mix(ALT_SEED,&[alternative,simulation as u64]),"incomplete_sessions_unperturbed":excluded});
        match project(spec, &changed).and_then(|value| joint(&spec.joint, &train, &value)) {
            Ok(verdict) => {
                rejected += usize::from(verdict.rejected);
                let mut row = common;
                row["verdict"] = serde_json::to_value(verdict)?;
                detail.push(row);
            }
            Err(error) => {
                let refusal = json!({"hurdle":"H5","alternative":alternative,"simulation":simulation,"held_month":months[held].key,"reason":error.to_string()});
                refusals.push(refusal.clone());
                let mut row = common;
                row["refusal"] = refusal;
                detail.push(row);
            }
        }
    }
    Ok((rejected, detail, refusals))
}

fn no_slow(month: &mut Month, simulation: u64) {
    for hour in 0..=22 {
        let mut indices = month
            .cells
            .iter()
            .enumerate()
            .filter(|(_, c)| c.hour == hour)
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        indices.sort_by(|a, b| month.cells[*a].date.cmp(&month.cells[*b].date));
        let mut values = indices
            .iter()
            .map(|i| month.cells[*i].residual)
            .collect::<Vec<_>>();
        let mut state = tuple_mix(ALT_SEED, &[1, simulation, hour]);
        for i in (1..values.len()).rev() {
            state = splitmix64(state);
            values.swap(i, (state % (i as u64 + 1)) as usize);
        }
        for (i, v) in indices.into_iter().zip(values) {
            month.cells[i].residual = v;
        }
    }
}

fn excess(month: &mut Month, simulation: u64, w: f64) -> anyhow::Result<usize> {
    let complete = complete_dates(&month.cells);
    let dates = month
        .cells
        .iter()
        .map(|c| c.date.clone())
        .collect::<BTreeSet<_>>();
    for date in &complete {
        let ymd: u64 = date.replace('-', "").parse()?;
        let mut state = tuple_mix(ALT_SEED, &[2, simulation, month.key, ymd]);
        state = splitmix64(state);
        let mut u1 = (state >> 11) as f64 * 2f64.powi(-53);
        if u1 == 0.0 {
            u1 = 2f64.powi(-53);
        }
        state = splitmix64(state);
        let u2 = (state >> 11) as f64 * 2f64.powi(-53);
        let g =
            (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos() * (4.0 * w).sqrt();
        for cell in month.cells.iter_mut().filter(|c| &c.date == date) {
            cell.residual += g;
        }
    }
    Ok(dates.len() - complete.len())
}

fn excess_baseline(months: &[Month]) -> (Option<f64>, Vec<Value>) {
    let mut table = Vec::new();
    let mut values = Vec::new();
    for month in months {
        let complete = complete_dates(&month.cells);
        let all = month
            .cells
            .iter()
            .map(|c| c.date.clone())
            .collect::<BTreeSet<_>>();
        let us = complete
            .iter()
            .map(|d| {
                mean(
                    &month
                        .cells
                        .iter()
                        .filter(|c| &c.date == d)
                        .map(|c| c.residual)
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>();
        let wm = (us.len() >= 2).then(|| variance(&us));
        if month.key != 202_607
            && let Some(x) = wm
        {
            values.push(x);
        }
        table.push(json!({"month":month.key,"eligible_sessions":complete,"excluded_sessions":all.difference(&complete).collect::<Vec<_>>(),"W_m":wm}));
    }
    ((!values.is_empty()).then(|| mean(&values)), table)
}

fn complete_dates(cells: &[Cell]) -> BTreeSet<String> {
    let mut hours: BTreeMap<String, BTreeSet<u64>> = BTreeMap::new();
    for c in cells {
        hours.entry(c.date.clone()).or_default().insert(c.hour);
    }
    hours
        .into_iter()
        .filter(|(_, h)| h.len() == 23 && (0..=22).all(|x| h.contains(&x)))
        .map(|(d, _)| d)
        .collect()
}

fn project_all(spec: &Candidate, months: &[Month]) -> anyhow::Result<Vec<Vec<f64>>> {
    months
        .iter()
        .map(|m| project(spec, m).with_context(|| format!("projecting month/walk {}", m.key)))
        .collect()
}
fn project(spec: &Candidate, m: &Month) -> anyhow::Result<Vec<f64>> {
    let needs_c3 = spec.coordinates.iter().any(|coordinate| {
        matches!(
            coordinate.statistic,
            Statistic::SessionMeanVarianceRatio
                | Statistic::PermutationStandardizedLeadingCovarianceShare
        )
    });
    let c3 = needs_c3.then(|| c3_coordinates(m)).transpose()?;
    spec.coordinates
        .iter()
        .map(|c| {
            let x = match c.statistic {
                Statistic::ResidualVariance => {
                    variance(&m.cells.iter().map(|x| x.residual).collect::<Vec<_>>())
                }
                Statistic::SessionMeanVariance => session_means(m).and_then(|x| {
                    (x.len() >= 2)
                        .then(|| variance(&x.values().copied().collect::<Vec<_>>()))
                        .ok_or_else(|| anyhow!("fewer than 2 eligible sessions"))
                })?,
                Statistic::SessionMeanOneDayCovariance => one_day_covariance(m)?,
                Statistic::CrossHourCoherence => cross_hour_result(&m.cells)?,
                Statistic::SessionMeanVarianceRatio => {
                    c3.expect("C3 coordinates were precomputed").0
                }
                Statistic::PermutationStandardizedLeadingCovarianceShare => {
                    c3.expect("C3 coordinates were precomputed").1
                }
                Statistic::ScoreVariance => (m.scores.len() >= 8)
                    .then(|| variance(&m.scores.values().copied().collect::<Vec<_>>()))
                    .ok_or_else(|| anyhow!("fewer than 8 scored sessions"))?,
                Statistic::ScoreGapCovariance {
                    minimum_days,
                    maximum_days,
                } => score_gap(m, minimum_days, maximum_days),
                Statistic::LocalHourResidualVariance { local_hour } => variance(
                    &m.cells
                        .iter()
                        .filter(|x| x.hour == local_hour)
                        .map(|x| x.residual)
                        .collect::<Vec<_>>(),
                ),
            };
            if x.is_finite() {
                Ok(x)
            } else {
                bail!("H6 nonfinite statistic {} in month/walk {}", c.name, m.key)
            }
        })
        .collect()
}
fn session_means(m: &Month) -> anyhow::Result<BTreeMap<String, f64>> {
    Ok(complete_dates(&m.cells)
        .into_iter()
        .map(|d| {
            let u = mean(
                &m.cells
                    .iter()
                    .filter(|c| c.date == d)
                    .map(|c| c.residual)
                    .collect::<Vec<_>>(),
            );
            (d, u)
        })
        .collect())
}
fn one_day_covariance(m: &Month) -> anyhow::Result<f64> {
    let u = session_means(m)?;
    let mu = mean(&u.values().copied().collect::<Vec<_>>());
    let x = u.iter().collect::<Vec<_>>();
    let pairs = (0..x.len().saturating_sub(1))
        .filter(|i| civil_day(x[i + 1].0) - civil_day(x[*i].0) == 1)
        .map(|i| (*x[i].1 - mu) * (*x[i + 1].1 - mu))
        .collect::<Vec<_>>();
    if pairs.len() < 8 {
        bail!("fewer than 8 adjacent-calendar-date pairs")
    }
    Ok(mean(&pairs))
}
fn cross_hour_result(cells: &[Cell]) -> anyhow::Result<f64> {
    let complete = complete_dates(cells);
    if complete.len() < 2 {
        bail!("fewer than 2 complete sessions")
    };
    let rows = complete
        .iter()
        .map(|d| {
            (0..23)
                .map(|h| {
                    cells
                        .iter()
                        .find(|c| &c.date == d && c.hour == h)
                        .unwrap()
                        .residual
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let means = (0..23)
        .map(|h| mean(&rows.iter().map(|r| r[h]).collect::<Vec<_>>()))
        .collect::<Vec<_>>();
    let sds = (0..23)
        .map(|h| variance(&rows.iter().map(|r| r[h]).collect::<Vec<_>>()).sqrt())
        .collect::<Vec<_>>();
    if let Some(h) = sds.iter().position(|x| *x == 0.0 || !x.is_finite()) {
        bail!("zero or nonfinite variance in hour column {h}")
    };
    let corr = (0..23)
        .map(|i| {
            (0..23)
                .map(|j| {
                    mean(
                        &rows
                            .iter()
                            .map(|r| ((r[i] - means[i]) / sds[i]) * ((r[j] - means[j]) / sds[j]))
                            .collect::<Vec<_>>(),
                    )
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let eigen = crate::slow_geometry::tier2_leading_eigenvalue(corr)
        .ok_or_else(|| anyhow!("slow-geometry Jacobi eigensolver failure"))?;
    Ok(eigen / 23.0)
}

fn c3_coordinates(month: &Month) -> anyhow::Result<(f64, f64)> {
    let dates = complete_dates(&month.cells);
    if dates.len() < 2 {
        bail!("fewer than 2 complete sessions");
    }
    let rows = dates
        .iter()
        .map(|date| {
            (0..23)
                .map(|hour| {
                    month
                        .cells
                        .iter()
                        .find(|cell| &cell.date == date && cell.hour == hour)
                        .expect("complete session has every local hour")
                        .residual
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let covariance = population_covariance(&rows);
    let trace = matrix_trace(&covariance);
    if trace == 0.0 || !trace.is_finite() {
        bail!("zero or nonfinite covariance trace");
    }
    let ones_s_ones = covariance.iter().flatten().sum::<f64>();
    let n1 = ones_s_ones / trace;
    let observed_share = leading_share(covariance, trace)?;
    let mut permutation_shares = Vec::with_capacity(200);
    for permutation in 1..=200_u64 {
        let mut permuted = rows.clone();
        for hour in 0..23_usize {
            let mut values = rows.iter().map(|row| row[hour]).collect::<Vec<_>>();
            let mut state = tuple_mix(STAGE_M_SEED, &[3, month.key, permutation, hour as u64]);
            for index in (1..values.len()).rev() {
                state = splitmix64(state);
                values.swap(index, (state % (index as u64 + 1)) as usize);
            }
            for (row, value) in permuted.iter_mut().zip(values) {
                row[hour] = value;
            }
        }
        let covariance = population_covariance(&permuted);
        let trace = matrix_trace(&covariance);
        if trace == 0.0 || !trace.is_finite() {
            bail!("zero or nonfinite permuted covariance trace");
        }
        permutation_shares.push(leading_share(covariance, trace)?);
    }
    let baseline_mean = mean(&permutation_shares);
    let baseline_sd = (permutation_shares
        .iter()
        .map(|value| (value - baseline_mean).powi(2))
        .sum::<f64>()
        / 199.0)
        .sqrt();
    if baseline_sd == 0.0 || !baseline_sd.is_finite() {
        bail!("zero or nonfinite permutation baseline standard deviation");
    }
    let n2 = (observed_share - baseline_mean) / baseline_sd;
    if !n1.is_finite() || !n2.is_finite() {
        bail!("nonfinite C3 coordinate");
    }
    Ok((n1, n2))
}

fn population_covariance(rows: &[Vec<f64>]) -> Vec<Vec<f64>> {
    let means = (0..23)
        .map(|hour| mean(&rows.iter().map(|row| row[hour]).collect::<Vec<_>>()))
        .collect::<Vec<_>>();
    (0..23)
        .map(|i| {
            (0..23)
                .map(|j| {
                    rows.iter()
                        .map(|row| (row[i] - means[i]) * (row[j] - means[j]))
                        .sum::<f64>()
                        / rows.len() as f64
                })
                .collect()
        })
        .collect()
}

fn matrix_trace(matrix: &[Vec<f64>]) -> f64 {
    matrix
        .iter()
        .enumerate()
        .map(|(index, row)| row[index])
        .sum()
}

fn leading_share(covariance: Vec<Vec<f64>>, trace: f64) -> anyhow::Result<f64> {
    let leading = crate::slow_geometry::tier2_leading_eigenvalue(covariance)
        .ok_or_else(|| anyhow!("slow-geometry Jacobi eigensolver failure"))?;
    let share = leading / trace;
    if !share.is_finite() {
        bail!("nonfinite trace-normalized leading eigenvalue share");
    }
    Ok(share)
}
fn score_gap(m: &Month, min: u64, max: Option<u64>) -> f64 {
    let x = m.scores.iter().collect::<Vec<_>>();
    let mu = mean(&x.iter().map(|(_, v)| **v).collect::<Vec<_>>());
    let mut out = Vec::new();
    for i in 0..x.len() {
        for j in i + 1..x.len() {
            let gap = (civil_day(x[j].0) - civil_day(x[i].0)).unsigned_abs();
            if gap >= min && max.is_none_or(|z| gap <= z) {
                out.push((*x[i].1 - mu) * (*x[j].1 - mu));
            }
        }
    }
    mean(&out)
}

fn joint(rule: &JointRule, train: &[Vec<f64>], held: &[f64]) -> anyhow::Result<Verdict> {
    if matches!(rule, JointRule::StudentMax) {
        return student_max(train, held);
    }
    let JointRule::HotellingPredictive { ridge } = *rule else {
        unreachable!()
    };
    let n = train.len();
    let d = held.len();
    if n <= d {
        bail!("H6 singular predictive fit: n={n}, d={d}")
    }
    let mu = (0..d)
        .map(|j| mean(&train.iter().map(|x| x[j]).collect::<Vec<_>>()))
        .collect::<Vec<_>>();
    let mut cov = vec![vec![0.0; d]; d];
    for i in 0..d {
        for j in 0..d {
            cov[i][j] = train
                .iter()
                .map(|x| (x[i] - mu[i]) * (x[j] - mu[j]))
                .sum::<f64>()
                / (n - 1) as f64;
        }
        cov[i][i] += ridge;
    }
    let inv = invert(cov).ok_or_else(|| anyhow!("H6 singular predictive fit"))?;
    let delta = held.iter().zip(&mu).map(|(a, b)| a - b).collect::<Vec<_>>();
    let statistic = (0..d)
        .map(|i| (0..d).map(|j| delta[i] * inv[i][j] * delta[j]).sum::<f64>())
        .sum();
    let f = f_quantile_90(d, n - d);
    let critical = d as f64 * (n + 1) as f64 * (n - 1) as f64 / (n as f64 * (n - d) as f64) * f;
    Ok(Verdict {
        rejected: statistic > critical,
        statistic,
        critical_value: critical,
    })
}

fn student_max(train: &[Vec<f64>], held: &[f64]) -> anyhow::Result<Verdict> {
    let n = train.len();
    let k = held.len();
    if n < 2 || k == 0 || train.iter().any(|x| x.len() != k) {
        bail!("H6 invalid Student-max predictive fit")
    };
    let mut statistic: f64 = 0.0;
    for j in 0..k {
        let x = train.iter().map(|r| r[j]).collect::<Vec<_>>();
        if x.iter()
            .chain(std::iter::once(&held[j]))
            .any(|v| !v.is_finite())
        {
            bail!("H6 nonfinite Student-max coordinate {j}")
        }
        let m = mean(&x);
        let s = (x.iter().map(|v| (v - m).powi(2)).sum::<f64>() / (n - 1) as f64).sqrt();
        if s == 0.0 || !s.is_finite() {
            bail!("H6 zero or nonfinite reference standard deviation for coordinate {j}")
        }
        statistic = statistic.max(((held[j] - m) / (s * (1.0 + 1.0 / n as f64).sqrt())).abs());
    }
    let p = 1.0 - 0.10 / (2 * k) as f64;
    let critical = t_quantile(n - 1, p);
    Ok(Verdict {
        rejected: statistic > critical,
        statistic,
        critical_value: critical,
    })
}
fn t_quantile(df: usize, p: f64) -> f64 {
    let mut lo = -64.0;
    let mut hi = 64.0;
    for _ in 0..120 {
        let mid = (lo + hi) / 2.0;
        if t_cdf(mid, df) < p {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    (lo + hi) / 2.0
}
fn t_cdf(t: f64, df: usize) -> f64 {
    let v = df as f64;
    let x = v / (v + t * t);
    let tail = 0.5 * beta_i(x, v / 2.0, 0.5);
    if t >= 0.0 { 1.0 - tail } else { tail }
}

// Regularized incomplete beta and bisection make the finite-sample 90 percent
// predictive cutoff executable without a platform statistics dependency.
fn f_quantile_90(d1: usize, d2: usize) -> f64 {
    let (a, b) = (d1 as f64 / 2.0, d2 as f64 / 2.0);
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..100 {
        let x = (lo + hi) / 2.0;
        if beta_i(x, a, b) < 0.9 {
            lo = x;
        } else {
            hi = x;
        }
    }
    let x = (lo + hi) / 2.0;
    d2 as f64 * x / (d1 as f64 * (1.0 - x))
}
fn beta_i(x: f64, a: f64, b: f64) -> f64 {
    let bt = if x == 0.0 || x == 1.0 {
        0.0
    } else {
        (ln_gamma(a + b) - ln_gamma(a) - ln_gamma(b) + a * x.ln() + b * (-x).ln_1p()).exp()
    };
    if x < (a + 1.0) / (a + b + 2.0) {
        bt * beta_cf(a, b, x) / a
    } else {
        1.0 - bt * beta_cf(b, a, 1.0 - x) / b
    }
}
fn beta_cf(a: f64, b: f64, x: f64) -> f64 {
    let qab = a + b;
    let qap = a + 1.0;
    let qam = a - 1.0;
    let mut c = 1.0;
    let mut d = 1.0 - qab * x / qap;
    if d.abs() < 3e-14 {
        d = 3e-14;
    }
    d = 1.0 / d;
    let mut h = d;
    for m in 1..=200 {
        let m2 = 2 * m;
        let mut aa = m as f64 * (b - m as f64) * x / ((qam + m2 as f64) * (a + m2 as f64));
        d = 1.0 + aa * d;
        if d.abs() < 3e-14 {
            d = 3e-14;
        }
        c = 1.0 + aa / c;
        if c.abs() < 3e-14 {
            c = 3e-14;
        }
        d = 1.0 / d;
        h *= d * c;
        aa = -(a + m as f64) * (qab + m as f64) * x / ((a + m2 as f64) * (qap + m2 as f64));
        d = 1.0 + aa * d;
        if d.abs() < 3e-14 {
            d = 3e-14;
        }
        c = 1.0 + aa / c;
        if c.abs() < 3e-14 {
            c = 3e-14;
        }
        d = 1.0 / d;
        let del = d * c;
        h *= del;
        if (del - 1.0).abs() < 3e-14 {
            break;
        }
    }
    h
}
fn ln_gamma(z: f64) -> f64 {
    const C: [f64; 9] = [
        0.9999999999998099,
        676.5203681218851,
        -1259.1392167224028,
        771.3234287776531,
        -176.6150291621406,
        12.507343278686905,
        -0.13857109526572012,
        9.984369578019572e-6,
        1.5056327351493116e-7,
    ];
    if z < 0.5 {
        return std::f64::consts::PI.ln()
            - (std::f64::consts::PI * z).sin().ln()
            - ln_gamma(1.0 - z);
    }
    let z = z - 1.0;
    let mut x = C[0];
    for (i, c) in C.iter().enumerate().skip(1) {
        x += c / (z + i as f64);
    }
    let t = z + 7.5;
    0.5 * (2.0 * std::f64::consts::PI).ln() + (z + 0.5) * t.ln() - t + x.ln()
}
fn invert(mut a: Vec<Vec<f64>>) -> Option<Vec<Vec<f64>>> {
    let n = a.len();
    let mut b = vec![vec![0.0; n]; n];
    for (i, row) in b.iter_mut().enumerate() {
        row[i] = 1.0;
    }
    for i in 0..n {
        let p = (i..n).max_by(|x, y| a[*x][i].abs().total_cmp(&a[*y][i].abs()))?;
        if a[p][i].abs() < 1e-14 {
            return None;
        }
        a.swap(i, p);
        b.swap(i, p);
        let q = a[i][i];
        for j in 0..n {
            a[i][j] /= q;
            b[i][j] /= q;
        }
        for k in 0..n {
            if k != i {
                let q = a[k][i];
                for j in 0..n {
                    a[k][j] -= q * a[i][j];
                    b[k][j] -= q * b[i][j];
                }
            }
        }
    }
    Some(b)
}

fn designate(args: &DesignateArgs) -> anyhow::Result<()> {
    let records = ledger(&args.artifact)?;
    let closure = records
        .iter()
        .rposition(|x| x["record"] == "search_closed")
        .ok_or_else(|| anyhow!("designation requires an explicit search_closed record"))?;
    if records
        .iter()
        .skip(closure + 1)
        .any(|x| x["record"] == "designation")
    {
        bail!("post-closure designation was already recorded");
    }
    let mut eligible = records
        .iter()
        .filter(|x| x["record"] == "candidate_evaluation" && x["admissible"] == true)
        .collect::<Vec<_>>();
    eligible.sort_by(|a, b| {
        let ka = (
            a["hurdle"]["H1"]["coordinates"].as_u64().unwrap(),
            std::cmp::Reverse(a["hurdle"]["H4"]["rejection_count"].as_u64().unwrap()),
            std::cmp::Reverse(
                a["hurdle"]["H5"]["no_slow"]["rejection_count"]
                    .as_u64()
                    .unwrap()
                    .min(
                        a["hurdle"]["H5"]["excess"]["rejection_count"]
                            .as_u64()
                            .unwrap(),
                    ),
            ),
            a["search_order"].as_u64().unwrap(),
        );
        let kb = (
            b["hurdle"]["H1"]["coordinates"].as_u64().unwrap(),
            std::cmp::Reverse(b["hurdle"]["H4"]["rejection_count"].as_u64().unwrap()),
            std::cmp::Reverse(
                b["hurdle"]["H5"]["no_slow"]["rejection_count"]
                    .as_u64()
                    .unwrap()
                    .min(
                        b["hurdle"]["H5"]["excess"]["rejection_count"]
                            .as_u64()
                            .unwrap(),
                    ),
            ),
            b["search_order"].as_u64().unwrap(),
        );
        ka.cmp(&kb)
    });
    let outcome = eligible.first().map_or(
        "no_one_month_slow_confirmation_design",
        |_| "designated_projection",
    );
    append(
        &args.artifact,
        &json!({"record":"designation","search_closed":true,"complete_history_through_search_order":records[closure]["last_search_order"],"supersedes_prior_premature_designation":true,"outcome":outcome,"designated_candidate":eligible.first().map(|x|x["candidate_id"].clone()),"ordered_rule":["fewest_coordinates","highest_H4_rejection_count","highest_minimum_H5_rejection_count","earliest_committed"],"eligible_order":eligible.iter().map(|x|x["candidate_id"].clone()).collect::<Vec<_>>(),"stage_f_rule":"freeze unchanged or rerun the entire H1-H6 hurdle for any change"}),
    )?;
    println!("Tier 2 designation: {outcome}");
    Ok(())
}

fn load_manifest(path: &Path) -> anyhow::Result<Vec<Month>> {
    let paths: Vec<PathBuf> = serde_json::from_slice(&std::fs::read(path)?)?;
    let mut out = paths
        .iter()
        .map(|p| load_month(p))
        .collect::<anyhow::Result<Vec<_>>>()?;
    out.sort_by_key(|m| m.key);
    Ok(out)
}
fn load_month(path: &Path) -> anyhow::Result<Month> {
    let v: Value = serde_json::from_slice(&std::fs::read(path)?)?;
    if v["detail"]["coordinate_system"]["name"] != "session_local_hour" {
        bail!("{} is not a session-local artifact", path.display())
    }
    let key = v["binding"]["month"]
        .as_u64()
        .ok_or_else(|| anyhow!("{} has no month/walk binding", path.display()))?;
    let cells = v["detail"]["residual_matrix"]["cells"]
        .as_array()
        .ok_or_else(|| anyhow!("{} has no residual cells", path.display()))?
        .iter()
        .map(|x| {
            Ok(Cell {
                date: x["session_date"]
                    .as_str()
                    .ok_or_else(|| anyhow!("cell date"))?
                    .to_string(),
                hour: x["local_hour"]
                    .as_u64()
                    .ok_or_else(|| anyhow!("local hour"))?,
                residual: x["residual"].as_f64().ok_or_else(|| anyhow!("residual"))?,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let scores = v["detail"]["cross_fitted_factor"]["scores"]
        .as_array()
        .ok_or_else(|| anyhow!("{} has no scores", path.display()))?
        .iter()
        .map(|x| {
            Ok((
                x["session_date"]
                    .as_str()
                    .ok_or_else(|| anyhow!("score date"))?
                    .to_string(),
                x["score"].as_f64().ok_or_else(|| anyhow!("score"))?,
            ))
        })
        .collect::<anyhow::Result<BTreeMap<_, _>>>()?;
    let dates = cells
        .iter()
        .map(|c| c.date.clone())
        .collect::<BTreeSet<_>>();
    Ok(Month {
        key,
        cells,
        scores,
        thin: dates.len() < 15,
    })
}
fn ledger(path: &Path) -> anyhow::Result<Vec<Value>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    BufReader::new(File::open(path)?)
        .lines()
        .map(|x| Ok(serde_json::from_str(&x?)?))
        .collect()
}
fn append(path: &Path, value: &Value) -> anyhow::Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    serde_json::to_writer(&mut f, value)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(())
}
fn mean(x: &[f64]) -> f64 {
    x.iter().sum::<f64>() / x.len() as f64
}
fn variance(x: &[f64]) -> f64 {
    let m = mean(x);
    x.iter().map(|v| (v - m).powi(2)).sum::<f64>() / x.len() as f64
}
fn civil_day(s: &str) -> i64 {
    let mut p = s.split('-');
    let y: i64 = p.next().unwrap().parse().unwrap();
    let m: u32 = p.next().unwrap().parse().unwrap();
    let d: u32 = p.next().unwrap().parse().unwrap();
    mogwai_lab::session::days_from_civil(y, m, d)
}

// Filled in below. Keeping control generation as a dedicated command makes it
// impossible for candidate evaluation to opportunistically run or inspect it.
fn controls(args: &ControlsArgs) -> anyhow::Result<()> {
    if args.manifest.exists() {
        bail!(
            "incumbent controls are already materialized at {}",
            args.manifest.display()
        )
    }
    let designs = load_manifest(&args.design_manifest)?
        .into_iter()
        .filter(|m| m.key != 202_607)
        .collect::<Vec<_>>();
    if designs.len() != 8 {
        bail!("incumbent rotation requires eight design months")
    }
    std::fs::create_dir_all(&args.output)?;
    let profile =
        mogwai_venue::config::profile_from_preset("MNQ").map_err(|e| anyhow!(e.to_string()))?;
    let frame = mogwai_lab::session::ScheduleFrame::stage_m(Path::new(
        "analysis/tz-america-chicago-2026c.json",
    ))
    .map_err(|e| anyhow!(e.to_string()))?;
    let mut paths = Vec::new();
    for i in 1..=24_u64 {
        let design = &designs[(i as usize - 1) % designs.len()];
        let dates = design
            .cells
            .iter()
            .map(|c| c.date.clone())
            .collect::<BTreeSet<_>>();
        let bounds = dates
            .iter()
            .map(|d| frame.bounds(d).map_err(|e| anyhow!(e.to_string())))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let start = bounds
            .iter()
            .map(|b| b.open_ns)
            .min()
            .ok_or_else(|| anyhow!("empty design calendar"))?;
        let end = bounds
            .iter()
            .map(|b| b.close_ns)
            .max()
            .unwrap()
            .saturating_add(1);
        let seed = tuple_mix(INCUMBENT_SEED, &[i]);
        let walk_start = start
            .checked_sub(14 * 86_400_000_000_000)
            .ok_or_else(|| anyhow!("control burn-in underflow"))?;
        let mut source = mogwai_data::GeneratedSource::try_new_with_session_profile(
            profile.scalars.clone(),
            seed,
            walk_start,
            mogwai_venue::source::fingerprint(),
            &profile.session,
            None,
            mogwai_data::SizeGrid::from_def(&profile.def),
            profile.calendar.clone(),
        )
        .map_err(|e| anyhow!("building incumbent control: {e:?}"))?;
        let mut pending = false;
        let mut pending_normal = false;
        let mut parent_open = false;
        let mut counts: BTreeMap<(String, u64), u64> = BTreeMap::new();
        while let Some(event) = source.next_tick() {
            if event.ts_event() >= end {
                break;
            }
            match event {
                TickEvent::Quote(q) => {
                    parent_open = false;
                    pending = true;
                    pending_normal = q.bid_px > rust_decimal::Decimal::ZERO && q.bid_px < q.ask_px;
                }
                TickEvent::Trade(t) => {
                    if matches!(t.aggressor, AggressorSide::NoAggressor) {
                        parent_open = false;
                        continue;
                    }
                    if !parent_open && pending {
                        parent_open = true;
                        pending = false;
                        if pending_normal
                            && t.ts_event >= start
                            && let Some(seg) = frame
                                .session_segment_at(t.ts_event)
                                .map_err(|e| anyhow!(e.to_string()))?
                        {
                            let date = mogwai_lab::session::format_trade_date(seg.trade_day);
                            if dates.contains(&date) {
                                let b = frame.bounds(&date).map_err(|e| anyhow!(e.to_string()))?;
                                let h = ((t.ts_event - b.open_ns) / 3_600_000_000_000).min(22);
                                *counts.entry((date, h)).or_default() += 1;
                            }
                        }
                    }
                }
            }
        }
        if source.fault().is_some() {
            bail!("incumbent control {i} generator fault")
        }
        let mut raw = Vec::new();
        for b in &bounds {
            for h in 0..=22_u64 {
                let lo = b.open_ns + h * 3_600_000_000_000;
                let hi = (lo + 3_600_000_000_000).min(b.close_ns);
                let exposure = overlap(lo, hi, b.open_ns, b.halt_start_ns)
                    + overlap(lo, hi, b.halt_end_ns, b.close_ns);
                let n = *counts.get(&(b.session.clone(), h)).unwrap_or(&0);
                if exposure > 0 && n > 0 {
                    raw.push((
                        b.session.clone(),
                        h,
                        n,
                        exposure,
                        (n as f64 / (exposure as f64 / 1e9)).ln(),
                    ));
                }
            }
        }
        let means = (0..=22)
            .map(|h| {
                (
                    h,
                    mean(
                        &raw.iter()
                            .filter(|x| x.1 == h)
                            .map(|x| x.4)
                            .collect::<Vec<_>>(),
                    ),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let cells=raw.iter().map(|x|json!({"session_date":x.0,"local_hour":x.1,"parents":x.2,"exposure_s":x.3/1_000_000_000,"log_rate":x.4,"residual":x.4-means[&x.1]})).collect::<Vec<_>>();
        let score_input = raw
            .iter()
            .map(|x| (x.0.clone(), x.1, x.4 - means[&x.1]))
            .collect();
        let scores = crate::slow_geometry::tier2_scores(score_input)
            .into_iter()
            .map(|(d, s)| json!({"session_date":d,"score":s}))
            .collect::<Vec<_>>();
        let artifact = json!({"outcome":"completed","binding":{"month":i,"control_walk":i,"projected_design_month":design.key,"seed":seed,"seed_derivation":"tuple_mix(STAGE_M_INCUMBENT_SEED, [i])","stage_m_incumbent_seed":INCUMBENT_SEED,"tape_protocol_version":mogwai_data::TAPE_PROTOCOL_VERSION,"schedule_authority":{"artifact":"analysis/tz-america-chicago-2026c.json","sha256":mogwai_lab::session::STAGE_M_TZ_AUTHORITY_SHA256}},"detail":{"coordinate_system":{"name":"session_local_hour","coordinates":(0..=22).collect::<Vec<_>>()},"residual_matrix":{"cells":cells,"excluded_cells":[]},"cross_fitted_factor":{"scores":scores,"refusals":[]}},"inspection_restriction":"projection_evaluation_only"});
        let path = args.output.join(format!("control-{i:02}.json"));
        mogwai_lab::aggregate::artifact::write_json_atomic(&path, &artifact)
            .map_err(|e| anyhow!(e.to_string()))?;
        paths.push(path);
        println!("incumbent control {i}/24 complete");
    }
    mogwai_lab::aggregate::artifact::write_json_atomic(
        &args.manifest,
        &serde_json::to_value(paths)?,
    )
    .map_err(|e| anyhow!(e.to_string()))?;
    Ok(())
}
fn overlap(a: u64, b: u64, c: u64, d: u64) -> u64 {
    b.min(d).saturating_sub(a.max(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finite_sample_f_cutoff_matches_reference_values() {
        // F(1,6; .90) = t(6; .95)^2.
        assert!((f_quantile_90(1, 6) - 3.775_949).abs() < 1e-5);
        assert!((f_quantile_90(4, 3) - 5.342_644).abs() < 1e-5);
    }

    /// The `excess` draw is keyed on the session DATE, so the order the cells
    /// happen to sit in must not reach it.
    ///
    /// THE FIXTURE IS DELIBERATELY NOT DEGENERATE. Zero residuals on one date
    /// make this a comparison of all-zeros against all-zeros: every cell would
    /// carry the same value whether the draw were per-date, per-cell, or
    /// chained across the vector in iteration order, so no order-dependent
    /// defect could be seen. It needs distinct per-cell residuals, at least two
    /// COMPLETE dates (one date cannot show a draw taken in the wrong order),
    /// and an INCOMPLETE one, which is what the return count is about.
    #[test]
    fn excess_draw_is_session_order_independent() {
        let residual = |day: usize, hour: u64| (day as f64) * 100.0 + (hour as f64);
        let mut cells = Vec::new();
        for (day, date) in ["2026-03-09", "2026-03-10"].iter().enumerate() {
            for hour in 0..23 {
                cells.push(Cell {
                    date: (*date).into(),
                    hour,
                    residual: residual(day, hour),
                });
            }
        }
        // A third date missing hours 6..=22: not a complete session, so it
        // draws nothing and is counted as excluded instead.
        for hour in 0..6 {
            cells.push(Cell {
                date: "2026-03-11".into(),
                hour,
                residual: residual(2, hour),
            });
        }

        let mut a = Month {
            key: 202_603,
            cells,
            scores: BTreeMap::new(),
            thin: true,
        };
        let before = a.cells.clone();

        // A genuine shuffle rather than a reverse: a reverse preserves the
        // date blocks intact, so a draw chained across cells in vector order
        // would still see each date's cells consecutively.
        let mut b = a.clone();
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        for i in (1..b.cells.len()).rev() {
            state = splitmix64(state);
            b.cells.swap(i, (state % (i as u64 + 1)) as usize);
        }
        assert_ne!(
            b.cells.iter().map(|c| c.hour).collect::<Vec<_>>(),
            a.cells.iter().map(|c| c.hour).collect::<Vec<_>>(),
            "the shuffle must actually move cells"
        );

        // One date of the three is incomplete, so one is excluded.
        assert_eq!(excess(&mut a, 17, 0.25).unwrap(), 1);
        assert_eq!(excess(&mut b, 17, 0.25).unwrap(), 1);

        let key = |c: &Cell| (c.date.clone(), c.hour);
        a.cells.sort_by_key(key);
        b.cells.sort_by_key(key);
        let mut expected = before.clone();
        expected.sort_by_key(key);
        assert_eq!(
            a.cells.iter().map(key).collect::<Vec<_>>(),
            b.cells.iter().map(key).collect::<Vec<_>>()
        );
        for ((x, y), original) in a.cells.iter().zip(&b.cells).zip(&expected) {
            assert_eq!(
                x.residual, y.residual,
                "{} hour {} moved with the cell order",
                x.date, x.hour
            );
            let delta = x.residual - original.residual;
            if original.date == "2026-03-11" {
                // Incomplete: untouched, so the excess draw is not simply
                // added to everything in sight.
                assert_eq!(delta, 0.0, "an incomplete session drew an excess");
            } else {
                assert_ne!(delta, 0.0, "a complete session drew nothing");
            }
        }

        // One draw PER DATE, shared by that date's cells - and the two dates
        // draw differently, which is what makes a per-date stream observable
        // at all.
        let delta_of = |date: &str| {
            let deltas = a
                .cells
                .iter()
                .zip(&expected)
                .filter(|(c, _)| c.date == date)
                .map(|(c, o)| c.residual - o.residual)
                .collect::<Vec<_>>();
            // Recovered by subtraction, so the cells only agree to within the
            // rounding of `original + g` at their own magnitudes - the draw
            // itself is one number, but `residual` spans 0 to 122 here.
            assert!(
                deltas.windows(2).all(|w| (w[0] - w[1]).abs() < 1e-12),
                "{date} did not share one draw across its cells: {deltas:?}"
            );
            deltas[0]
        };
        let first = delta_of("2026-03-09");
        let second = delta_of("2026-03-10");
        assert!(
            (first - second).abs() > 1e-6,
            "the two sessions drew the same excess: {first} and {second}"
        );
    }

    #[test]
    fn no_slow_uses_independent_hour_streams_deterministically() {
        let cells = (0..3)
            .flat_map(|day| {
                (0..2).map(move |hour| Cell {
                    date: format!("2026-04-0{}", day + 1),
                    hour,
                    residual: (10 * hour + day) as f64,
                })
            })
            .collect();
        let mut a = Month {
            key: 202_604,
            cells,
            scores: BTreeMap::new(),
            thin: true,
        };
        let mut b = a.clone();
        no_slow(&mut a, 9);
        no_slow(&mut b, 9);
        assert!(
            a.cells
                .iter()
                .zip(&b.cells)
                .all(|(x, y)| x.residual == y.residual)
        );
        for hour in 0..2 {
            let mut before = (0..3)
                .map(|day| (10 * hour + day) as f64)
                .collect::<Vec<_>>();
            let mut after = a
                .cells
                .iter()
                .filter(|c| c.hour == hour)
                .map(|c| c.residual)
                .collect::<Vec<_>>();
            before.sort_by(f64::total_cmp);
            after.sort_by(f64::total_cmp);
            assert_eq!(before, after);
        }
    }

    #[test]
    fn c3_perfectly_coherent_field_has_variance_ratio_23() {
        let cells = (1..=5)
            .flat_map(|day| {
                (0..23).map(move |hour| Cell {
                    date: format!("2026-04-{day:02}"),
                    hour,
                    residual: day as f64,
                })
            })
            .collect();
        let month = Month {
            key: 202_604,
            cells,
            scores: BTreeMap::new(),
            thin: true,
        };
        let first = c3_coordinates(&month).unwrap();
        let second = c3_coordinates(&month).unwrap();
        assert!((first.0 - 23.0).abs() < 1e-12);
        assert_eq!(first, second);
    }

    #[test]
    fn c3_refuses_zero_covariance_trace() {
        let cells = (1..=3)
            .flat_map(|day| {
                (0..23).map(move |hour| Cell {
                    date: format!("2026-04-{day:02}"),
                    hour,
                    residual: 0.0,
                })
            })
            .collect();
        let month = Month {
            key: 202_604,
            cells,
            scores: BTreeMap::new(),
            thin: true,
        };
        assert!(
            c3_coordinates(&month)
                .unwrap_err()
                .to_string()
                .contains("covariance trace")
        );
    }
}
