// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Protocol 12b brick N's driver: the `arrival-control` subcommand. The lab
//! crate owns the correction, the walks and the five statistical gates; this
//! module owns the two gates that are not statistics - B1's legacy byte
//! identity and B5's standing build check - the artifact's binding block, and
//! the atomic write.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use anyhow::{anyhow, bail};
use clap::Args;
use mogwai_lab::aggregate::context::ObsContext;
use mogwai_lab::arrival_control::{
    CONTROL_FIT_SEEDS, CONTROL_TEST_SEEDS, GateRec, GeneratedBinding, control_walk, gate_b2,
    gate_b3, gate_b4, gate_b6, gate_b7, gate_hours, hourly_mean_parents, normalizer_drift,
    recentred_curve, seed_median,
};
use mogwai_lab::ledger::{fresh_tree_state, require_clean_tree, sha256_bytes, sha256_file};
use mogwai_lab::sampler::ResourceSampler;
use serde_json::{Value, json};

const DEFAULT_MEASURE: &str = "analysis/mnq-measure-12a.json";
const DEFAULT_ENVELOPE: &str = "analysis/mnq-minute-range-envelope.json";
const DEFAULT_BASELINE: &str = "analysis/out/arrival-control-b1-baseline";
const DEFAULT_AFTER: &str = "analysis/out/arrival-control-b1-after";
const DEFAULT_OUT: &str = "analysis/mnq-arrival-control.json";
const B1_SYMBOLS: [&str; 5] = ["BTCUSDT", "ETHUSDT", "SOLUSDT", "MES", "MNQ"];

#[derive(Args)]
pub struct ArrivalControlArgs {
    /// The committed protocol-12a artifact: the observed side, the exposure
    /// binding and the input hash.
    #[arg(long, value_name = "PATH")]
    pub measure: Option<PathBuf>,
    /// Brick B4's committed minute-range bound.
    #[arg(long, value_name = "PATH")]
    pub envelope: Option<PathBuf>,
    /// The directory holding the five PRE-LANDING legacy tapes gate B1
    /// compares against, produced by the shipped binary at the parent commit.
    #[arg(long, value_name = "DIR")]
    pub b1_baseline: Option<PathBuf>,
    /// Where to write the five AFTER tapes B1 generates before comparing them
    /// byte for byte against the baseline.
    #[arg(long, value_name = "DIR")]
    pub b1_after: Option<PathBuf>,
    /// Where to write the artifact.
    #[arg(long, value_name = "PATH")]
    pub out: Option<PathBuf>,
}

fn gate_json(g: &GateRec) -> Value {
    json!({"passed":g.passed,"evidence":g.evidence,"refusals":g.refusals.iter().map(mogwai_lab::aggregate::RefusalRec::to_json).collect::<Vec<_>>()})
}

/// B5: the driver runs the standing build gate ITSELF and records the exit
/// status and output digests. An operator-typed attestation would let a red
/// check reach `negative-control-passed`, which is the verdict that ends the
/// whole 12b landing; a hard gate a typo satisfies is not a gate. It runs
/// FIRST, before the six minutes of walking, so a red check costs seconds.
/// The program name is a parameter for exactly one reason: the refusal path is
/// testable without a test that mutates `PATH` for the whole process, and
/// without any test ever running the real check - which BUILDS, and so would
/// deadlock on the target-directory lock the test runner already holds.
fn run_b5_with(program: &str) -> anyhow::Result<(Value, f64)> {
    let started = Instant::now();
    let output = Command::new(program)
        .args(["check", "--gate"])
        .output()
        .map_err(|e| anyhow!("B5 refused: cannot spawn brokkr check --gate: {e}"))?;
    let elapsed = started.elapsed().as_secs_f64();
    Ok((
        json!({"command":"brokkr check --gate","exit_status":output.status.code(),"stdout_sha256":sha256_bytes(&output.stdout),"stderr_sha256":sha256_bytes(&output.stderr),"duration_s":elapsed}),
        elapsed,
    ))
}

