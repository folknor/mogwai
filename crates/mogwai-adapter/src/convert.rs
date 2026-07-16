// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

use std::str::FromStr;

use anyhow::Context;
use mogwai_protocol::{AggressorSide as MogwaiAggressorSide, InstrumentDef, Side};
use nautilus_core::UnixNanos;
use nautilus_model::{
    data::{QuoteTick as NautilusQuoteTick, TradeTick as NautilusTradeTick},
    enums::{AggressorSide, OrderSide, OrderType, TimeInForce},
    identifiers::{InstrumentId, Symbol as NautilusSymbol, TradeId},
    instruments::{InstrumentAny, currency_pair::CurrencyPair},
    types::{Money, Price, Quantity, currency::Currency},
};
use rust_decimal::Decimal;

use crate::MOGWAI_VENUE;

/// Converts a wire `Decimal` price into a nautilus `Price` at `precision`,
/// returning an error rather than panicking on a hostile wire value.
///
/// `Price::new` (and the `Quantity`/`Money` twins) `assert!`-panic when the
/// f64 is NaN/inf or outside `[PRICE_MIN, PRICE_MAX]`, or when `precision`
/// exceeds nautilus `FIXED_PRECISION`. The whole point of mogwai is feeding
/// ugly data, so the `decimal_to_f64` saturation alone is not enough: an
/// over-precise `price_precision` advertised by a havoc'd instrument, or a
/// magnitude past the fixed-point range, still trips the constructor. Routing
/// through `new_checked` lets the call sites drop the offending tick with a
/// warning instead of downing the spawned reader/exec task that has no
/// supervisor.
pub(crate) fn price(d: Decimal, precision: u8) -> anyhow::Result<Price> {
    Price::new_checked(mogwai_protocol::decimal_to_f64(d), precision).context("convert price")
}

/// Converts a wire `Decimal` size into a nautilus `Quantity` at `precision`.
/// Fallible for the same reason as [`price`]: `Quantity::new` also rejects
/// negatives, so any negative `size`/`leaves_qty`/`bid_sz` off the wire would
/// otherwise panic the whole adapter task.
pub(crate) fn quantity(d: Decimal, precision: u8) -> anyhow::Result<Quantity> {
    Quantity::new_checked(mogwai_protocol::decimal_to_f64(d), precision).context("convert quantity")
}

/// Converts a wire `Decimal` amount into a nautilus `Money` of `currency`.
/// Fallible for the same reason as [`price`]: the `decimal_to_f64` saturation
/// caps the magnitude, but `Money::new` still `assert!`-panics on a NaN/inf or
/// out-of-range f64, so the exec task that books commissions and balances must
/// be able to drop a pathological amount rather than crash.
pub(crate) fn money(d: Decimal, currency: Currency) -> anyhow::Result<Money> {
    Money::new_checked(mogwai_protocol::decimal_to_f64(d), currency).context("convert money")
}

pub(crate) fn aggressor(a: MogwaiAggressorSide) -> AggressorSide {
    match a {
        MogwaiAggressorSide::NoAggressor => AggressorSide::NoAggressor,
        MogwaiAggressorSide::Buyer => AggressorSide::Buyer,
        MogwaiAggressorSide::Seller => AggressorSide::Seller,
    }
}

pub(crate) fn wire_side(side: OrderSide) -> anyhow::Result<Side> {
    match side {
        OrderSide::Buy => Ok(Side::Buy),
        OrderSide::Sell => Ok(Side::Sell),
        other => anyhow::bail!("unsupported order side {other:?}"),
    }
}

pub(crate) fn wire_order_type(order_type: OrderType) -> anyhow::Result<mogwai_protocol::OrderType> {
    match order_type {
        OrderType::Market => Ok(mogwai_protocol::OrderType::Market),
        OrderType::Limit => Ok(mogwai_protocol::OrderType::Limit),
        other => anyhow::bail!("unsupported order type {other:?}"),
    }
}

pub(crate) fn wire_time_in_force(tif: TimeInForce) -> anyhow::Result<mogwai_protocol::TimeInForce> {
    match tif {
        TimeInForce::Gtc => Ok(mogwai_protocol::TimeInForce::Gtc),
        TimeInForce::Ioc => Ok(mogwai_protocol::TimeInForce::Ioc),
        TimeInForce::Fok => Ok(mogwai_protocol::TimeInForce::Fok),
        other => anyhow::bail!("unsupported time in force {other:?}"),
    }
}

pub(crate) fn instrument_id(def: &InstrumentDef) -> InstrumentId {
    InstrumentId::new(NautilusSymbol::from(def.symbol.as_str()), *MOGWAI_VENUE)
}

