// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Protocol 12b brick N's driver: the `arrival-control` subcommand. The lab
//! crate owns the correction, the walks and the five statistical gates; this
//! module owns the two gates that are not statistics - B1's legacy byte
//! identity and B5's standing build check - the artifact's binding block, and
//! the atomic write.
//!
//! This binary runs no build tooling. B1 execs the shipped `mogwai gen`, which
//! is this same binary and the only thing that can produce the bytes B1
//! compares; B5 reads a transcript the operator captured from the standing
//! build gate and never runs that gate itself. See [`read_b5_log`] for why the
//! asymmetry is deliberate rather than an inconsistency.

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
const DEFAULT_B5_LOG: &str = "analysis/out/arrival-control-b5-gate.log";
// ETHUSDT and SOLUSDT left this list with their presets (owner ruling
// 2026-08-09, amended into the 12b spec): both were BTCUSDT aliases with
// identical generator paths, and at THIS gate's CSV layer their digests
// grouped with BTCUSDT's, so B1 exercised three distinct tapes then and
// exercises the same three now.
const B1_SYMBOLS: [&str; 3] = ["BTCUSDT", "MES", "MNQ"];

#[derive(Args)]
pub struct ArrivalControlArgs {
    /// The committed protocol-12a artifact: the observed side, the exposure
    /// binding and the input hash.
    #[arg(long, value_name = "PATH")]
    pub measure: Option<PathBuf>,
    /// Brick B4's committed minute-range bound.
    #[arg(long, value_name = "PATH")]
    pub envelope: Option<PathBuf>,
    /// The directory holding the per-symbol PRE-LANDING legacy tapes gate B1
    /// compares against, produced by the shipped binary at the parent commit.
    #[arg(long, value_name = "DIR")]
    pub b1_baseline: Option<PathBuf>,
    /// Where to write the per-symbol AFTER tapes B1 generates before comparing
    /// them
    /// byte for byte against the baseline.
    #[arg(long, value_name = "DIR")]
    pub b1_after: Option<PathBuf>,
    /// The commit the baseline tapes were generated from, which bounds B1's
    /// supporting frozen-path diff. Defaults to `HEAD~1`.
    #[arg(long, value_name = "COMMIT")]
    pub b1_baseline_commit: Option<String>,
    /// The captured output of an externally-run standing build gate, which is
    /// gate B5's evidence. This binary never runs that gate itself: see
    /// [`read_b5_log`].
    #[arg(long, value_name = "PATH")]
    pub b5_log: Option<PathBuf>,
    /// Where to write the artifact.
    #[arg(long, value_name = "PATH")]
    pub out: Option<PathBuf>,
}

fn gate_json(g: &GateRec) -> Value {
    json!({"passed":g.passed,"evidence":g.evidence,"refusals":g.refusals.iter().map(mogwai_lab::aggregate::RefusalRec::to_json).collect::<Vec<_>>()})
}

