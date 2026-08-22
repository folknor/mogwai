// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Phase 3b gate 2: `mogwai_lab::session_profile` against the real NQ
//! one-minute archive, compared with `analysis/fit_session_profile.py`'s own
//! printed numbers.
//!
//! `#[ignore]`d: needs `research/market-data/nq-1m_bk.zip` on local disk.
//!
//! WHAT THIS SCRIPT ACTUALLY FEEDS THE PRESET, checked rather than assumed.
//! `crates/mogwai-venue/presets/mnq.toml`'s provenance table names three
//! `[instrument.session]` entries; only ONE of them still descends from this
//! fit:
//!
//! - `session.dow_weight` - "NQ one-minute ratio-back-adjusted bars; contract
//!   volume per calendar-open minute used as an arrival-count proxy",
//!   `2020-01-01 through 2026-07-08`, the recent era. That is this script's
//!   `Fit.day` over the designated era, and it is what the gate below
//!   reproduces.
//! - `session.intensity_hour` and `session.vol_hour` - both re-provenanced to
//!   the July MNQ TBBO corpus by the protocol-11 refit (`mnq-fit.json`'s
//!   `session_refit.candidate`), which gate 1 reproduces. Their NQ-bar
//!   ancestors were overwritten, so no currently-committed array can be
//!   reproduced from this archive.
//!
//! The gate therefore pins `dow_weight` against the preset, and the
//! acceptance artifacts (separability, era stability, peak-to-trough,
//! outcome) against the Python's own run over the same archive.

use std::path::{Path, PathBuf};

use mogwai_lab::session_profile::{Alignment, fit_report, preflight_report};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the crate sits two levels under the repo root")
        .to_path_buf()
}

fn archive() -> PathBuf {
    repo_root().join("research/market-data/nq-1m_bk.zip")
}

#[test]
#[ignore = "needs research/market-data/nq-1m_bk.zip on local disk"]
fn parity3b_session_profile_preflight_matches_the_python() {
    let report = preflight_report(&archive(), Alignment::Civil, "MNQ").expect("the preflight");
    // Written to target/ scratch so a mismatch is inspectable; a committed
    // artifact is never touched.
    let out = repo_root().join("target/parity3b-scratch/session-profile-preflight.json");
    std::fs::create_dir_all(out.parent().expect("a parent")).expect("scratch dir");
    std::fs::write(
        &out,
        serde_json::to_vec_pretty(&report).expect("serializable"),
    )
    .expect("write");
    eprintln!("{}", serde_json::to_string_pretty(&report).expect("pretty"));

    // The invariants the Python's own NOTE calls out: the archive carries no
    // zero-volume rows, so exposure MUST come from the calendar.
    assert_eq!(report["present_zero_volume_rows"], 0);
    assert!(report["rows"].as_u64().expect("a count") > 5_000_000);
    assert!(
        report["missing_minutes_inside_eligible_sessions"]
            .as_i64()
            .expect("a count")
            > 0
    );
}

#[test]
#[ignore = "needs research/market-data/nq-1m_bk.zip on local disk"]
fn parity3b_session_profile_reproduces_the_preset_dow_weight() {
    let report = fit_report(&archive(), Alignment::Civil, "MNQ").expect("the fit");
    let out = repo_root().join("target/parity3b-scratch/session-profile-fit.json");
    std::fs::create_dir_all(out.parent().expect("a parent")).expect("scratch dir");
    std::fs::write(
        &out,
        serde_json::to_vec_pretty(&report).expect("serializable"),
    )
    .expect("write");

    let recent = &report["scopes"]["recent"];
    let day: Vec<f64> = recent["day"]
        .as_array()
        .expect("seven day factors")
        .iter()
        .map(|v| v.as_f64().expect("a float"))
        .collect();
    eprintln!("recent-era day factors: {day:?}");
    eprintln!(
        "separable: {} share {} cells {}",
        recent["separable"], recent["material_exposure_share"], recent["material_cells"]
    );
    eprintln!("era stability: {}", report["era_stability"]);
    eprintln!("outcome: {}", report["outcome"]);

    // The preset ships four-decimal values, so the comparison is at that
    // resolution - the preset is what the fit produced, rounded.
    let shipped = [1.5179, 0.9080, 0.9865, 1.0157, 1.0535, 1.0225, 1.0000];
    let rounded: Vec<f64> = day
        .iter()
        .map(|v| format!("{v:.4}").parse::<f64>().expect("parses"))
        .collect();
    assert_eq!(rounded, shipped, "the recent-era day factors");

    // The ACCEPTANCE ARTIFACTS, at the resolution
    // `analysis/fit_session_profile.py` prints them - checked against a
    // direct run of that script over the same archive, scope by scope.
    let scope = |name: &str| -> (String, u64, String, String, String, String) {
        let s = &report["scopes"][name];
        (
            format!("{:.4}", s["alpha"].as_f64().expect("alpha")),
            s["sweeps"].as_u64().expect("sweeps"),
            format!(
                "{:.4}",
                s["material_exposure_share"].as_f64().expect("share")
            ),
            s["material_cells"].to_string(),
            format!(
                "{:.2}",
                s["peak_to_trough"]["fitted"].as_f64().expect("ptt")
            ),
            format!(
                "{:.2}",
                s["peak_to_trough"]["observed"].as_f64().expect("ptt")
            ),
        )
    };
    assert_eq!(
        scope("full"),
        (
            "290.0524".into(),
            21,
            "0.0000".into(),
            "0".into(),
            "36.45".into(),
            "36.45".into()
        ),
    );
    assert_eq!(
        scope("early"),
        (
            "186.8553".into(),
            25,
            "0.0336".into(),
            "4".into(),
            "117.55".into(),
            "113.53".into()
        ),
    );
    assert_eq!(
        scope("middle"),
        (
            "239.7084".into(),
            20,
            "0.0087".into(),
            "1".into(),
            "37.99".into(),
            "37.99".into()
        ),
    );
    assert_eq!(
        scope("recent"),
        (
            "424.8419".into(),
            21,
            "0.0000".into(),
            "0".into(),
            "27.51".into(),
            "27.51".into()
        ),
    );
    for name in ["full", "early", "middle", "recent"] {
        assert_eq!(
            report["scopes"][name]["separable"], true,
            "{name} separable"
        );
    }
    assert_eq!(
        format!(
            "{:.4}",
            report["era_stability"]["mismatch_share"]
                .as_f64()
                .expect("share")
        ),
        "0.2283"
    );
    assert_eq!(report["era_stability"]["mismatch_cells"], 26);
    assert_eq!(report["era_stability"]["verdict"], "ERA-DEPENDENT");
    assert_eq!(
        report["outcome"],
        "Outcome 2: the full corpus misrepresents the designated era."
    );
}
