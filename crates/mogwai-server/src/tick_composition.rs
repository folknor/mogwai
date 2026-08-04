// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Protocol composition and budget-denomination measurement for the BBO layer.
//!
//! Both fixtures come out of ONE traversal. Protocol 6 is a projection of the
//! protocol-7 tape - quote placement consumes no randomness, so excluding quote
//! frames changes neither timestamps nor child counts - which means a second
//! generator pass would rebuild a bit-identical stream just to count a subset of
//! it. The two counter sets are carried side by side instead, which halves the
//! work and makes the paired outputs describe the same traversal by
//! construction rather than by two runs that merely ought to agree.
//!
//! This lives in the server rather than as a `mogwai-data` example because the
//! numbers are only worth anything if each preset is measured through the
//! profile the venue would actually boot: preset inheritance, size grid, session
//! profile and calendar. That resolution is `config::profile_from_preset`, and
//! it is not reachable from a lower crate. An earlier attempt reimplemented a
//! slice of it in an example and immediately drifted - it could not parse the
//! three presets that inherit from a parent. `gen` learned this same lesson when
//! it was taught to resolve `--symbol` through `effective_preset`.

use std::{
    collections::BTreeMap,
    fs,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    process,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use clap::Args;
use mogwai_data::{GeneratedSource, SizeGrid, TickEvent, TickSource};
use serde::Serialize;

use crate::source::{InstrumentProfile, fingerprint};

const VOL_WINDOW_SECS: u64 = 300;
const WARMUP_SECS: u64 = 86_400;
const WALL_HORIZON_SECS: u64 = 600;
const MAX_MEASURED_SPEED: u64 = 10;
const PRESETS: [&str; 5] = ["MNQ", "MES", "BTCUSDT", "ETHUSDT", "SOLUSDT"];

/// `mogwai tick-composition` arguments. Writes the paired BBO composition
/// fixtures the budget constants are derived from.
#[derive(Args)]
pub(crate) struct TickCompositionArgs {
    /// Destination for the fixture.
    #[arg(long)]
    out: PathBuf,
    /// Parent events measured per preset/seed/configuration combination.
    #[arg(long, default_value_t = 2_000_000)]
    parents: usize,
    /// Concurrent workers. Defaults to the machine's parallelism.
    #[arg(long)]
    jobs: Option<usize>,
}

#[derive(Clone, Copy)]
enum Mode {
    Quiet,
    Active,
    Natural,
    Surged,
}

impl Mode {
    fn label(self) -> &'static str {
        match self {
            Self::Quiet => "quiet",
            Self::Active => "active",
            Self::Natural => "natural",
            Self::Surged => "surged",
        }
    }
}

const MODES: [Mode; 4] = [Mode::Quiet, Mode::Active, Mode::Natural, Mode::Surged];

#[derive(Debug, Clone, Serialize)]
struct Tail {
    p999: f64,
    max: f64,
}

#[derive(Debug, Clone, Serialize)]
struct Reading {
    preset: String,
    seed: u64,
    configuration: String,
    parents: usize,
    ticks_per_parent: Tail,
    ticks_per_vol_window: Tail,
    ticks_per_sim_second: Tail,
    ticks_per_warmup: Tail,
    frames_per_wall_second: BTreeMap<String, Tail>,
}

#[derive(Serialize)]
struct Report {
    tape_protocol_version: u32,
    measured_from_protocol_7_stream: bool,
    /// Identifies the traversal both fixtures were counted off. Two files whose
    /// identifiers differ were not paired, whatever their row keys say.
    pairing_id: String,
    projection: String,
    parent_events_per_combination: usize,
    entries: Vec<Reading>,
}

/// One preset/seed/mode combination, ordered as the fixtures have always listed
/// them so the emitted entries stay stable no matter which worker claims what.
struct Task {
    preset: &'static str,
    seed: u64,
    mode: Mode,
}

