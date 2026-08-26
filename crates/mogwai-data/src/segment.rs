// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The compose half of the session-segment sampler: a [`TickSource`] that
//! loops real session slices forever, re-anchored in returns space at every
//! seam.
//!
//! The cut half is `mogwai_lab::segments`, which writes the library artifact
//! this module reads. The two halves define the same JSON shape from opposite
//! sides on purpose - `mogwai-lab` depends on `mogwai-data` and never the
//! reverse, so a shared Rust type would point the dependency the wrong way and
//! drag the corpus parser into the serving crate. What keeps the two readings
//! honest is `analysis/segment_library_conformance.json`, a committed fixture
//! both crates parse in their own tests: if the shapes drift, one side fails on
//! the fixture rather than both staying green against their own idea of the
//! format.
//!
//! Why returns space makes the loop seamless. A segment carries no absolute
//! price. Composing is therefore integration: the river holds a running price,
//! each stored log return multiplies it, and a segment boundary is just the
//! point where the returns start coming from a different slice. There is no
//! level to reconcile, so any segment follows any other without a
//! discontinuity - which is the whole reason an endless single-session river is
//! expressible at all. Absolute price level is an integration constant (owner
//! ruling, 2026-08-12).
//!
//! What lands at the seam. Each segment records `open_gap_ret`, the measured
//! return from the last real print before its window to its own first print -
//! for an Asia slice, the jump across the daily break. Applying it at the seam
//! reproduces real reopen gaps in the composed river, which the fitted generator
//! does not produce at all (the owner's defect 2). It is a config knob because
//! the direction calls for feature injectors that toggle: see
//! [`SegmentCompose::reopen_gaps`].

use std::path::Path;

use mogwai_protocol::{AggressorSide, Symbol, TradeTick};
use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha12Rng;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde::Deserialize;

use crate::generated::MID_CEILING;
use crate::{TickEvent, TickSource};

/// The library format version this build reads. Must track
/// `mogwai_lab::segments::SEGMENT_LIBRARY_VERSION`; the conformance fixture is
/// what fails when it does not.
pub const SEGMENT_LIBRARY_VERSION: u32 = 1;

/// The reader's view of one cut segment. Deliberately a subset: the writer
/// records provenance fields the composer has no business reading.
#[derive(Clone, Debug, Deserialize)]
pub struct Segment {
    pub trade_date: String,
    pub trade_count: usize,
    pub open_gap_ret: Option<i64>,
    pub dt_ns: Vec<u64>,
    pub ret: Vec<i64>,
    pub size: Vec<i64>,
    pub side: Vec<char>,
}

/// The reader's view of a library.
#[derive(Clone, Debug, Deserialize)]
pub struct SegmentLibrary {
    pub version: u32,
    pub window: String,
    pub tick_size: String,
    pub segments: Vec<Segment>,
}

/// A library that failed to load, or that this build cannot compose from.
#[derive(Debug)]
pub enum SegmentError {
    Io(std::io::Error),
    Parse(serde_json::Error),
    Refusal(String),
}

