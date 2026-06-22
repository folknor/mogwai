use std::str::FromStr;

use anyhow::Context;
use mogwai_protocol::{AggressorSide as MogwaiAggressorSide, InstrumentDef};
use nautilus_core::UnixNanos;
use nautilus_model::{
    data::{QuoteTick as NautilusQuoteTick, TradeTick as NautilusTradeTick},
    enums::AggressorSide,
    identifiers::{InstrumentId, Symbol as NautilusSymbol, TradeId},
    instruments::{InstrumentAny, currency_pair::CurrencyPair},
    types::{Price, Quantity, currency::Currency},
};
use rust_decimal::{Decimal, prelude::ToPrimitive};

use crate::MOGWAI_VENUE;

pub(crate) fn price(d: Decimal, precision: u8) -> Price {
    Price::new(d.to_f64().expect("decimal fits f64"), precision)
}

pub(crate) fn quantity(d: Decimal, precision: u8) -> Quantity {
    Quantity::new(d.to_f64().expect("decimal fits f64"), precision)
}

pub(crate) fn aggressor(a: MogwaiAggressorSide) -> AggressorSide {
    match a {
        MogwaiAggressorSide::NoAggressor => AggressorSide::NoAggressor,
        MogwaiAggressorSide::Buyer => AggressorSide::Buyer,
        MogwaiAggressorSide::Seller => AggressorSide::Seller,
    }
}

pub(crate) fn instrument_id(def: &InstrumentDef) -> InstrumentId {
    InstrumentId::new(NautilusSymbol::from(def.symbol.as_str()), *MOGWAI_VENUE)
}

pub(crate) fn trade_tick(
    t: &mogwai_protocol::TradeTick,
    id: InstrumentId,
    def: &InstrumentDef,
    ts_init: UnixNanos,
) -> NautilusTradeTick {
    NautilusTradeTick::new(
        id,
        price(t.price, def.price_precision),
        quantity(t.size, def.size_precision),
        aggressor(t.aggressor),
        TradeId::from(format!("{}-{}", t.symbol, t.ts_event)),
        UnixNanos::from(t.ts_event),
        ts_init,
    )
}

pub(crate) fn quote_tick(
    q: &mogwai_protocol::QuoteTick,
    id: InstrumentId,
    def: &InstrumentDef,
    ts_init: UnixNanos,
) -> NautilusQuoteTick {
    NautilusQuoteTick::new(
        id,
        price(q.bid_px, def.price_precision),
        price(q.ask_px, def.price_precision),
        quantity(q.bid_sz, def.size_precision),
        quantity(q.ask_sz, def.size_precision),
        UnixNanos::from(q.ts_event),
        ts_init,
    )
}

pub(crate) fn instrument_any(
    def: &InstrumentDef,
    ts_init: UnixNanos,
) -> anyhow::Result<InstrumentAny> {
    let id = instrument_id(def);
    let base =
        Currency::from_str(&def.base).with_context(|| format!("unknown base {}", def.base))?;
    let quote =
        Currency::from_str(&def.quote).with_context(|| format!("unknown quote {}", def.quote))?;
    let pair = CurrencyPair::new(
        id,
        NautilusSymbol::from(def.symbol.as_str()),
        base,
        quote,
        def.price_precision,
        def.size_precision,
        price(def.price_increment, def.price_precision),
        quantity(def.size_increment, def.size_precision),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        UnixNanos::from(0),
        ts_init,
    );
    Ok(InstrumentAny::CurrencyPair(pair))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def() -> InstrumentDef {
        InstrumentDef {
            symbol: "BTCUSDT".into(),
            base: "BTC".into(),
            quote: "USDT".into(),
            price_precision: 2,
            size_precision: 8,
            price_increment: Decimal::new(1, 2),
            size_increment: Decimal::new(1, 8),
        }
    }

    #[test]
    fn trade_conversion_uses_instrument_precision_and_ts_trade_id() {
        let def = def();
        let trade = mogwai_protocol::TradeTick {
            symbol: "BTCUSDT".into(),
            price: Decimal::new(123456, 3),
            size: Decimal::new(1, 3),
            aggressor: MogwaiAggressorSide::Buyer,
            ts_event: 42,
        };

        let tick = trade_tick(&trade, instrument_id(&def), &def, UnixNanos::from(7));

        assert_eq!(tick.price.precision, 2);
        assert_eq!(tick.size.precision, 8);
        assert_eq!(tick.trade_id, TradeId::from("BTCUSDT-42"));
        assert_eq!(tick.ts_event, UnixNanos::from(42));
        assert_eq!(tick.ts_init, UnixNanos::from(7));
    }
}
