//! Selects the market-data source the server replays.
//!
//! Every tick is generated from the committed fingerprint; the server opens no
//! CSV on either the live or historical market-data path.

use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};

use mogwai_data::{
    CheckpointIndex, Fingerprint, GeneratedSource, GeneratorScalars, MergeSource, SessionProfile,
    TickEvent, TickSource,
};
use mogwai_protocol::{InstrumentDef, MarketRegime, Symbol, default_instruments};
use rust_decimal::Decimal;

// Runaway backstop on a from-origin seek, NOT the binding window check - that is
// the analytic `start >= data_origin` refuse the `/trades` handler applies before
// any synthesis. Sized to the `seek_throughput_measurement` reporter below: at
// ~1.9M ticks/sec synthesis, ~190k ticks is the largest from-origin drain that
// still finishes inside the ~100ms request-path budget. Every legitimate on-tape
// request lives in `[data_origin, sim_now]` by construction, well inside this; the
// cap only stops a pathological seek (an accelerated session run long enough that a
// fresh-subscribe seek to sim-now exceeds it - the uptime ceiling Landing 4 lifts
// via checkpointing). There is no longer a frozen tape origin: both builders anchor
// each generator at the boot-derived `data_origin` the caller threads in, so the
// data timeline tracks the advertised clock and cannot drift into the multi-year
// seek that emptied forward warmups. `pub(crate)` so the replay spawn can name
// the exhausted budget when it reports a seek that died against this cap.
pub(crate) const MAX_HISTORY_SEEK_TICKS: usize = 190_000;

// Checkpoint spacing for the per-symbol `CheckpointIndex`, in ticks. A seek
// restores the nearest checkpoint at or before the target and replays at most
// this many ticks, so the residual drain costs `K / synthesis_rate` - at
// ~1.9M ticks/sec and K = 8192, ~4 ms, far inside the request budget and far
// under `MAX_HISTORY_SEEK_TICKS`. Small enough that snapshots stay sparse (one
// generator clone per K ticks), large enough that the index never bloats.
const CHECKPOINT_K: usize = 8192;

// Process-global per-symbol checkpoint index over the CLEAN (regime-free) tape,
// keyed by `(symbol, data_origin)`. Shared across every live subscribe and
// `/trades` request, so the O(span) walk to the frontier is paid once per symbol
// rather than per request - this is what lifts the accelerated uptime ceiling
// Landing 1 priced (a from-origin seek to sim-now grows with session length and
// eventually blows the cap; a checkpoint resume is flat in K). Regime'd
// realizations are rare havoc requests on a different tape and are not cached;
// they take the plain from-origin drain, still bounded by the backstop cap.
fn checkpoint_store() -> &'static Mutex<HashMap<(String, u64), CheckpointIndex>> {
    static STORE: OnceLock<Mutex<HashMap<(String, u64), CheckpointIndex>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn fingerprint() -> &'static Fingerprint {
    static FP: OnceLock<Fingerprint> = OnceLock::new();
    FP.get_or_init(Fingerprint::from_repo_json)
}

fn seed_for(symbol: &str) -> u64 {
    const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    symbol.bytes().fold(FNV_OFFSET_BASIS, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME)
    })
}

#[derive(Debug, Clone)]
pub(crate) struct InstrumentProfile {
    pub(crate) def: InstrumentDef,
    pub(crate) scalars: GeneratorScalars,
    pub(crate) session: SessionProfile,
}

