// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! CLI-level coverage for `mogwai characterize`'s OUTPUT NAMING.
//!
//! The subcommand landed with library-level coverage of the estimand layer and
//! none of the write path, which is where it broke: `resolve_path` accepts a
//! path-shaped argument exactly as `characterize.py` does, but the report name
//! was formatted from the raw CLI argument rather than from the report's own
//! `pair` field. `mogwai characterize path/to/KEUR.csv` therefore wrote
//! `char_path/to/KEUR.csv.json` - a nested directory that
//! `fingerprint::load_reports` cannot see, since it scans only the output
//! directory itself. The intake chain broke on precisely the input form the
//! help text advertises, and no test ran the command.
//!
//! Python's behaviour is the contract: `characterize.py:247` derives `pair` as
//! the basename minus extension, and `:487` writes `char_<rep["pair"]>.json`.
//! These tests drive the real binary, because the defect lived in the CLI and a
//! library test would have missed it exactly as the original coverage did.

use std::path::{Path, PathBuf};
use std::process::Command;

mod common;

/// A three-row `ts,px,sz` corpus, enough for the report to carry a `pair`.
fn corpus(dir: &Path, name: &str) -> PathBuf {
    std::fs::create_dir_all(dir).expect("creating the corpus directory");
    let path = dir.join(name);
    std::fs::write(
        &path,
        "1700000000,100,1\n1700000001,101,1\n1700000003,103,1\n",
    )
    .expect("writing the corpus");
    path
}

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).expect("clearing the scratch directory");
    }
    std::fs::create_dir_all(&dir).expect("creating the scratch directory");
    dir
}

#[test]
fn a_path_shaped_argument_writes_the_report_named_after_the_file() {
    let root = scratch("characterize_path_shaped");
    let data = root.join("data");
    let out = root.join("out");
    let input = corpus(&data, "KEUR.csv");

    let status = Command::new(common::venue_binary())
        .arg("characterize")
        .arg(&input)
        .arg("--data-dir")
        .arg(&data)
        .arg("--out-dir")
        .arg(&out)
        .status()
        .expect("running characterize");
    assert!(status.success(), "characterize exited {status:?}");

    // The name Python writes, directly in the output directory.
    let expected = out.join("char_KEUR.json");
    assert!(
        expected.is_file(),
        "expected {} - the report must be named from the report's own `pair`",
        expected.display()
    );

    // And the defect's signature: nothing nested under the output directory.
    let entries: Vec<String> = std::fs::read_dir(&out)
        .expect("reading the output directory")
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        entries,
        vec!["char_KEUR.json".to_string()],
        "the output directory must hold exactly the one flat report"
    );

    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&expected).expect("reading the report"))
            .expect("the report parses");
    assert_eq!(
        report["pair"], "KEUR",
        "the report's own pair field is what the filename must agree with"
    );
}

/// The consumer's view, which is the reason the naming matters at all: the
/// report has to be discoverable by the loader `mogwai synth fingerprint`
/// uses. A report written into a nested directory left this refusing with
/// `no char_*.json found` while `characterize` had exited successfully.
#[test]
fn the_written_report_is_discoverable_by_the_fingerprint_loader() {
    let root = scratch("characterize_loader_visible");
    let data = root.join("data");
    let out = root.join("out");
    let input = corpus(&data, "KEUR.csv");

    let status = Command::new(common::venue_binary())
        .arg("characterize")
        .arg(&input)
        .arg("--data-dir")
        .arg(&data)
        .arg("--out-dir")
        .arg(&out)
        .status()
        .expect("running characterize");
    assert!(status.success(), "characterize exited {status:?}");

    let reports = mogwai_lab::fingerprint::load_reports(&out).expect("the loader finds the report");
    assert!(
        reports.contains_key("KEUR"),
        "load_reports scans the output directory itself, so a nested write is invisible to it"
    );
}

/// A bare symbol still resolves against `--data-dir` and names its report the
/// same way, so the fix did not trade one entry point for the other.
#[test]
fn a_bare_symbol_argument_still_writes_the_same_name() {
    let root = scratch("characterize_bare_symbol");
    let data = root.join("data");
    let out = root.join("out");
    corpus(&data, "KEUR.csv");

    let status = Command::new(common::venue_binary())
        .arg("characterize")
        .arg("KEUR")
        .arg("--data-dir")
        .arg(&data)
        .arg("--out-dir")
        .arg(&out)
        .status()
        .expect("running characterize");
    assert!(status.success(), "characterize exited {status:?}");
    assert!(out.join("char_KEUR.json").is_file());
}
