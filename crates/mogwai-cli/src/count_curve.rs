// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Stage 0 of the signed count-curve preregistration. This is deliberately a
//! thin comparator around the unchanged protocol-12a generated walk and Block
//! 2 pooler. It does not implement the later count-curve measurement.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, anyhow, bail};
use clap::Args;
use mogwai_lab::aggregate::artifact::write_json_atomic;
use serde_json::{Value, json};

use crate::measure::run_final_walk;

const OUT: &str = "analysis/out/count-curve-measurement.json";
const PRESET_PATH: &str = "crates/mogwai-server/presets/mnq.toml";
const FINGERPRINT_PATH: &str = "analysis/fingerprint.json";
const HISTORICAL_PRESET: &str = "46622ce226922d96457fcc0ea57411b63b5d7f0f";
const CURRENT_PRESET: &str = "c1b352efbc35c878dd3cc75cb282fa29fde57f6a";
const HISTORICAL_FINGERPRINT: &str = "f63d9570d5cad4b2ca6c109a439dbbc48311c122";
const CURRENT_FINGERPRINT: &str = "19238d94ab0747f86fcdd4635889964e576972db";
const HORIZONS: [u64; 3] = [1, 5, 60];
const FIELDS: [&str; 3] = ["scheduled_windows", "zero_windows", "count_hist"];

#[derive(Args)]
pub struct CountCurveArgs {
    /// Run the generated-only signed Stage 0 backcheck.
    #[arg(long, required = true)]
    stage0: bool,
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

pub fn run(args: &CountCurveArgs) -> anyhow::Result<()> {
    if !args.stage0 {
        bail!("count-curve currently implements only --stage0");
    }
    let reference: Value =
        serde_json::from_str(include_str!("../../../analysis/mnq-measure-12a.json"))?;
    let exposure = Exposure::from_reference(&reference)?;
    let commit = executing_commit()?;
    let actual_identity = InputIdentity {
        preset: git_blob_id(
            mogwai_server::config::preset_document("MNQ")
                .ok_or_else(|| anyhow!("the embedded MNQ preset is absent"))?
                .as_bytes(),
        )?,
        fingerprint: git_blob_id(include_bytes!("../../../analysis/fingerprint.json"))?,
    };
    let expected_identity = InputIdentity {
        preset: CURRENT_PRESET.into(),
        fingerprint: CURRENT_FINGERPRINT.into(),
    };
    let binding = binding_json(&exposure, &commit, &actual_identity);

    if actual_identity != expected_identity || !exposure.is_frozen() {
        let artifact = input_mismatch_artifact(&binding);
        write_artifact(Path::new(OUT), &artifact)?;
        println!("artifact -> {OUT}");
        println!("verdict: execution_input_mismatch");
        return Ok(());
    }

    for seed in &exposure.seeds {
        let walk = run_final_walk(*seed)?;
        let sessions = walk["per_session"]
            .as_array()
            .ok_or_else(|| anyhow!("seed {seed} walk carries no per_session array"))?;
        let blocks = mogwai_lab::aggregate::monthly::blocks_from_sessions(sessions)
            .map_err(|error| anyhow!(error.to_string()))?;
        let expected_seed = reference["generated"]["per_seed"]
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["seed"].as_u64() == Some(*seed)))
            .ok_or_else(|| anyhow!("committed artifact is missing seed {seed}"))?;
        if let Some(divergence) =
            first_divergence(*seed, &expected_seed["blocks"]["block2"], &blocks["block2"])?
        {
            let artifact = json!({
                "binding": binding,
                "verdict": "generated_identity_mismatch_unattributed",
                "first_divergence": divergence,
            });
            write_artifact(Path::new(OUT), &artifact)?;
            println!("artifact -> {OUT}");
            println!("verdict: generated_identity_mismatch_unattributed");
            return Ok(());
        }
        println!("seed {seed} matched exactly");
    }

    let artifact = json!({"binding": binding, "verdict": "passed_exactly"});
    write_artifact(Path::new(OUT), &artifact)?;
    println!("artifact -> {OUT}");
    println!("verdict: passed_exactly");
    Ok(())
}