/// Builds the synthetic nautilus `TradeId` for a wire trade.
///
/// The wire `TradeTick` carries no exchange-assigned trade id or sequence
/// number, and the adapter's own `PollCursor` explicitly tolerates multiple
/// trades sharing one `ts_event` (see its doc comment in client.rs), so the
/// id must be derived from the tick's own fields. Keying on symbol+ts_event
/// alone collides for any two such trades; folding in price/size/aggressor
/// closes the collision for the common case PollCursor exists to handle -
/// genuinely distinct trades landing on the same nanosecond.
///
/// Nautilus caps a `TradeId` at 36 ASCII chars and the panicking
/// constructors (`TradeId::new`, the `From<String>` impl) assert past that.
/// A readable composition of all five fields
/// (`symbol-ts-price-size-aggressor`) blows the cap for any realistic
/// nanosecond timestamp plus an 8-decimal size - 40+ chars - which would
/// panic on essentially every live trade. So the id keeps the decimal
/// `ts_event` for debuggability and compresses the full field tuple into a
/// 56-bit FNV-1a hash: at most 20 digits + 1 dash + 14 hex chars = 35, so
/// the length cap can never trip. Two same-nanosecond trades now collide
/// only on a 56-bit hash collision of their full field tuples.
fn trade_id(t: &mogwai_protocol::TradeTick) -> anyhow::Result<TradeId> {
    let key = format!(
        "{}-{}-{}-{}-{:?}",
        t.symbol, t.ts_event, t.price, t.size, t.aggressor
    );
    // 64-bit FNV-1a, truncated below. Deterministic across processes and
    // platforms (unlike std's RandomState-seeded hasher), so a replayed
    // stream re-derives identical ids.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in key.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    // `new_checked` is belt-and-braces: the format above is structurally
    // non-empty ASCII within the length cap, but the trade path must never
    // hold a panicking constructor even if the format drifts.
    TradeId::new_checked(format!(
        "{}-{:014x}",
        t.ts_event,
        hash & 0x00ff_ffff_ffff_ffff
    ))
    .context("convert trade id")
}

pub(crate) fn trade_tick(
    t: &mogwai_protocol::TradeTick,
    id: InstrumentId,
    def: &InstrumentDef,
    ts_init: UnixNanos,
) -> anyhow::Result<NautilusTradeTick> {
    // `quantity` correctly accepts zero (QUANTITY_MIN is 0 in nautilus, and a
    // zero Quantity is legitimate elsewhere, e.g. a filled order's
    // leaves_qty), but the panicking `TradeTick::new` asserts a positive size
    // on the *raw* fixed-point value. That trips on a wire size of exactly
    // zero and on any positive size small enough to round to raw 0 at the
    // instrument's size_precision. Route through `new_checked` so the caller
    // drops the offending tick with a warning instead of panicking the
    // spawned reader/poll task that has no supervisor.
    NautilusTradeTick::new_checked(
        id,
        price(t.price, def.price_precision)?,
        quantity(t.size, def.size_precision)?,
        aggressor(t.aggressor),
        trade_id(t)?,
        UnixNanos::from(t.ts_event),
        ts_init,
    )
    .context("convert trade tick")
}

