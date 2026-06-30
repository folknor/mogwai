//! Selects the market-data source the server replays.
//!
//! Every tick is generated from the committed fingerprint; the server opens no
//! CSV on either the live or historical market-data path.

use std::{collections::HashMap, sync::OnceLock};

use mogwai_data::{
    Fingerprint, GeneratedSource, GeneratorScalars, MergeSource, SessionProfile, TickEvent,
    TickSource,
};
use mogwai_protocol::{InstrumentDef, MarketRegime, Symbol, default_instruments};

const ORIGIN_TS: u64 = 1_700_438_400_000_000_000;
const MAX_HISTORY_SEEK_TICKS: usize = 50_000;

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

pub(crate) fn build_live_source(
    symbols: &[String],
    start_ts: Option<u64>,
    regime: Option<MarketRegime>,
    profiles: &InstrumentProfiles,
) -> Option<Box<dyn TickSource>> {
    let fp = fingerprint();
    let anchor = start_ts.unwrap_or(ORIGIN_TS);
    let sources: Vec<Box<dyn TickSource>> = symbols
        .iter()
        .filter_map(|sym| {
            // A symbol absent from the configured set produces no source - the
            // generator never synthesizes a phantom tape for an instrument the
            // venue does not list, mirroring the engine rejecting an order for
            // an unknown instrument. A mixed subscribe streams only its known
            // symbols; an all-unknown subscribe collapses to None below.
            let profile = profiles.get(sym)?;
            let source: Box<dyn TickSource> = Box::new(GeneratedSource::new_with_session_profile(
                profile.scalars.clone(),
                seed_for(sym),
                anchor,
                fp,
                &profile.session,
                regime,
            ));
            Some(source)
        })
        .collect();

    if sources.is_empty() {
        return None;
    }

    Some(Box::new(MergeSource::new(sources)))
}

pub(crate) fn build_history_source(
    symbol: &str,
    start: Option<u64>,
    regime: Option<MarketRegime>,
    profiles: &InstrumentProfiles,
) -> Option<Box<dyn TickSource>> {
    let fp = fingerprint();
    let profile = profiles.get(symbol)?;
    let source: Box<dyn TickSource> = Box::new(GeneratedSource::new_with_session_profile(
        profile.scalars.clone(),
        seed_for(symbol),
        ORIGIN_TS,
        fp,
        &profile.session,
        regime,
    ));
    let bounded: Box<dyn TickSource> = Box::new(BoundedSeek {
        inner: source,
        cap: MAX_HISTORY_SEEK_TICKS,
    });
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

    fn flat_session(intensity: f64) -> SessionProfile {
        SessionProfile {
            intensity_hour: [intensity; 24],
            vol_hour: [1.0; 24],
            dow_weight: [1.0; 7],
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

    // Landing 1 of `docs/forward-origin-spec.md`: price the from-origin
    // `BoundedSeek` so the backstop cap `C` and the checkpoint-vs-raise-cap
    // decision (Landing 4) are throughput-justified rather than guessed. This is
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
                ORIGIN_TS,
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

    #[test]
    fn live_source_honors_symbol_and_window_anchor() {
        let symbols = vec!["KEUR".to_string()];
        let profiles = InstrumentProfiles::from_profiles(vec![btc_profile(
            "KEUR",
            Decimal::from(100),
            default_session(),
        )]);
        let mut source =
            build_live_source(&symbols, Some(86_401_000_000_000), None, &profiles).expect("source");
        let TickEvent::Trade(trade) = source.next_tick().expect("first tick") else {
            panic!("generated source emits trades");
        };

        assert_eq!(trade.symbol, "KEUR");
        assert!(trade.ts_event >= 86_401_000_000_000);
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
        let mut clean =
            build_live_source(&symbols, Some(ORIGIN_TS), None, &profiles).expect("clean source");
        let mut drought = build_live_source(
            &symbols,
            Some(ORIGIN_TS),
            Some(MarketRegime::LiquidityDrought { thin_factor: 5.0 }),
            &profiles,
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

        assert!(build_live_source(&symbols, Some(ORIGIN_TS), None, &profiles).is_none());
        assert!(build_history_source("FAKE", None, None, &profiles).is_none());
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

        let mut eur = build_history_source("EURUSD", None, None, &profiles).expect("eur source");
        let mut btc = build_history_source("BTCUSDT", None, None, &profiles).expect("btc source");
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
        let slow_profiles = InstrumentProfiles::from_profiles(vec![btc_profile(
            "BTCUSDT",
            Decimal::from(60_000),
            flat_session(0.01),
        )]);
        let fast_profiles = InstrumentProfiles::from_profiles(vec![btc_profile(
            "BTCUSDT",
            Decimal::from(60_000),
            flat_session(1.0),
        )]);

        let mut slow =
            build_history_source("BTCUSDT", None, None, &slow_profiles).expect("slow source");
        let mut fast =
            build_history_source("BTCUSDT", None, None, &fast_profiles).expect("fast source");

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