pub(crate) fn run(args: &TickCompositionArgs) -> anyhow::Result<()> {
    let started = Instant::now();
    if args.jobs == Some(0) {
        anyhow::bail!("--jobs must be at least 1");
    }
    let jobs = args
        .jobs
        .unwrap_or_else(|| thread::available_parallelism().map_or(1, NonZeroUsize::get));

    let fp = fingerprint();
    // Every preset resolves before any measurement starts. The run is hours
    // long; a preset that cannot resolve must fail in the first second, and it
    // must be ALL of them that are tried, not the two that happen to be
    // self-contained documents.
    let profiles = resolve_profiles()?;
    let start_ns = peak_hour_ns();
    let fanout_span = (WALL_HORIZON_SECS * MAX_MEASURED_SPEED) as usize;
    let parents = args.parents;

    let tasks = tasks();
    // A shared cursor over the flat task list, rather than one thread per
    // seed/mode pair walking five presets in series: presets differ severalfold
    // in cost, so the fixed shape paid the slowest preset's tail on every
    // worker and pinned concurrency at 32 regardless of the machine.
    let cursor = AtomicUsize::new(0);
    // Distinct from `cursor`, which counts combinations CLAIMED. Progress has to
    // report finished work: with one worker per core the claimed count runs a
    // whole cohort ahead, and on a long tail it would sit at the total while the
    // run still had its slowest combinations to go.
    let completed = AtomicUsize::new(0);
    let total = tasks.len();
    let results: Vec<Mutex<Option<Reading>>> = tasks.iter().map(|_| Mutex::new(None)).collect();
    eprintln!(
        "[tick-composition] measuring {total} combinations at {parents} parent events each, {jobs} workers"
    );
    thread::scope(|scope| {
        for _ in 0..jobs.min(tasks.len()) {
            let (cursor, completed, tasks, results, profiles) =
                (&cursor, &completed, &tasks, &results, &profiles);
            scope.spawn(move || {
                loop {
                    let index = cursor.fetch_add(1, Ordering::Relaxed);
                    let Some(task) = tasks.get(index) else {
                        break;
                    };
                    let reading = measure(
                        task.preset,
                        &profiles[task.preset],
                        task.seed,
                        task.mode,
                        parents,
                        start_ns,
                        fanout_span,
                        fp,
                    );
                    *results[index].lock().expect("composition mutex poisoned") = Some(reading);
                    // Naming the combination is the point, not the count: a run
                    // that wedges does so on one preset/seed/mode, and this is
                    // the only place that identity is ever printed.
                    let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                    eprintln!(
                        "[tick-composition] {done}/{total} {} seed {} {} - {} elapsed",
                        task.preset,
                        task.seed,
                        task.mode.label(),
                        humanize(started.elapsed())
                    );
                }
            });
        }
    });

    let entries: Vec<Reading> = results
        .into_iter()
        .map(|slot| {
            slot.into_inner()
                .expect("composition mutex poisoned")
                .expect("composition worker left a task unmeasured")
        })
        .collect();

    let pairing = pairing_id();
    write_report(&args.out, &pairing, parents, entries)?;
    // The command was silent on success until now, so a finished run and a
    // wedged one looked identical from outside. The elapsed time is also the
    // number `reference/performance.md` wants recorded beside the fixtures, and
    // the pairing identifier is otherwise only reachable by opening a
    // five-thousand-line document - while being the thing the ratio script
    // refuses a mismatched pair on.
    println!(
        "tick-composition wrote {} in {}, protocol {}, pairing {pairing}",
        args.out.display(),
        humanize(started.elapsed()),
        mogwai_data::TAPE_PROTOCOL_VERSION
    );
    Ok(())
}

/// Wall time at both ends of this command's range: a sub-minute smoke run with
/// `--parents` turned down, and a full measurement that runs for hours.
fn humanize(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    if secs < 60 {
        return format!("{:.1}s", elapsed.as_secs_f64());
    }
    if secs < 3_600 {
        return format!("{}m {:02}s", secs / 60, secs % 60);
    }
    format!(
        "{}h {:02}m {:02}s",
        secs / 3_600,
        secs % 3_600 / 60,
        secs % 60
    )
}

