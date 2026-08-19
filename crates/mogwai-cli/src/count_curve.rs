// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The two-stage count-curve measurement signed in
//! `notes/count-curve-preregistration.md`.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, anyhow, bail};
use clap::{ArgGroup, Args};
use mogwai_lab::aggregate::artifact::write_json_atomic;
use mogwai_lab::aggregate::bootstrap::bootstrap_multiplicities;
use serde_json::{Value, json};

use crate::measure::{
    run_final_walk, run_final_walk_with_count_windows, run_observed_with_count_windows,
};

const OUT: &str = "analysis/out/count-curve-measurement.json";
const CORPUS: &str = "research/market-data/databento/mnqv/2026-07.full.tbbo";
const LEDGER: &str = "analysis/databento-jobs.json";
const PREFLIGHT: &str = "analysis/out/mnq-fit-preflight.json";
const PRESET_PATH: &str = "crates/mogwai-server/presets/mnq.toml";
const FINGERPRINT_PATH: &str = "analysis/fingerprint.json";
const HISTORICAL_PRESET: &str = "46622ce226922d96457fcc0ea57411b63b5d7f0f";
const CURRENT_PRESET: &str = "c1b352efbc35c878dd3cc75cb282fa29fde57f6a";
const HISTORICAL_FINGERPRINT: &str = "f63d9570d5cad4b2ca6c109a439dbbc48311c122";
const CURRENT_FINGERPRINT: &str = "19238d94ab0747f86fcdd4635889964e576972db";
const BACKCHECK_HORIZONS: [u64; 3] = [1, 5, 60];
const CURVE_WINDOWS: &[i64] = &[1, 5, 15, 60, 300];
const FIELDS: [&str; 3] = ["scheduled_windows", "zero_windows", "count_hist"];
const BOOTSTRAP_REPLICATES: usize = 2_000;

#[derive(Args)]
#[command(group(ArgGroup::new("mode").required(true).args(["stage0", "full", "ordered_counts", "slow_geometry"])))]
pub struct CountCurveArgs {
    /// Run the generated-only signed Stage 0 backcheck.
    #[arg(long)]
    stage0: bool,
    /// Run the licensed Stage 1 observed backcheck and Stage 2 measurement.
    #[arg(long)]
    full: bool,
    /// Run the signed ordered-count extraction and both frozen panels.
    #[arg(long)]
    ordered_counts: bool,
    /// Reduce the retained ordered sequence into the signed slow-geometry artifact.
    #[arg(long)]
    slow_geometry: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct InputIdentity {
    preset: String,
    fingerprint: String,
}

#[derive(Clone, Debug)]
struct Exposure {
    seeds: Vec<u64>,
    start_ns: u64,
    length_ns: u64,
    warmup: String,
}

#[derive(Clone, Debug)]
struct Estimates {
    fano: Option<f64>,
    fano_within: Option<f64>,
    fano_between: Option<f64>,
    zero_probability: f64,
    count_mean: f64,
    p99: f64,
}

pub fn run(args: &CountCurveArgs) -> anyhow::Result<()> {
    if args.stage0 {
        run_stage0()
    } else if args.slow_geometry {
        crate::slow_geometry::run()
    } else if args.ordered_counts {
        crate::ordered_counts::run()
    } else {
        run_full()
    }
}

fn reference() -> anyhow::Result<Value> {
    serde_json::from_str(include_str!("../../../analysis/mnq-measure-12a.json")).map_err(Into::into)
}

fn run_stage0() -> anyhow::Result<()> {
    let reference = reference()?;
    let exposure = Exposure::from_reference(&reference)?;
    let commit = executing_commit()?;
    let actual_identity = current_identity()?;
    let expected_identity = InputIdentity {
        preset: CURRENT_PRESET.into(),
        fingerprint: CURRENT_FINGERPRINT.into(),
    };
    let binding = binding_json(&exposure, &commit, &actual_identity);
    if actual_identity != expected_identity || !exposure.is_frozen() {
        write_artifact(
            Path::new(OUT),
            &json!({"binding": binding, "verdict": "execution_input_mismatch"}),
        )?;
        return report("execution_input_mismatch");
    }
    for seed in &exposure.seeds {
        let walk = run_final_walk(*seed)?;
        let sessions = array_at(&walk, "per_session")?;
        let blocks = mogwai_lab::aggregate::monthly::blocks_from_sessions(sessions)
            .map_err(|e| anyhow!(e.to_string()))?;
        let expected_seed = reference["generated"]["per_seed"]
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["seed"].as_u64() == Some(*seed)))
            .ok_or_else(|| anyhow!("committed artifact is missing seed {seed}"))?;
        if let Some(divergence) = first_divergence(
            Some(*seed),
            &expected_seed["blocks"]["block2"],
            &blocks["block2"],
        )? {
            write_artifact(
                Path::new(OUT),
                &json!({"binding": binding, "verdict": "generated_identity_mismatch_unattributed", "first_divergence": divergence}),
            )?;
            return report("generated_identity_mismatch_unattributed");
        }
        println!("seed {seed} matched exactly");
    }
    write_artifact(
        Path::new(OUT),
        &json!({"binding": binding, "verdict": "passed_exactly"}),
    )?;
    report("passed_exactly")
}

