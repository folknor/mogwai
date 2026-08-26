// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

use std::process::{Command, Output};

mod common;

fn run(args: &[&str]) -> Output {
    Command::new(common::venue_binary())
        .args(args)
        .output()
        .expect("run mogwai gen")
}

fn refusal(args: &[&str], needle: &str) {
    let output = run(args);
    assert!(!output.status.success(), "unexpected success for {args:?}");
    let stderr = String::from_utf8(output.stderr).expect("utf-8 stderr");
    assert!(
        stderr.contains(needle),
        "{args:?} refused for the wrong reason: {stderr}"
    );
}

#[test]
fn trace_window_flags_reach_the_shipped_binary_and_until_equal_to_end_is_legal() {
    let output = run(&[
        "gen",
        "--type",
        "trace",
        "--start",
        "0",
        "--length",
        "1s",
        "--trace-from",
        "0",
        "--trace-until",
        "1000000000",
    ]);
    assert!(
        output.status.success(),
        "legal trace window failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !output.stdout.is_empty(),
        "the legal trace window emitted no records"
    );
}

#[test]
fn trace_window_validation_and_mode_only_flags_are_cli_contracts() {
    let base = ["gen", "--type", "trace", "--start", "10", "--length", "10s"];
    for window in [
        ["--trace-from", "9", "--trace-until", "11"],
        ["--trace-from", "10", "--trace-until", "10"],
        ["--trace-from", "12", "--trace-until", "11"],
        ["--trace-from", "10", "--trace-until", "10000000011"],
    ] {
        let args: Vec<_> = base.iter().chain(window.iter()).copied().collect();
        refusal(
            &args,
            "trace window must satisfy start <= trace-from < trace-until <= start + length",
        );
    }
    refusal(
        &[
            "gen",
            "--type",
            "trace",
            "--length",
            "1s",
            "--trace-from",
            "0",
        ],
        "--type trace requires both --trace-from and --trace-until",
    );
    refusal(
        &[
            "gen",
            "--type",
            "trace",
            "--length",
            "1s",
            "--interval",
            "1s",
            "--trace-from",
            "0",
            "--trace-until",
            "1",
        ],
        "--interval is only valid with --type bars",
    );
    refusal(
        &[
            "gen",
            "--type",
            "trace",
            "--length",
            "1s",
            "--burn-in",
            "1s",
            "--trace-from",
            "0",
            "--trace-until",
            "1",
        ],
        "--burn-in is only valid with --type summary or --type measure12a",
    );
}
