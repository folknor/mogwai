// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Protocol composition and budget-denomination measurement for the BBO layer.
//!
//! Both fixtures come out of one traversal. Protocol 6 is a projection of the
//! protocol-7 tape - quote placement consumes no randomness, so excluding quote
//! frames changes neither timestamps nor child counts - which means a second
//! generator pass would rebuild a bit-identical stream just to count a subset of
//! it. The two counter sets are carried side by side instead, which halves the
//! work and makes the paired outputs describe the same traversal by
//! construction rather than by two runs that merely ought to agree.
//!
//! This lives in the venue rather than as a `mogwai-data` example because the
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
use mogwai_data::{GeneratedSource, ParentSummary, SizeGrid};
use serde::Serialize;

use mogwai_venue::source::{InstrumentProfile, fingerprint};

const VOL_WINDOW_SECS: u64 = 300;
const WARMUP_SECS: u64 = 86_400;
const WALL_HORIZON_SECS: u64 = 600;
const MAX_MEASURED_SPEED: u64 = 10;

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
    /// Traversal workers: how many preset, seed and configuration
    /// combinations are measured concurrently. Defaults to the machine's
    /// reported parallelism, uncapped, and never exceeds the number of
    /// combinations in the run.
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
    /// Identifies the traversal both fixtures were counted off. Two files whose
    /// identifiers differ were not paired, whatever their row keys say.
    pairing_id: String,
    projection: String,
    parent_events_per_combination: usize,
    surged_configuration: &'static str,
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
        .unwrap_or_else(mogwai_cli::arrival_screen::default_jobs);

    let fp = fingerprint();
    // Every preset resolves before any measurement starts. The run is hours
    // long; a preset that cannot resolve must fail in the first second, and it
    // must be all of them that are tried, not the two that happen to be
    // self-contained documents.
    let profiles = resolve_profiles()?;
    let start_ns = peak_hour_ns();
    let fanout_span = (WALL_HORIZON_SECS * MAX_MEASURED_SPEED) as usize;
    let parents = args.parents;

    let tasks = tasks();
    let work_order = work_order(&tasks);
    // A shared cursor over the flat task list, rather than one thread per
    // seed/mode pair walking five presets in series: presets differ severalfold
    // in cost, so the fixed shape paid the slowest preset's tail on every
    // worker and pinned concurrency at 32 regardless of the machine.
    let cursor = AtomicUsize::new(0);
    // Distinct from `cursor`, which counts combinations claimed. Progress has to
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
            let (cursor, completed, tasks, work_order, results, profiles) = (
                &cursor,
                &completed,
                &tasks,
                &work_order,
                &results,
                &profiles,
            );
            scope.spawn(move || {
                loop {
                    let claim = cursor.fetch_add(1, Ordering::Relaxed);
                    let Some(&index) = work_order.get(claim) else {
                        break;
                    };
                    let task = &tasks[index];
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
    mogwai_venue::config::preset_names()
        .into_iter()
        .map(|preset| {
            let profile = mogwai_venue::config::profile_from_preset(preset)
                .with_context(|| format!("resolving preset {preset} for tick composition"))?;
            Ok((preset, profile))
        })
        .collect()
}

fn tasks() -> Vec<Task> {
    (1..=8_u64)
        .flat_map(|seed| {
            MODES.into_iter().flat_map(move |mode| {
                mogwai_venue::config::preset_names()
                    .into_iter()
                    .map(move |preset| Task { preset, seed, mode })
            })
        })
        .collect()
}

/// Claim expensive combinations first while retaining fixture order in
/// `tasks` and `results`. Surged work dominates by a wide margin because it
/// multiplies both arrival rate and child fanout.
fn work_order(tasks: &[Task]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..tasks.len()).collect();
    order.sort_by_key(|&index| match tasks[index].mode {
        Mode::Surged => 0,
        Mode::Active => 1,
        Mode::Natural => 2,
        Mode::Quiet => 3,
    });
    order
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
/// This emits one fixture, at the live `TAPE_PROTOCOL_VERSION`. It used to emit
/// two, because protocol 6 is a count projection of the protocol-7 stream -
/// quote placement draws no randomness, so the same traversal carries both. That
/// trick is specific to 6-and-7 and does not generalize: the protocol-8 session
/// profile divides the duration draw and scales the return, so its tape has
/// different timestamps and different prices. 7 and 8 are two walks. Protocol 9
/// is a third case again: it preserves the protocol-8 tape exactly and only
/// changes how the traversal is counted, so the protocol-8 fixture is the
/// baseline a protocol-9 report is compared against. The seeds are fixed and
/// each combination is an independent deterministic walk, so the comparison is
/// not merely commensurable: every measured field should match exactly, and
/// only the metadata - protocol version, projection and pairing id - differs. A
/// discrepancy in any counter is a defect in the compact traversal, not noise.
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
        pairing_id: pairing.to_owned(),
        projection: projection.into(),
        parent_events_per_combination: parents,
        surged_configuration: "maximum multipliers from the measurement start through u64::MAX",
        entries,
    })
    .with_context(|| format!("serializing the protocol-{version} report"))
}