fn run_full() -> anyhow::Result<()> {
    let stage0: Value = serde_json::from_slice(
        &std::fs::read(OUT).context("reading the retained Stage 0 artifact")?,
    )?;
    if stage0["verdict"] != "passed_exactly" {
        bail!("Stage 0 did not license the full measurement");
    }
    let reference = reference()?;
    let exposure = Exposure::from_reference(&reference)?;
    if current_identity()?
        != (InputIdentity {
            preset: CURRENT_PRESET.into(),
            fingerprint: CURRENT_FINGERPRINT.into(),
        })
        || !exposure.is_frozen()
    {
        bail!("execution_input_mismatch");
    }

    let observed = run_observed_with_count_windows(
        Path::new(CORPUS),
        Path::new(LEDGER),
        Path::new(PREFLIGHT),
        CURVE_WINDOWS,
    )?;
    let observed_sessions = array_at(&observed, "per_session")?;
    let observed_blocks = mogwai_lab::aggregate::monthly::blocks_from_sessions(observed_sessions)
        .map_err(|e| anyhow!(e.to_string()))?;
    if let Some(divergence) = first_divergence(
        None,
        &reference["observed"]["monthly"]["block2"],
        &observed_blocks["block2"],
    )? {
        let artifact = json!({"stage0": stage0, "verdict": "observed_method_mismatch", "first_divergence": divergence});
        write_artifact(Path::new(OUT), &artifact)?;
        return report("observed_method_mismatch");
    }
    println!("Stage 1 observed backcheck matched exactly");

    let observed_output = observed_statistics(observed_sessions)?;
    let mut generated_sessions = Vec::new();
    for seed in &exposure.seeds {
        let walk = run_final_walk_with_count_windows(*seed, CURVE_WINDOWS)?;
        generated_sessions.push((*seed, array_at(&walk, "per_session")?.clone()));
        println!("seed {seed} count curve complete");
    }
    let generated_output = generated_statistics(&generated_sessions)?;
    let artifact = json!({
        "stage0": stage0,
        "binding": {"observed": observed["binding"].clone(), "horizons_s": CURVE_WINDOWS, "bootstrap_replicates": BOOTSTRAP_REPLICATES},
        "verdict": "completed",
        "observed": observed_output,
        "generated": generated_output,
    });
    write_artifact(Path::new(OUT), &artifact)?;
    report("completed")
}

fn observed_statistics(sessions: &[Value]) -> anyhow::Result<Value> {
    observed_statistics_with_bootstrap(sessions, &bootstrap_multiplicities(sessions.len()))
}

pub(crate) fn july_observed_statistics(sessions: &[Value]) -> anyhow::Result<Value> {
    observed_statistics(sessions)
}