/// Every measured preset, resolved through the boot path so inheritance,
/// scalar defaulting, the size grid, the session profile and the calendar are
/// the venue's own rather than this command's idea of them.
fn resolve_profiles() -> anyhow::Result<BTreeMap<&'static str, InstrumentProfile>> {
    PRESETS
        .into_iter()
        .map(|preset| {
            let profile = crate::config::profile_from_preset(preset)
                .with_context(|| format!("resolving preset {preset} for tick composition"))?;
            Ok((preset, profile))
        })
        .collect()
}

fn tasks() -> Vec<Task> {
    (1..=8_u64)
        .flat_map(|seed| {
            MODES.into_iter().flat_map(move |mode| {
                PRESETS
                    .into_iter()
                    .map(move |preset| Task { preset, seed, mode })
            })
        })
        .collect()
}

/// Distinguishes this traversal from every other. The run is hours long, so
/// second resolution plus the pid is ample; this identifies a pairing, it does
/// not have to be unguessable.
fn pairing_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    format!("{nanos:032x}-{:08x}", process::id())
}

/// Serialized in full and staged beside its destination before that destination
/// is touched, so a serialization failure or a full disk cannot consume an
/// hours-long run by truncating the file it was replacing.
///
/// This emits ONE fixture, at the live `TAPE_PROTOCOL_VERSION`. It used to emit
/// two, because protocol 6 is a count PROJECTION of the protocol-7 stream -
/// quote placement draws no randomness, so the same traversal carries both. That
/// trick is specific to 6-and-7 and does not generalize: the protocol-8 session
/// profile divides the duration draw and scales the return, so its tape has
/// different timestamps and different prices. 7 and 8 are two walks, and the
/// committed protocol-7 fixture is the baseline the new one is compared against.
fn write_report(
    path: &Path,
    pairing: &str,
    parents: usize,
    entries: Vec<Reading>,
) -> anyhow::Result<()> {
    let version = mogwai_data::TAPE_PROTOCOL_VERSION;
    let bytes = serialize(
        version,
        &format!("all protocol-{version} frames"),
        pairing,
        parents,
        entries,
    )?;
    let staged = staging_path(path, pairing);
    fs::write(&staged, bytes).with_context(|| format!("staging {}", staged.display()))?;
    fs::rename(&staged, path).with_context(|| format!("publishing {}", path.display()))
}

fn staging_path(path: &Path, pairing: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{pairing}.partial"));
    path.with_file_name(name)
}

fn serialize(
    version: u32,
    projection: &str,
    pairing: &str,
    parents: usize,
    entries: Vec<Reading>,
) -> anyhow::Result<Vec<u8>> {
    serde_json::to_vec_pretty(&Report {
        tape_protocol_version: version,
        measured_from_protocol_7_stream: true,
        pairing_id: pairing.to_owned(),
        projection: projection.into(),
        parent_events_per_combination: parents,
        entries,
    })
    .with_context(|| format!("serializing the protocol-{version} report"))
}

/// The measurement anchor: the fingerprint's highest-intensity session hour,
/// counted from the unix epoch.
///
/// This is a WEEKDAY choice as much as a time-of-day one, and the weekday half
/// is load-bearing for the calendar-bearing presets. The epoch is a Thursday, so
/// hour 16 lands at 10:00 Chicago on a Thursday - inside CME hours, with the
/// whole fanout window still inside them. Move the peak hour into a closed
/// window, or change a calendar's offset, and a futures stream's first tick
/// jumps past the end of the fanout window: every bin stays zero, the reported
/// wall-rate tails are zero, and the ratio script's `p999 > 0` guards silently
/// drop those rows rather than failing. `every_preset_opens_inside_its_own_fanout_window`
/// is what turns that into a test failure.
fn peak_hour_ns() -> u64 {
    let peak_hour = fingerprint()
        .session_profile
        .intensity_hour
        .iter()
        .enumerate()
        .max_by(|left, right| left.1.total_cmp(right.1))
        .map_or(0, |(hour, _)| hour as u64);
    peak_hour * 3_600_000_000_000
}

/// Everything counted for one protocol projection across a single traversal.
struct Counters {
    /// Children per parent as a frequency histogram indexed by the child count
    /// itself. The generator clips fan-out at `CHILD_CAP` (4096), which is the
    /// only reason indexing by value is bounded; raising that constant raises
    /// this vector's worst-case length with it.
    per_parent: Histogram,
    per_second: SecondBins,
    fanout_second: Vec<u64>,
    current: u64,
}