pub(crate) fn quote_tick(
    q: &mogwai_protocol::QuoteTick,
    id: InstrumentId,
    def: &InstrumentDef,
    ts_init: UnixNanos,
) -> anyhow::Result<NautilusQuoteTick> {
    Ok(NautilusQuoteTick::new(
        id,
        price(q.bid_px, def.price_precision)?,
        price(q.ask_px, def.price_precision)?,
        quantity(q.bid_sz, def.size_precision)?,
        quantity(q.ask_sz, def.size_precision)?,
        UnixNanos::from(q.ts_event),
        ts_init,
    ))
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
        price(def.price_increment, def.price_precision)?,
        quantity(def.size_increment, def.size_precision)?,
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

        let tick = trade_tick(&trade, instrument_id(&def), &def, UnixNanos::from(7))
            .expect("well-formed trade converts");

        assert_eq!(tick.price.precision, 2);
        assert_eq!(tick.size.precision, 8);
        // The id is ts_event plus a 14-hex-char hash of the full field tuple
        // (see `trade_id`); pin the shape rather than the hash value.
        let id = tick.trade_id.to_string();
        assert!(id.starts_with("42-"), "id keeps decimal ts_event: {id}");
        assert_eq!(id.len(), "42-".len() + 14, "ts plus 56-bit hex hash: {id}");
        assert_eq!(tick.ts_event, UnixNanos::from(42));
        assert_eq!(tick.ts_init, UnixNanos::from(7));
    }

    #[test]
    fn trade_id_fits_nautilus_length_cap_for_realistic_ticks() {
        // Nautilus caps TradeId at 36 ASCII chars and panics past it. The old
        // readable id (symbol-ts-price-size-aggressor) exceeded the cap for
        // any realistic nanosecond timestamp plus 8-decimal size, panicking
        // the reader/poll task on essentially every live trade. The hashed id
        // is bounded at 35 chars even for a u64::MAX timestamp.
        let def = def();
        let trade = mogwai_protocol::TradeTick {
            symbol: "BTCUSDT".into(),
            price: Decimal::new(6_512_345, 2),
            size: Decimal::new(32_332_866, 8),
            aggressor: MogwaiAggressorSide::Seller,
            ts_event: u64::MAX,
        };

        let tick = trade_tick(&trade, instrument_id(&def), &def, UnixNanos::from(0))
            .expect("realistic trade converts without tripping the id cap");

        assert!(
            tick.trade_id.to_string().len() <= 36,
            "id must fit the nautilus cap: {}",
            tick.trade_id
        );
    }

    #[test]
    fn trade_id_disambiguates_same_ts_event_trades() {
        // bug-hunt A.4: keying the id on symbol+ts_event alone collided for
        // any two trades landing on the same nanosecond - exactly the case
        // the adapter's own PollCursor is built to tolerate. Folding in
        // price/size/aggressor must keep two such trades distinct.
        let def = def();
        let id = instrument_id(&def);
        let first = mogwai_protocol::TradeTick {
            symbol: "BTCUSDT".into(),
            price: Decimal::new(10_000, 2),
            size: Decimal::new(1, 0),
            aggressor: MogwaiAggressorSide::Buyer,
            ts_event: 42,
        };
        let second = mogwai_protocol::TradeTick {
            symbol: "BTCUSDT".into(),
            price: Decimal::new(10_100, 2),
            size: Decimal::new(2, 0),
            aggressor: MogwaiAggressorSide::Seller,
            ts_event: 42,
        };

        let first_tick = trade_tick(&first, id, &def, UnixNanos::from(0)).expect("converts");
        let second_tick = trade_tick(&second, id, &def, UnixNanos::from(0)).expect("converts");

        assert_ne!(first_tick.trade_id, second_tick.trade_id);
    }

    #[test]
    fn price_rejects_precision_beyond_fixed_precision() {
        // A havoc'd instrument advertising an over-fine precision would panic
        // the bare `Price::new`; `new_checked` must surface it as an error so
        // the call site can drop the offending tick instead.
        let err = price(Decimal::new(1, 0), 50);
        assert!(err.is_err(), "over-precise price must be rejected");
    }

    #[test]
    fn quantity_rejects_negative_size() {
        // `Quantity::new` panics on negatives; the wire can carry a negative
        // size/leaves_qty under market-regime havoc.
        let err = quantity(Decimal::new(-1, 0), 8);
        assert!(err.is_err(), "negative quantity must be rejected");
    }

    #[test]
    fn quantity_accepts_zero() {
        // QUANTITY_MIN is 0 in nautilus: a zero Quantity is legitimate in
        // general (a filled order's leaves_qty, an empty book level), so the
        // positive-size guard belongs to the trade path, not here.
        assert!(quantity(Decimal::ZERO, 8).is_ok());
    }

    #[test]
    fn trade_tick_rejects_zero_size_without_panicking() {
        // A zero size passes `quantity` (see `quantity_accepts_zero`) but the
        // panicking `TradeTick::new` asserts a positive size, which would down
        // the unsupervised reader/poll task. `trade_tick` must surface it as
        // an error instead.
        let def = def();
        let trade = mogwai_protocol::TradeTick {
            symbol: "BTCUSDT".into(),
            price: Decimal::new(10_000, 2),
            size: Decimal::ZERO,
            aggressor: MogwaiAggressorSide::Buyer,
            ts_event: 42,
        };

        let err = trade_tick(&trade, instrument_id(&def), &def, UnixNanos::from(0));
        assert!(err.is_err(), "zero-size trade must be rejected, not panic");
    }

    #[test]
    fn trade_tick_rejects_size_that_rounds_to_zero() {
        // Nautilus checks positivity on the raw fixed-point value, so a
        // positive wire size below half a step at the instrument's
        // size_precision (0.000000004 at precision 8) rounds to raw 0 and is
        // the same panic hazard as an exact zero.
        let def = def();
        let trade = mogwai_protocol::TradeTick {
            symbol: "BTCUSDT".into(),
            price: Decimal::new(10_000, 2),
            size: Decimal::new(4, 9),
            aggressor: MogwaiAggressorSide::Buyer,
            ts_event: 42,
        };

        let err = trade_tick(&trade, instrument_id(&def), &def, UnixNanos::from(0));
        assert!(
            err.is_err(),
            "rounds-to-zero size must be rejected, not panic"
        );
    }
}