fn observed_statistics_with_bootstrap(
    sessions: &[Value],
    mults: &[Vec<i64>],
) -> anyhow::Result<Value> {
    let mut out = serde_json::Map::new();
    for hour in observed_traded_hours(sessions)? {
        let mut windows = serde_json::Map::new();
        for &window in CURVE_WINDOWS {
            let point = estimates(sessions, None, hour, window)?;
            let reps = mults
                .iter()
                .take(BOOTSTRAP_REPLICATES)
                .map(|m| estimates(sessions, Some(m), hour, window))
                .collect::<anyhow::Result<Vec<_>>>()?;
            windows.insert(window.to_string(), uncertainty_json(&point, &reps)?);
        }
        out.insert(hour.to_string(), Value::Object(windows));
    }
    Ok(Value::Object(out))
}

fn observed_traded_hours(sessions: &[Value]) -> anyhow::Result<Vec<i64>> {
    let first = sessions
        .first()
        .ok_or_else(|| anyhow!("no observed sessions"))?;
    let block2 = first["block2"]
        .as_object()
        .ok_or_else(|| anyhow!("block2 is absent"))?;
    let mut hours = block2
        .iter()
        .filter(|(_, windows)| windows["1"]["count_hist"].is_object())
        .map(|(hour, _)| hour.parse::<i64>())
        .collect::<Result<Vec<_>, _>>()?;
    hours.sort_unstable();
    if hours.len() != 23 {
        bail!(
            "ordinary session has {} traded UTC endpoint hours, not 23",
            hours.len()
        );
    }
    Ok(hours)
}

pub struct CountCurveMonthRun {
    pub month: u64,
    pub corpus: PathBuf,
    pub ledger: PathBuf,
    pub preflight: PathBuf,
    pub output: PathBuf,
}

/// Run the observed Stage M count curve for exactly one new-design month.
/// July is deliberately rejected here because its original-domain backcheck
/// is a separate command path.
pub fn run_month(config: &CountCurveMonthRun) -> anyhow::Result<()> {
    if config.month == 202_607 {
        bail!("July must use the Stage M backcheck path");
    }
    let observed = run_observed_with_count_windows(
        &config.corpus,
        &config.ledger,
        &config.preflight,
        CURVE_WINDOWS,
    )?;
    write_month_from_observed(config, &observed)
}

pub(crate) fn write_month_from_observed(
    config: &CountCurveMonthRun,
    observed: &Value,
) -> anyhow::Result<()> {
    let sessions = array_at(observed, "per_session")?;
    let usable_sessions = sessions.len();
    let mults = mogwai_lab::aggregate::bootstrap::stage_m_bootstrap_multiplicities(
        config.month,
        usable_sessions,
    );
    let curve = observed_statistics_with_bootstrap(sessions, &mults)?;
    let monthly = mogwai_lab::aggregate::monthly::blocks_from_sessions(sessions)
        .map_err(|e| anyhow!(e.to_string()))?;
    let artifact = json!({
        "outcome":"completed",
        "binding": {
            "month":config.month,
            "observed":observed["binding"].clone(),
            "usable_sessions":usable_sessions,
            "thin":usable_sessions < 15,
            "horizons_s":CURVE_WINDOWS,
            "bootstrap_replicates":BOOTSTRAP_REPLICATES,
            "seed_domain":{"base":mogwai_lab::aggregate::bootstrap::STAGE_M_SEED,"tuple":["YYYYMM","replicate_index","block_index"]},
        },
        "measure12a_observed":{"monthly":monthly},
        "count_curve":curve,
        "refusals":[],
    });
    write_artifact(&config.output, &artifact)
}

fn generated_statistics(seeds: &[(u64, Vec<Value>)]) -> anyhow::Result<Value> {
    let mut out = serde_json::Map::new();
    for hour in traded_hours() {
        let mut windows = serde_json::Map::new();
        for &window in CURVE_WINDOWS {
            let vals = seeds
                .iter()
                .map(|(seed, sessions)| Ok((*seed, estimates(sessions, None, hour, window)?)))
                .collect::<anyhow::Result<Vec<_>>>()?;
            windows.insert(window.to_string(), generated_json(&vals)?);
        }
        out.insert(hour.to_string(), Value::Object(windows));
    }
    Ok(Value::Object(out))
}