impl Counters {
    fn new(fanout_span: usize) -> Self {
        Self {
            per_parent: Histogram::default(),
            per_second: SecondBins::default(),
            fanout_second: vec![0; fanout_span],
            current: 0,
        }
    }

    fn finish(self, preset: &str, seed: u64, mode: Mode, parents: usize) -> Reading {
        let counts = self.per_second.into_dense();
        // Rolling windows need the bins in time order, so they run first and the
        // sim-second tail then consumes the same allocation - sorting it in
        // place rather than taking a copy of a span-length vector.
        let (vol, warmup) = rolling_tails(&counts);
        let sim = tail_of(counts);
        let frames_per_wall_second = [1_u64, 10]
            .into_iter()
            .map(|speed| {
                let speed = speed as usize;
                let values: Vec<u64> = (0..WALL_HORIZON_SECS as usize)
                    .map(|wall_second| {
                        let lo = (wall_second * speed).min(self.fanout_second.len());
                        let hi = (lo + speed).min(self.fanout_second.len());
                        self.fanout_second[lo..hi].iter().sum()
                    })
                    .collect();
                (format!("{speed}.0"), tail_of(values))
            })
            .collect();
        Reading {
            preset: preset.into(),
            seed,
            configuration: mode.label().into(),
            parents,
            ticks_per_parent: self.per_parent.tail(),
            ticks_per_vol_window: vol,
            ticks_per_sim_second: sim,
            ticks_per_warmup: warmup,
            frames_per_wall_second,
        }
    }
}

/// The served construction, field for field: `source::generator` builds this and
/// so does `gen`. A profile measured any other way is not the instrument the
/// venue runs.
fn build_source(
    profile: &InstrumentProfile,
    seed: u64,
    start_ns: u64,
    fp: &mogwai_data::Fingerprint,
) -> GeneratedSource {
    GeneratedSource::new_with_session_profile(
        profile.scalars.clone(),
        seed,
        start_ns,
        fp,
        &profile.session,
        None,
        SizeGrid::from_def(&profile.def),
        profile.calendar.clone(),
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "a measurement combination is what it is; bundling it into a struct \
              would only move the same eight values behind a name"
)]
fn measure(
    preset: &str,
    profile: &InstrumentProfile,
    seed: u64,
    mode: Mode,
    parents: usize,
    start_ns: u64,
    fanout_span: usize,
    fp: &mogwai_data::Fingerprint,
) -> Reading {
    let fanout_end_ns = start_ns.saturating_add(fanout_span as u64 * 1_000_000_000);
    let start_second = start_ns / 1_000_000_000;
    let mut source = build_source(profile, seed, start_ns, fp);
    match mode {
        Mode::Quiet => source.set_arrival_quiet_for_measurement(Some(true)),
        Mode::Active => source.set_arrival_quiet_for_measurement(Some(false)),
        Mode::Natural => {}
        Mode::Surged => source.arm_flow_surge(start_ns, u64::MAX, 1_000.0, 100.0),
    }
    let mut counters = Counters::new(fanout_span);
    let mut recorded = 0_usize;
    let mut seen_parents = 0_usize;
    loop {
        let event = source.next_tick().expect("generated source is infinite");
        let ts = event.ts_event();
        if matches!(event, TickEvent::Quote(_)) {
            if seen_parents > 0 && recorded < parents {
                counters.per_parent.add(counters.current);
                recorded += 1;
            }
            if recorded == parents && ts >= fanout_end_ns {
                break;
            }
            seen_parents += 1;
            counters.current = 0;
        }
        let second = ts / 1_000_000_000;
        if recorded < parents {
            counters.current += 1;
            counters.per_second.add(second);
        }
        if ts < fanout_end_ns
            && let Some(index) = second.checked_sub(start_second)
            && let Some(bin) = counters.fanout_second.get_mut(index as usize)
        {
            *bin += 1;
        }
    }
    counters.finish(preset, seed, mode, parents)
}