/// A missing, unreadable or zero-length baseline REFUSES B1 rather than
/// passing it. B1 is a real gate even though this brick touches no generator:
/// the point of it is that it is RUN, so an absent comparand cannot be allowed
/// to read as agreement.
fn require_baseline(path: &Path) -> anyhow::Result<()> {
    let meta = std::fs::metadata(path)
        .map_err(|e| anyhow!("B1 refused: missing baseline {}: {e}", path.display()))?;
    if !meta.is_file() || meta.len() == 0 {
        bail!(
            "B1 refused: baseline {} is empty or not a file",
            path.display()
        );
    }
    Ok(())
}

/// The four paths section 1 freezes for this brick, plus the fingerprint. A
/// brick that moved any of them could not claim a byte-identical tape however
/// the five comparisons came out, so the supporting check below reads the
/// commit's own diff rather than trusting the claim.
const FROZEN_PATHS: [&str; 4] = [
    "crates/mogwai-data/",
    "crates/mogwai-protocol/",
    "crates/mogwai-server/presets/",
    "analysis/fingerprint.json",
];

/// B1's supporting check: `git diff --name-only <parent>..HEAD` touches none of
/// the frozen paths, and `TAPE_PROTOCOL_VERSION` is still 11. Recorded ALONGSIDE
/// the five byte comparisons and never substituted for them - it is a much
/// weaker statement than tape identity, since a generator change outside those
/// paths would pass it - but it is ANDed into B1's verdict rather than merely
/// reported, because a decorative check nobody can fail is not evidence.
///
/// The parent is `HEAD~1`: the artifact runs at the brick's own commit, and the
/// pre-landing binary the baseline tapes came from is that commit's parent. A
/// repository with no parent commit REFUSES rather than passing vacuously.
fn b1_supporting_check() -> anyhow::Result<Value> {
    let out = Command::new("git")
        .args(["diff", "--name-only", "HEAD~1..HEAD"])
        .output()
        .map_err(|e| anyhow!("B1 refused: cannot spawn git diff: {e}"))?;
    if !out.status.success() {
        bail!("B1 refused: git diff HEAD~1..HEAD failed; the parent commit is unreachable");
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let touched: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| FROZEN_PATHS.iter().any(|frozen| line.starts_with(frozen)))
        .collect();
    let version_ok = mogwai_data::TAPE_PROTOCOL_VERSION == 11;
    Ok(json!({
        "command": "git diff --name-only HEAD~1..HEAD",
        "frozen_paths": FROZEN_PATHS,
        "touched_frozen_paths": touched,
        "tape_protocol_version": mogwai_data::TAPE_PROTOCOL_VERSION,
        "passed": touched.is_empty() && version_ok,
    }))
}