fn estimates(
    sessions: &[Value],
    mult: Option<&Vec<i64>>,
    hour: i64,
    window: i64,
) -> anyhow::Result<Estimates> {
    let mut pooled = BTreeMap::<u64, u64>::new();
    let mut session_parts = Vec::<(f64, f64, f64)>::new();
    for (idx, session) in sessions.iter().enumerate() {
        let weight = mult.map_or(1, |m| m[idx]);
        if weight <= 0 {
            continue;
        }
        let cell = &session["block2"][hour.to_string()][window.to_string()];
        let hist = histogram(cell)
            .with_context(|| format!("session {idx}, hour {hour}, window {window}"))?;
        let (n, mean, var) = moments(&hist)?;
        for (&count, &occurrences) in &hist {
            *pooled.entry(count).or_default() += occurrences * u64::try_from(weight)?;
        }
        session_parts.push((n * weight as f64, mean, var));
    }
    let (n, mean, total_var) = moments(&pooled)?;
    if n == 0.0 {
        bail!("empty cell at hour {hour}, window {window}");
    }
    let within_var = session_parts
        .iter()
        .map(|(sn, _, var)| sn * var)
        .sum::<f64>()
        / n;
    let between_var = session_parts
        .iter()
        .map(|(sn, sm, _)| sn * (sm - mean).powi(2))
        .sum::<f64>()
        / n;
    let fano = (mean != 0.0).then_some(total_var / mean);
    let fano_within = (mean != 0.0).then_some(within_var / mean);
    let fano_between = (mean != 0.0).then_some(between_var / mean);
    if let (Some(total), Some(within), Some(between)) = (fano, fano_within, fano_between) {
        let diff = (total - within - between).abs();
        if diff > 1e-10 * total.abs().max(1.0) {
            bail!(
                "decomposition identity defect at hour {hour}, window {window}: difference {diff}"
            );
        }
    }
    Ok(Estimates {
        fano,
        fano_within,
        fano_between,
        zero_probability: pooled.get(&0).copied().unwrap_or(0) as f64 / n,
        count_mean: mean,
        p99: nearest_rank(&pooled, 99, 100)?,
    })
}

fn histogram(cell: &Value) -> anyhow::Result<BTreeMap<u64, u64>> {
    cell["count_hist"]
        .as_object()
        .ok_or_else(|| anyhow!("count_hist is absent"))?
        .iter()
        .map(|(k, v)| {
            Ok((
                k.parse()?,
                v.as_u64()
                    .ok_or_else(|| anyhow!("histogram weight is not u64"))?,
            ))
        })
        .collect()
}

fn moments(hist: &BTreeMap<u64, u64>) -> anyhow::Result<(f64, f64, f64)> {
    let n = hist.values().sum::<u64>() as f64;
    if n == 0.0 {
        return Ok((0.0, 0.0, 0.0));
    }
    let mean = hist.iter().map(|(&x, &w)| x as f64 * w as f64).sum::<f64>() / n;
    let var = hist
        .iter()
        .map(|(&x, &w)| (x as f64 - mean).powi(2) * w as f64)
        .sum::<f64>()
        / n;
    Ok((n, mean, var))
}

fn nearest_rank(
    hist: &BTreeMap<u64, u64>,
    numerator: u64,
    denominator: u64,
) -> anyhow::Result<f64> {
    let total = hist.values().sum::<u64>();
    if total == 0 {
        bail!("nearest-rank quantile over an empty histogram");
    }
    let target = (numerator * total).div_ceil(denominator);
    let mut cumulative = 0;
    for (&value, &weight) in hist {
        cumulative += weight;
        if cumulative >= target {
            return Ok(value as f64);
        }
    }
    unreachable!("positive histogram has a final rank")
}