impl std::fmt::Display for SegmentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "reading segment library: {e}"),
            Self::Parse(e) => write!(f, "parsing segment library: {e}"),
            Self::Refusal(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for SegmentError {}

impl From<std::io::Error> for SegmentError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<serde_json::Error> for SegmentError {
    fn from(e: serde_json::Error) -> Self {
        Self::Parse(e)
    }
}

impl SegmentLibrary {
    /// Reads and validates a library written by `mogwai_lab::segments::cut`.
    pub fn load(path: &Path) -> Result<Self, SegmentError> {
        let bytes = std::fs::read(path)?;
        let library: Self = serde_json::from_slice(&bytes)?;
        library.validate()?;
        Ok(library)
    }

    /// The reader-side shape contract.
    ///
    /// This repeats the writer's check rather than trusting it, because the
    /// artifact is a plain file: it can be hand-edited, truncated by a full disk, or
    /// written by an older build. The composer indexes four arrays with one
    /// cursor, and the failure of an unchecked short array is a panic in the
    /// middle of a serving walk instead of a refusal at the boundary.
    pub fn validate(&self) -> Result<(), SegmentError> {
        if self.version != SEGMENT_LIBRARY_VERSION {
            return Err(SegmentError::Refusal(format!(
                "segment library is version {}, this build composes {SEGMENT_LIBRARY_VERSION}",
                self.version
            )));
        }
        if self.segments.is_empty() {
            return Err(SegmentError::Refusal(
                "segment library carries no segments; there is nothing to compose".into(),
            ));
        }
        for segment in &self.segments {
            let n = segment.trade_count;
            if n == 0 {
                return Err(SegmentError::Refusal(format!(
                    "segment {} is empty; a zero-length draw would advance the loop \
                     without advancing the river",
                    segment.trade_date
                )));
            }
            if segment.dt_ns.len() != n
                || segment.ret.len() != n
                || segment.size.len() != n
                || segment.side.len() != n
            {
                return Err(SegmentError::Refusal(format!(
                    "segment {} declares {n} trades but carries {}/{}/{}/{} \
                     dt/ret/size/side entries",
                    segment.trade_date,
                    segment.dt_ns.len(),
                    segment.ret.len(),
                    segment.size.len(),
                    segment.side.len()
                )));
            }
            // Rule 2 of the conformance fixture, and the one that makes a seam
            // level-continuous: the incoming segment's first return must not
            // move the price, because its displacement lives in `open_gap_ret`.
            // A nonzero `ret[0]` puts a silent extra jump at every seam, on top
            // of or instead of the reopen gap, so `reopen_gaps: false` would
            // stop meaning "no gap at the seam" - and the composer's own
            // `a_seam_without_a_reopen_gap_moves_no_price` would keep passing,
            // because its fixture happens to satisfy the rule the code did not
            // check.
            if segment.ret[0] != 0 {
                return Err(SegmentError::Refusal(format!(
                    "segment {} opens with ret[0]={}; a segment's first trade is \
                     its own anchor and its displacement belongs in open_gap_ret",
                    segment.trade_date, segment.ret[0]
                )));
            }
            // The `units` block's aggressor alphabet. Without this an
            // unrecognised char is composed as `NoAggressor`, so a typo in a
            // hand-edited library reads as a legitimately unsided print. The
            // corpus parser already refuses anything outside B/A/N on the way
            // in; this is the file boundary saying the same thing.
            if let Some(bad) = segment.side.iter().find(|c| !matches!(c, 'B' | 'A' | 'N')) {
                return Err(SegmentError::Refusal(format!(
                    "segment {} carries side {bad:?}, outside the DBN alphabet B/A/N",
                    segment.trade_date
                )));
            }
        }
        Ok(())
    }
}

/// The composer's configuration - the feature injectors of the direction note,
/// each an independent toggle.
#[derive(Clone, Debug)]
pub struct SegmentCompose {
    /// Symbol the composed ticks are published under.
    pub symbol: String,
    /// Where the river's price level starts. An integration constant: it scales
    /// the whole river and changes no return.
    pub start_price: f64,
    /// First tick's timestamp, unix ns.
    pub start_ns: u64,
    /// Draw order seed.
    pub seed: u64,
    /// Dead time inserted between the last trade of one segment and the window
    /// start of the next. Real calendar time between two Asia sessions is a
    /// day; an endless Asia river deliberately elides it, so this is the visible
    /// seam and one second is enough to separate two sessions without opening a
    /// hole on the chart.
    ///
    /// It buys strict increase across seams only, and deliberately so. Within a
    /// segment the timestamps are the corpus's own: `dt_ns[i] == 0` is a normal
    /// row, because a sweep across several price levels prints several trades at
    /// one nanosecond and `mogwai_lab::segments` records the difference
    /// verbatim. The composed river is therefore non-decreasing, not strictly
    /// increasing, and nothing here may be written as if it were - the loader
    /// does not refuse a zero `dt_ns`, and refusing one would throw away real
    /// sessions.
    pub seam_gap_ns: u64,
    /// Apply each segment's measured `open_gap_ret` at the seam. Off yields a
    /// continuous river with no reopen gaps - which is what the fitted generator
    /// produces today, and therefore the useful A/B against it.
    pub reopen_gaps: bool,
    /// Sample segments uniformly with replacement (endless variety from a
    /// bounded library) instead of cycling them in library order.
    pub sample: bool,
}

impl SegmentCompose {
    /// Defaults for an endless single-session river: real reopen gaps on,
    /// sampling on, a one-second seam.
    pub fn new(symbol: impl Into<String>, seed: u64) -> Self {
        Self {
            symbol: symbol.into(),
            start_price: 20_000.0,
            start_ns: 0,
            seed,
            seam_gap_ns: 1_000_000_000,
            reopen_gaps: true,
            sample: true,
        }
    }
}

/// An endless river composed from a segment library.
///
/// Effectively infinite: like [`crate::GeneratedSource`] a caller bounds it by
/// span rather than by exhaustion. A `None` from this source is never ordinary
/// end-of-stream, and a consumer that reports it as one is reporting the wrong
/// thing. Every way it can end is a terminal fault named on
/// [`TickSource::fault`], and there are three:
///
/// - [`crate::TickFault::SegmentClockExhausted`], the nanosecond clock running
///   out of range. Mirrored by the inherent
///   [`SegmentSource::clock_exhausted`], which predates the fault channel and
///   is what the offline composer reads.
/// - [`crate::TickFault::SegmentPrice`], the running level leaving the decimal
///   grid every emitted price is snapped to.
/// - [`crate::TickFault::SegmentSeekUnreachable`], a forward seek against a
///   library whose clock cannot advance at all.
///
/// Distance is deliberately not among them. See [`SegmentSource::seek_to`].
#[derive(Debug)]
pub struct SegmentSource {
    library: SegmentLibrary,
    config: SegmentCompose,
    rng: ChaCha12Rng,
    tick_size: Decimal,
    /// `tick_size` as the running level's own type - the price floor, mirroring
    /// the generator's `tick_f64`.
    tick_f64: f64,
    /// Index into `library.segments` of the segment being played.
    segment: usize,
    /// Cursor into that segment's parallel arrays.
    cursor: usize,
    /// How many segments have been started, so order mode can cycle.
    played: usize,
    price: f64,
    ts: u64,
    /// Set when the next tick opens a segment, so the seam work (gap return,
    /// seam dead time) happens exactly once per boundary. False at
    /// construction: the river's origin is not a seam, so the first segment's
    /// `open_gap_ret` - a jump measured against a session that is not in this
    /// river - must not land there and silently displace `start_price`.
    at_seam: bool,
    /// How many times the running level hit a rail. Reported rather than
    /// swallowed: a clamp that fires is the river telling you the composed walk
    /// has drifted somewhere the library's returns cannot describe, and a
    /// silent rail is indistinguishable from a healthy one.
    clamps: u64,
    /// Latched when the nanosecond clock cannot advance any further. Terminal:
    /// `next_tick` returns `None` from then on rather than saturating, which
    /// would hand every later tick the same `ts_event` forever.
    clock_exhausted: bool,
    fault: Option<crate::TickFault>,
    /// Whether any tick this library can play advances the nanosecond clock.
    /// False for a library whose every `dt_ns` is zero under a zero seam gap -
    /// legal input, since a sweep across price levels genuinely prints several
    /// trades at one nanosecond, and the loader refuses none of it. Such a
    /// river is endless and frozen: it emits forever at one instant and can
    /// never reach a later one, which is the only thing that makes a forward
    /// seek unreachable rather than merely far. Computed once at construction
    /// so `seek_to` decides it without walking.
    clock_can_advance: bool,
}

/// One step of composed state, as [`SegmentSource::step`] reports it.
enum Step {
    /// The composer advanced exactly one tick. `Some` when that tick was at or
    /// after the caller's emit threshold and was therefore materialized;
    /// `None` when it was skipped, which costs no symbol or decimal work but
    /// leaves identical state behind.
    Advanced(Option<TickEvent>),
    /// The river ended. `fault` names why; it is never ordinary exhaustion.
    Ended,
}

impl SegmentSource {
    /// Builds a composer over `library`. Refuses a tick size that is not a
    /// positive decimal, since every emitted price is snapped to it.
    pub fn new(library: SegmentLibrary, config: SegmentCompose) -> Result<Self, SegmentError> {
        library.validate()?;
        let tick_size: Decimal = library.tick_size.parse().map_err(|e| {
            SegmentError::Refusal(format!(
                "segment library tick_size {:?} is not a decimal: {e}",
                library.tick_size
            ))
        })?;
        if tick_size <= Decimal::ZERO {
            return Err(SegmentError::Refusal(format!(
                "segment library tick_size {tick_size} is not positive"
            )));
        }
        let tick_f64 = tick_size
            .to_f64()
            .filter(|t| t.is_finite() && *t > 0.0)
            .ok_or_else(|| {
                SegmentError::Refusal(format!(
                    "segment library tick_size {tick_size} has no positive finite f64 \
                 image; the running level is integrated in f64 and floored at it"
                ))
            })?;
        if !(config.start_price.is_finite() && config.start_price > 0.0) {
            return Err(SegmentError::Refusal(format!(
                "start price {} is not a positive finite level; returns-space \
                 composition integrates against it",
                config.start_price
            )));
        }
        // Refused rather than silently collapsed. The generator clamps a
        // start_price outside the band on its first tick, which turns a typo
        // into an ~80 percent crash printed as if it were market data; the
        // composer says so at the boundary instead.
        //
        // This check's position is load-bearing, not stylistic. It precedes
        // every `integrate` call, and `integrate`'s `clamp(tick_f64,
        // MID_CEILING)` panics when the low bound exceeds the high one - so a
        // library declaring a tick_size above MID_CEILING has to be refused
        // here, before any level is integrated. Reordering the constructor so
        // a tick above the ceiling reaches `integrate` reintroduces that panic.
        if config.start_price < tick_f64 || config.start_price > MID_CEILING {
            return Err(SegmentError::Refusal(format!(
                "start price {} is outside the composable band [{tick_f64}, {MID_CEILING}]",
                config.start_price
            )));
        }
        let clock_can_advance = config.seam_gap_ns > 0
            || library
                .segments
                .iter()
                .any(|s| s.dt_ns.iter().any(|dt| *dt > 0));
        let mut source = Self {
            clock_can_advance,
            rng: ChaCha12Rng::seed_from_u64(config.seed),
            tick_size,
            tick_f64,
            segment: 0,
            cursor: 0,
            played: 0,
            price: config.start_price,
            ts: config.start_ns,
            at_seam: false,
            clamps: 0,
            clock_exhausted: false,
            fault: None,
            library,
            config,
        };
        source.segment = source.draw_segment();
        Ok(source)
    }

    /// The window this river is composed from, for provenance in a dump header.
    pub fn window(&self) -> &str {
        &self.library.window
    }

    /// How many times the running level was pinned to a rail. Nonzero means the
    /// composed walk left the band the library's returns can describe, and the
    /// prices after that point are the rail rather than the integration.
    pub fn clamps(&self) -> u64 {
        self.clamps
    }

    /// Whether the source stopped because the nanosecond clock ran out of
    /// range. The reason the offline composer reports, and the only one it can
    /// provoke - it walks forward from the origin and never seeks. It is not
    /// the only way this source ends: see [`TickSource::fault`], which names
    /// all three and is what a `dyn TickSource` reader must consult.
    pub fn clock_exhausted(&self) -> bool {
        self.clock_exhausted
    }

    /// Integrates one log return into the running level, holding it inside the
    /// band every emitted price must come from.
    ///
    /// Why a band at all. The composer integrates `price *= ret.exp()` forever,
    /// and an endless river has no re-anchoring event: a run of negative drift
    /// walks the level toward zero and a run of positive drift toward infinity.
    /// Both ends are wrong in a way that is not merely inaccurate. Below half a
    /// tick, `emit_price` rounds to exactly zero and the river carries
    /// non-positive prices, which this crate's own Kraken parser refuses on the
    /// grounds that they poison downstream ln-return math; above `Decimal`'s
    /// range - about 7.9e28, far below `f64`'s - the level has no decimal image
    /// at all. The generator was given the same floor and ceiling on the same
    /// reasoning; the constant is shared so the two bands cannot drift.
    fn integrate(&mut self, log_return: f64) {
        let next = self.price * log_return.exp();
        // A `NaN` cannot arise from a finite level times a finite factor, but
        // `clamp` panics on one, so it is handled rather than argued about.
        let bounded = if next.is_nan() {
            self.tick_f64
        } else {
            next.clamp(self.tick_f64, MID_CEILING)
        };
        if bounded != next {
            self.clamps += 1;
        }
        self.price = bounded;
    }

    /// Advances the clock, or latches exhaustion. Returns false when the river
    /// has to end.
    fn advance_clock(&mut self, by: u64) -> bool {
        match self.ts.checked_add(by) {
            Some(ts) => {
                self.ts = ts;
                true
            }
            None => {
                self.clock_exhausted = true;
                self.fault = Some(crate::TickFault::SegmentClockExhausted { clock_ns: self.ts });
                false
            }
        }
    }

    fn draw_segment(&mut self) -> usize {
        let n = self.library.segments.len();
        if self.config.sample {
            self.rng.random_range(0..n)
        } else {
            self.played % n
        }
    }

    /// Advances to the next segment, taking the seam. Kept separate from
    /// `next_tick` so the seam's two effects - the dead time and the gap
    /// return - are applied in one place and cannot drift apart.
    fn take_seam(&mut self) -> bool {
        self.played += 1;
        self.segment = self.draw_segment();
        self.cursor = 0;
        self.at_seam = true;
        self.advance_clock(self.config.seam_gap_ns)
    }

    /// Snaps the running level onto the instrument's grid.
    ///
    /// The running price stays in f64 and only the emitted value is snapped:
    /// rounding the running level would accumulate the rounding error into
    /// every subsequent return, which over an endless river is a slow drift
    /// rather than a bounded one.
    fn emit_price(&mut self) -> Option<Decimal> {
        // Enforced, not argued. This used to fall back to `self.tick_size` on a
        // `None`, which printed a one-tick trade in the middle of a runaway river
        // with no error and no log - and `None` is reachable well before `inf`,
        // because `Decimal` tops out around 7.9e28. `integrate` holds the level
        // inside [tick, MID_CEILING], which has a decimal image by construction,
        // so a `None` here means that band was breached and the river is wrong;
        // failing loudly beats printing a price nobody can trace.
        let Some(level) = Decimal::from_f64_retain(self.price) else {
            self.fault = Some(crate::TickFault::SegmentPrice { clock_ns: self.ts });
            return None;
        };
        let steps = (level / self.tick_size).round();
        Some((steps * self.tick_size).normalize())
    }

    /// Advances exactly one tick of composed state.
    ///
    /// `emit_from_ns` decides only whether the tick is materialized: a tick
    /// landing before it is skipped, which costs no `Symbol` allocation and no
    /// decimal snap while leaving byte-identical state behind. That is the
    /// whole reason `next_tick` and `seek_to` are one function rather than
    /// two - the walk a seek performs must be the walk a read performs, and a
    /// second copy of the seam, integration and clock rules is exactly where
    /// the two would drift.
    fn step(&mut self, emit_from_ns: u64) -> Step {
        // A segment whose cursor has run out hands over at the seam. `while`
        // rather than `if`: `validate` forbids an empty segment, so one hop
        // always suffices, but the loop makes that a property of the data
        // rather than an assumption the walk would panic on.
        if self.fault.is_some() {
            return Step::Ended;
        }
        while self.cursor >= self.library.segments[self.segment].trade_count {
            if !self.take_seam() {
                return Step::Ended;
            }
        }
        let i = self.cursor;
        // Copied out before any `&mut self` call: `integrate` and
        // `advance_clock` both need the whole source.
        let segment = &self.library.segments[self.segment];
        let open_gap_ret = segment.open_gap_ret;
        let ret_i = segment.ret[i];
        let dt_i = segment.dt_ns[i];
        let size_i = segment.size[i];
        let side_i = segment.side[i];

        if self.at_seam {
            if self.config.reopen_gaps
                && let Some(gap) = open_gap_ret
            {
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "a nano-log-return is far inside f64's exact integer range"
                )]
                let gap_ret = gap as f64 * 1e-9;
                self.integrate(gap_ret);
            }
            self.at_seam = false;
        }

        #[expect(
            clippy::cast_precision_loss,
            reason = "a nano-log-return is far inside f64's exact integer range"
        )]
        let ret = ret_i as f64 * 1e-9;
        self.integrate(ret);
        if !self.advance_clock(dt_i) {
            // The level moved and the cursor did not, so the source is left
            // mid-segment and internally inconsistent. Harmless because the
            // state is terminal: `clock_exhausted` latches, the fault guard at
            // the top of `step` ends every later call, and nothing
            // reads the level again. Anything that ever makes exhaustion
            // recoverable owes an undo of the integration or a reordering.
            return Step::Ended;
        }

        // Skipped ticks advance the cursor and nothing else. The emitted value
        // is a pure function of state this step has already settled, so
        // withholding it changes no later tick.
        if self.ts < emit_from_ns {
            self.cursor += 1;
            return Step::Advanced(None);
        }

        let Some(price) = self.emit_price() else {
            return Step::Ended;
        };
        let tick = TradeTick {
            symbol: Symbol::from(self.config.symbol.as_str()),
            price,
            size: Decimal::from(size_i),
            aggressor: match side_i {
                'B' => AggressorSide::Buyer,
                'A' => AggressorSide::Seller,
                // `validate` refuses anything outside B/A/N at load, so this
                // arm is exactly the fixture's `N`.
                _ => AggressorSide::NoAggressor,
            },
            ts_event: self.ts,
        };
        self.cursor += 1;
        Step::Advanced(Some(TickEvent::Trade(tick)))
    }
}

