// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Protocol 12b brick N3: the section 2.7 equality pin between the two copies
//! of the 12b section 8 exposure contract.
//!
//! There are two copies because there must be. `mogwai_cli::measure::run_final_walk`
//! is the 12a copy and lives in this crate; `mogwai_lab::arrival_control::control_generated_pass`
//! is the control's copy and lives in the lab, which cannot depend on the CLI.
//! Nothing in the type system holds them together, so this test does: it drives
//! both at the SAME window, warmup and seed with no curve override, and asserts
//! the two `GeneratedAcc` records are equal once the wall-clock `cost` block -
//! the one field that is legitimately allowed to differ between two runs of the
//! same tape - is removed from each.
//!
//! `#[ignore]`d for cost, like the `parity12a` gates: each side is a month-long
//! in-process walk, so the pin is roughly ten minutes of CPU and belongs to an
//! explicit run rather than to `brokkr check`. Run it with
//! `brokkr test -p mogwai-cli the_lab_walk_matches_the_measure_exposure_contract`.

use std::path::PathBuf;

use mogwai_lab::arrival_control::{GeneratedBinding, control_generated_pass};
use serde_json::Value;

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

#[test]
#[ignore = "runs two month-long in-process walks; run explicitly in release"]
fn the_lab_walk_matches_the_measure_exposure_contract() {
    let root = repo_root();
    let artifact: Value = serde_json::from_slice(
        &std::fs::read(root.join("analysis/mnq-measure-12a.json"))
            .expect("the committed 12a artifact"),
    )
    .expect("the 12a artifact parses");
    let binding = GeneratedBinding::from_measure12a(&artifact).expect("binding.generated");

    // The 12a binding must itself agree with the constants `run_final_walk`
    // walks under, or the comparison below would be pinning two different
    // windows to each other and passing only by accident.
    assert_eq!(
        binding.window_start_ns, 1_782_856_800_000_000_000,
        "the committed binding no longer carries the FINAL_START_NS anchor"
    );

    let lab = control_generated_pass(
        &root.join("target/arrival-control-exposure"),
        &binding,
        None,
        1,
    )
    .expect("the lab-side generated pass");
    let cli = mogwai_cli::measure::run_final_walk(1).expect("the measure-side generated pass");

    assert_eq!(
        without_cost(lab),
        without_cost(cli),
        "the lab copy of the exposure contract has drifted from run_final_walk"
    );
}
