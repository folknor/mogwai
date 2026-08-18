// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The COMPOSE half of the session-segment sampler: a [`TickSource`] that
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
//! WHY RETURNS SPACE MAKES THE LOOP SEAMLESS. A segment carries no absolute
//! price. Composing is therefore integration: the tape holds a running price,
//! each stored log return multiplies it, and a segment boundary is just the
//! point where the returns start coming from a different slice. There is no
//! level to reconcile, so any segment follows any other without a
//! discontinuity - which is the whole reason an endless single-session tape is
//! expressible at all. Absolute price level is an integration constant (owner
//! ruling, 2026-08-12).
//!
//! WHAT LANDS AT THE SEAM. Each segment records `open_gap_ret`, the measured
//! return from the last real print before its window to its own first print -
//! for an Asia slice, the jump across the daily break. Applying it at the seam
//! reproduces real reopen gaps in the composed tape, which the fitted generator
//! does not produce at all (the owner's defect 2). It is a config knob because
//! the direction calls for feature injectors that toggle: see
//! [`SegmentCompose::reopen_gaps`].

use std::path::Path;

use mogwai_protocol::{AggressorSide, Symbol, TradeTick};
use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha12Rng;
use rust_decimal::Decimal;
use serde::Deserialize;

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
    /// artifact is a FILE: it can be hand-edited, truncated by a full disk, or
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
                     without advancing the tape",
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
    /// Where the tape's price level starts. An integration constant: it scales
    /// the whole tape and changes no return.
    pub start_price: f64,
    /// First tick's timestamp, unix ns.
    pub start_ns: u64,
    /// Draw order seed.
    pub seed: u64,
    /// Dead time inserted between the last trade of one segment and the window
    /// start of the next. Real calendar time between two Asia sessions is a
    /// day; an ENDLESS-Asia tape deliberately elides it, so this is the visible
    /// seam and one second is enough to keep timestamps strictly increasing
    /// without opening a hole on the chart.
    pub seam_gap_ns: u64,
    /// Apply each segment's measured `open_gap_ret` at the seam. Off yields a
    /// continuous tape with no reopen gaps - which is what the fitted generator
    /// produces today, and therefore the useful A/B against it.
    pub reopen_gaps: bool,
    /// Sample segments uniformly with replacement (endless variety from a
    /// bounded library) instead of cycling them in library order.
    pub sample: bool,
}

impl SegmentCompose {
    /// Defaults for an endless single-session tape: real reopen gaps on,
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

/// An endless tape composed from a segment library.
///
/// Never returns `None`: like [`crate::GeneratedSource`] it is effectively
/// infinite, so a caller bounds it by span rather than by exhaustion.
#[derive(Debug)]
pub struct SegmentSource {
    library: SegmentLibrary,
    config: SegmentCompose,
    rng: ChaCha12Rng,
    tick_size: Decimal,
    /// Index into `library.segments` of the segment being played.
    segment: usize,
    /// Cursor into that segment's parallel arrays.
    cursor: usize,
    /// How many segments have been started, so ORDER mode can cycle.
    played: usize,
    price: f64,
    ts: u64,
    /// Set when the next tick opens a segment, so the seam work (gap return,
    /// seam dead time) happens exactly once per boundary.
    at_seam: bool,
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
        if !(config.start_price.is_finite() && config.start_price > 0.0) {
            return Err(SegmentError::Refusal(format!(
                "start price {} is not a positive finite level; returns-space \
                 composition integrates against it",
                config.start_price
            )));
        }
        let mut source = Self {
            rng: ChaCha12Rng::seed_from_u64(config.seed),
            tick_size,
            segment: 0,
            cursor: 0,
            played: 0,
            price: config.start_price,
            ts: config.start_ns,
            at_seam: true,
            library,
            config,
        };
        source.segment = source.draw_segment();
        Ok(source)
    }

    /// The window this tape is composed from, for provenance in a dump header.
    pub fn window(&self) -> &str {
        &self.library.window
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
    fn take_seam(&mut self) {
        self.played += 1;
        self.segment = self.draw_segment();
        self.cursor = 0;
        self.at_seam = true;
        self.ts = self.ts.saturating_add(self.config.seam_gap_ns);
    }

    /// Snaps the running level onto the instrument's grid.
    ///
    /// The running price stays in f64 and only the EMITTED value is snapped:
    /// rounding the running level would accumulate the rounding error into
    /// every subsequent return, which over an endless tape is a slow drift
    /// rather than a bounded one.
    fn emit_price(&self) -> Decimal {
        let Some(level) = Decimal::from_f64_retain(self.price) else {
            // Unreachable for a finite positive price, which the constructor
            // established and multiplication by a finite factor preserves.
            return self.tick_size;
        };
        let steps = (level / self.tick_size).round();
        (steps * self.tick_size).normalize()
    }
}

impl TickSource for SegmentSource {
    fn next_tick(&mut self) -> Option<TickEvent> {
        // A segment whose cursor has run out hands over at the seam. `while`
        // rather than `if`: `validate` forbids an empty segment, so one hop
        // always suffices, but the loop makes that a property of the data
        // rather than an assumption the walk would panic on.
        while self.cursor >= self.library.segments[self.segment].trade_count {
            self.take_seam();
        }
        let segment = &self.library.segments[self.segment];
        let i = self.cursor;

        if self.at_seam {
            if self.config.reopen_gaps
                && let Some(gap) = segment.open_gap_ret
            {
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "a nano-log-return is far inside f64's exact integer range"
                )]
                let gap_ret = gap as f64 * 1e-9;
                self.price *= gap_ret.exp();
            }
            self.at_seam = false;
        }

        #[expect(
            clippy::cast_precision_loss,
            reason = "a nano-log-return is far inside f64's exact integer range"
        )]
        let ret = segment.ret[i] as f64 * 1e-9;
        self.price *= ret.exp();
        self.ts = self.ts.saturating_add(segment.dt_ns[i]);

        let tick = TradeTick {
            symbol: Symbol::from(self.config.symbol.as_str()),
            price: self.emit_price(),
            size: Decimal::from(segment.size[i]),
            aggressor: match segment.side[i] {
                'B' => AggressorSide::Buyer,
                'A' => AggressorSide::Seller,
                _ => AggressorSide::NoAggressor,
            },
            ts_event: self.ts,
        };
        self.cursor += 1;
        Some(TickEvent::Trade(tick))
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
        assert_eq!(run_a, run_b, "same seed, same tape");
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
    /// gaps OFF, a seam introduces no price move, because the incoming
    /// segment's first return is zero and its level came from the running tape.
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
                "a zero-return tape must stay flat across seams when gaps are off"
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
            assert!(source.next_tick().is_some(), "the composed tape is endless");
        }
    }
}
