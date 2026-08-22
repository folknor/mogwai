// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Protocol 12b brick N3: the section 2.7 equality pin between the two copies
//! of the 12b section 8 exposure contract.
//!
//! There are two copies because there must be. `mogwai_cli::measure::run_final_walk`
//! is the 12a copy and lives in this crate; `mogwai_lab::arrival_control::control_generated_pass`
//! is the control's copy and lives in the lab, which cannot depend on the CLI.
//! Nothing in the type system holds them together, so this test does: it drives
//! both at the SAME window, burn-in and seed with no curve override, and asserts
//! the two `GeneratedAcc` records are equal once the wall-clock `cost` block -
//! the one field that is legitimately allowed to differ between two runs of the
//! same tape - is removed from each.
//!
//! ITS PREMISE IS A SEPARATE, UNIGNORED TEST. The equality pin's inputs - the
//! window, its length and the burn-in - are the half most likely to drift, and
//! they are the half that is genuinely cross-checked here: the lab side reads
//! them from the committed 12a artifact, `run_final_walk` from
//! `mogwai_lab::subcontract`'s constants. Leaving that comparison inside the
//! ten-minute gate meant a check lane never ran it. Run this file's tests as a
//! pair; a filter naming only the pin below skips the premise.
//!
//! `#[ignore]`d for cost, like the `parity12a` gates: each side is a month-long
//! in-process walk, so the pin is roughly ten minutes of CPU and belongs to an
//! explicit run rather than to the general check lane. Run it with
//! `test -p mogwai-cli the_lab_walk_matches_the_measure_exposure_contract
//! --timeout 279` in the focused runner.

use std::path::PathBuf;

use mogwai_lab::arrival_control::{GeneratedBinding, control_generated_pass};
use serde_json::Value;

mod common;

/// The crate's own manifest dir, walked up to the workspace root: an
/// integration test's working directory is the crate, not the repository.
fn repo_root() -> PathBuf {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    dir.pop();
    dir.pop();
    dir
}

fn without_cost(mut record: Value) -> Value {
    record
        .as_object_mut()
        .expect("a GeneratedAcc record is an object")
        .remove("cost");
    record
}

fn committed_binding() -> GeneratedBinding {
    let root = repo_root();
    let artifact: Value = serde_json::from_slice(
        &std::fs::read(root.join("analysis/mnq-measure-12a.json"))
            .expect("the committed 12a artifact"),
    )
    .expect("the 12a artifact parses");
    GeneratedBinding::from_measure12a(&artifact).expect("binding.generated")
}

/// THE PREMISE OF THE PIN BELOW, SPLIT OUT SO A CHECK LANE RUNS IT.
///
/// The equality pin is `#[ignore]`d at roughly ten minutes, so until this
/// existed the only cross-check between the two sides' INPUTS lived inside a
/// gate nobody runs by accident - and the inputs are the drift-prone half.
/// The lab side takes its window, length and burn-in from the COMMITTED
/// artifact; `run_final_walk` takes them from `mogwai_lab::subcontract`'s
/// constants. So this is not two copies pinned against each other: it is
/// committed bytes against code, and changing `FINAL_LENGTH` or
/// `SUMMARY_BURN_IN` without regenerating the artifact fires here in
/// microseconds instead of surfacing ten minutes later as an opaque diff of
/// two 20 KB accumulator records blaming the exposure contract.
///
/// ALL THREE INPUTS, not just the start, which is all that was checked.
///
/// AND THE LENGTH IS COMPARED AGAINST WHAT THE WALK ACTUALLY READS. The first
/// cut of this test asserted against `FINAL_END_NS - FINAL_START_NS`, which is
/// what `measure.rs` writes INTO the artifact - but `run_final_walk` derives
/// the window it walks by parsing `FINAL_LENGTH`, a second encoding of the same
/// quantity. Editing `FINAL_LENGTH` alone therefore moved the measured window
/// while this test, which named the change in its own docstring, stayed green.
/// It parses the string exactly as `run_final_walk` does; the identity between
/// the two encodings is gated separately, in `subcontract.rs`'s own tests.
#[test]
fn the_committed_binding_carries_the_window_run_final_walk_measures() {
    let binding = committed_binding();
    assert_eq!(
        binding.window_start_ns,
        u64::try_from(mogwai_lab::subcontract::FINAL_START_NS).expect("a positive start"),
        "the committed binding no longer carries the FINAL_START_NS anchor"
    );
    let length_s: u64 = mogwai_lab::subcontract::FINAL_LENGTH
        .trim_end_matches('s')
        .parse()
        .expect("FINAL_LENGTH is seconds");
    assert_eq!(
        binding.window_length_ns,
        length_s * 1_000_000_000,
        "the committed binding's window length is not the one run_final_walk measures"
    );
    assert_eq!(
        binding.burn_in,
        mogwai_lab::subcontract::SUMMARY_BURN_IN,
        "the committed binding's burn-in is not the one run_final_walk walks"
    );
}

#[test]
#[ignore = "runs two month-long in-process walks; run explicitly in release"]
fn the_lab_walk_matches_the_measure_exposure_contract() {
    let binding = committed_binding();

    // A PER-PROCESS SCRATCH, not `target/arrival-control-exposure`. That was a
    // fixed shared path: `control_generated_pass` writes
    // `arrival-control-<seed>.toml` into it and removes the file again, so two
    // concurrent runs of this gate - it is `#[ignore]`d and invoked by hand,
    // which is exactly when two of them overlap - would write and delete one
    // another's config under one name and each would read whichever won. The
    // failure is silent: the pass that lost its config resolves a profile it did
    // not ask for. Under `curve: None` the directory is not touched at all
    // today, which is a property of this call site rather than of the callee.
    //
    // BOUND TO A NAME, not passed as a temporary: the value is a guard that
    // removes the directory when it drops, and a temporary would drop at the end
    // of this statement.
    let scratch = common::scratch("arrival-control-exposure");
    let lab = control_generated_pass(scratch.path(), &binding, None, 1)
        .expect("the lab-side generated pass");
    let cli = mogwai_cli::measure::run_final_walk(1).expect("the measure-side generated pass");

    assert_eq!(
        without_cost(lab),
        without_cost(cli),
        "the lab copy of the exposure contract has drifted from run_final_walk"
    );
}