/// Contiguous per-second bins. The stream is time-ordered, so an event's bin is
/// always the last one or a short extension past it, and the tree lookup a
/// `BTreeMap` charged on every single event bought nothing.
#[derive(Default)]
struct SecondBins {
    first: Option<u64>,
    counts: Vec<u64>,
}

impl SecondBins {
    fn add(&mut self, second: u64) {
        let first = *self.first.get_or_insert(second);
        // Time order is the invariant this indexing rests on. Were it ever
        // broken, release mode would turn the wrap into a colossal `resize` and
        // abort in the allocator instead of reporting a wrong number.
        debug_assert!(second >= first, "tick stream went backwards in time");
        let index = (second - first) as usize;
        if index >= self.counts.len() {
            self.counts.resize(index + 1, 0);
        }
        self.counts[index] += 1;
    }

    /// Counts from the first observed second through the last, quiet seconds in
    /// between represented as zero.
    fn into_dense(self) -> Vec<u64> {
        self.counts
    }
}

/// Frequency histogram over small bounded values, indexed by the value itself.
#[derive(Default)]
struct Histogram {
    counts: Vec<u64>,
    total: u64,
}

impl Histogram {
    fn add(&mut self, value: u64) {
        let index = value as usize;
        if index >= self.counts.len() {
            self.counts.resize(index + 1, 0);
        }
        self.counts[index] += 1;
        self.total += 1;
    }

    /// Value at sorted position `k`, without materializing the sorted samples.
    fn value_at(&self, k: u64) -> u64 {
        let mut seen = 0_u64;
        for (value, count) in self.counts.iter().enumerate() {
            seen += count;
            if seen > k {
                return value as u64;
            }
        }
        self.counts.len().saturating_sub(1) as u64
    }

    fn tail(&self) -> Tail {
        if self.total == 0 {
            return Tail {
                p999: 0.0,
                max: 0.0,
            };
        }
        let rank = (self.total - 1) as f64 * 0.999;
        let lower = self.value_at(rank.floor() as u64) as f64;
        let upper = self.value_at(rank.ceil() as u64) as f64;
        Tail {
            p999: lower + (upper - lower) * (rank - rank.floor()),
            max: self
                .counts
                .iter()
                .rposition(|count| *count > 0)
                .map_or(0.0, |value| value as f64),
        }
    }
}

/// Rolling 300-second and 24-hour counts in one pass over the dense seconds.
/// The bins are contiguous, so each window is exactly the trailing `width`
/// entries and both sums advance off the same iteration.
fn rolling_tails(counts: &[u64]) -> (Tail, Tail) {
    let mut vol = Vec::with_capacity(counts.len());
    let mut warmup = Vec::with_capacity(counts.len());
    let (mut vol_sum, mut warmup_sum) = (0_u64, 0_u64);
    for (end, count) in counts.iter().enumerate() {
        vol_sum += count;
        warmup_sum += count;
        if let Some(dropped) = end.checked_sub(VOL_WINDOW_SECS as usize) {
            vol_sum -= counts[dropped];
        }
        if let Some(dropped) = end.checked_sub(WARMUP_SECS as usize) {
            warmup_sum -= counts[dropped];
        }
        vol.push(vol_sum);
        warmup.push(warmup_sum);
    }
    (tail_of(vol), tail_of(warmup))
}

fn tail_of(mut sorted: Vec<u64>) -> Tail {
    if sorted.is_empty() {
        return Tail {
            p999: 0.0,
            max: 0.0,
        };
    }
    sorted.sort_unstable();
    let rank = (sorted.len() - 1) as f64 * 0.999;
    let lower = rank.floor() as usize;
    let upper = rank.ceil() as usize;
    let fraction = rank - lower as f64;
    let p999 = sorted[lower] as f64 + (sorted[upper] as f64 - sorted[lower] as f64) * fraction;
    Tail {
        p999,
        max: *sorted.last().expect("non-empty") as f64,
    }
}

#[cfg(test)]
mod tests {
    use rust_decimal::Decimal;

    use super::*;

    fn tail(values: &[u64]) -> Tail {
        tail_of(values.to_vec())
    }

