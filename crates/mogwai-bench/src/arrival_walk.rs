// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! A harness surface: the parent draw with no projection attached.
//!
//! Stage A's cost probe found that a screen cell is dominated by measurement
//! rather than by simulation - the cadence-only walk was assumed ~10x cheaper
//! than a full generator walk and measured ~2x. This harness is the control for
//! that finding: it runs the kernel draw and nothing else, so whatever it costs
//! is the floor no measurement-side optimization can go below. Pair it with
//! `screen-projection`, which runs the same walk with the projection attached;
//! the difference between the two is the whole budget the Stage A round is
//! spending against.
//!
//! A designed workload, not the MNQ cell. It uses only `mogwai-data`, and
//! preset resolution lives in `mogwai-venue`, so the walk runs on fingerprint
//! medians, an all-ones session profile and no calendar. That makes the
//! numbers a property of the kernel itself, and means they are never
//! comparable with a screen cell's per-seed cost in absolute terms. What they
//! are comparable in is cost per parent, which is how the subtraction against
//! `screen-projection` is read; see the reading rules in
//! `reference/performance.md`.
//!
//! Every family is walked for the same number of parents rather than for the
//! same span: the four differ in realized rate by construction, so equal spans
//! would compare four different amounts of work and read as a family cost
//! difference.
//!
//! Profile it with the `hotpath` feature on. Built without it, the annotation
//! below compiles away and the run reports only its scraped counters - a
//! profiling run that silently measured nothing would look exactly like a
//! profile with no hot spots, so read a profile only from an instrumented
//! build.

use mogwai_data::{ArrivalConfig, CadenceWalk, Fingerprint, GeneratorScalars, SessionProfile};

/// A `key=value` scalar on stderr, the scrape surface a benchmark harness reads
/// an end-of-run metric from. Written out by hand, as the original harness in
/// `mogwai-data` had to; kept so this instrument's output stays the same.
fn kv(key: &str, value: &str) {
    eprintln!("{key}={value}");
}

/// Parents drawn per family. Large enough that the per-run fixed cost is
/// negligible and small enough that the whole harness is seconds, not minutes -
/// an instrument you avoid running because it is slow is not an instrument.
const PARENTS_PER_FAMILY: u64 = 2_000_000;

/// The walk origin, a Monday. Anchor-independent for the kernel families, and
/// fixed anyway so two runs of this harness are the same walk.
const ORIGIN_NS: u64 = 1_700_438_400_000_000_000;

/// One representative cell per kernel family. These are the Stage A probe cells
/// (`mogwai_lab::arrival_screen::probe_cell`), transcribed rather than imported
/// so the instrument stays a pure `mogwai-data` workload. A transcription can
/// drift from its source; that is acceptable for an instrument whose job is
/// relative cost, and would not be for anything a verdict reads.
const CELLS: [(&str, ArrivalConfig); 3] = [
    (
        "wall_mmpp",
        ArrivalConfig::WallMmpp {
            occupancy: 0.5,
            rate_ratio: 20.0,
            tau_s: 60.0,
        },
    ),
    (
        "log_ou_cox",
        ArrivalConfig::LogOuCox {
            sigma_y: 1.0,
            tau_s: 60.0,
        },
    ),
    (
        "self_exciting",
        ArrivalConfig::SelfExciting {
            phi: 0.5,
            tau_s: 60.0,
        },
    ),
];

/// The measured region: one family's whole walk.
///
/// Annotated here, deliberately, and not on `CadenceWalk::next`. A single draw is nanoseconds
/// and there are two million of them per family, so instrumenting the draw
/// would price the instrumentation rather than the draw. The annotation sits on
/// the caller, which is the standing rule for this workspace's profiles.
#[cfg_attr(feature = "hotpath", hotpath::measure)]
fn walk_family(config: ArrivalConfig, seed: u64) -> (u64, u64) {
    let fingerprint = Fingerprint::from_repo_json();
    let mut scalars = GeneratorScalars::from_fingerprint_medians("BTCUSDT", &fingerprint);
    scalars.arrival = Some(config);
    // All ones with no calendar: always open, no hour or weekday modulation.
    // The kernel's own state machine is what this harness prices, and a fitted
    // exposure profile would fold a second, unrelated cost into the reading.
    let session = SessionProfile {
        intensity_hour: [1.0; 24],
        vol_hour: [1.0; 24],
        dow_weight: [1.0; 7],
    };
    let mut walk = CadenceWalk::new(&scalars, &session, None, 1.0, seed, ORIGIN_NS)
        .expect("every cell here carries an integrated kernel");

    let mut parents = 0_u64;
    let mut children = 0_u64;
    while parents < PARENTS_PER_FAMILY {
        match walk.next() {
            Ok(draw) => {
                parents += 1;
                children += u64::from(draw.child_count);
            }
            // A refusal ends the family early and is always reported rather than
            // swallowed: a run that quietly walked a tenth of the parents would
            // read as a tenfold speedup.
            Err(refusal) => {
                eprintln!("{config:?} refused after {parents} parents: {refusal:?}");
                break;
            }
        }
    }
    (parents, children)
}

pub(crate) fn run() {
    let started = std::time::Instant::now();
    let mut parents = 0_u64;
    let mut children = 0_u64;
    for (name, config) in CELLS {
        let walked = walk_family(config, 1);
        eprintln!("{name}: {} parents, {} children", walked.0, walked.1);
        parents += walked.0;
        children += walked.1;
    }

    // Self-reported, because the guard's own report write happens after this
    // point and an external wall would include it.
    kv(
        "elapsed_ms",
        &format!("{:.3}", started.elapsed().as_secs_f64() * 1e3),
    );
    kv("parents", &parents.to_string());
    kv("children", &children.to_string());
}