impl InstrumentProfile {
    pub(crate) fn new(
        def: InstrumentDef,
        mut scalars: GeneratorScalars,
        session: SessionProfile,
    ) -> Self {
        scalars.symbol = def.symbol.clone();
        Self {
            def,
            scalars,
            session,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct InstrumentProfiles {
    by_symbol: HashMap<Symbol, InstrumentProfile>,
}

impl InstrumentProfiles {
    pub(crate) fn defaults() -> Self {
        let fp = fingerprint();
        let profiles = default_instruments()
            .into_iter()
            .map(|def| default_profile(def, fp))
            .collect();
        Self::from_profiles(profiles)
    }

    pub(crate) fn from_profiles(profiles: Vec<InstrumentProfile>) -> Self {
        let by_symbol = profiles
            .into_iter()
            .map(|profile| (profile.def.symbol.clone(), profile))
            .collect();
        Self { by_symbol }
    }

    pub(crate) fn get(&self, symbol: &str) -> Option<&InstrumentProfile> {
        self.by_symbol.get(symbol)
    }

    pub(crate) fn instrument_defs(&self) -> Vec<InstrumentDef> {
        let mut defs: Vec<_> = self
            .by_symbol
            .values()
            .map(|profile| profile.def.clone())
            .collect();
        defs.sort_by(|a, b| a.symbol.cmp(&b.symbol));
        defs
    }
}

fn default_profile(def: InstrumentDef, fp: &Fingerprint) -> InstrumentProfile {
    let mut scalars = GeneratorScalars::from_fingerprint_medians(&def.symbol, fp);
    scalars.modal_tick = def.price_increment;
    scalars.price_decimals = u32::from(def.price_precision);
    InstrumentProfile::new(def, scalars, fp.session_profile.clone())
}

/// One symbol's tape: a generator anchored at the shared boot-derived
/// `data_origin`, wrapped in the `BoundedSeek` runaway backstop. Both the live
/// and history builders compose this so every realization is a sample of the same
/// single coherent tape - the live stream and a warmup window over the same
/// `data_origin` are seeks into one sequence, never two divergent re-anchorings.
fn bounded_generator(
    profile: &InstrumentProfile,
    seed: u64,
    data_origin: u64,
    fp: &Fingerprint,
    regime: Option<MarketRegime>,
    seek_target: Option<u64>,
) -> Box<dyn TickSource> {
    let source = positioned_generator(profile, seed, data_origin, fp, regime, seek_target);
    Box::new(BoundedSeek {
        inner: Box::new(source),
        cap: MAX_HISTORY_SEEK_TICKS,
    })
}

/// The symbol's generator, already positioned for the upcoming seek. For the
/// clean tape with a real target, restore the nearest checkpoint at or before the
/// target from the shared index so the `BoundedSeek` drain that follows is O(K),
/// not O(span). Otherwise - a "from origin" request (`None` target) or a regime'd
/// realization - anchor a fresh generator at `data_origin` and let the drain walk
/// forward. The result is byte-identical either way: a checkpoint is the exact
/// walk state, so resuming and replaying reproduces the from-origin ticks.
fn positioned_generator(
    profile: &InstrumentProfile,
    seed: u64,
    data_origin: u64,
    fp: &Fingerprint,
    regime: Option<MarketRegime>,
    seek_target: Option<u64>,
) -> GeneratedSource {
    if regime.is_none()
        && let Some(target) = seek_target
        && target > data_origin
    {
        // A panic mid-synthesis (a pathological configured scalar/regime,
        // arithmetic overflow) while this `std::Mutex` is held would otherwise
        // poison it permanently: every later checkpoint-path call - price-less
        // market orders, seeked `/trades`, every live `Subscribe` - would
        // `.expect` its way into the same panic forever, turning one transient
        // fault into a standing partial outage. Recovering the guard instead
        // means at worst the entry the panicking call was updating is left
        // stale (a later seek against exactly that `(symbol, data_origin)` can
        // repeat the fault), while every other entry, and that one once it is
        // naturally re-derived, keeps serving.
        let mut store = checkpoint_store().lock().unwrap_or_else(|poisoned| {
            tracing::error!("checkpoint store mutex recovered from a prior panic");
            poisoned.into_inner()
        });
        let index = store
            .entry((profile.def.symbol.clone(), data_origin))
            .or_insert_with(|| {
                CheckpointIndex::new(
                    fresh_generator(profile, seed, data_origin, fp, None),
                    CHECKPOINT_K,
                    // The from-origin backstop doubles as the index's per-call
                    // extension cap: a `start` past the live frontier walks at
                    // most this far before the seek gives up, never spinning.
                    MAX_HISTORY_SEEK_TICKS,
                )
            });
        return index.source_at_or_before(target);
    }
    fresh_generator(profile, seed, data_origin, fp, regime)
}

/// The price a fresh MARKET order would print at `sim_now`: the next trade on
/// the same checkpointed tape live subscribers replay, drawn without
/// disturbing any subscriber's own seek position. Used to give the engine's
/// no-book "fill at the order's own price" a price to fill a price-less
/// MARKET order with - the wire's `SubmitOrder.price` is `None` for a market
/// order (mirroring Nautilus, which never stamps one), and the venue is the
/// only side with the synthesized tape, so it stamps the order before the
/// engine ever validates it. Returns `None` for an unconfigured symbol
/// (leaving the engine's "unknown instrument" rejection to fire as normal)
/// or when the bounded drain below cannot reach `sim_now` (the order stays
/// price-less and the order path rejects it with its own "could not
/// synthesize a market price" reason - an honest refusal rather than a fill
/// at a price hours behind the clock, and honest about WHOSE failure it is:
/// the engine's "submit price required" would blame a client that correctly
/// sent a market order price-less).
pub(crate) fn current_price(
    symbol: &str,
    profiles: &InstrumentProfiles,
    data_origin: u64,
    sim_now: u64,
) -> Option<Decimal> {
    let fp = fingerprint();
    let profile = profiles.get(symbol)?;
    // `positioned_generator` only restores the nearest checkpoint AT OR BEFORE
    // the target; the residual walk up to the target itself is the caller's to
    // drain. The live and history paths drain it via `MergeSource::starting_at`
    // -> `BoundedSeek::seek_to`; composing the identical `BoundedSeek` here
    // keeps the stamped price on the identical walk. Taking one bare
    // `next_tick()` instead would return the first tick after the last
    // checkpoint - up to `CHECKPOINT_K` ticks (hours of tape at the default
    // cadence) behind the price a live subscriber sees at the same instant.
    let mut source = BoundedSeek {
        inner: Box::new(positioned_generator(
            profile,
            seed_for(symbol),
            data_origin,
            fp,
            None,
            Some(sim_now),
        )),
        cap: MAX_HISTORY_SEEK_TICKS,
    };
    let TickEvent::Trade(trade) = source.seek_to(sim_now)? else {
        return None;
    };
    Some(trade.price)
}

fn fresh_generator(
    profile: &InstrumentProfile,
    seed: u64,
    data_origin: u64,
    fp: &Fingerprint,
    regime: Option<MarketRegime>,
) -> GeneratedSource {
    GeneratedSource::new_with_session_profile(
        profile.scalars.clone(),
        seed,
        data_origin,
        fp,
        &profile.session,
        regime,
    )
}

pub(crate) fn build_live_source(
    symbols: &[String],
    start_ts: Option<u64>,
    regime: Option<MarketRegime>,
    profiles: &InstrumentProfiles,
    data_origin: u64,
    sim_now: u64,
) -> Option<Box<dyn TickSource>> {
    let fp = fingerprint();
    // Anchor at `data_origin` and SEEK to sim-now (or the resume cursor) rather
    // than re-anchoring a fresh seed-head at the cursor. A fresh re-anchor restarts
    // the price walk at `start_price` on every (re)subscribe - the reconnect price
    // reset - and a generator parked behind now firehoses (accelerated) or replays
    // hours of stale backfill at real-time gaps (identity). Seeking the shared tape
    // makes the first live tick land at `seek_target` on the one coherent walk.
    let seek_target = start_ts.unwrap_or(sim_now);
    let sources: Vec<Box<dyn TickSource>> = symbols
        .iter()
        .filter_map(|sym| {
            // A symbol absent from the configured set produces no source - the
            // generator never synthesizes a phantom tape for an instrument the
            // venue does not list, mirroring the engine rejecting an order for
            // an unknown instrument. A mixed subscribe streams only its known
            // symbols; an all-unknown subscribe collapses to None below.
            let profile = profiles.get(sym)?;
            Some(bounded_generator(
                profile,
                seed_for(sym),
                data_origin,
                fp,
                regime,
                Some(seek_target),
            ))
        })
        .collect();

    if sources.is_empty() {
        return None;
    }

    Some(Box::new(MergeSource::starting_at(
        sources,
        Some(seek_target),
    )))
}

pub(crate) fn build_history_source(
    symbol: &str,
    start: Option<u64>,
    regime: Option<MarketRegime>,
    profiles: &InstrumentProfiles,
    data_origin: u64,
) -> Option<Box<dyn TickSource>> {
    let fp = fingerprint();
    let profile = profiles.get(symbol)?;
    let bounded = bounded_generator(profile, seed_for(symbol), data_origin, fp, regime, start);
    // `start = None` means "from the origin": `starting_at` buffers the first tick
    // with no seek, serving the head of the tape. A `Some(start)` seeks into the
    // shared tape; the handler has already refused any `start < data_origin`, so a
    // legitimate seek always lands inside `[data_origin, sim_now]`.
    Some(Box::new(MergeSource::starting_at(vec![bounded], start)))
}

struct BoundedSeek {
    inner: Box<dyn TickSource>,
    cap: usize,
}

impl TickSource for BoundedSeek {
    fn next_tick(&mut self) -> Option<TickEvent> {
        self.inner.next_tick()
    }

    fn seek_to(&mut self, start_ts: u64) -> Option<TickEvent> {
        let mut tick = self.inner.next_tick()?;
        let mut drained = 0;
        while tick.ts_event() < start_ts && drained < self.cap {
            tick = self.inner.next_tick()?;
            drained += 1;
        }
        // If the cap was reached before any tick caught up to `start_ts`, the
        // seek failed within its bound: returning the still-pre-`start` tick
        // would leak trades earlier than the requested window and break the
        // `/trades` cursor contract (the next page must begin at the cursor,
        // never before it). Report the failed seek as `None` so the caller
        // yields a correct/empty page instead.
        if tick.ts_event() < start_ts {
            return None;
        }
        Some(tick)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;

    // A fixed boot-derived origin stand-in for the unit tests. The production
    // origin is `sim_now_at_boot - backfill_horizon`; the tests pin a concrete
    // instant so a from-origin seek and its expected tick are reproducible.
    const TEST_ORIGIN: u64 = 1_700_438_400_000_000_000;

    fn profile(
        symbol: &str,
        start_price: Decimal,
        price_increment: Decimal,
        price_precision: u8,
        session: SessionProfile,
    ) -> InstrumentProfile {
        let fp = fingerprint();
        let mut def = default_instruments()
            .into_iter()
            .next()
            .expect("default instrument");
        def.symbol = symbol.to_string();
        def.price_increment = price_increment;
        def.price_precision = price_precision;

        let mut scalars = GeneratorScalars::from_fingerprint_medians(symbol, fp);
        scalars.modal_tick = def.price_increment;
        scalars.price_decimals = u32::from(def.price_precision);
        scalars.start_price = start_price;
        InstrumentProfile::new(def, scalars, session)
    }

    fn btc_profile(
        symbol: &str,
        start_price: Decimal,
        session: SessionProfile,
    ) -> InstrumentProfile {
        profile(symbol, start_price, Decimal::new(1, 2), 2, session)
    }

    fn default_session() -> SessionProfile {
        fingerprint().session_profile.clone()
    }

    // A normalized session profile whose intensity_hour and dow_weight each sum
    // to 1.0, so it expresses intraday SHAPE rather than an absolute level - the
    // arrival rate itself lives in the generator's `mean_duration_s`, not here.
    // Hour 0 (which is where TEST_ORIGIN lands - it is exactly midnight UTC)
    // carries `hour0_share` of the day's intensity mass and the other 23 hours
    // split the remainder evenly, so the arrival multiplier at the first tick
    // (`share * 24`) is `hour0_share * 24`: a large share there means a dense
    // hour and a fast first tick, a small share a thin hour and a slow one. A
    // uniform level is deliberately NOT expressible this way, because a flat
    // profile carries no intraday shape and its multiplier normalizes to 1.0.
    fn shaped_session(hour0_share: f64) -> SessionProfile {
        let mut intensity_hour = [(1.0 - hour0_share) / 23.0; 24];
        intensity_hour[0] = hour0_share;
        SessionProfile {
            intensity_hour,
            vol_hour: [1.0; 24],
            dow_weight: [1.0 / 7.0; 7],
        }
    }

    // A single-symbol profile whose base cadence is pinned to `mean_duration_s`,
    // carrying the committed session envelope (whose arrival multiplier centers
    // near 1.0, so realized inter-tick spacing tracks the scalar). Used by the
    // Landing-1 measurement to price the from-origin seek at the committed
    // cadence extremes (median ~7.19s, fastest ~3.75s). The scalar must stay
    // inside the fingerprint range or `GeneratedSource::new` rejects it.
    fn cadence_profile(symbol: &str, mean_duration_s: f64) -> InstrumentProfile {
        let fp = fingerprint();
        let mut def = default_instruments()
            .into_iter()
            .next()
            .expect("default instrument");
        def.symbol = symbol.to_string();

        let mut scalars = GeneratorScalars::from_fingerprint_medians(symbol, fp);
        scalars.mean_duration_s = mean_duration_s;
        InstrumentProfile::new(def, scalars, fp.session_profile.clone())
    }

    // Price the from-origin `BoundedSeek` so the `MAX_HISTORY_SEEK_TICKS`
    // backstop cap `C` and the checkpoint-vs-raise-cap decision are
    // throughput-justified rather than guessed. This is
    // a REPORTER test - read the printed numbers; the only hard assertion pins
    // the request-path budget `B` for the worst legitimate on-tape warmup. The
    // F-floor verdict (does a single `C` satisfy both the per-request budget and
    // the 2h fresh-subscribe uptime floor?) is printed for the operator to read.
    //
    // Per-tick synthesis (GARCH walk + Weibull/ACD duration draw) dominates seek
    // cost; `BoundedSeek::seek_to` adds one integer compare per tick, negligible
    // against the float work, so a tight `next_tick` drain prices the seek
    // faithfully.
    #[test]
    fn seek_throughput_measurement() {
        use std::time::Instant;

        const HORIZON_S: f64 = 86_400.0; // 24h default backfill horizon
        const SPEED: f64 = 120.0; // accelerated clock factor
        const B_MS: f64 = 100.0; // per-request synthesis budget
        const F_WALL_S: f64 = 7_200.0; // venue-contract fresh-subscribe floor (2h)
        const DRAIN: usize = 100_000; // ticks pulled to time synthesis

        println!("\n=== Landing 1: from-origin seek measurement ===");
        println!(
            "horizon={HORIZON_S}s speed={SPEED}x B={B_MS}ms F={F_WALL_S}s drain={DRAIN} ticks"
        );

        // Throughput is per-tick CPU and is cadence-independent; the fast cadence
        // binds the cap because it packs the most ticks into a fixed time span,
        // so its tick-per-second-of-tape rate drives every budget conversion.
        let mut fast_tps = 0.0_f64;
        let mut fast_cadence_s = 0.0_f64;

        let range = &fingerprint().scalar_ranges.mean_duration_s;
        for (label, cadence_s) in [("default", range.median), ("fast", range.min)] {
            let profile = cadence_profile("BTCUSDT", cadence_s);
            let fp = fingerprint();
            let mut source = GeneratedSource::new_with_session_profile(
                profile.scalars.clone(),
                seed_for("BTCUSDT"),
                TEST_ORIGIN,
                fp,
                &profile.session,
                None,
            );

            let first = source.next_tick().expect("first tick").ts_event();
            let start = Instant::now();
            let mut last = first;
            for _ in 1..DRAIN {
                last = source.next_tick().expect("tick").ts_event();
            }
            let elapsed = start.elapsed();

            let tps = (DRAIN - 1) as f64 / elapsed.as_secs_f64();
            let realized_cadence_s = (last - first) as f64 / 1e9 / (DRAIN - 1) as f64;
            let ticks_24h = HORIZON_S / realized_cadence_s;
            let ticks_52h = (52.0 * 3_600.0) / realized_cadence_s;
            let seek_ms = |ticks: f64| ticks / tps * 1e3;

            println!("\n[{label}] requested cadence {cadence_s}s");
            println!("  realized cadence:   {realized_cadence_s:.3} s/tick");
            println!("  synthesis throughput: {tps:.0} ticks/sec");
            println!(
                "  24h-of-tape: {ticks_24h:.0} ticks -> {:.1} ms seek",
                seek_ms(ticks_24h)
            );
            println!(
                "  52h-of-tape: {ticks_52h:.0} ticks -> {:.1} ms seek",
                seek_ms(ticks_52h)
            );

            if label == "fast" {
                fast_tps = tps;
                fast_cadence_s = realized_cadence_s;
            }
        }

        // The decision (technical-implementation-spec pin 5): one analytic call.
        //   C_B = ticks affordable within the per-request budget B.
        //   C_F = ticks a fresh subscribe must seek to honor the F=2h floor at
        //         the fastest cadence: (horizon + F*speed) sim-seconds of tape.
        // A single backstop C satisfies both iff C_F <= C_B.
        let c_b = B_MS / 1e3 * fast_tps;
        let c_f = (HORIZON_S + F_WALL_S * SPEED) / fast_cadence_s;
        let ticks_24h_fast = HORIZON_S / fast_cadence_s;
        let warmup_24h_ms = ticks_24h_fast / fast_tps * 1e3;
        let c_f_ms = c_f / fast_tps * 1e3;

        // Uptime ceiling if C is sized to the per-request budget alone.
        let uptime_ceiling_s = (c_b * fast_cadence_s - HORIZON_S) / SPEED;

        println!("\n=== Verdict (fastest cadence binds) ===");
        println!("  C_B (budget-affordable cap): {c_b:.0} ticks ({B_MS} ms)");
        println!("  C_F (F=2h floor demand):     {c_f:.0} ticks ({c_f_ms:.0} ms at C_F-deep seek)");
        println!(
            "  24h warmup at fast cadence:  {ticks_24h_fast:.0} ticks -> {warmup_24h_ms:.1} ms"
        );
        println!(
            "  uptime ceiling under C_B:    {:.0} s wall ({:.1} min) vs F={F_WALL_S}s",
            uptime_ceiling_s,
            uptime_ceiling_s / 60.0
        );
        if c_f <= c_b {
            println!(
                "  VERDICT: single C in [{c_f:.0}, {c_b:.0}] satisfies B and F -> Landing 4 CLOSES"
            );
        } else {
            println!(
                "  VERDICT: B and F unsatisfiable by one C (C_F {c_f:.0} > C_B {c_b:.0}) -> Landing 4 PROCEEDS"
            );
        }

        // Hard gate: the worst LEGITIMATE on-tape from-origin seek - a 24h warmup
        // at the fastest cadence - must clear the request-path budget B. This is
        // the invariant Landing 2 sizes the raised cap against; it must hold
        // regardless of which seek strategy Landing 4 selects.
        assert!(
            warmup_24h_ms < B_MS,
            "24h fast-cadence warmup seek {warmup_24h_ms:.1} ms exceeds B={B_MS} ms budget"
        );
    }

    // Landing 4 re-measurement: with the checkpoint index warm, a seek's residual
    // replay is bounded by K REGARDLESS of how far the target sits from the origin
    // - this is the flat-in-K property that decouples per-request cost from session
    // length and lifts the accelerated uptime ceiling the from-origin seek could
    // not. A REPORTER (read the printed residuals); the hard assertion is that no
    // warm seek replays more than K ticks, at any distance.
    #[test]
    fn checkpointed_seek_is_flat_in_k() {
        use std::time::Instant;

        const HOUR_NS: u64 = 3_600_000_000_000;
        let fp = fingerprint();
        // Fast cadence packs the most ticks per hour of tape, so it is the worst
        // case for residual size; it must still stay within K.
        let profile = cadence_profile("BTCUSDT", fp.scalar_ranges.mean_duration_s.min);
        let mut index = CheckpointIndex::new(
            GeneratedSource::new_with_session_profile(
                profile.scalars.clone(),
                seed_for("BTCUSDT"),
                TEST_ORIGIN,
                fp,
                &profile.session,
                None,
            ),
            CHECKPOINT_K,
            MAX_HISTORY_SEEK_TICKS,
        );

        // Warm the index past the farthest target so each timed seek below is a
        // pure restore + residual drain, not an index extension.
        let targets = [6, 24, 48, 96].map(|h| TEST_ORIGIN + h * HOUR_NS);
        let _ = index.source_at_or_before(*targets.last().expect("targets"));

        println!("\n=== Landing 4: checkpointed seek cost vs distance (K={CHECKPOINT_K}) ===");
        for &target in &targets {
            let start = Instant::now();
            let mut source = index.source_at_or_before(target);
            let mut tick = source.next_tick().expect("first tick after resume");
            let mut residual = 1usize;
            while tick.ts_event() < target {
                tick = source.next_tick().expect("residual tick");
                residual += 1;
            }
            let elapsed = start.elapsed();
            let dist_h = (target - TEST_ORIGIN) as f64 / HOUR_NS as f64;
            println!(
                "  {dist_h:>3.0}h from origin: resume + {residual} residual ticks in {elapsed:?}"
            );
            assert!(
                residual <= CHECKPOINT_K,
                "residual replay {residual} exceeds K={CHECKPOINT_K} at {dist_h}h"
            );
        }
        println!(
            "  checkpoints held: {} (flat seek independent of distance)",
            index.checkpoint_count()
        );
    }

    // The MARKET-order price stamp and a live subscriber sample the same
    // checkpointed tape at the same instant, so they must agree exactly. The
    // target sits 24h (~12k ticks at the default cadence) past the origin -
    // beyond CHECKPOINT_K, so the nearest checkpoint is thousands of ticks
    // behind the target and a `current_price` that skips the residual drain
    // (taking the first tick after the checkpoint instead of the first tick at
    // or after sim-now) returns a price from hours earlier on the walk.
    #[test]
    fn current_price_matches_live_subscriber_at_same_instant() {
        const HOUR_NS: u64 = 3_600_000_000_000;
        let profiles = InstrumentProfiles::defaults();
        let sim_now = TEST_ORIGIN + 24 * HOUR_NS;
        let symbols = ["BTCUSDT".to_string()];

        let stamped =
            current_price("BTCUSDT", &profiles, TEST_ORIGIN, sim_now).expect("stamped price");
        let mut live = build_live_source(&symbols, None, None, &profiles, TEST_ORIGIN, sim_now)
            .expect("live source");
        let TickEvent::Trade(first) = live.next_tick().expect("first live tick") else {
            panic!("generated source emits trades");
        };

        assert!(
            first.ts_event >= sim_now,
            "live subscriber's first tick {} precedes sim_now {sim_now}",
            first.ts_event
        );
        assert_eq!(
            stamped, first.price,
            "a MARKET order stamped at sim_now must fill at the price a live \
             subscriber at the same instant sees, not a checkpoint-stale one"
        );
    }

    #[test]
    fn live_source_honors_symbol_and_window_anchor() {
        let symbols = vec!["KEUR".to_string()];
        let profiles = InstrumentProfiles::from_profiles(vec![btc_profile(
            "KEUR",
            Decimal::from(100),
            default_session(),
        )]);
        let mut source = build_live_source(
            &symbols,
            Some(86_401_000_000_000),
            None,
            &profiles,
            86_400_000_000_000,
            86_401_000_000_000,
        )
        .expect("source");
        let TickEvent::Trade(trade) = source.next_tick().expect("first tick") else {
            panic!("generated source emits trades");
        };

        assert_eq!(trade.symbol, "KEUR");
        assert!(trade.ts_event >= 86_401_000_000_000);
    }

    #[test]
    fn omitted_start_ts_seeks_to_sim_now_instead_of_dumping_from_origin() {
        // A `Subscribe` with no `start_ts` must seek the live stream to
        // sim-now, not re-anchor it at `data_origin` and let the first read
        // drain the whole elapsed-since-boot window at max speed before live
        // pacing kicks in. 48h between origin and sim-now is far enough that a
        // from-origin catch-up dump and a seek-to-now land nowhere near each
        // other, so the first tick's timestamp tells them apart.
        const HOUR_NS: u64 = 3_600_000_000_000;
        let sim_now = TEST_ORIGIN + 48 * HOUR_NS;
        let symbols = vec!["BTCUSDT".to_string()];
        let profiles = InstrumentProfiles::defaults();

        let mut source = build_live_source(&symbols, None, None, &profiles, TEST_ORIGIN, sim_now)
            .expect("source");
        let TickEvent::Trade(trade) = source.next_tick().expect("first tick") else {
            panic!("generated source emits trades");
        };

        assert!(
            trade.ts_event >= sim_now,
            "first live tick at {} should land at or after sim_now {sim_now}, not back near \
             data_origin {TEST_ORIGIN}",
            trade.ts_event
        );
    }

    #[test]
    fn btcusdt_uses_engine_price_grid() {
        let fp = fingerprint();
        let profiles = InstrumentProfiles::defaults();
        let profile = profiles.get("BTCUSDT").expect("BTCUSDT profile");

        assert_eq!(profile.scalars.modal_tick, profile.def.price_increment);
        assert_eq!(
            profile.scalars.price_decimals,
            u32::from(profile.def.price_precision)
        );
        assert!(profile.scalars.validate(fp).is_ok());
    }

    #[test]
    fn live_source_applies_regime() {
        let symbols = vec!["BTCUSDT".to_string()];
        let profiles = InstrumentProfiles::defaults();
        let mut clean = build_live_source(
            &symbols,
            Some(TEST_ORIGIN),
            None,
            &profiles,
            TEST_ORIGIN,
            TEST_ORIGIN,
        )
        .expect("clean source");
        let mut drought = build_live_source(
            &symbols,
            Some(TEST_ORIGIN),
            Some(MarketRegime::LiquidityDrought { thin_factor: 5.0 }),
            &profiles,
            TEST_ORIGIN,
            TEST_ORIGIN,
        )
        .expect("drought source");

        let clean_mean = mean_duration(&mut clean, 2_000);
        let drought_mean = mean_duration(&mut drought, 2_000);
        assert!(
            drought_mean >= clean_mean * 4.0,
            "clean_mean={clean_mean} drought_mean={drought_mean}"
        );
    }

    #[test]
    fn unknown_symbols_do_not_generate_sources() {
        let profiles = InstrumentProfiles::defaults();
        let symbols = vec!["FAKE".to_string()];

        assert!(
            build_live_source(
                &symbols,
                Some(TEST_ORIGIN),
                None,
                &profiles,
                TEST_ORIGIN,
                TEST_ORIGIN
            )
            .is_none()
        );
        assert!(build_history_source("FAKE", None, None, &profiles, TEST_ORIGIN).is_none());
    }

    #[test]
    fn configured_scalars_set_price_level_by_symbol() {
        let profiles = InstrumentProfiles::from_profiles(vec![
            profile(
                "EURUSD",
                Decimal::new(11_000, 4),
                Decimal::new(1, 4),
                4,
                default_session(),
            ),
            btc_profile("BTCUSDT", Decimal::from(60_000), default_session()),
        ]);

        let mut eur =
            build_history_source("EURUSD", None, None, &profiles, TEST_ORIGIN).expect("eur source");
        let mut btc = build_history_source("BTCUSDT", None, None, &profiles, TEST_ORIGIN)
            .expect("btc source");
        let TickEvent::Trade(eur) = eur.next_tick().expect("eur first trade") else {
            panic!("generated source emits trades");
        };
        let TickEvent::Trade(btc) = btc.next_tick().expect("btc first trade") else {
            panic!("generated source emits trades");
        };

        assert!(eur.price < Decimal::from(2), "eur price={}", eur.price);
        assert!(btc.price > Decimal::from(59_000), "btc price={}", btc.price);
    }

    #[test]
    fn configured_session_profile_controls_arrival_shape() {
        // Both profiles are normalized (they pass session validation); they
        // differ only in how much of the day's intensity mass sits in hour 0,
        // where TEST_ORIGIN begins. The dense-hour-0 profile fires its first
        // tick sooner than the thin-hour-0 one.
        let slow_profiles = InstrumentProfiles::from_profiles(vec![btc_profile(
            "BTCUSDT",
            Decimal::from(60_000),
            shaped_session(0.001),
        )]);
        let fast_profiles = InstrumentProfiles::from_profiles(vec![btc_profile(
            "BTCUSDT",
            Decimal::from(60_000),
            shaped_session(0.5),
        )]);

        let mut slow = build_history_source("BTCUSDT", None, None, &slow_profiles, TEST_ORIGIN)
            .expect("slow source");
        let mut fast = build_history_source("BTCUSDT", None, None, &fast_profiles, TEST_ORIGIN)
            .expect("fast source");

        let slow_ts = slow.next_tick().expect("slow first trade").ts_event();
        let fast_ts = fast.next_tick().expect("fast first trade").ts_event();

        assert!(fast_ts < slow_ts, "fast_ts={fast_ts} slow_ts={slow_ts}");
    }

    fn mean_duration(source: &mut Box<dyn TickSource>, draw: usize) -> f64 {
        let mut prior = source.next_tick().expect("first tick").ts_event();
        let mut total = 0.0;
        for _ in 1..draw {
            let ts = source.next_tick().expect("next tick").ts_event();
            total += (ts - prior) as f64 / 1_000_000_000.0;
            prior = ts;
        }
        total / (draw - 1) as f64
    }
}