/// B1: byte identity of `gen --type trades` output against the pre-landing
/// tapes. The bytes compared are the CLI's OWN bytes on both sides - the
/// driver execs the shipped binary rather than re-serializing in process,
/// because a fresh in-process writer could match the baseline while the real
/// CLI path had drifted, or differ from it while the tape was identical. The
/// binary is `current_exe`, since the driver IS the shipped binary and so
/// cannot disagree with itself about which build ran.
fn run_b1(baseline: &Path, after: &Path) -> anyhow::Result<(Value, f64)> {
    let started = Instant::now();
    let exe = std::env::current_exe()?;
    if !exe.ends_with("target/release/mogwai") {
        bail!("B1 refused: current executable is not target/release/mogwai");
    }
    std::fs::create_dir_all(after)?;
    let mut rows = Vec::new();
    let mut passed = true;
    for symbol in B1_SYMBOLS {
        let before = baseline.join(format!("{symbol}.csv"));
        require_baseline(&before)?;
        let out = after.join(format!("{symbol}.csv"));
        let argv = vec![
            "gen",
            "--type",
            "trades",
            "--symbol",
            symbol,
            "--seed",
            "7",
            "--length",
            "2d",
            "--start",
            "1782856800000000000",
            "--out",
            out.to_str().ok_or_else(|| anyhow!("non-UTF8 B1 path"))?,
        ];
        let status = Command::new(&exe).args(&argv).status()?;
        if !status.success() {
            bail!("B1 refused: {symbol} generation exited {status}");
        }
        let a = std::fs::read(&before)?;
        let b = std::fs::read(&out)?;
        let identical = a == b;
        passed &= identical;
        rows.push(json!({"symbol":symbol,"argv":argv,"baseline_sha256":sha256_bytes(&a),"after_sha256":sha256_bytes(&b),"identical":identical}));
    }
    let supporting = b1_supporting_check()?;
    passed &= supporting["passed"] == true;
    Ok((
        json!({"passed":passed,"evidence":{"tapes":rows,"supporting":supporting},"refusals":[]}),
        started.elapsed().as_secs_f64(),
    ))
}

/// Refuses a dirty tree BEFORE reading any input, exactly as
/// `minute_range_envelope::run` does and for the same reason: an artifact may
/// only bind a commit that is exactly the code that ran. Re-attests the tree
/// from git immediately BEFORE the atomic write, so a HEAD move or an edit
/// during the walking unbinds the artifact rather than being recorded as clean.
/// Public for the scratch-path regression tests.
pub fn run(args: ArrivalControlArgs) -> anyhow::Result<Value> {
    run_with(args, Seams::production())
}

/// The artifact run's test seams, and the whole of them. Production is
/// [`Seams::production`] and `run` is the only caller that can construct it, so
/// no command-line surface can reach any of the substitutions below - they exist
/// because two of this brick's pins are otherwise unbuildable, not because the
/// operator has a use for them.
///
/// The mid-run re-attestation pin (spec section 2.9) is the reason all four
/// fields exist at once: it needs a CLEAN tree to get past step 1, so it cannot
/// piggyback on the dirty-tree tests; it must not spawn the real `brokkr check
/// --gate`, which builds and would deadlock on the target lock the test runner
/// already holds; it has no pre-landing baseline tapes to compare against; and
/// at the committed 2674800 s window it would cost six minutes to prove a
/// property that has nothing to do with window length.
struct Seams {
    /// The program B5 spawns. Production is `brokkr`.
    b5_program: String,
    /// Whether B1 execs the five `gen --type trades` subprocesses. False only
    /// in the re-attestation pin, which is about step 10, not about B1.
    run_b1: bool,
    /// Replaces the 12a binding's measured window and warmup, so a pin about
    /// step ordering costs seconds instead of minutes.
    window: Option<(u64, String)>,
    /// Fires after the walks and before the gates - the widest part of the
    /// window section 2.9 is about, where an artifact bound to the commit read
    /// at step 1 would be binding code that is no longer there.
    mid_run: Option<Box<dyn Fn()>>,
}

impl Seams {
    fn production() -> Self {
        Self {
            b5_program: "brokkr".to_string(),
            run_b1: true,
            window: None,
            mid_run: None,
        }
    }
}

