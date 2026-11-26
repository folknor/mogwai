// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Harness surface: one Stage A screen cell, profiled.
//!
//! This is the harness the Stage A optimization round stares at. It runs
//! exactly what `arrival-screen` runs per cell - `project_seed` over the frozen
//! window, cache bypassed - for one probe cell per family at one seed, and
//! nothing else. Its counterpart, `arrival-walk`, runs the same draw with no
//! projection attached; the gap between the two is the measurement cost the
//! round exists to attack.
//!
//! Why the cache is bypassed, stated because reinstating it would be an easy
//! and silent mistake: `project_seed` serves a cached product in microseconds,
//! so a cached harness measures deserialization and reports it as a projection
//! profile. The screen's own cost probe bypasses the cache for the same reason.
//!
//! Reads the committed `analysis/mnq-measure-12a.json`, relative to the working
//! directory, so it runs from the repository root anywhere it is checked out
//! and needs no market data on disk.
//!
//! The two are read by subtraction on cost per parent: this harness cannot
//! attribute anything finer by annotation, because `project_stream` is one
//! loop over tens of millions of children per cell and an annotation inside it
//! would price the profiler rather than the code. See the reading rules in
//! `reference/performance.md`.
//!
//! Profile it with the `hotpath` feature on; without it the annotations
//! compile away and only the scraped counters remain.

use std::path::Path;

use mogwai_lab::arrival_screen::{Family, ScreenContext, probe_cell, project_seed};
use mogwai_venue::SourceStamp;

/// The 12a measurement artifact the screen projects against.
const MEASURE: &str = "analysis/mnq-measure-12a.json";

/// One seed, not the Stage A pair. A profile wants the shape of one walk, and a
/// second seed doubles the runtime to re-measure the same code path.
const SEED: u64 = 1;

/// The measured region: one cell, one seed, through the real screen entry
/// point.
///
/// Annotated at this level and not inside `project_stream`: the loop body runs
/// tens of millions of times per call, so annotating it would price the
/// instrumentation. Finer attribution comes from the annotations the lab
/// carries at its own phase boundaries, which this call reaches through.
#[cfg_attr(feature = "hotpath", hotpath::measure)]
fn project(ctx: &ScreenContext, family: Family) -> (u64, u64) {
    let walk = project_seed(ctx, &probe_cell(family), SEED).expect("the probe cell projects");
    if let Some(refusal) = walk.refusal.as_ref() {
        // Reported, never swallowed: a refused walk did a fraction of the work
        // and its wall means nothing next to a completed one.
        eprintln!("{family:?} refused: {refusal:?}");
    }
    (walk.parents, walk.prints)
}

pub(crate) fn run() {
    // Unstamped: `measured()` below bypasses the walk cache the stamp keys.
    let context = ScreenContext::open(Path::new(MEASURE), None, &SourceStamp::unknown())
        .expect("the committed 12a artifact opens")
        // `measured()` is the screen's own cost-probe posture: bypass the walk
        // cache so the projection is actually run.
        .measured();

    let started = std::time::Instant::now();
    let mut parents = 0_u64;
    let mut prints = 0_u64;
    for family in Family::ALL {
        let (walked, projected) = project(&context, family);
        eprintln!("{family:?}: {walked} parents, {projected} prints");
        parents += walked;
        prints += projected;
    }

    // Self-reported: the guard writes its report on drop, after this line, and
    // an externally timed wall would fold that write into the measurement.
    // Work sizes beside the timing, as everywhere else - a projection that got
    // faster by projecting fewer prints is not a projection that got faster.
    eprintln!("elapsed_ms={:.3}", started.elapsed().as_secs_f64() * 1e3);
    eprintln!("parents={parents}");
    eprintln!("prints={prints}");
}