/// B5: the driver READS the standing build gate's output; it never runs it.
///
/// The gate is local development tooling; `mogwai` is the shipped venue
/// binary. A shipped binary that shells out to the developer's build tool is a
/// layering inversion twice over: on a clone without that tool installed the
/// command cannot produce an artifact at all, and under this repository's own
/// convention - every command runs through the tool - the spawned child blocks
/// forever on the workspace lock its own parent is holding. That is not a
/// hazard to detect and route around; it is a design that must not exist, so
/// the subprocess is gone. The workspace lint forbidding the tool's name in
/// Rust source is what keeps it gone.
///
/// What replaces it is the gate's own machine-readable verdict. The operator
/// runs the gate with its `--json` flag and captures the output; this function
/// refuses unless that capture exists and its LAST line parses as the gate's
/// versioned summary object. B5 passes on `verdict == "complete"`.
///
/// Reading the summary rather than grepping the human output is deliberate.
/// The prose lines are console formatting and change freely; `--json` is a
/// declared contract carrying `schema`, and it reports `failed_phase`, so a
/// red run says WHICH phase failed instead of being inferred from an error
/// marker appearing somewhere in the text. A transcript from a run that died
/// partway has no summary line at all and so cannot read as a pass, which is
/// what the first draft's `--b5-green-at <commit>` flag could not manage: a
/// commit string matches whether or not the gate was ever run.
fn read_b5_log(path: &Path) -> anyhow::Result<(Value, f64)> {
    let started = Instant::now();
    let bytes = std::fs::read(path).map_err(|e| {
        anyhow!(
            "B5 refused: no gate transcript at {}: {e}. Run the standing build gate with its \
             --json flag and capture the output there before running this command; see AGENTS.md \
             for the command.",
            path.display()
        )
    })?;
    let text = String::from_utf8_lossy(&bytes);
    let summary: Value = text
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .and_then(|line| serde_json::from_str(line).ok())
        .ok_or_else(|| {
            anyhow!(
                "B5 refused: {} carries no machine-readable summary on its last line. Capture \
                 the gate's output with its --json flag; a transcript without a summary is \
                 either from a run that died partway or from a run without that flag.",
                path.display()
            )
        })?;
    Ok((
        json!({"gate":"the standing build gate","transcript":path.to_string_lossy(),"transcript_sha256":sha256_bytes(&bytes),"transcript_bytes":bytes.len(),"summary":summary,"run_by":"the operator, before this command - never spawned from the venue binary"}),
        started.elapsed().as_secs_f64(),
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
/// the per-symbol comparisons came out, so the supporting check below reads
/// the commit's own diff rather than trusting the claim.
const FROZEN_PATHS: [&str; 4] = [
    "crates/mogwai-data/",
    "crates/mogwai-protocol/",
    "crates/mogwai-server/presets/",
    "analysis/fingerprint.json",
];

/// B1's supporting check: `git diff --name-only <parent>..HEAD` touches none of
/// the frozen paths, and `TAPE_PROTOCOL_VERSION` still reads what it read at the
/// baseline commit. Recorded ALONGSIDE
/// the per-symbol byte comparisons and never substituted for them - it is a much
/// weaker statement than tape identity, since a generator change outside those
/// paths would pass it - but it is ANDed into B1's verdict rather than merely
/// reported, because a decorative check nobody can fail is not evidence.
///
/// THE ACCEPTED IDENTITY IS DERIVED, NOT WRITTEN DOWN, and that is a repair
/// rather than a refinement. This check spent its whole life comparing the
/// constant against a hand-edited literal, re-baselined 14 to 16 to 17 to 18 as
/// the bumps landed, and then the literal stopped being edited: three further
/// bumps took the constant to 20 while the check still demanded 18, so B1 could
/// only ever fail, whatever the per-symbol byte comparisons said. That is the
/// exact failure [`is_non_shipping`] records having found once already - a
/// supporting check contradicting the evidence it supports - and a decorative
/// check nobody can PASS is worse than one nobody can fail. Nothing detected it:
/// `tape_version_prose.rs` reads markdown, so an executable statement of a live
/// tape identity is invisible to it.
///
/// So the baseline version is read out of the baseline commit, the same way and
/// for the same reason the baseline COMMIT is an argument rather than a
/// hardcoded `HEAD~1`. It also states something stronger than the literal did:
/// the tapes B1 compares came from that commit's binary, so what matters is that
/// no bump landed between it and HEAD, which a literal cannot express and a
/// derivation cannot get stale. A baseline commit whose constant cannot be read
/// REFUSES, on [`require_baseline`]'s doctrine - an unreadable comparand must
/// not read as agreement.
///
/// The baseline commit is an ARGUMENT rather than a hardcoded `HEAD~1`. It has
/// to be: `HEAD~1` is only the pre-landing boundary while the brick's landing
/// is the tip, and any follow-up commit - a repair, a lint sweep - silently
/// moves the range off the commit the baseline tapes actually came from. It
/// defaults to `HEAD~1` for the ordinary case and is stated explicitly in the
/// artifact, so the range is auditable rather than assumed. An unreachable
/// commit REFUSES rather than passing vacuously.
fn b1_supporting_check(baseline_commit: &str) -> anyhow::Result<Value> {
    let range = format!("{baseline_commit}..HEAD");
    let out = Command::new("git")
        .args(["diff", "--name-only", &range])
        .output()
        .map_err(|e| anyhow!("B1 refused: cannot spawn git diff: {e}"))?;
    if !out.status.success() {
        bail!("B1 refused: git diff {range} failed; {baseline_commit} is unreachable");
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let frozen: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| FROZEN_PATHS.iter().any(|frozen| line.starts_with(frozen)))
        .collect();
    // A test, bench or example target inside a frozen crate cannot reach the
    // shipped tape: none of them is linked into the binary that writes one.
    // Splitting them OUT of the verdict rather than out of the record is
    // deliberate - the paths are still reported, so a reader sees exactly what
    // moved and can disagree with this reasoning.
    let (non_shipping, shipping): (Vec<&str>, Vec<&str>) =
        frozen.iter().partition(|p| is_non_shipping(p));
    let baseline_version = tape_version_at(baseline_commit)?;
    let version_ok = mogwai_data::TAPE_PROTOCOL_VERSION == baseline_version;
    Ok(json!({
        "command": format!("git diff --name-only {range}"),
        "baseline_commit": baseline_commit,
        "frozen_paths": FROZEN_PATHS,
        "touched_frozen_shipping_paths": shipping,
        "touched_frozen_non_shipping_paths": non_shipping,
        "tape_protocol_version": mogwai_data::TAPE_PROTOCOL_VERSION,
        "baseline_tape_protocol_version": baseline_version,
        "passed": shipping.is_empty() && version_ok,
    }))
}

/// The path and the declaration `TAPE_PROTOCOL_VERSION` is written at, as this
/// check has to read them out of an OLD commit rather than out of the tree.
/// Reading the file at a revision is the only way to learn what the constant was
/// then: the compiled-in value is HEAD's by construction, and comparing it
/// against itself would pass vacuously.
const TAPE_VERSION_PATH: &str = "crates/mogwai-data/src/lib.rs";
const TAPE_VERSION_DECL: &str = "pub const TAPE_PROTOCOL_VERSION: u32 = ";

/// `TAPE_PROTOCOL_VERSION` as it stood at `commit`, read from that revision's
/// copy of the declaring file.
///
/// Every failure REFUSES rather than defaulting: an unreachable commit, a
/// revision predating the file, a declaration this parser cannot find. A silent
/// fallback here would put the check straight back where it was - passing or
/// failing for a reason unrelated to the tape.
fn tape_version_at(commit: &str) -> anyhow::Result<u32> {
    let spec = format!("{commit}:{TAPE_VERSION_PATH}");
    let out = Command::new("git")
        .args(["show", &spec])
        .output()
        .map_err(|e| anyhow!("B1 refused: cannot spawn git show: {e}"))?;
    if !out.status.success() {
        bail!("B1 refused: cannot read {spec}; {commit} is unreachable or predates the file");
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let value = text
        .lines()
        .find_map(|line| line.trim().strip_prefix(TAPE_VERSION_DECL))
        .and_then(|rest| rest.trim_end_matches(';').trim().parse::<u32>().ok())
        .ok_or_else(|| {
            anyhow!(
                "B1 refused: no `{TAPE_VERSION_DECL}<N>;` declaration in {spec}. The constant \
                 moved or was respelled; teach this check where it lives before running the gate."
            )
        })?;
    Ok(value)
}

/// Whether a repository path is outside the shipped library and so cannot
/// contribute a byte to a generated tape: a `cfg(test)` module, an integration
/// test, a bench or an example target. None of them is linked into the binary
/// that writes a tape.
///
/// The `examples/` arm is not hypothetical tidiness. The first artifact run
/// failed B1 on `crates/mogwai-data/examples/fill_walk_bench.rs`, a criterion
/// bench target whose only change was a comment, while every tape
/// comparisons reported byte identity. A supporting check that contradicts the
/// evidence it supports is worse than no supporting check.
fn is_non_shipping(path: &str) -> bool {
    path.ends_with("/tests.rs")
        || path.contains("/tests/")
        || path.contains("/benches/")
        || path.contains("/examples/")
}

/// B1: byte identity of `gen --type trades` output against the pre-landing
/// tapes. The bytes compared are the CLI's OWN bytes on both sides - the
/// driver execs the shipped binary rather than re-serializing in process,
/// because a fresh in-process writer could match the baseline while the real
/// CLI path had drifted, or differ from it while the tape was identical. The
/// binary is `current_exe`, since the driver IS the shipped binary and so
/// cannot disagree with itself about which build ran.
fn run_b1(baseline: &Path, after: &Path, baseline_commit: &str) -> anyhow::Result<(Value, f64)> {
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
    let supporting = b1_supporting_check(baseline_commit)?;
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
/// piggyback on the dirty-tree tests; it has no gate transcript and no
/// pre-landing baseline tapes; and at the committed 2674800 s window it would
/// cost six minutes to prove a property that has nothing to do with window
/// length.
struct Seams {
    /// Whether B5's transcript is read at all. False only in the
    /// re-attestation pin, which is about step ordering rather than B5.
    read_b5: bool,
    /// Whether B1 execs the per-symbol `gen --type trades` subprocesses. False
    /// only
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
            read_b5: true,
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
    let (b5_ev, b5_s) = if seams.read_b5 {
        read_b5_log(&args.b5_log.unwrap_or_else(|| DEFAULT_B5_LOG.into()))?
    } else {
        (json!({"gate":"the standing build gate","read":false}), 0.0)
    };
    // A transcript that did not complete, or that carries an error line, fails
    // B5 rather than refusing the run: a red suite is a legitimate gate result
    // and belongs in the artifact as one.
    let b5_passed = b5_ev["summary"]["verdict"] == "complete";
    let baseline = args.b1_baseline.unwrap_or_else(|| DEFAULT_BASELINE.into());
    let after = args.b1_after.unwrap_or_else(|| DEFAULT_AFTER.into());
    let baseline_commit = args
        .b1_baseline_commit
        .unwrap_or_else(|| "HEAD~1".to_string());
    let (b1, b1_s) = if seams.run_b1 {
        run_b1(&baseline, &after, &baseline_commit)?
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
    // THE ARTIFACT REFUSES A SCRIPTED ATTESTATION. `binding.harness_tree_commit`
    // and `clean_tree: true` below are a provenance claim, and both ends of it -
    // step 1's `require_clean_tree` and this re-attestation - read through a
    // seam that tests can install a double into. The seam is compiled out of a
    // production build, so this is the second of two guards rather than the
    // only one; it exists because the cfg can be switched back on by an
    // `--all-features` build, and one install would otherwise forge both ends
    // at once.
    crate::attestation::refuse_scripted_tree_attestation()?;
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

    use std::rc::Rc;

    use mogwai_lab::ledger::{ScriptedTree, TreeQuery, install_tree_oracle};

    fn missing_inputs(out: &str) -> ArrivalControlArgs {
        ArrivalControlArgs {
            measure: Some("no/such/measure.json".into()),
            envelope: Some("no/such/envelope.json".into()),
            b1_baseline: None,
            b1_after: None,
            b1_baseline_commit: None,
            // Explicit and absent: the FIRST thing `run_with` does after the
            // tree gate is read this transcript, so it is what a clean tree
            // must be refused by.
            b5_log: Some("target/no-such-b5-for-the-ordering-pin.log".into()),
            out: Some(out.into()),
        }
    }

    /// The tree gate runs before a byte of any input is read, and BOTH
    /// verdicts are injected so the claim holds in the state the gate is
    /// actually run in. This test used to return early unless the developer's
    /// working tree happened to be dirty, which is precisely the state a gate
    /// run is not in; the reason recorded for the guard - that a clean tree
    /// would launch the real six-minute artifact run - is gone too, because
    /// the run now dies on the missing B5 transcript instead.
    #[test]
    fn arrival_control_refuses_a_dirty_tree_before_reading_inputs() {
        let dirty = Rc::new(ScriptedTree::dirty("d1r7y"));
        let err = {
            let _guard = install_tree_oracle(Rc::clone(&dirty));
            run(missing_inputs("target/arrival-control-test.json"))
                .expect_err("a dirty tree refuses")
        };
        assert!(
            err.to_string().contains("the working tree is dirty"),
            "{err}"
        );
        assert!(
            !err.to_string().contains("B5 refused"),
            "the B5 transcript was read before the tree was checked: {err}"
        );
        assert_eq!(dirty.queries(), vec![TreeQuery::Status]);

        // THE CLEAN DIRECTION RUNS WITH B1 DISARMED, and that is a hazard
        // removal rather than a weakening: the pin is that the B5 read is what
        // a bound run reaches first, and B1 is downstream of it either way.
        // Left on production `Seams` this test's outcome would depend on
        // `target/no-such-b5-for-the-ordering-pin.log` NOT EXISTING - and if
        // it ever did, the run would sail past B5 into `run_b1`, which execs a
        // `mogwai gen` subprocess per symbol from inside a unit test. Low
        // probability, and nothing about the ordering claim needs it.
        let clean = Rc::new(ScriptedTree::clean("c1ean"));
        let err = {
            let _guard = install_tree_oracle(Rc::clone(&clean));
            run_with(
                missing_inputs("target/arrival-control-clean-test.json"),
                Seams {
                    run_b1: false,
                    ..Seams::production()
                },
            )
            .expect_err("the B5 transcript is not there either")
        };
        assert!(
            err.to_string().contains("B5 refused"),
            "a clean tree must be bound and the run carried into its first input: {err}"
        );
        assert_eq!(clean.queries(), vec![TreeQuery::Status, TreeQuery::Head]);
    }

    /// The derivation B1's supporting check now rests on has to actually find
    /// the constant. This asserts the PARSE, not an equality against the
    /// compiled-in value: the moment a bump lands, the edit is uncommitted for a
    /// while, and a test demanding that HEAD's committed constant match the
    /// tree's would fail for the duration of every legitimate bump - the same
    /// stale-literal trap in a new place.
    #[test]
    fn the_baseline_tape_version_is_read_out_of_a_commit() {
        let version = tape_version_at("HEAD")
            .expect("HEAD declares the tape constant where this check looks for it");
        // Zero is the value a silent fallback would have produced, and the whole
        // point of the refusal is that there is no such fallback.
        assert!(version > 0);
    }

    /// An unreachable baseline commit REFUSES. A version check that defaulted
    /// here would be decoration again, passing for a reason unrelated to the
    /// tape - which is how the hardcoded literal this replaced went stale
    /// unnoticed.
    #[test]
    fn an_unreadable_baseline_commit_refuses_rather_than_defaulting() {
        let err = tape_version_at("no-such-ref-for-a-tape-version")
            .expect_err("an unreachable commit must refuse");
        assert!(err.to_string().contains("B1 refused"));
    }

    /// The venue binary must never spawn the build tool: it is local
    /// development tooling, absent on a clone, and under this repository's
    /// run-everything-through-it convention a spawned gate deadlocks on the
    /// lock its own parent holds. So an
    /// ABSENT transcript refuses the run outright rather than passing B5 or
    /// quietly running the gate.
    #[test]
    fn arrival_control_refuses_a_missing_b5_transcript() {
        let err = read_b5_log(Path::new("target/no-such-b5-gate.log"))
            .expect_err("a missing gate transcript must refuse, not pass");
        assert!(err.to_string().contains("B5 refused"));
    }

    /// A transcript recording a RED gate reads successfully and reports a
    /// failing verdict - a red suite is a real gate result and belongs in the
    /// artifact as one. A transcript with no summary line is a different
    /// thing: it is not evidence at all, so it REFUSES rather than failing.
    #[test]
    fn a_red_gate_reads_as_failed_and_a_summaryless_one_refuses() {
        let dir = Path::new("target/arrival-control-b5");
        std::fs::create_dir_all(dir).unwrap();
        let green = dir.join("green.log");
        std::fs::write(
            &green,
            b"[result]  check complete\n{\"schema\":1,\"verdict\":\"complete\",\"failed_phase\":null}\n",
        )
        .unwrap();
        let (ev, _) = read_b5_log(&green).unwrap();
        assert_eq!(ev["summary"]["verdict"], "complete");

        let red = dir.join("red.log");
        std::fs::write(
            &red,
            b"[error]   cargo test: 1 failure\n{\"schema\":1,\"verdict\":\"failed\",\"failed_phase\":\"test\"}\n",
        )
        .unwrap();
        let (ev, _) = read_b5_log(&red).unwrap();
        assert_eq!(ev["summary"]["verdict"], "failed");
        assert_eq!(ev["summary"]["failed_phase"], "test");

        let truncated = dir.join("truncated.log");
        std::fs::write(&truncated, b"[run] clippy workspace\n").unwrap();
        let err = read_b5_log(&truncated).expect_err("a summaryless transcript is not evidence");
        assert!(err.to_string().contains("no machine-readable summary"));
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
    /// with `test -p mogwai-cli arrival_control_refuses_a_tree_that_changed_during_the_run`
    /// from a committed tree.
    #[test]
    #[ignore = "needs a CLEAN git tree and runs eight short walks; run explicitly"]
    fn arrival_control_refuses_a_tree_that_changed_during_the_run() {
        let root = repo_root();
        require_clean_tree()
            .expect("this pin needs a clean tree: commit or stash before running it");
        // Untracked at the repository root, so `git status --porcelain` reports
        // it and no .gitignore rule can hide it. `target/` cannot serve here:
        // it is ignored, so a probe planted there would not dirty the tree and
        // the pin would stop biting.
        //
        // WHICH MAKES THE CLEANUP LOAD-BEARING, and a straight-line
        // `remove_file` after the call is not it: a panic inside `run_with` or
        // inside the planting closure unwinds past it and LEAVES the probe in
        // the tree. This same test is the one documented as refusing a dirty
        // tree by design, so a leaked probe poisons every later run of the
        // suite - the failure would present as an unrelated refusal. The guard
        // removes it on the unwind path too.
        struct Sweep(PathBuf);
        impl Drop for Sweep {
            fn drop(&mut self) {
                drop(std::fs::remove_file(&self.0));
            }
        }
        let probe = root.join("arrival-control-midrun-probe.txt");
        let sweep = Sweep(probe.clone());
        let out = root.join("target/arrival-control-midrun.json");
        drop(std::fs::remove_file(&out));
        let planted = probe.clone();
        let result = run_with(
            ArrivalControlArgs {
                measure: Some(root.join(DEFAULT_MEASURE)),
                envelope: Some(root.join(DEFAULT_ENVELOPE)),
                b1_baseline: None,
                b1_after: None,
                b1_baseline_commit: None,
                b5_log: None,
                out: Some(out.clone()),
            },
            Seams {
                read_b5: false,
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
        drop(sweep);
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
    /// cadence grid for B8 to be sensitive to. Pins the COMMITTED artifact,
    /// which is in the tree, so a missing file is a failure rather than a
    /// reason to skip.
    ///
    /// `DEFAULT_OUT` is relative to the REPOSITORY, and a unit test's working
    /// directory is its crate - so this must go through `repo_root`. It did
    /// not, and read `crates/mogwai-cli/analysis/...`, which does not exist:
    /// the read failed on every run and the early return meant not one of the
    /// assertions below ever executed.
    #[test]
    fn the_control_artifact_carries_no_b8_field() {
        let path = repo_root().join(DEFAULT_OUT);
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("the committed control artifact {}: {e}", path.display()));
        let artifact: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(artifact["gates"].get("B8").is_none());
        for name in ["B1", "B2", "B3", "B4", "B5", "B6", "B7"] {
            assert!(artifact["gates"].get(name).is_some(), "{name} is missing");
        }
    }
}