fn run_with(args: ArrivalControlArgs, seams: Seams) -> anyhow::Result<Value> {
    let total = Instant::now();
    let commit = require_clean_tree().map_err(|e| anyhow!("{e}"))?;
    let sampler = ResourceSampler::start(Vec::new(), None);
    let (b5_ev, b5_s) = run_b5_with(&seams.b5_program)?;
    let b5_passed = b5_ev["exit_status"].as_i64() == Some(0);
    let baseline = args.b1_baseline.unwrap_or_else(|| DEFAULT_BASELINE.into());
    let after = args.b1_after.unwrap_or_else(|| DEFAULT_AFTER.into());
    let (b1, b1_s) = if seams.run_b1 {
        run_b1(&baseline, &after)?
    } else {
        (
            json!({"passed":false,"evidence":"B1 was not run","refusals":[]}),
            0.0,
        )
    };
    let measure_path = args.measure.unwrap_or_else(|| DEFAULT_MEASURE.into());
    let envelope_path = args.envelope.unwrap_or_else(|| DEFAULT_ENVELOPE.into());
    let measure: Value = serde_json::from_slice(&std::fs::read(&measure_path)?)?;
    let envelope: Value = serde_json::from_slice(&std::fs::read(&envelope_path)?)?;
    let mut binding = GeneratedBinding::from_measure12a(&measure)?;
    if let Some((length_ns, warmup)) = seams.window {
        binding.window_length_ns = length_ns;
        binding.warmup = warmup;
    }
    let per = measure["observed"]["per_session"]
        .as_array()
        .ok_or_else(|| anyhow!("observed.per_session is missing"))?
        .clone();
    let obs = ObsContext::new(per);
    let profile = mogwai_server::config::profile_from_preset("MNQ")?;
    let hours = gate_hours(&profile)?;
    // Derived from the same calendar the index set is, so a calendar change
    // moves both together rather than leaving a hardcoded 21 to contradict it.
    let unexposed: Vec<i64> = (0..24_i64).filter(|h| !hours.contains(h)).collect();
    let observed = hourly_mean_parents(&obs);
    let scratch = PathBuf::from("target/arrival-control");
    let fit_started = Instant::now();
    let mut fit_walks = Vec::new();
    for seed in CONTROL_FIT_SEEDS {
        fit_walks.push(control_walk(&scratch, &binding, None, seed)?);
    }
    let fit_s = fit_started.elapsed().as_secs_f64();
    // Once per seed, not once per (seed, hour): `hourly_mean_parents` scans
    // every block1 row of all 22 sessions, so hoisting it out of the hour loop
    // is a 23x saving on a scan that is not cheap.
    let fit_means: Vec<_> = fit_walks
        .iter()
        .map(|w| hourly_mean_parents(&w.ctx))
        .collect();
    let mut ratios = BTreeMap::new();
    let mut ratio_json = serde_json::Map::new();
    for &h in &hours {
        let readings: [Option<f64>; 4] = std::array::from_fn(|i| fit_means[i][&h].mean);
        let generated = seed_median(&readings);
        let observed_mean = observed[&h].mean;
        let ratio = generated
            .zip(observed_mean)
            .and_then(|(g, o)| (o > 0.0).then_some(g / o));
        if let Some(r) = ratio {
            ratios.insert(h, r);
        }
        ratio_json.insert(h.to_string(),json!({"generated_mean":generated,"observed_mean":observed_mean,"ratio":ratio,"generated_per_seed":readings,"dropped_nonfinite":readings.iter().filter(|v|!v.is_some_and(f64::is_finite)).count()}));
    }
    let old = profile.session.intensity_hour;
    let new = recentred_curve(&old, &ratios);
    let drift = normalizer_drift(&old, &new, &profile)?;
    let test_started = Instant::now();
    let mut tests = Vec::new();
    for seed in CONTROL_TEST_SEEDS {
        tests.push(control_walk(&scratch, &binding, Some(&new), seed)?);
    }
    let test_s = test_started.elapsed().as_secs_f64();
    if let Some(hook) = &seams.mid_run {
        hook();
    }
    let mut gates = serde_json::Map::new();
    gates.insert("B1".into(), b1);
    gates.insert("B2".into(), gate_json(&gate_b2(&obs, &tests, &hours)?));
    gates.insert("B3".into(), gate_json(&gate_b3(&obs, &tests, &hours)?));
    gates.insert("B4".into(), gate_json(&gate_b4(&envelope, &tests)?));
    gates.insert(
        "B5".into(),
        json!({"passed":b5_passed,"evidence":b5_ev,"refusals":[]}),
    );
    gates.insert("B6".into(), gate_json(&gate_b6(&obs, &tests, &hours)?));
    gates.insert("B7".into(), gate_json(&gate_b7(&obs, &tests, &hours)?));
    let failing: Vec<String> = gates
        .iter()
        .filter(|(_, v)| v["passed"] != true)
        .map(|(k, _)| k.clone())
        .collect();
    let (peak_rss, _) = sampler.stop(&[], None)?;
    let (head, clean) = fresh_tree_state().map_err(|e| anyhow!("{e}"))?;
    if !clean || head != commit {
        bail!("the tree changed during the arrival-control run; the artifact is unbound");
    }
    let out = args.out.unwrap_or_else(|| DEFAULT_OUT.into());
    let artifact = json!({"binding":{"harness_tree_commit":commit,"clean_tree":true,"input_hashes":{measure_path.to_string_lossy().to_string():sha256_file(&measure_path)?,envelope_path.to_string_lossy().to_string():sha256_file(&envelope_path)?},"exposure":{"instrument":"MNQ","preset":"crates/mogwai-server/presets/mnq.toml","window_start_ns":binding.window_start_ns,"window_length_ns":binding.window_length_ns,"warmup":binding.warmup,"divergence":Value::Null,"regime":"neutral"},"control_fit_seeds":CONTROL_FIT_SEEDS,"control_test_seeds":CONTROL_TEST_SEEDS,"gate_hours":hours,"unexposed_hours":unexposed,"tape_protocol_version":mogwai_data::TAPE_PROTOCOL_VERSION,"spec":"notes/protocol-12b-arrival-composition-spec.md section 5.5, brick N"},"ratios":ratio_json,"old_curve":old,"new_curve":new,"normalizer_drift":drift,"gates":gates,"verdict":if failing.is_empty(){"negative-control-passed"}else{"negative-control-failed"},"failing_gates":failing,"cost":{"fit_walk_s":fit_s,"test_walk_s":test_s,"b1_s":b1_s,"b5_s":b5_s,"total_s":total.elapsed().as_secs_f64(),"peak_rss_bytes":peak_rss}});
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = out.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&artifact)?)?;
    std::fs::rename(tmp, &out)?;
    println!("arrival control artifact -> {}", out.display());
    Ok(artifact)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether this tree can be walked into `run` without the six-minute
    /// artifact run actually starting. The refusal tests below all depend on
    /// the development tree being dirty, exactly as the `minute_range_envelope`
    /// sibling does; on a clean tree they would launch the real run, which
    /// spawns `brokkr check --gate` from inside a cargo invocation that already
    /// holds the target lock.
    fn tree_is_dirty() -> bool {
        require_clean_tree().is_err()
    }

    #[test]
    fn arrival_control_refuses_a_dirty_tree_before_reading_inputs() {
        if !tree_is_dirty() {
            return;
        }
        let err = run(ArrivalControlArgs {
            measure: Some("no/such/measure.json".into()),
            envelope: Some("no/such/envelope.json".into()),
            b1_baseline: None,
            b1_after: None,
            out: Some("target/arrival-control-test.json".into()),
        })
        .expect_err("this development tree is deliberately dirty");
        // The missing input paths above are the point: the refusal must come
        // from the tree, before a byte of either artifact is read.
        assert!(err.to_string().contains("working tree is dirty"));
    }

    #[test]
    fn arrival_control_refuses_a_b5_that_cannot_be_spawned() {
        let err = run_b5_with("brokkr-no-such-program-on-this-machine")
            .expect_err("an unspawnable gate command must refuse, not pass");
        assert!(err.to_string().contains("B5 refused"));
    }

    #[test]
    fn arrival_control_refuses_a_missing_b1_baseline_rather_than_passing_b1() {
        let missing = require_baseline(Path::new("target/no-such-b1-baseline/MNQ.csv"))
            .expect_err("a missing baseline must refuse");
        assert!(missing.to_string().contains("B1 refused"));
        let empty_dir = Path::new("target/arrival-control-empty-baseline");
        std::fs::create_dir_all(empty_dir).unwrap();
        let empty = empty_dir.join("MNQ.csv");
        std::fs::write(&empty, b"").unwrap();
        let err = require_baseline(&empty).expect_err("a zero-length baseline must refuse");
        assert!(err.to_string().contains("empty or not a file"));
    }

    /// The workspace root: a unit test's working directory is its crate, and
    /// every default path this driver carries is relative to the repository.
    fn repo_root() -> PathBuf {
        let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        dir.pop();
        dir.pop();
        dir
    }

    /// Spec section 2.9: `require_clean_tree` runs before any input is read and
    /// the artifact is written minutes later, so the run re-attests the tree
    /// immediately before serializing and bails UNBOUND if HEAD moved or the
    /// tree went dirty in between. The pin dirties the tree in exactly that
    /// window - after the walks, before the gates - and asserts both that the
    /// run refuses and that nothing reached disk.
    ///
    /// `#[ignore]`d because it needs the opposite precondition to its dirty-tree
    /// siblings: a CLEAN tree, which the development tree usually is not. Run it
    /// with `brokkr test -p mogwai-cli arrival_control_refuses_a_tree_that_changed_during_the_run`
    /// from a committed tree.
    #[test]
    #[ignore = "needs a CLEAN git tree and runs eight short walks; run explicitly"]
    fn arrival_control_refuses_a_tree_that_changed_during_the_run() {
        let root = repo_root();
        require_clean_tree()
            .expect("this pin needs a clean tree: commit or stash before running it");
        // Untracked at the repository root, so `git status --porcelain` reports
        // it and no .gitignore rule can hide it.
        let probe = root.join("arrival-control-midrun-probe.txt");
        let out = root.join("target/arrival-control-midrun.json");
        drop(std::fs::remove_file(&out));
        let planted = probe.clone();
        let result = run_with(
            ArrivalControlArgs {
                measure: Some(root.join(DEFAULT_MEASURE)),
                envelope: Some(root.join(DEFAULT_ENVELOPE)),
                b1_baseline: None,
                b1_after: None,
                out: Some(out.clone()),
            },
            Seams {
                b5_program: "true".to_string(),
                run_b1: false,
                // One hour, no warmup. Eight walks of two passes each run
                // here, so the window is an order shorter than the lab's
                // one-day `the_control_walk_pair_replays_one_tape` pin: this
                // is a pin about step ordering, and the shortest window that
                // still produces real block records serves it exactly as well.
                window: Some((3_600_000_000_000, "0s".to_string())),
                mid_run: Some(Box::new(move || {
                    std::fs::write(&planted, b"dirty\n").expect("planting the probe");
                })),
            },
        );
        drop(std::fs::remove_file(&probe));
        let err = result.expect_err("a tree that moved mid-run must unbind the artifact");
        assert!(
            err.to_string().contains("the artifact is unbound"),
            "wrong refusal: {err}"
        );
        assert!(
            !out.exists(),
            "an unbound run still wrote {}",
            out.display()
        );
    }

    /// Inapplicability is ABSENCE, not a recorded pass: the control has no
    /// cadence grid for B8 to be sensitive to. Pins the committed artifact once
    /// the run has produced it; before then there is nothing to contradict.
    #[test]
    fn the_control_artifact_carries_no_b8_field() {
        let Ok(bytes) = std::fs::read(DEFAULT_OUT) else {
            return;
        };
        let artifact: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(artifact["gates"].get("B8").is_none());
        for name in ["B1", "B2", "B3", "B4", "B5", "B6", "B7"] {
            assert!(artifact["gates"].get(name).is_some(), "{name} is missing");
        }
    }
}