impl TickSource for SegmentSource {
    fn next_tick(&mut self) -> Option<TickEvent> {
        // Threshold zero: every instant is at or after it, so every step this
        // takes materializes its tick.
        match self.step(0) {
            Step::Advanced(tick) => tick,
            Step::Ended => None,
        }
    }

    fn fault(&self) -> Option<crate::TickFault> {
        self.fault
    }

    /// Walks to the first tick at or after `start_ts`, skipping the emission
    /// work for everything before it.
    ///
    /// Distance is not a failure and must never be turned into one. A bound
    /// stated as a tick count did exactly that: it read as a work bound and
    /// behaved as a terminal fault, so a reachable instant far enough from the
    /// origin - which is precisely what a named tape window asks for - came
    /// back as a dead source, latched, and took every later read with it. The
    /// only thing that legitimately ends a seek short is something that ends
    /// the source: the clock running out of range, the level leaving its
    /// decimal grid, or a library whose clock cannot advance at all.
    ///
    /// That last one is what makes this terminate. `self.ts` is monotone, and
    /// `clock_can_advance` is false exactly when no tick in the library can
    /// move it - the one case where a forward target is genuinely out of
    /// reach, decided without walking. Otherwise the walk advances the clock
    /// toward a bounded `u64` target and arrives, or exhausts the clock
    /// trying. With `sample = true` and only some segments carrying time that
    /// arrival is almost sure rather than certain, since the draw could in
    /// principle keep choosing a timeless segment forever; every shipped
    /// window carries time in every segment and a positive seam gap besides.
    ///
    /// What this does not give is a cheap distant seek: the walk is O(ticks),
    /// because the composed level and the sampling draw are path-dependent and
    /// no segment can be skipped without changing the river. A caller that
    /// needs a bound owes itself a checkpoint chain, the way `mogwai-venue`
    /// resumes a generated river from a snapshot and replays only the
    /// residual, rather than a cap here that would refuse the walk.
    fn seek_to(&mut self, start_ts: u64) -> Option<TickEvent> {
        if self.fault.is_some() {
            return None;
        }
        if start_ts > self.ts && !self.clock_can_advance {
            self.fault = Some(crate::TickFault::SegmentSeekUnreachable {
                target_ns: start_ts,
            });
            return None;
        }
        loop {
            match self.step(start_ts) {
                Step::Advanced(Some(tick)) => return Some(tick),
                Step::Advanced(None) => (),
                Step::Ended => return None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(date: &str, gap: Option<i64>, rets: &[i64]) -> Segment {
        Segment {
            trade_date: date.into(),
            trade_count: rets.len(),
            open_gap_ret: gap,
            dt_ns: vec![1_000_000; rets.len()],
            ret: rets.to_vec(),
            size: vec![1; rets.len()],
            side: vec!['B'; rets.len()],
        }
    }

    fn library(segments: Vec<Segment>) -> SegmentLibrary {
        SegmentLibrary {
            version: SEGMENT_LIBRARY_VERSION,
            window: "asia".into(),
            tick_size: "0.25".into(),
            segments,
        }
    }

    fn price_of(tick: &TickEvent) -> Decimal {
        match tick {
            TickEvent::Trade(t) => t.price,
            TickEvent::Quote(_) => unreachable!("the composer emits trades only"),
        }
    }

    #[test]
    fn composition_is_endless_and_deterministic_per_seed() {
        let build = || {
            let mut config = SegmentCompose::new("MNQ", 7);
            config.start_price = 20_000.0;
            SegmentSource::new(
                library(vec![
                    segment("d1", Some(1_000_000), &[0, 500_000, -200_000]),
                    segment("d2", Some(-2_000_000), &[0, 300_000]),
                ]),
                config,
            )
            .expect("a valid library")
        };
        let a: Vec<Decimal> = (0..50)
            .map(|_| price_of(&build().next_tick().expect("endless")))
            .take(1)
            .collect();
        let mut first = build();
        let mut second = build();
        let run_a: Vec<Decimal> = (0..200)
            .map(|_| price_of(&first.next_tick().expect("endless")))
            .collect();
        let run_b: Vec<Decimal> = (0..200)
            .map(|_| price_of(&second.next_tick().expect("endless")))
            .collect();
        assert_eq!(run_a, run_b, "same seed, same river");
        assert_eq!(run_a.len(), 200, "the source never exhausts");
        assert!(!a.is_empty());
    }

    #[test]
    fn timestamps_are_strictly_increasing_across_seams() {
        let mut config = SegmentCompose::new("MNQ", 3);
        config.seam_gap_ns = 1_000_000_000;
        let mut source = SegmentSource::new(
            library(vec![
                segment("d1", None, &[0, 10]),
                segment("d2", None, &[0]),
            ]),
            config,
        )
        .expect("a valid library");
        let mut last = 0u64;
        for _ in 0..100 {
            let ts = source.next_tick().expect("endless").ts_event();
            assert!(ts > last, "ts {ts} did not advance past {last}");
            last = ts;
        }
    }

    /// The property that makes an endless loop possible at all: with reopen
    /// gaps off, a seam introduces no price move, because the incoming
    /// segment's first return is zero and its level came from the running river.
    #[test]
    fn a_seam_without_a_reopen_gap_moves_no_price() {
        let mut config = SegmentCompose::new("MNQ", 11);
        config.reopen_gaps = false;
        config.sample = false;
        // Every stored gap is enormous; with the injector off none may land.
        let mut source = SegmentSource::new(
            library(vec![
                segment("d1", Some(50_000_000), &[0]),
                segment("d2", Some(50_000_000), &[0]),
            ]),
            config,
        )
        .expect("a valid library");
        let first = price_of(&source.next_tick().expect("endless"));
        for _ in 0..20 {
            assert_eq!(
                price_of(&source.next_tick().expect("endless")),
                first,
                "a zero-return river must stay flat across seams when gaps are off"
            );
        }
    }

    #[test]
    fn the_reopen_gap_injector_moves_the_price_at_the_seam() {
        let mut config = SegmentCompose::new("MNQ", 11);
        config.reopen_gaps = true;
        config.sample = false;
        config.start_price = 20_000.0;
        let mut source = SegmentSource::new(
            library(vec![
                segment("d1", None, &[0]),
                // ln(1.01) is about 9_950_331 nano-log-returns: a 1 % gap.
                segment("d2", Some(9_950_331), &[0]),
            ]),
            config,
        )
        .expect("a valid library");
        let first = price_of(&source.next_tick().expect("endless"));
        let after_seam = price_of(&source.next_tick().expect("endless"));
        assert!(
            after_seam > first,
            "the measured gap must land: {first} then {after_seam}"
        );
        let ratio = (after_seam / first).round_dp(3);
        assert_eq!(ratio, Decimal::from_f64_retain(1.010).unwrap().round_dp(3));
    }

    #[test]
    fn every_emitted_price_sits_on_the_tick_grid() {
        let mut source = SegmentSource::new(
            library(vec![segment("d1", None, &[0, 137, -4_211, 88_000])]),
            SegmentCompose::new("MNQ", 5),
        )
        .expect("a valid library");
        let grid: Decimal = "0.25".parse().unwrap();
        for _ in 0..200 {
            let price = price_of(&source.next_tick().expect("endless"));
            assert_eq!(price % grid, Decimal::ZERO, "{price} is off the grid");
        }
    }

    #[test]
    fn a_library_with_disagreeing_arrays_is_refused_at_the_boundary() {
        let mut broken = segment("d1", None, &[0, 1]);
        broken.size.pop();
        let err = SegmentSource::new(library(vec![broken]), SegmentCompose::new("MNQ", 1))
            .expect_err("a truncated segment is refused")
            .to_string();
        assert!(err.contains("declares 2 trades"), "{err}");
    }

    #[test]
    fn a_future_library_version_is_refused_rather_than_composed() {
        let mut future = library(vec![segment("d1", None, &[0])]);
        future.version = SEGMENT_LIBRARY_VERSION + 1;
        let err = SegmentSource::new(future, SegmentCompose::new("MNQ", 1))
            .expect_err("an unreadable version is refused")
            .to_string();
        assert!(err.contains("version"), "{err}");
    }

    /// Finding 3, the floor half: an endless negative drift walks the level
    /// below half a tick, and `emit_price` then rounds every print to exactly
    /// zero - a non-positive price this crate's own Kraken parser refuses.
    #[test]
    fn a_runaway_negative_drift_never_prints_a_non_positive_price() {
        let mut config = SegmentCompose::new("MNQ", 4);
        config.sample = false;
        config.reopen_gaps = false;
        config.start_price = 20_000.0;
        // -0.5 in nano-log-returns, once per segment (the segment's first
        // return is the mandatory zero anchor): `ln(20000 / 0.125) / 0.5` is
        // about 24 segments to take 20000 below 0.125, which is half of the
        // 0.25 tick and the level at which `emit_price` rounds to zero.
        let mut source = SegmentSource::new(
            library(vec![segment("d1", None, &[0, -500_000_000])]),
            config,
        )
        .expect("a valid library");
        for _ in 0..500 {
            let price = price_of(&source.next_tick().expect("endless"));
            assert!(price > Decimal::ZERO, "the composer printed {price}");
        }
        assert!(
            source.clamps() > 0,
            "a walk this steep must reach the floor, or the test proves nothing"
        );
    }

    /// Finding 3, the ceiling half - and the report had the mechanism wrong,
    /// which the bite-check showed. It predicted a silent one-tick print from
    /// `from_f64_retain` returning `None` above `Decimal`'s roughly 7.9e28
    /// range. Measured directly, an unbounded rising walk never gets there: with a 0.25
    /// tick, `level / tick_size` overflows `Decimal` around 1.98e28, so the
    /// walk panics inside rust_decimal's division several factors of e before
    /// the `None` fallback could ever fire. The silent print is real code but
    /// unreachable this way; the reachable damage is a panic mid-walk. Without
    /// the ceiling this test dies on that division, with it every print stays
    /// on the trajectory.
    #[test]
    fn a_runaway_positive_drift_stays_on_the_trajectory() {
        let mut config = SegmentCompose::new("MNQ", 4);
        config.sample = false;
        config.reopen_gaps = false;
        config.start_price = 20_000.0;
        let mut source = SegmentSource::new(
            library(vec![segment("d1", None, &[0, 500_000_000])]),
            config,
        )
        .expect("a valid library");
        // The ceiling is on the tick grid (1e9 divided by 0.25 is exact), so
        // the pinned print is the ceiling itself rather than a rounding of it.
        let ceiling = Decimal::from(1_000_000_000_u64);
        let mut last = Decimal::ZERO;
        for _ in 0..500 {
            let price = price_of(&source.next_tick().expect("endless"));
            assert!(
                price >= Decimal::from(20_000) && price <= ceiling,
                "a monotonically rising river printed {price}, outside \
                 [20000, MID_CEILING]"
            );
            last = price;
        }
        assert_eq!(
            last, ceiling,
            "a walk this steep must END pinned to the ceiling; a price below it \
             after 500 ticks means the rail is not where the band says"
        );
        assert!(
            source.clamps() > 0,
            "a walk this steep must reach the ceiling"
        );
    }

    /// Finding 5: the clock used to `saturating_add`, so a near-max `start_ns`
    /// froze every later tick at `u64::MAX` and the source became a
    /// constant-time stream that still claimed to be endless. `--start` is a
    /// raw u64 an operator types, so this is one command away, not 580 years.
    ///
    /// The `start_ns` is deliberately chosen to leave room for four ticks, and that is the
    /// whole design of the test. The helper's `dt_ns` is 1 ms per trade, so
    /// `u64::MAX - 4_500_000` emits at MAX-3.5ms, -2.5ms, -1.5ms and -0.5ms and
    /// then cannot advance. A `start_ns` one tick from the end would emit a single
    /// timestamp, and the duplicate-timestamp assertion - the one that names
    /// the defect - would be vacuously true over a single-element vector while
    /// the test still went red under the bug for an unrelated reason. The loop
    /// therefore breaks at a cap rather than asserting on the count, so the
    /// frozen-timestamp assertion is reached and fires under `saturating_add`.
    #[test]
    fn a_clock_that_cannot_advance_ends_the_river_instead_of_freezing_it() {
        let mut config = SegmentCompose::new("MNQ", 8);
        config.sample = false;
        config.start_ns = u64::MAX - 4_500_000;
        let mut source =
            SegmentSource::new(library(vec![segment("d1", None, &[0, 0, 0, 0])]), config)
                .expect("a valid library");
        let mut seen = Vec::new();
        while let Some(tick) = source.next_tick() {
            seen.push(tick.ts_event());
            if seen.len() >= 12 {
                break;
            }
        }
        let mut deduped = seen.clone();
        deduped.dedup();
        assert_eq!(
            deduped, seen,
            "no two ticks may share a frozen timestamp; saw {seen:?}"
        );
        assert!(
            seen.len() >= 3,
            "the fixture must leave room for several ticks or the duplicate \
             check above proves nothing; saw {}",
            seen.len()
        );
        assert!(
            source.clock_exhausted(),
            "the source stopped for a reason it did not name"
        );
        assert_eq!(
            source.fault(),
            Some(crate::TickFault::SegmentClockExhausted {
                clock_ns: u64::MAX - 500_000
            }),
            "a dyn TickSource reader must see why the endless river ended"
        );
    }

    /// The distance a named tape window asks for is a cost, never a fault.
    ///
    /// The target here sits at tick 1,500,000 - deliberately past the
    /// 1,000,000-tick cap a first attempt at bounding this walk imposed, which
    /// turned every reachable instant beyond it into a latched terminal fault
    /// that killed the source for good. A target only reachable by a long walk
    /// is exactly the case a composed window opens with, so the test walks one.
    #[test]
    fn a_reachable_distant_seek_arrives_and_leaves_the_river_readable() {
        let mut config = SegmentCompose::new("MNQ", 8);
        // No seam dead time and no sampling, so the target instant is a plain
        // multiple of the per-tick 1,000,000 ns the fixture carries and the
        // tick count the walk must cover is arithmetic rather than a guess.
        config.seam_gap_ns = 0;
        config.sample = false;
        let mut source = SegmentSource::new(
            library(vec![segment("d1", None, &[0, 0, 0, 0, 0, 0, 0, 0])]),
            config,
        )
        .expect("a valid library");

        let target = 1_500_000 * 1_000_000;
        let head = source
            .seek_to(target)
            .expect("a reachable instant must be reached, however far it is");
        assert_eq!(
            head.ts_event(),
            target,
            "the seek must return the first tick at or after the target, not the one after it"
        );
        assert_eq!(
            source.fault(),
            None,
            "walking a long way is not a source failure and must latch nothing"
        );
        assert_eq!(
            source.next_tick().map(|t| t.ts_event()),
            Some(target + 1_000_000),
            "the river must go on being read from where the seek left it"
        );
    }

    /// The one thing that genuinely puts a forward instant out of reach: a
    /// library with no time in it. Every `dt_ns` is zero and the seam gap is
    /// zero, so the composed clock is frozen at its origin and no amount of
    /// walking reaches a later instant. Decided without walking, and terminal
    /// because the river really is broken.
    #[test]
    fn a_seek_on_a_river_whose_clock_cannot_advance_faults_without_walking() {
        let mut frozen = segment("d1", None, &[0, 0, 0]);
        frozen.dt_ns = vec![0; frozen.trade_count];
        let mut config = SegmentCompose::new("MNQ", 8);
        config.seam_gap_ns = 0;
        let mut source =
            SegmentSource::new(library(vec![frozen]), config).expect("a valid library");

        // The origin itself is still servable: the frozen clock is only a bar
        // to instants after it.
        assert_eq!(source.seek_to(0).map(|t| t.ts_event()), Some(0));
        assert_eq!(source.fault(), None);

        assert!(source.seek_to(1).is_none());
        assert_eq!(
            source.fault(),
            Some(crate::TickFault::SegmentSeekUnreachable { target_ns: 1 }),
            "an unreachable forward window must stop with a visible fault"
        );
        assert!(
            source.next_tick().is_none(),
            "that fault is terminal, like every other way this source ends"
        );
    }

    #[test]
    fn an_unrepresentable_composed_price_faults_instead_of_panicking() {
        let mut source = SegmentSource::new(
            library(vec![segment("d1", None, &[0])]),
            SegmentCompose::new("MNQ", 8),
        )
        .expect("a valid library");
        source.price = f64::INFINITY;
        assert!(source.emit_price().is_none());
        assert_eq!(
            source.fault(),
            Some(crate::TickFault::SegmentPrice { clock_ns: 0 })
        );
        assert!(
            source.next_tick().is_none(),
            "a faulted source is terminal and may not recover on its next draw"
        );
    }

    /// Finding 4: `ret[0] != 0` is a rule the shared fixture states and the
    /// loader did not check, so a hand-edited library put a silent extra jump
    /// at every seam while `reopen_gaps: false` still claimed to make none.
    #[test]
    fn a_segment_whose_first_return_is_not_zero_is_refused() {
        let err = SegmentSource::new(
            library(vec![segment("d1", None, &[17, 0])]),
            SegmentCompose::new("MNQ", 1),
        )
        .expect_err("a non-anchoring first trade is refused")
        .to_string();
        assert!(err.contains("ret[0]=17"), "{err}");
    }

    #[test]
    fn a_side_outside_the_dbn_alphabet_is_refused_rather_than_read_as_unsided() {
        let mut typo = segment("d1", None, &[0, 0]);
        typo.side = vec!['B', 'b'];
        let err = SegmentSource::new(library(vec![typo]), SegmentCompose::new("MNQ", 1))
            .expect_err("a typo is not an unsided print")
            .to_string();
        assert!(err.contains("outside the DBN alphabet"), "{err}");
    }

    /// The river's origin is not a seam. A sampled first segment carrying an
    /// `open_gap_ret` used to apply it before the first print, so the river
    /// started somewhere other than `start_price` - a gap measured against a
    /// session that is not in this river.
    #[test]
    fn the_first_print_sits_at_the_configured_start_price() {
        let mut config = SegmentCompose::new("MNQ", 2);
        config.sample = false;
        config.reopen_gaps = true;
        config.start_price = 20_000.0;
        let mut source =
            SegmentSource::new(library(vec![segment("d1", Some(9_950_331), &[0])]), config)
                .expect("a valid library");
        assert_eq!(
            price_of(&source.next_tick().expect("endless")),
            Decimal::from(20_000),
            "the first segment's gap must not displace the river's origin"
        );
    }

    /// The rule the fixture states must be a rule something checks. Adding a
    /// sixth `rules` entry to the shared artifact without wiring it to a
    /// validator is exactly the "nothing detects a missing fixture" hole one
    /// level down, and nothing but this test would notice it. Each pin names
    /// where its rule is enforced; a new rule fails here until it has one.
    ///
    /// Its scope is the whole contract, not the `rules` array alone, because a
    /// gate whose stated scope differs from its real scope is the defect this
    /// arc keeps finding. `validate` also enforces the aggressor alphabet,
    /// which the fixture states in `units` rather than in `rules` - so that
    /// statement is pinned here too, separately and by name, instead of being
    /// left as a sixth enforcement nobody counted.
    ///
    /// Matching is by search, never by position. Zipping the two arrays would
    /// report a reorder of the JSON as "this rule is no longer enforced", which
    /// sends the next reader to the wrong place; each pin finds its own rule.
    #[test]
    fn every_rule_the_conformance_fixture_states_is_enforced_somewhere() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../analysis/segment_library_conformance.json");
        let text = std::fs::read_to_string(&path).expect("the committed fixture");
        let raw: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
        let rules: Vec<&str> = raw["rules"]
            .as_array()
            .expect("a rules array")
            .iter()
            .map(|r| r.as_str().expect("a rule string"))
            .collect();
        // (rule prefix, where it is enforced)
        let pinned = [
            (
                "dt_ns, ret, size and side are PARALLEL",
                "validate: length agreement",
            ),
            ("ret[0] is always 0", "validate: the anchor refusal"),
            (
                "A segment carries NO absolute price",
                "structural: the reader type has no price field to carry one",
            ),
            (
                "trade_count is never 0",
                "validate: the empty-segment refusal",
            ),
            ("version must equal", "validate: the version refusal"),
        ];
        for (prefix, site) in pinned {
            assert!(
                rules.iter().any(|rule| rule.starts_with(prefix)),
                "no fixture rule starts with {prefix:?} any more, but {site} \
                 still enforces it; the artifact and the validator disagree"
            );
        }
        assert_eq!(
            rules.len(),
            pinned.len(),
            "the fixture states {} rules but only {} are pinned to an enforcement \
             site; wire the new one up before pinning it here. Rules: {rules:?}",
            rules.len(),
            pinned.len()
        );
        // The sixth enforcement, stated in `units` rather than in `rules`.
        let side_units = raw["units"]["side"].as_str().expect("a side unit");
        assert!(
            side_units.contains('B') && side_units.contains('A') && side_units.contains('N'),
            "the fixture's side alphabet {side_units:?} no longer names B/A/N, \
             which validate's aggressor refusal enforces verbatim"
        );
    }

    /// The shared fixture both crates parse. `mogwai-lab` has the matching
    /// test on its own reading of the same file; a format drift fails one side
    /// here rather than leaving both green against private assumptions.
    #[test]
    fn the_conformance_fixture_composes() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../analysis/segment_library_conformance.json");
        let library = SegmentLibrary::load(&path).expect("the committed fixture");
        assert_eq!(library.window, "asia");
        let mut source = SegmentSource::new(library, SegmentCompose::new("MNQ", 42))
            .expect("the fixture composes");
        for _ in 0..500 {
            assert!(
                source.next_tick().is_some(),
                "the composed river is endless"
            );
        }
    }
}
