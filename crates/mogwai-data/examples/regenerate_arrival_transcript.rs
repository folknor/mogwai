// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Re-blessing of the layer-2 arrival transcript fixtures.
//!
//! `crates/mogwai-data/tests/fixtures/arrival-transcript-*.json` are committed
//! regression pins: a seeded realization of each kernel family, with no
//! correctness claim of their own. Under the correctness contract (owner
//! ruling, 2026-08-09) an exact-equality gate like these is a free refactoring
//! check, re-blessed knowingly in the change that legitimately moves it - a
//! change to `ArrivalKernel::next_parent`, a family's parameterization, the
//! cadence stream or its seed derivation, which also owes an
//! `ARRIVAL_KERNEL_VERSION` bump. The commit carrying the new fixtures names
//! that change.
//!
//! Every transcript is regenerated from its own header - family, parameters,
//! seed, origin and the prose describing the point - so the tool cannot
//! retarget a fixture or give one another's account of itself. Only the
//! records are recomputed.
//!
//! Why this is an example target and not a test - which is the whole point of
//! the file. It used to be `regenerate_arrival_transcripts_amendment_only`, an
//! `#[ignore]`d `#[test]` in `generated::arrival`'s test module, and the doc
//! comment justifying that rested on `#[ignore]` keeping it out of the suite.
//! It does not, in this workspace, by design: the gate profile sets
//! `include_ignored` - that is how the socket-backed suites get covered - so
//! the regenerator ran on every full gate and overwrote the pin it exists to
//! protect. A kernel change lands, run 1 fails
//! `arrival_transcripts_replay_bit_exact` (correctly) and rewrites the fixture
//! in the same run, and run 2 recompiles against the rewritten `include_str!`
//! and reports green. One re-run is all it takes to convert a regression into
//! a blessing.
//!
//! An example target is compiled by every check lane, so this code cannot rot
//! unnoticed, and it is never run by anything at all. That is the property that was
//! wanted; `#[ignore]` never provided it.
//!
//! Invocation, which deliberately refuses to do anything without naming the
//! change that moved the draw:
//!
//! ```text
//! run --release regenerate_arrival_transcript -- --reason "<the change>"
//! ```
//!
//! Then run `test -p mogwai-data arrival_transcripts_replay_bit_exact` and
//! `test -p mogwai-data a_previously_valid_self_exciting_config_is_byte_identical`
//! and inspect the diff before committing. The replay tests are what prove
//! the regenerated fixtures and the kernel agree; this tool asserts nothing.

use mogwai_data::{
    ArrivalEnv, ArrivalKernel, ArrivalState, Fingerprint, GeneratorScalars, LogOuParams,
    RuntimeModifiers, SelfExcitingParams, ShotNoiseParams, SweepShape, TapeRng, WallMmppParams,
};
use serde_json::{Value, json};

/// The cadence stream's domain, as the replay harness derives it.
const CADENCE_STREAM_TAG: u64 = 0x6D6F_6777_6169_3132;

const FIXTURES: [&str; 5] = [
    "wall-mmpp",
    "log-ou-cox",
    "self-exciting",
    "shot_noise",
    "self-exciting-v2-phi085",
];

fn kernel(family: &str, params: &Value) -> ArrivalKernel {
    let f = |name: &str| params[name].as_f64().expect("a numeric parameter");
    match family {
        "wall_mmpp" => ArrivalKernel::WallMmpp(WallMmppParams {
            occupancy: f("occupancy"),
            rate_ratio: f("rate_ratio"),
            tau_s: f("tau_s"),
        }),
        "log_ou_cox" => ArrivalKernel::LogOuCox(LogOuParams {
            sigma_y: f("sigma_y"),
            tau_s: f("tau_s"),
        }),
        "self_exciting" => ArrivalKernel::SelfExciting(SelfExcitingParams {
            phi: f("phi"),
            tau_s: f("tau_s"),
        }),
        "shot_noise" => ArrivalKernel::ShotNoise(ShotNoiseParams {
            m: f("m"),
            k: f("k"),
            tau_s: f("tau_s"),
        }),
        other => panic!("unknown transcript family {other}"),
    }
}

fn main() {
    // The reason is itself the gate. A regeneration with nothing behind it is
    // exactly what the pin exists to refuse, and a tool that regenerates on a
    // bare invocation is one shell-history recall away from doing it by
    // accident.
    let mut args = std::env::args().skip(1);
    let reason = match (args.next().as_deref(), args.next()) {
        (Some("--reason"), Some(reason)) if !reason.trim().is_empty() => reason,
        _ => {
            eprintln!(
                "refusing: this tool replaces committed regression pins and runs only for a \
                 change that legitimately moved the cadence draw.\n\nusage: \
                 regenerate_arrival_transcript --reason \"<the change>\"\n\nName the change in \
                 the commit beside the regenerated fixtures."
            );
            std::process::exit(2);
        }
    };

    let fp = Fingerprint::from_repo_json();
    for stem in FIXTURES {
        let path = format!(
            "{}/tests/fixtures/arrival-transcript-{stem}.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let raw = std::fs::read_to_string(&path).expect("fixture readable");
        let mut transcript: Value = serde_json::from_str(&raw).expect("fixture is JSON");
        let family = transcript["family"].as_str().expect("family").to_owned();
        let seed = transcript["seed"].as_u64().expect("seed");
        let origin_ns = transcript["origin_ns"].as_u64().expect("origin");
        let count = transcript["records"].as_array().expect("records").len();
        let kernel = kernel(&family, &transcript["params"]);

        let mut cadence_rng = TapeRng::new(seed, CADENCE_STREAM_TAG);
        let mut state = ArrivalState::new(&kernel, origin_ns, &mut cadence_rng);
        let environment = ArrivalEnv::for_profile(&fp.session_profile, None, 1.0, origin_ns);
        let scalars = GeneratorScalars::xbtusd_anchor(&fp);
        let shape = SweepShape::new(
            scalars.children_mean,
            scalars.children_single_frac,
            scalars.levels_mean,
        );
        let base_mean_s = scalars.mean_event_duration_s;

        let mut from_ns = origin_ns;
        let mut records = Vec::with_capacity(count);
        for _ in 0..count {
            let draw = kernel
                .next_parent(
                    &mut state,
                    from_ns,
                    base_mean_s,
                    &shape,
                    &environment,
                    RuntimeModifiers::NEUTRAL,
                    &mut cadence_rng,
                )
                .expect("open test exposure");
            from_ns = draw.next_from_ns;
            records.push(json!({
                "child_count": draw.child_count,
                "latent_x_bits": draw.latent_x.to_bits(),
                "parent_ts_ns": draw.parent_ts_ns,
            }));
        }
        transcript["records"] = Value::Array(records);
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&transcript).expect("serializable transcript") + "\n",
        )
        .expect("fixture written");
        println!("wrote {path}");
    }
    println!("reason: {reason}");
    println!(
        "now run the replay pins and inspect the diff:\n  test -p mogwai-data \
         arrival_transcripts_replay_bit_exact\n  test -p mogwai-data \
         a_previously_valid_self_exciting_config_is_byte_identical"
    );
}