/// The measurement anchor: the fingerprint's highest-intensity session hour,
/// counted from the unix epoch.
///
/// This is a weekday choice as much as a time-of-day one, and the weekday half
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
}

impl Counters {
    fn new(fanout_span: usize) -> Self {
        Self {
            per_parent: Histogram::default(),
            per_second: SecondBins::default(),
            fanout_second: vec![0; fanout_span],
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
    if matches!(mode, Mode::Surged) {
        source = source.with_surge(start_ns, u64::MAX, 1_000.0, 100.0);
    }
    match mode {
        Mode::Quiet => source.set_arrival_quiet_for_measurement(Some(true)),
        Mode::Active => source.set_arrival_quiet_for_measurement(Some(false)),
        Mode::Natural | Mode::Surged => {}
    }
    let mut counters = Counters::new(fanout_span);
    let mut recorded = 0_usize;
    loop {
        // A refusal draws no parent. The loop's only exits are timestamp-based,
        // so continuing past one would spin on a stale timestamp forever; the
        // combination is named because that identity is what a wedged run needs.
        //
        // A panic is the only local option because `measure` returns `Reading`,
        // not a `Result`, and it is the sole reporting site in this change that
        // is not a value - the rest of the refusal plumbing is a `Result` all
        // the way out. If `measure` is ever made fallible, this is the site to
        // revisit: the fault is already a value here, so the conversion is
        // mechanical and nothing else about this loop needs to move.
        let parent = source.advance_parent().unwrap_or_else(|fault| {
            panic!(
                "tick-composition walk refused for {preset} seed {seed} {}: {fault:?}",
                mode.label()
            )
        });
        if recorded == parents && parent.parent_ts_ns >= fanout_end_ns {
            break;
        }
        if recorded < parents {
            counters.per_parent.add(u64::from(parent.child_count) + 1);
            counters.per_second.add(parent.parent_ts_ns / 1_000_000_000);
            add_child_run_to_second_bins(&mut counters.per_second, parent);
            recorded += 1;
        }
        if parent.parent_ts_ns < fanout_end_ns {
            add_fanout(
                &mut counters.fanout_second,
                start_second,
                parent.parent_ts_ns / 1_000_000_000,
                1,
            );
            add_child_run_to_fanout(
                &mut counters.fanout_second,
                parent,
                start_second,
                fanout_end_ns,
            );
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
        self.add_count(second, 1);
    }

    fn add_count(&mut self, second: u64, count: u64) {
        let first = *self.first.get_or_insert(second);
        // Time order is the invariant this indexing rests on. Were it ever
        // broken, release mode would turn the wrap into a colossal `resize` and
        // abort in the allocator instead of reporting a wrong number.
        debug_assert!(second >= first, "tick stream went backwards in time");
        let index = (second - first) as usize;
        if index >= self.counts.len() {
            self.counts.resize(index + 1, 0);
        }
        self.counts[index] += count;
    }

    /// Counts from the first observed second through the last, quiet seconds in
    /// between represented as zero.
    fn into_dense(self) -> Vec<u64> {
        self.counts
    }
}

fn child_run_bins(parent: ParentSummary, end_ns: Option<u64>, mut add: impl FnMut(u64, u64)) {
    assert!(
        parent.child_stride_ns > 0,
        "parent child stride must be positive"
    );
    let mut ts = parent.parent_ts_ns;
    let mut remaining = u64::from(parent.child_count);
    while remaining > 0 && end_ns.is_none_or(|end| ts < end) {
        let second = ts / 1_000_000_000;
        let boundary = second.saturating_add(1).saturating_mul(1_000_000_000);
        let until_boundary = boundary.saturating_sub(ts);
        let fit = if until_boundary == 0 {
            remaining
        } else {
            until_boundary.saturating_add(parent.child_stride_ns - 1) / parent.child_stride_ns
        };
        let before_end = end_ns.map_or(remaining, |end| {
            end.saturating_sub(ts)
                .saturating_add(parent.child_stride_ns - 1)
                / parent.child_stride_ns
        });
        let count = remaining.min(fit).min(before_end);
        add(second, count);
        remaining -= count;
        ts = ts.saturating_add(count.saturating_mul(parent.child_stride_ns));
    }
}

fn add_child_run_to_second_bins(bins: &mut SecondBins, parent: ParentSummary) {
    child_run_bins(parent, None, |second, count| {
        bins.add_count(second, count);
    });
}

/// Fold `count` frames into the wall-second bin for `second`, dropping anything
/// outside the window. The horizon is a nanosecond interval and the bins are
/// whole seconds, so the two align only while the traversal's start instant sits
/// on a second boundary. That is true of today's `peak_hour_ns` anchor and is
/// exactly the kind of assumption a later caller would break silently, so the
/// bounds live here rather than in a comment: an out-of-window frame is dropped,
/// not counted into a neighbouring second and not a panic hours into a run.
fn add_fanout(bins: &mut [u64], start_second: u64, second: u64, count: u64) {
    if let Some(index) = second.checked_sub(start_second)
        && let Some(bin) = bins.get_mut(index as usize)
    {
        *bin += count;
    }
}

fn add_child_run_to_fanout(
    bins: &mut [u64],
    parent: ParentSummary,
    start_second: u64,
    end_ns: u64,
) {
    child_run_bins(parent, Some(end_ns), |second, count| {
        add_fanout(bins, start_second, second, count);
    });
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
fn rolling_tail(counts: &[u64], width: usize) -> Tail {
    let mut values = Vec::with_capacity(counts.len());
    let mut sum = 0_u64;
    for (end, count) in counts.iter().enumerate() {
        sum += count;
        if let Some(dropped) = end.checked_sub(width) {
            sum -= counts[dropped];
        }
        values.push(sum);
    }
    tail_of(values)
}

fn rolling_tails(counts: &[u64]) -> (Tail, Tail) {
    (
        rolling_tail(counts, VOL_WINDOW_SECS as usize),
        rolling_tail(counts, WARMUP_SECS as usize),
    )
}

fn tail_of(mut values: Vec<u64>) -> Tail {
    if values.is_empty() {
        return Tail {
            p999: 0.0,
            max: 0.0,
        };
    }
    let max = *values.iter().max().expect("non-empty");
    let rank = (values.len() - 1) as f64 * 0.999;
    let lower = rank.floor() as usize;
    let upper = rank.ceil() as usize;
    let fraction = rank - lower as f64;
    let upper_value = *values.select_nth_unstable(upper).1;
    let lower_value = if lower == upper {
        upper_value
    } else {
        *values[..upper].select_nth_unstable(lower).1
    };
    let p999 = lower_value as f64 + (upper_value as f64 - lower_value as f64) * fraction;
    Tail {
        p999,
        max: max as f64,
    }
}

#[cfg(test)]
mod tests {
    use mogwai_data::{TickEvent, TickSource};
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
        assert_eq!(tasks.len(), 96);
        assert_eq!(
            (tasks[0].seed, tasks[0].mode.label(), tasks[0].preset),
            (1, "quiet", "MNQ")
        );
        assert_eq!(tasks[2].preset, "BTCUSDT");
        assert_eq!(
            (tasks[3].seed, tasks[3].mode.label(), tasks[3].preset),
            (1, "active", "MNQ")
        );
        assert_eq!(
            (tasks[12].seed, tasks[12].mode.label(), tasks[12].preset),
            (2, "quiet", "MNQ")
        );
    }

    #[test]
    fn work_claims_surged_first_without_changing_fixture_order() {
        let tasks = tasks();
        let order = work_order(&tasks);
        assert_eq!(order.len(), tasks.len());
        assert!(
            order[..24]
                .iter()
                .all(|&index| matches!(tasks[index].mode, Mode::Surged))
        );
        assert_eq!(tasks[0].mode.label(), "quiet");
        assert_eq!(tasks[order[0]].mode.label(), "surged");
    }

    #[test]
    fn child_runs_split_at_second_boundaries_and_clip_the_horizon() {
        let parent = ParentSummary {
            parent_ts_ns: 1_999_998_000,
            child_count: 5,
            child_stride_ns: 1_000,
        };
        let mut measured = SecondBins::default();
        add_child_run_to_second_bins(&mut measured, parent);
        assert_eq!(measured.into_dense(), vec![2, 3]);

        let mut fanout = vec![0; 2];
        add_child_run_to_fanout(&mut fanout, parent, 1, 2_000_002_000);
        assert_eq!(fanout, vec![2, 2]);
    }

    fn assert_compact_matches_wire(
        compact: &mut GeneratedSource,
        wire: &mut GeneratedSource,
        parents: usize,
    ) {
        for _ in 0..parents {
            let summary = compact.advance_parent().expect("compact walk parent");
            let quote = wire.next_tick().expect("wire quote");
            assert!(matches!(quote, TickEvent::Quote(_)));
            assert_eq!(quote.ts_event(), summary.parent_ts_ns);
            for child in 0..summary.child_count {
                let trade = wire.next_tick().expect("wire child");
                assert!(matches!(trade, TickEvent::Trade(_)));
                assert_eq!(
                    trade.ts_event(),
                    summary.parent_ts_ns + u64::from(child) * summary.child_stride_ns
                );
            }
        }
        for _ in 0..64 {
            assert_eq!(
                format!("{:?}", compact.next_tick()),
                format!("{:?}", wire.next_tick())
            );
        }
    }

    #[test]
    fn compact_sink_matches_every_preset_mode_and_a_surge_transition() {
        let profiles = resolve_profiles().expect("every measured preset resolves");
        let fp = fingerprint();
        let start_ns = peak_hour_ns();
        for preset in mogwai_venue::config::preset_names() {
            for mode in MODES {
                let source = build_source(&profiles[preset], 17, start_ns, fp);
                // Applied before the clone, so both sides carry the same
                // immutable window - which is now the only way a surge can be
                // present at all, and is what keeps this a surge transition
                // rather than two differently-watered sources.
                let mut compact = if matches!(mode, Mode::Surged) {
                    source.with_surge(start_ns, 50, 1_000.0, 100.0)
                } else {
                    source
                };
                let mut wire = compact.clone();
                match mode {
                    Mode::Quiet => {
                        compact.set_arrival_quiet_for_measurement(Some(true));
                        wire.set_arrival_quiet_for_measurement(Some(true));
                    }
                    Mode::Active => {
                        compact.set_arrival_quiet_for_measurement(Some(false));
                        wire.set_arrival_quiet_for_measurement(Some(false));
                    }
                    Mode::Natural | Mode::Surged => {}
                }
                assert_compact_matches_wire(&mut compact, &mut wire, 64);
            }
        }

        // Thursday 21:59:59 UTC approaches the CME maintenance boundary used
        // by the futures presets. The calendar jump is deterministic and must
        // leave both sinks at the same continuation state.
        let boundary_start_ns = 21 * 3_600_000_000_000 + 3_599_000_000_000;
        for preset in ["MNQ", "MES"] {
            let mut compact = build_source(&profiles[preset], 29, boundary_start_ns, fp);
            let mut wire = compact.clone();
            assert_compact_matches_wire(&mut compact, &mut wire, 128);
        }
    }

    /// Every preset, not only the self-contained documents. MES inherits from
    /// a parent preset, and a loader that cannot follow that inheritance
    /// kills an hours-long run at its first second.
    #[test]
    fn every_measured_preset_resolves_through_the_boot_path() {
        let profiles = resolve_profiles().expect("every measured preset resolves");
        assert_eq!(profiles.len(), mogwai_venue::config::preset_names().len());
        for preset in mogwai_venue::config::preset_names() {
            let profile = &profiles[preset];
            assert_eq!(profile.def.symbol.as_ref(), preset);
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
            // The protocol-10 landing: three-lot fitted top sizes (MES
            // inheriting MNQ's July TBBO fit as the standing stopgap).
            assert_eq!(profile.scalars.top_sizes.bid, Decimal::from(3));
            assert_eq!(profile.scalars.top_sizes.ask, Decimal::from(3));
            assert!(profile.calendar.is_some(), "{preset} carries CME hours");
        }
        let preset = "BTCUSDT";
        let profile = &profiles[preset];
        assert!(
            !SizeGrid::from_def(&profile.def).integral,
            "{preset} trades fractional size"
        );
        assert!(profile.calendar.is_none(), "{preset} never closes");
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
        for preset in mogwai_venue::config::preset_names() {
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
    /// The session profile changes when events happen, never how many: child
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
