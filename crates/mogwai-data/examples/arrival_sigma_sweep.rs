//! What does `LogOuCox`'s `sigma_y` cost, one value at a time.
//!
//! WRITTEN TO CHOOSE A NUMBER. `reference/performance.md`'s cost-cliff table
//! prices `sigma_y` 1, 8 and 12, and the owner ruled on 2026-08-20 that the knob
//! gets an admission bound. Three points is not enough to place a ceiling: 8 is
//! already pathological at milliseconds per draw and 12 walks to the terminal
//! refusal, so everything between 1 and 8 is unmeasured and the bound would
//! otherwise be picked by eye.
//!
//! The measured region is the same one the cost-cliff probe used - the shipped
//! `CadenceWalk::next` with nothing attached, at fingerprint-median BTCUSDT
//! scalars and a flat session profile, so the reading is the kernel's own state
//! machine rather than a fitted exposure profile folded in beside it.
//!
//! `thin` defaults to 1000, the `LiquidityDrought` ceiling, because that is the
//! arm the table's 3.6 ms row was measured on and the one an operator can
//! actually reach. Pass a different one to see the knob in isolation.
//!
//! NOT A GATE AND NOT A BENCHED ROW. It prints a table for a human to read and
//! exits; nothing asserts on its numbers, and a threshold derived from it
//! belongs in `ArrivalConfig::is_valid` with the reasoning written beside it.
//!
//! Takes an optional draw count and thinning factor as positional arguments.

use mogwai_data::{ArrivalConfig, CadenceWalk, Fingerprint, GeneratorScalars, SessionProfile};

/// The walk origin, a Monday, matching the cost-cliff probe so the rows here
/// can be read against that table.
const ORIGIN_NS: u64 = 1_700_438_400_000_000_000;

/// Parent draws per `sigma_y`. Deliberately small: at the top of the sweep a
/// single draw costs milliseconds, so a large count turns a diagnostic into a
/// coffee break. The quantity of interest is the per-draw cost and its tail,
/// and both are visible within a few hundred draws.
const DEFAULT_DRAWS: u64 = 500;

/// The `LiquidityDrought` thinning ceiling, which is the arm the published
/// 3.6 ms row was measured on.
const DEFAULT_THIN: f64 = 1000.0;

fn main() {
    let mut argv = std::env::args().skip(1);
    let draws: u64 = argv
        .next()
        .map_or(DEFAULT_DRAWS, |arg| arg.parse().expect("draws is a u64"));
    let thin: f64 = argv
        .next()
        .map_or(DEFAULT_THIN, |arg| arg.parse().expect("thin is an f64"));

    let fingerprint = Fingerprint::from_repo_json();
    // Always open, no hour or weekday modulation: the kernel's state machine is
    // what this prices, and a fitted profile would fold a second cost in.
    let session = SessionProfile {
        intensity_hour: [1.0; 24],
        vol_hour: [1.0; 24],
        dow_weight: [1.0; 7],
    };

    println!("draws={draws} thin={thin}");
    println!("sigma_y      mean          p50          max      outcome");
    sweep_sigma_y(draws, thin, &fingerprint, &session);
    println!();
    println!("mean_event_duration_s      mean          p50          max      outcome");
    sweep_event_duration(draws, thin, &fingerprint, &session);
}

/// `LogOuCox`'s `sigma_y`, whose `x = exp(y - sigma^2 / 2)` latent is unbounded
/// BELOW - so a thin latent stretches the budget traversal one cell at a time.
fn sweep_sigma_y(draws: u64, thin: f64, fingerprint: &Fingerprint, session: &SessionProfile) {
    for tenths in 10..=90 {
        if tenths % 5 != 0 {
            continue;
        }
        let sigma_y = f64::from(tenths) / 10.0;
        let mut scalars = GeneratorScalars::from_fingerprint_medians("BTCUSDT", fingerprint);
        scalars.arrival = Some(ArrivalConfig::LogOuCox {
            sigma_y,
            tau_s: 60.0,
        });
        let (mean, p50, max, outcome) = time_walk(&scalars, session, thin, draws);
        println!("{sigma_y:>7.1}  {mean:>10.3?}  {p50:>10.3?}  {max:>10.3?}      {outcome}");
    }
}

/// `GeneratorScalars::mean_event_duration_s`, validated strictly-positive-finite
/// only and reaching the same traversal from the other direction. Its
/// fingerprint-median value for BTCUSDT is about 0.171, so the sweep walks
/// decades rather than tenths.
fn sweep_event_duration(
    draws: u64,
    thin: f64,
    fingerprint: &Fingerprint,
    session: &SessionProfile,
) {
    for exponent in -1..=6 {
        for mantissa in [1.0, 3.0] {
            let seconds = mantissa * 10_f64.powi(exponent);
            let mut scalars = GeneratorScalars::from_fingerprint_medians("BTCUSDT", fingerprint);
            scalars.mean_event_duration_s = seconds;
            // A BENIGN KERNEL, held fixed. This knob reaches the budget
            // traversal through the kernel rather than instead of it - with no
            // arrival family there is no integrated walk to measure at all - so
            // the family is pinned at the healthy `sigma_y` the sweep above
            // prices at tens of nanoseconds, and every departure from that in
            // this table is the duration knob's own.
            scalars.arrival = Some(ArrivalConfig::LogOuCox {
                sigma_y: 1.0,
                tau_s: 60.0,
            });
            let (mean, p50, max, outcome) = time_walk(&scalars, session, thin, draws);
            println!("{seconds:>21}  {mean:>10.3?}  {p50:>10.3?}  {max:>10.3?}      {outcome}");
        }
    }
}

/// Time `draws` parent draws off one configuration, reporting mean, median, max
/// and whether the walk ever refused.
///
/// A REFUSAL ENDS THE ROW rather than being folded into the timings: past some
/// setting the walk stops succeeding-but-slow and starts refusing outright, and
/// those are different failures with different remedies.
fn time_walk(
    scalars: &GeneratorScalars,
    session: &SessionProfile,
    thin: f64,
    draws: u64,
) -> (
    std::time::Duration,
    std::time::Duration,
    std::time::Duration,
    &'static str,
) {
    let Some(mut walk) = CadenceWalk::new(scalars, session, None, thin, 42, ORIGIN_NS) else {
        return (
            std::time::Duration::ZERO,
            std::time::Duration::ZERO,
            std::time::Duration::ZERO,
            "no kernel",
        );
    };
    let mut samples = Vec::with_capacity(draws as usize);
    let mut outcome = "ok";
    for _ in 0..draws {
        let started = std::time::Instant::now();
        let step = walk.next();
        samples.push(started.elapsed());
        if step.is_err() {
            outcome = "refused";
            break;
        }
    }
    samples.sort_unstable();
    let total: std::time::Duration = samples.iter().sum();
    let mean = total / u32::try_from(samples.len()).expect("a sample count fits a u32");
    (
        mean,
        samples[samples.len() / 2],
        samples[samples.len() - 1],
        outcome,
    )
}