    #[test]
    fn p999_interpolates_instead_of_collapsing_to_the_maximum() {
        let values = (1..=600).collect::<Vec<_>>();
        let result = tail(&values);
        assert_eq!(result.p999, 599.401);
        assert_eq!(result.max, 600.0);
    }

    #[test]
    fn second_bins_represent_quiet_seconds_as_zero() {
        let mut bins = SecondBins::default();
        bins.add(10);
        bins.add(10);
        bins.add(10);
        bins.add(12);
        assert_eq!(bins.into_dense(), vec![3, 0, 1]);
    }

    #[test]
    fn histogram_tail_matches_the_sorting_tail() {
        let values: Vec<u64> = (1..=600).collect();
        let mut hist = Histogram::default();
        for value in &values {
            hist.add(*value);
        }
        let expected = tail(&values);
        let actual = hist.tail();
        assert!((actual.p999 - expected.p999).abs() < 1e-9);
        assert_eq!(actual.max, expected.max);
    }

    #[test]
    fn histogram_tail_handles_repeated_values() {
        let mut values = vec![7_u64; 4_000];
        values.extend([9, 11, 40]);
        let mut hist = Histogram::default();
        for value in &values {
            hist.add(*value);
        }
        let expected = tail(&values);
        let actual = hist.tail();
        assert!((actual.p999 - expected.p999).abs() < 1e-9);
        assert_eq!(actual.max, expected.max);
    }

    #[test]
    fn rolling_tails_window_the_trailing_width() {
        let counts = vec![1_u64; VOL_WINDOW_SECS as usize + 10];
        let (vol, warmup) = rolling_tails(&counts);
        assert_eq!(vol.max, VOL_WINDOW_SECS as f64);
        assert_eq!(warmup.max, counts.len() as f64);
    }

    #[test]
    fn task_order_is_seed_then_mode_then_preset() {
        let tasks = tasks();
        assert_eq!(tasks.len(), 160);
        assert_eq!(
            (tasks[0].seed, tasks[0].mode.label(), tasks[0].preset),
            (1, "quiet", "MNQ")
        );
        assert_eq!(tasks[4].preset, "SOLUSDT");
        assert_eq!(
            (tasks[5].seed, tasks[5].mode.label(), tasks[5].preset),
            (1, "active", "MNQ")
        );
        assert_eq!(
            (tasks[20].seed, tasks[20].mode.label(), tasks[20].preset),
            (2, "quiet", "MNQ")
        );
    }

    /// Every preset, not the two self-contained documents. MES, ETHUSDT and
    /// SOLUSDT inherit from a parent preset, and a loader that cannot follow
    /// that inheritance kills an hours-long run at its first second.
    #[test]
    fn every_measured_preset_resolves_through_the_boot_path() {
        let profiles = resolve_profiles().expect("every measured preset resolves");
        assert_eq!(profiles.len(), PRESETS.len());
        for preset in PRESETS {
            let profile = &profiles[preset];
            assert_eq!(profile.def.symbol, preset);
            assert_eq!(profile.scalars.symbol, preset);
            assert_eq!(profile.scalars.modal_tick, profile.def.price_increment);
        }
    }

    /// The futures presets are the reason profile fidelity matters: measured
    /// through `GeneratedSource::new` they would run on the crypto fractional
    /// size grid with no CME closures at all.
    #[test]
    fn futures_presets_carry_their_shipped_grid_and_calendar() {
        let profiles = resolve_profiles().expect("every measured preset resolves");
        for preset in ["MNQ", "MES"] {
            let profile = &profiles[preset];
            let grid = SizeGrid::from_def(&profile.def);
            assert!(grid.integral, "{preset} trades in whole contracts");
            assert_eq!(grid.min_size, Decimal::ONE);
            assert_eq!(profile.scalars.top_sizes.bid, Decimal::ONE);
            assert_eq!(profile.scalars.top_sizes.ask, Decimal::ONE);
            assert!(profile.calendar.is_some(), "{preset} carries CME hours");
        }
        for preset in ["BTCUSDT", "ETHUSDT", "SOLUSDT"] {
            let profile = &profiles[preset];
            assert!(
                !SizeGrid::from_def(&profile.def).integral,
                "{preset} trades fractional size"
            );
            assert!(profile.calendar.is_none(), "{preset} never closes");
        }
    }

