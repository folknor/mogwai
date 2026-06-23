//! Selects the market-data source the server replays.
//!
//! Every tick is generated from the committed fingerprint; the server opens no
//! CSV on either the live or historical market-data path.

use std::sync::OnceLock;

use mogwai_data::{
    Fingerprint, GeneratedSource, GeneratorScalars, MergeSource, TickEvent, TickSource,
};
use mogwai_protocol::MarketRegime;
use rust_decimal::Decimal;

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

fn scalars_for(symbol: &str, fp: &Fingerprint) -> GeneratorScalars {
    match symbol {
        "XBTUSD" => GeneratorScalars::xbtusd_anchor(fp),
        "BTCUSDT" => {
            let mut scalars = GeneratorScalars::from_fingerprint_medians(symbol, fp);
            scalars.modal_tick = Decimal::new(1, 2);
            scalars.price_decimals = 2;
            scalars
        }
        _ => GeneratorScalars::from_fingerprint_medians(symbol, fp),
    }
}

pub(crate) fn build_live_source(
    symbols: &[String],
    start_ts: Option<u64>,
    regime: Option<MarketRegime>,
) -> Option<Box<dyn TickSource>> {
    let fp = fingerprint();
    let anchor = start_ts.unwrap_or(ORIGIN_TS);
    let sources: Vec<Box<dyn TickSource>> = symbols
        .iter()
        .map(|sym| {
            let scalars = scalars_for(sym, fp);
            let source: Box<dyn TickSource> = Box::new(GeneratedSource::new(
                scalars,
                seed_for(sym),
                anchor,
                fp,
                regime,
            ));
            source
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
) -> Box<dyn TickSource> {
    let fp = fingerprint();
    let scalars = scalars_for(symbol, fp);
    let source: Box<dyn TickSource> = Box::new(GeneratedSource::new(
        scalars,
        seed_for(symbol),
        ORIGIN_TS,
        fp,
        regime,
    ));
    let bounded: Box<dyn TickSource> = Box::new(BoundedSeek {
        inner: source,
        cap: MAX_HISTORY_SEEK_TICKS,
    });
    Box::new(MergeSource::starting_at(vec![bounded], start))
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
        Some(tick)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_source_honors_symbol_and_window_anchor() {
        let symbols = vec!["KEUR".to_string()];
        let mut source =
            build_live_source(&symbols, Some(86_401_000_000_000), None).expect("source");
        let TickEvent::Trade(trade) = source.next_tick().expect("first tick") else {
            panic!("generated source emits trades");
        };

        assert_eq!(trade.symbol, "KEUR");
        assert!(trade.ts_event >= 86_401_000_000_000);
    }

    #[test]
    fn btcusdt_uses_engine_price_grid() {
        let fp = fingerprint();
        let scalars = scalars_for("BTCUSDT", fp);

        assert_eq!(scalars.modal_tick, Decimal::new(1, 2));
        assert_eq!(scalars.price_decimals, 2);
        assert!(scalars.validate(fp).is_ok());
    }

    #[test]
    fn live_source_applies_regime() {
        let symbols = vec!["KEUR".to_string()];
        let mut clean = build_live_source(&symbols, Some(ORIGIN_TS), None).expect("clean source");
        let mut drought = build_live_source(
            &symbols,
            Some(ORIGIN_TS),
            Some(MarketRegime::LiquidityDrought { thin_factor: 5.0 }),
        )
        .expect("drought source");

        let clean_mean = mean_duration(&mut clean, 2_000);
        let drought_mean = mean_duration(&mut drought, 2_000);
        assert!(
            drought_mean >= clean_mean * 4.0,
            "clean_mean={clean_mean} drought_mean={drought_mean}"
        );
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
