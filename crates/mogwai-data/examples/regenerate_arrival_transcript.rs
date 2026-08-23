// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Amendment-only regeneration of the shot-noise layer-2 transcript fixture.
//!
//! `crates/mogwai-data/tests/fixtures/arrival-transcript-*.json` are committed
//! regression pins. They are never regenerated to match a later change, except
//! under a signed section 17 amendment that authorizes a one-time replacement
//! and is cited in the commit carrying the new fixtures. Last sanctioned use:
//! the 2026-08-10 screen-recalibration and family-extension amendment
//! (`ARRIVAL_KERNEL_VERSION` 3).
//!
//! Why this is an example target and not a test - which is the whole point of
//! the file. It used to be `regenerate_arrival_transcripts_amendment_only`, an
//! `#[ignore]`d `#[test]` in `generated::arrival`'s test module, and the doc
//! comment justifying that rested on `#[ignore]` keeping it out of the suite.
//! It does not, in this workspace, by design: the gate profile sets
//! `include_ignored` - that is how the socket-backed suites get covered - so
//! the regenerator ran on every full gate and overwrote the pin it exists to
//! protect. Today the output happens to be identical, which is why nobody
//! noticed; the failure mode is the one the pin was written against. A kernel
//! change lands, run 1 fails `arrival_transcripts_replay_bit_exact` (correctly)
//! and rewrites the fixture in the same run, and run 2 recompiles against the
//! rewritten `include_str!` and reports green. One re-run is all it takes to
//! convert a regression into a blessing.
//!
//! An example target is compiled by every check lane, so this code cannot rot
//! unnoticed, and it is never run by anything at all. That is the property that was
//! wanted; `#[ignore]` never provided it.
//!
//! Invocation, which deliberately refuses to do anything without the citation:
//!
//! ```text
//! run --release regenerate_arrival_transcript -- --amendment "<section 17 citation>"
//! ```
//!
//! Then run `test -p mogwai-data arrival_transcripts_replay_bit_exact` and
//! inspect the diff before committing. The replay test is what proves the
//! regenerated fixture and the kernel agree; this tool asserts nothing.

use mogwai_data::{
    ArrivalConfig, ArrivalEnv, ArrivalKernel, ArrivalState, Fingerprint, GeneratorScalars,
    RuntimeModifiers, ShotNoiseParams, SweepShape,
};
use rand::SeedableRng;
use rand_chacha::ChaCha12Rng;
use serde_json::json;

/// The frozen coarse-grid point nearest the shot-noise family's domain centre.
const PARAMS: ShotNoiseParams = ShotNoiseParams {
    m: 0.5,
    k: 1.0,
    tau_s: 46.415_888_336_127_8,
};
const FILE_STEM: &str = "shot_noise";
const FAMILY: &str = "shot_noise";
const SEED: u64 = 201;
const ORIGIN_NS: u64 = 0;
const RECORDS: usize = 10_000;
/// The cadence stream's domain tag, as the replay harness derives it.
const CADENCE_STREAM_TAG: u64 = 0x6D6F_6777_6169_3132;

fn main() {
    // The citation is itself the gate. A regeneration with no amendment behind it is
    // exactly what the pin exists to refuse, and a tool that regenerates on a
    // bare invocation is one shell-history recall away from doing it by
    // accident.
    let mut args = std::env::args().skip(1);
    let amendment = match (args.next().as_deref(), args.next()) {
        (Some("--amendment"), Some(citation)) if !citation.trim().is_empty() => citation,
        _ => {
            eprintln!(
                "refusing: this tool replaces a committed regression pin and is sanctioned only \
                 by a signed section 17 amendment.\n\nusage: regenerate_arrival_transcript \
                 --amendment \"<citation>\"\n\nThe citation goes in the commit message beside the \
                 regenerated fixture."
            );
            std::process::exit(2);
        }
    };

    let fp = Fingerprint::from_repo_json();
    let kernel = ArrivalKernel::ShotNoise(PARAMS);
    let mut cadence_rng = ChaCha12Rng::seed_from_u64(mogwai_protocol::seeds::splitmix64(
        SEED ^ CADENCE_STREAM_TAG,
    ));
    let mut state = ArrivalState::new(&kernel, ORIGIN_NS, &mut cadence_rng);
    let environment = ArrivalEnv::for_profile(&fp.session_profile, None, 1.0, ORIGIN_NS);
    let mut scalars = GeneratorScalars::xbtusd_anchor(&fp);
    scalars.arrival = Some(ArrivalConfig::ShotNoise {
        m: PARAMS.m,
        k: PARAMS.k,
        tau_s: PARAMS.tau_s,
    });
    let shape = SweepShape::new(
        scalars.children_mean,
        scalars.children_single_frac,
        scalars.levels_mean,
    );
    let base_mean_s = scalars.mean_event_duration_s;

    let mut from_ns = ORIGIN_NS;
    let mut records = Vec::with_capacity(RECORDS);
    for _ in 0..RECORDS {
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

    let transcript = json!({
        "correctness_claim": "none",
        "exposure": "fixed fully-open fingerprint session profile used by the kernel/GeneratedSource parity harness",
        "family": FAMILY,
        "kind": "regression-transcript",
        "origin_ns": ORIGIN_NS,
        "parameter_point": "frozen coarse-grid point nearest the family domain centre",
        "params": {"m": PARAMS.m, "k": PARAMS.k, "tau_s": PARAMS.tau_s},
        "records": records,
        "seed": SEED,
    });
    let path = format!(
        "{}/tests/fixtures/arrival-transcript-{FILE_STEM}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&transcript).expect("serializable transcript") + "\n",
    )
    .expect("fixture written");
    println!("wrote {path}\namendment: {amendment}");
    println!(
        "now run the replay pin and inspect the diff:\n  test -p mogwai-data \
         arrival_transcripts_replay_bit_exact"
    );
}