fn uncertainty_json(point: &Estimates, reps: &[Estimates]) -> anyhow::Result<Value> {
    // A null point (Fano over a zero-mean hour) or a null replicate is a
    // frozen REFUSAL, never an error: the statistic reports null with the
    // finite-replicate count, and uncertainty is never computed over the
    // surviving subset.
    let field = |name: &str,
                 point: Option<f64>,
                 values: Vec<Option<f64>>|
     -> anyhow::Result<(String, Value)> {
        let Some(point) = point else {
            return Ok((
                name.into(),
                json!({"point": null, "standard_error": null, "p2_5": null, "p97_5": null,
                       "reason": "point estimate refused under the frozen null rules"}),
            ));
        };
        let finite = values.iter().flatten().count();
        let Some(mut xs) = values.into_iter().collect::<Option<Vec<_>>>() else {
            return Ok((
                name.into(),
                json!({"point": point, "standard_error": null, "p2_5": null, "p97_5": null,
                       "finite_replicates": finite,
                       "reason": "a bootstrap replicate refused; uncertainty is null rather than a surviving-subset estimate"}),
            ));
        };
        let mean = xs.iter().sum::<f64>() / xs.len() as f64;
        let se = (xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / 1999.0).sqrt();
        xs.sort_by(f64::total_cmp);
        let lo = xs[49];
        let hi = xs[1949];
        Ok((
            name.into(),
            json!({"point": point, "standard_error": se, "p2_5": lo, "p97_5": hi}),
        ))
    };
    let pairs = [
        field("fano", point.fano, reps.iter().map(|x| x.fano).collect())?,
        field(
            "fano_within",
            point.fano_within,
            reps.iter().map(|x| x.fano_within).collect(),
        )?,
        field(
            "fano_between",
            point.fano_between,
            reps.iter().map(|x| x.fano_between).collect(),
        )?,
        field(
            "zero_probability",
            Some(point.zero_probability),
            reps.iter().map(|x| Some(x.zero_probability)).collect(),
        )?,
        field(
            "count_mean",
            Some(point.count_mean),
            reps.iter().map(|x| Some(x.count_mean)).collect(),
        )?,
        field(
            "p99",
            Some(point.p99),
            reps.iter().map(|x| Some(x.p99)).collect(),
        )?,
    ];
    Ok(Value::Object(pairs.into_iter().collect()))
}

fn generated_json(vals: &[(u64, Estimates)]) -> anyhow::Result<Value> {
    let metric = |f: fn(&Estimates) -> Option<f64>| -> anyhow::Result<Value> {
        let per_seed = vals.iter().map(|(seed, e)| Ok(json!({"seed": seed, "value": f(e).ok_or_else(|| anyhow!("generated metric is null"))?}))).collect::<anyhow::Result<Vec<_>>>()?;
        let mut xs = vals
            .iter()
            .map(|(_, e)| f(e).ok_or_else(|| anyhow!("generated metric is null")))
            .collect::<anyhow::Result<Vec<_>>>()?;
        xs.sort_by(f64::total_cmp);
        Ok(
            json!({"per_seed": per_seed, "median": (xs[3] + xs[4]) / 2.0, "min": xs[0], "max": xs[7]}),
        )
    };
    Ok(json!({
        "fano": metric(|e| e.fano)?, "fano_within": metric(|e| e.fano_within)?,
        "fano_between": metric(|e| e.fano_between)?,
        "zero_probability": metric(|e| Some(e.zero_probability))?,
        "count_mean": metric(|e| Some(e.count_mean))?, "p99": metric(|e| Some(e.p99))?,
    }))
}

fn traded_hours() -> impl Iterator<Item = i64> {
    (0..24).filter(|hour| *hour != 21)
}