    /// The fanout arm of every preset must actually receive frames. For the two
    /// calendar-bearing futures that depends on the measurement anchor landing
    /// inside CME hours - true today only because the epoch is a Thursday and
    /// the fingerprint's peak hour is 16, which is 10:00 Chicago. Were that to
    /// stop holding, the futures would report zero wall-rate tails and the ratio
    /// script would drop those rows through its `p999 > 0` guards, deriving the
    /// fanout budget from the crypto presets alone with nothing reported wrong.
    #[test]
    fn every_preset_opens_inside_its_own_fanout_window() {
        let profiles = resolve_profiles().expect("every measured preset resolves");
        let start_ns = peak_hour_ns();
        let fanout_span = WALL_HORIZON_SECS * MAX_MEASURED_SPEED;
        let fanout_end_ns = start_ns.saturating_add(fanout_span * 1_000_000_000);
        for preset in PRESETS {
            let mut source = build_source(&profiles[preset], 1, start_ns, fingerprint());
            let first = source
                .next_tick()
                .expect("generated source is infinite")
                .ts_event();
            assert!(
                first < fanout_end_ns,
                "{preset} opens at {first}, past the fanout window ending {fanout_end_ns}; \
                 its wall-rate bins would all be zero and the ratio script would silently \
                 drop the row"
            );
        }
    }

    /// The prediction the protocol 7-to-8 comparison rests on, checkable in
    /// seconds rather than after an hour of measurement.
    ///
    /// The session profile changes WHEN events happen, never HOW MANY: child
    /// count comes from `next_count`, whose inputs are the arrival-state Markov
    /// chain and the surge window, neither of which reads the profile. So the
    /// RNG draw sequence is identical across the change and `ticks_per_parent`
    /// must be too - only time-denominated fields may move. If this fails, the
    /// change did something other than reshape the session and the budget ratios
    /// are measuring the wrong thing.
    #[test]
    fn a_session_profile_moves_timestamps_but_never_the_parent_fanout() {
        let profiles = resolve_profiles().expect("every measured preset resolves");
        let start_ns = peak_hour_ns();
        let fp = fingerprint();
        for preset in ["MNQ", "MES"] {
            let fitted = &profiles[preset];
            // The same instrument measured against the fingerprint's flat curve
            // instead of its own fitted one - the protocol-7 shape.
            let mut flat = fitted.clone();
            flat.session = fp.session_profile.clone();
            let with_fit = measure(preset, fitted, 1, Mode::Active, 512, start_ns, 8, fp);
            let with_flat = measure(preset, &flat, 1, Mode::Active, 512, start_ns, 8, fp);
            assert_eq!(
                with_fit.ticks_per_parent.max, with_flat.ticks_per_parent.max,
                "{preset} parent fanout max moved with the session profile"
            );
            assert_eq!(
                with_fit.ticks_per_parent.p999, with_flat.ticks_per_parent.p999,
                "{preset} parent fanout p99.9 moved with the session profile"
            );
        }
    }

    #[test]
    fn humanize_reads_at_both_ends_of_the_range() {
        assert_eq!(humanize(Duration::from_millis(1_500)), "1.5s");
        assert_eq!(humanize(Duration::from_secs(59)), "59.0s");
        assert_eq!(humanize(Duration::from_secs(60)), "1m 00s");
        assert_eq!(humanize(Duration::from_secs(905)), "15m 05s");
        assert_eq!(humanize(Duration::from_secs(3_599)), "59m 59s");
        assert_eq!(humanize(Duration::from_secs(3_600)), "1h 00m 00s");
        assert_eq!(humanize(Duration::from_secs(7_384)), "2h 03m 04s");
    }

    #[test]
    fn staging_path_sits_beside_its_destination() {
        let staged = staging_path(
            Path::new("analysis/tick-composition-protocol-6.json"),
            "abc",
        );
        assert_eq!(
            staged,
            PathBuf::from("analysis/tick-composition-protocol-6.json.abc.partial")
        );
    }
}
