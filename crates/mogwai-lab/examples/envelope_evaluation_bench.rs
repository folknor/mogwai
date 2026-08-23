// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Harness surface: predictive-envelope month simulation, priced.
//!
//! Spec 9.7's envelope is 500 paired replicates of `1 + K` month simulations
//! over the section 8 exposure, so one evaluation is 1,500 months at `K = 2`
//! and 4,500 at `K = 8`. That price decides three frozen constants
//! (`ENVELOPE_CELL_BUDGET_S`, `STAGE_A_ENVELOPE_BUDGET_S`,
//! `STAGE_B_ENVELOPE_BUDGET_S`) and, through the lazy rule, how much of the
//! marginal shell a run can afford before it stops on shortfall.
//!
//! Why this exists rather than the shipped cost probe. `arrival-screen
//! --cost-probe` walks every family at every `K` and fails fast on the first
//! miss, so it prices exactly one cell before it stops - and that cell is
//! whichever family sorts first, not the one a pricing question is about. It
//! also cannot answer "what does one month cost" at all, which is the number an
//! optimization round moves. This harness takes the family and the month count
//! on the command line and reports per-month cost, so a K-tier price is
//! arithmetic on a measured unit rather than an hour of waiting.
//!
//! Reads the committed `analysis/mnq-measure-12a.json` for the frozen exposure,
//! so it runs anywhere the repository is checked out and needs no market data.
//!
//! Argv: `<family> [months]`, family being one of the seam spellings
//! (`event_markov`, `wall_mmpp`, `log_ou_cox`, `self_exciting`, `shot_noise`)
//! and `months` defaulting to 32. Emits `key=value` scalars on stderr per the
//! output-channel convention, work size beside the timing: a simulator that got
//! faster by simulating fewer grid cells is not a simulator that got faster.

use std::path::Path;

use mogwai_lab::arrival_envelope::{
    ExposureGrid, envelope_probe_cell, envelope_seed, simulate_month,
};
use mogwai_lab::arrival_screen::{CADENCE_STEP_NS, Family, ScreenContext};

/// hotpath leaves installing its counting allocator to the binary.
#[cfg(feature = "hotpath-alloc")]
#[global_allocator]
static ALLOC: hotpath::CountingAllocator = hotpath::CountingAllocator::new();

const MEASURE: &str = "analysis/mnq-measure-12a.json";

/// One replicate month at the family's frozen probe cell, through the same
/// `simulate_month` the envelope calls. Annotated at this level and not inside
/// the per-grid-cell loop, which runs millions of times per call and would
/// price the profiler rather than the code.
#[cfg_attr(feature = "hotpath", hotpath::measure)]
fn month(cell: &mogwai_lab::arrival_screen::Cell, grid: &ExposureGrid, replicate: usize) -> f64 {
    let stats = simulate_month(cell, grid, envelope_seed(cell, 2, grid, replicate, 1, 1))
        .expect("the probe cell simulates");
    // Returned and summed so the optimizer cannot delete the work.
    stats.hourly_rate.iter().sum()
}

fn main() {
    #[cfg(feature = "hotpath")]
    let _guard = hotpath::HotpathGuardBuilder::new("main").build();

    let mut argv = std::env::args().skip(1);
    let family = match argv.next().as_deref() {
        Some("event_markov") => Family::EventMarkov,
        Some("wall_mmpp") => Family::WallMmpp,
        Some("log_ou_cox") => Family::LogOuCox,
        Some("self_exciting") => Family::SelfExciting,
        Some("shot_noise") | None => Family::ShotNoise,
        Some(other) => panic!("unknown family {other}"),
    };
    let months: usize = argv
        .next()
        .map_or(32, |value| value.parse().expect("months parses"));

    let context = ScreenContext::open(Path::new(MEASURE), None).expect("the 12a artifact opens");
    let grid = ExposureGrid::new(
        &context.profile,
        context.binding.window_start_ns,
        context.binding.window_length_ns,
        CADENCE_STEP_NS,
    )
    .expect("the frozen envelope exposure");
    let cell = envelope_probe_cell(family);

    let started = std::time::Instant::now();
    let mut sink = 0.0;
    for replicate in 1..=months {
        sink += month(&cell, &grid, replicate);
    }
    let elapsed = started.elapsed().as_secs_f64();

    eprintln!("family={}", family.as_str());
    eprintln!("months={months}");
    eprintln!("grid_cells={}", grid.len());
    eprintln!("elapsed_s={elapsed:.3}");
    eprintln!("per_month_s={:.4}", elapsed / months as f64);
    // The three frozen tiers, projected from the measured unit: one evaluation
    // is 500 * (1 + K) months.
    for k in [2_usize, 4, 8] {
        eprintln!(
            "projected_k{k}_s={:.1}",
            elapsed / months as f64 * 500.0 * (1 + k) as f64
        );
    }
    eprintln!("rate_sink={sink:.6}");
}