impl Exposure {
    fn from_reference(reference: &Value) -> anyhow::Result<Self> {
        let generated = &reference["binding"]["generated"];
        let seeds = generated["seeds"]
            .as_array()
            .ok_or_else(|| anyhow!("binding.generated.seeds is absent"))?
            .iter()
            .map(|seed| {
                seed.as_u64()
                    .ok_or_else(|| anyhow!("a bound seed is not u64"))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(Self {
            seeds,
            start_ns: generated["window_start_ns"]
                .as_u64()
                .ok_or_else(|| anyhow!("binding.generated.window_start_ns is absent"))?,
            length_ns: generated["window_length_ns"]
                .as_u64()
                .ok_or_else(|| anyhow!("binding.generated.window_length_ns is absent"))?,
            warmup: generated["warmup"]
                .as_str()
                .ok_or_else(|| anyhow!("binding.generated.warmup is absent"))?
                .to_string(),
        })
    }

    fn is_frozen(&self) -> bool {
        self.seeds == (1..=8).collect::<Vec<_>>()
            && self.start_ns
                == u64::try_from(mogwai_lab::subcontract::FINAL_START_NS)
                    .ok()
                    .unwrap_or_default()
            && self.length_ns
                == u64::try_from(
                    mogwai_lab::subcontract::FINAL_END_NS - mogwai_lab::subcontract::FINAL_START_NS,
                )
                .ok()
                .unwrap_or_default()
            && self.warmup == mogwai_lab::subcontract::SUMMARY_WARMUP
    }
}

fn binding_json(exposure: &Exposure, commit: &str, actual: &InputIdentity) -> Value {
    json!({
        "blob_ids": {
            (PRESET_PATH): {"historical": HISTORICAL_PRESET, "stage0_bound": CURRENT_PRESET, "executed": actual.preset},
            (FINGERPRINT_PATH): {"historical": HISTORICAL_FINGERPRINT, "stage0_bound": CURRENT_FINGERPRINT, "executed": actual.fingerprint},
        },
        "generated": {
            "seeds": exposure.seeds,
            "window_start_ns": exposure.start_ns,
            "window_length_ns": exposure.length_ns,
            "warmup": exposure.warmup,
        },
        "executing_commit": commit,
        "tape_protocol_version": mogwai_data::TAPE_PROTOCOL_VERSION,
    })
}

fn input_mismatch_artifact(binding: &Value) -> Value {
    json!({"binding": binding, "verdict": "execution_input_mismatch"})
}

fn first_divergence(seed: u64, expected: &Value, actual: &Value) -> anyhow::Result<Option<Value>> {
    let expected_hours = expected
        .as_object()
        .ok_or_else(|| anyhow!("expected Block 2 is not an object"))?;
    let mut hours = expected_hours
        .keys()
        .map(|hour| hour.parse::<u64>().map_err(anyhow::Error::from))
        .collect::<anyhow::Result<Vec<_>>>()?;
    hours.sort_unstable();
    for hour in hours {
        for horizon in HORIZONS {
            for field in FIELDS {
                let expected_value = &expected[hour.to_string()][horizon.to_string()][field];
                let actual_value = &actual[hour.to_string()][horizon.to_string()][field];
                if expected_value != actual_value {
                    return Ok(Some(json!({
                        "seed": seed,
                        "hour": hour,
                        "horizon_s": horizon,
                        "field": field,
                        "expected": expected_value,
                        "actual": actual_value,
                    })));
                }
            }
        }
    }
    Ok(None)
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
    write_json_atomic(path, artifact)
        .map_err(|error| anyhow!("writing {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perturbed_comparator_reports_the_first_divergence() {
        let expected = json!({"0":{"1":{"scheduled_windows":2,"zero_windows":1,"count_hist":{"0":1,"1":1}},"5":{"scheduled_windows":1,"zero_windows":0,"count_hist":{"2":1}},"60":{"scheduled_windows":1,"zero_windows":0,"count_hist":{"3":1}}}});
        let mut actual = expected.clone();
        actual["0"]["1"]["zero_windows"] = json!(0);
        let divergence = first_divergence(3, &expected, &actual)
            .expect("comparison")
            .expect("a divergence");
        assert_eq!(divergence["seed"], 3);
        assert_eq!(divergence["hour"], 0);
        assert_eq!(divergence["horizon_s"], 1);
        assert_eq!(divergence["field"], "zero_windows");
        assert_eq!(divergence["expected"], 1);
        assert_eq!(divergence["actual"], 0);
    }

    #[test]
    fn wrong_input_identity_has_no_statistics() {
        let exposure = Exposure {
            seeds: (1..=8).collect(),
            start_ns: 1,
            length_ns: 2,
            warmup: "3d".into(),
        };
        let wrong = InputIdentity {
            preset: "wrong".into(),
            fingerprint: CURRENT_FINGERPRINT.into(),
        };
        assert_ne!(wrong.preset, CURRENT_PRESET);
        let artifact = input_mismatch_artifact(&binding_json(&exposure, "commit", &wrong));
        assert_eq!(artifact["verdict"], "execution_input_mismatch");
        assert!(artifact.get("statistics").is_none());
        assert!(artifact.get("first_divergence").is_none());
    }
}