impl Exposure {
    fn from_reference(reference: &Value) -> anyhow::Result<Self> {
        let generated = &reference["binding"]["generated"];
        Ok(Self {
            seeds: generated["seeds"]
                .as_array()
                .ok_or_else(|| anyhow!("binding.generated.seeds is absent"))?
                .iter()
                .map(|s| s.as_u64().ok_or_else(|| anyhow!("a bound seed is not u64")))
                .collect::<anyhow::Result<_>>()?,
            start_ns: generated["window_start_ns"]
                .as_u64()
                .ok_or_else(|| anyhow!("window_start_ns is absent"))?,
            length_ns: generated["window_length_ns"]
                .as_u64()
                .ok_or_else(|| anyhow!("window_length_ns is absent"))?,
            warmup: generated["warmup"]
                .as_str()
                .ok_or_else(|| anyhow!("warmup is absent"))?
                .into(),
        })
    }
    fn is_frozen(&self) -> bool {
        self.seeds == (1..=8).collect::<Vec<_>>()
            && self.start_ns
                == u64::try_from(mogwai_lab::subcontract::FINAL_START_NS).unwrap_or_default()
            && self.length_ns
                == u64::try_from(
                    mogwai_lab::subcontract::FINAL_END_NS - mogwai_lab::subcontract::FINAL_START_NS,
                )
                .unwrap_or_default()
            && self.warmup == mogwai_lab::subcontract::SUMMARY_WARMUP
    }
}

fn current_identity() -> anyhow::Result<InputIdentity> {
    Ok(InputIdentity {
        preset: git_blob_id(
            mogwai_server::config::preset_document("MNQ")
                .ok_or_else(|| anyhow!("embedded MNQ preset is absent"))?
                .as_bytes(),
        )?,
        fingerprint: git_blob_id(include_bytes!("../../../analysis/fingerprint.json"))?,
    })
}

fn binding_json(exposure: &Exposure, commit: &str, actual: &InputIdentity) -> Value {
    json!({
        "blob_ids": {
            (PRESET_PATH): {"historical": HISTORICAL_PRESET, "stage0_bound": CURRENT_PRESET, "executed": actual.preset},
            (FINGERPRINT_PATH): {"historical": HISTORICAL_FINGERPRINT, "stage0_bound": CURRENT_FINGERPRINT, "executed": actual.fingerprint}},
        "generated": {"seeds": exposure.seeds, "window_start_ns": exposure.start_ns, "window_length_ns": exposure.length_ns, "warmup": exposure.warmup},
        "executing_commit": commit, "tape_protocol_version": mogwai_data::TAPE_PROTOCOL_VERSION,
    })
}

fn first_divergence(
    seed: Option<u64>,
    expected: &Value,
    actual: &Value,
) -> anyhow::Result<Option<Value>> {
    for hour in traded_hours() {
        for horizon in BACKCHECK_HORIZONS {
            for field in FIELDS {
                let expected_value = &expected[hour.to_string()][horizon.to_string()][field];
                let actual_value = &actual[hour.to_string()][horizon.to_string()][field];
                if expected_value != actual_value {
                    return Ok(Some(
                        json!({"seed": seed, "hour": hour, "horizon_s": horizon, "field": field, "expected": expected_value, "actual": actual_value}),
                    ));
                }
            }
        }
    }
    Ok(None)
}

fn array_at<'a>(value: &'a Value, key: &str) -> anyhow::Result<&'a Vec<Value>> {
    value[key]
        .as_array()
        .ok_or_else(|| anyhow!("{key} is not an array"))
}
fn report(verdict: &str) -> anyhow::Result<()> {
    println!("artifact -> {OUT}");
    println!("verdict: {verdict}");
    Ok(())
}

fn git_blob_id(bytes: &[u8]) -> anyhow::Result<String> {
    let mut child = Command::new("git")
        .args(["hash-object", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("starting git hash-object")?;
    child.stdin.take().expect("piped stdin").write_all(bytes)?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!("git hash-object failed");
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}
fn executing_commit() -> anyhow::Result<String> {
    let output = Command::new("git").args(["rev-parse", "HEAD"]).output()?;
    if !output.status.success() {
        bail!("git rev-parse HEAD failed");
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}
fn write_artifact(path: &Path, artifact: &Value) -> anyhow::Result<()> {
    write_json_atomic(path, artifact).map_err(|e| anyhow!("writing {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(hist: &Value) -> Value {
        json!({"block2":{"20":{"1":{"count_hist":hist}}}})
    }

    #[test]
    fn decomposition_has_known_within_and_between_components() {
        let sessions = vec![cell(&json!({"0":1,"2":1})), cell(&json!({"2":1,"4":1}))];
        let got = estimates(&sessions, None, 20, 1).expect("estimate");
        assert_eq!(got.count_mean, 2.0);
        assert_eq!(got.fano_within, Some(0.5));
        assert_eq!(got.fano_between, Some(0.5));
        assert_eq!(got.fano, Some(1.0));
    }

    #[test]
    fn perturbed_observed_input_fails_stage1_comparator() {
        let expected = json!({"20":{"1":{"scheduled_windows":2,"zero_windows":1,"count_hist":{"0":1,"1":1}},"5":{"scheduled_windows":1,"zero_windows":0,"count_hist":{"2":1}},"60":{"scheduled_windows":1,"zero_windows":0,"count_hist":{"3":1}}}});
        let mut actual = expected.clone();
        actual["20"]["1"]["zero_windows"] = json!(0);
        let divergence = first_divergence(None, &expected, &actual)
            .expect("comparison")
            .expect("divergence");
        assert_eq!(divergence["hour"], 20);
        assert_eq!(divergence["field"], "zero_windows");
    }

    #[test]
    fn hour_20_is_an_independent_reported_stratum() {
        let hours = traded_hours().collect::<Vec<_>>();
        assert_eq!(hours.len(), 23);
        assert!(hours.contains(&20));
        assert!(!hours.contains(&21));
    }

    /// 2026-07-06T22:00Z, the July 7 MNQ session open at offset -300.
    const WALK_OPEN_NS: u64 = 1_783_375_200_000_000_000;
    /// 2026-07-07T21:00Z, its close. The measured window has to span the WHOLE
    /// session: `close_session` emits complete sessions only, so a shorter
    /// window yields an empty `per_session` and no windowed counts at all.
    const WALK_CLOSE_NS: u64 = 1_783_458_000_000_000_000;

    /// A short crafted session walk through `GeneratedAcc`, parameterized on
    /// the count-window list, returning the finished record's bytes.
    ///
    /// It has to carry real TRADES, and the mechanism is worth stating exactly
    /// because it is not the obvious one. Block 2's cells come from
    /// `window_schedule` over the session segment, INDEPENDENT of prints - so
    /// window keys appear as soon as any session closes. What requires trades
    /// is the session existing at all: `push_trade` is the only thing that
    /// rotates one in, so a walk of quotes alone finishes with an empty
    /// `per_session` and a record no window list can move. The trades span ten
    /// minutes so all five windows this file's two lists mention (1, 5, 15, 60,
    /// 300 s) have something to bin.
    fn windowed_walk_bytes(windows: Option<&'static [i64]>) -> Vec<u8> {
        use mogwai_lab::measure12a::generated::GeneratedAcc;
        use mogwai_protocol::{AggressorSide, QuoteTick, TradeTick};
        use rust_decimal::Decimal;

        let tick = Decimal::new(25, 2);
        let px = |level: i64| Decimal::from(23_000) + tick * Decimal::from(level);
        let end = WALK_CLOSE_NS;
        let mut acc = match windows {
            None => GeneratedAcc::new(1, WALK_OPEN_NS, end, -300, tick),
            Some(w) => GeneratedAcc::new_with_count_windows(1, WALK_OPEN_NS, end, -300, tick, w),
        };
        // A parent every 20 seconds, three prints each: enough occupied
        // 1-second windows for the coarser bins to differ from each other.
        for step in 0..30u64 {
            let ts = WALK_OPEN_NS + step * 20_000_000_000;
            let level = (step % 7) as i64;
            acc.push_quote(
                &QuoteTick {
                    symbol: "MNQ".into(),
                    bid_px: px(level - 1),
                    ask_px: px(level + 1),
                    bid_sz: Decimal::ONE,
                    ask_sz: Decimal::ONE,
                    ts_event: ts,
                },
                // Block 5 selects a forensic minute and refuses one with no
                // traced parent, so every parent here carries a quiet trace.
                Some(mogwai_data::VolTrace {
                    innovation_raw: 0.5 * std::f64::consts::SQRT_2,
                    innovation_std: 0.5,
                    sigma2_candidate: 1.0e-8,
                    sigma2_realized: 1.0e-8,
                    sigma_cap_hit: false,
                    garch_scale: 1.0e-4,
                    base_return_unclipped: 1.0e-4,
                    base_return: 1.0e-4,
                    feedback_clamp_hit: false,
                    session_vol_mult: 1.0,
                    regime_vol_mult: 1.0,
                    pre_realized_return: 0.0,
                    realized_return: 1.0e-4,
                    realized_clamp_hit: false,
                    mid_before: 23_000.0,
                    mid_after: 23_000.0 * (1.0 + 1.0e-4),
                }),
            )
            .expect("quote");
            for print in 0..3u64 {
                acc.push_trade(&TradeTick {
                    symbol: "MNQ".into(),
                    price: px(level),
                    size: Decimal::ONE,
                    aggressor: AggressorSide::Buyer,
                    ts_event: ts + print * 1_000_000_000,
                })
                .expect("trade");
            }
        }
        serde_json::to_vec(&acc.finish().expect("finish")).expect("record")
    }

    /// The window keys a finished record's block-2 cells are binned on, sorted
    /// and deduplicated. This is the artifact-visible face of the seam.
    fn windows_in(bytes: &[u8]) -> Vec<String> {
        let record: Value = serde_json::from_slice(bytes).expect("record");
        let mut seen = std::collections::BTreeSet::new();
        for session in record["per_session"].as_array().expect("per_session") {
            for (_hour, windows) in session["block2"].as_object().expect("block2") {
                for key in windows.as_object().expect("window map").keys() {
                    seen.insert(key.clone());
                }
            }
        }
        seen.into_iter().collect()
    }

    /// The count-window seam: the default constructor really does carry the
    /// frozen 12a list, and the list really does reach the artifact.
    ///
    /// THE SECOND HALF IS WHAT MAKES THE FIRST ONE MEAN ANYTHING. `new`
    /// delegates to `new_with_count_windows` with `COUNT_WINDOWS_S`, so
    /// comparing the two against that same constant goes red on exactly one
    /// class of edit: one that re-points `new` at a different list, which
    /// moves the `default` walk alone. That is a real class - it is what the
    /// test is for - but it can only be OBSERVED if a different list produces
    /// a different artifact in the first place. So the sensitivity is asserted
    /// here rather than assumed: `CURVE_WINDOWS`, the list this file's own
    /// month runs pass through the same seam, must move the bytes.
    #[test]
    fn frozen_12a_path_is_byte_identical_through_the_parameterized_seam() {
        let default = windowed_walk_bytes(None);
        let frozen = windowed_walk_bytes(Some(mogwai_lab::subcontract::COUNT_WINDOWS_S));
        let curve = windowed_walk_bytes(Some(CURVE_WINDOWS));

        // The record has to actually carry windowed counts, or the two lists
        // have nothing to disagree about and the comparisons below are empty.
        // Reported as the window keys the artifact ended up with, because a
        // raw byte diff of these records is 20 KB of noise.
        assert_eq!(
            windows_in(&default),
            vec!["1".to_string(), "5".into(), "60".into()],
            "the default walk did not bin on the frozen 12a windows"
        );
        assert_eq!(
            windows_in(&curve),
            vec![
                "1".to_string(),
                "15".into(),
                "300".into(),
                "5".into(),
                "60".into()
            ],
            "the curve walk did not bin on CURVE_WINDOWS"
        );
        assert!(
            default == frozen,
            "the default constructor no longer carries the frozen 12a window \
             list: {} bytes against {} on windows {:?} against {:?}",
            default.len(),
            frozen.len(),
            windows_in(&default),
            windows_in(&frozen)
        );
        assert!(
            default != curve,
            "the count-window list does not reach the artifact, so nothing here \
             can observe the seam being mis-plumbed"
        );
        assert_eq!(mogwai_lab::subcontract::COUNT_WINDOWS_S, &[1, 5, 60]);
    }
}
