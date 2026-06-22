//! Wire protocol shared by the mogwai fake broker and its broadarrow adapter.
//!
//! This is the single source of truth for the native JSON-over-WS protocol. The
//! broadarrow-side adapter path-deps this crate so both ends serialize identical types.
//! mogwai never imports nautilus; nautilus types are mirrored here only as far as
//! the wire needs them.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

pub type Symbol = String;
/// Client-assigned order id (nautilus `ClientOrderId`).
pub type ClientOrderId = String;
/// Venue-assigned order id (mogwai-assigned `VenueOrderId`).
pub type VenueOrderId = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    Buy,
    Sell,
}

/// Aggressor (taker) side of a trade. Kraken's history dump omits this, so
/// replayed ticks are `NoAggressor` unless a permutation infers it (tick rule).
/// Mirrors nautilus `AggressorSide`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AggressorSide {
    NoAggressor,
    Buyer,
    Seller,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderType {
    Market,
    Limit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeInForce {
    Gtc,
    Ioc,
    Fok,
}

/// Client → server messages (order entry + market-data subscription).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientMessage {
    Subscribe {
        symbols: Vec<Symbol>,
        /// Replay from this unix-nanosecond instant forward. `None` starts at
        /// the beginning of available history.
        #[serde(default)]
        start_ts: Option<u64>,
    },
    Unsubscribe {
        symbols: Vec<Symbol>,
    },
    SubmitOrder(SubmitOrder),
    CancelOrder {
        client_order_id: ClientOrderId,
    },
    ModifyOrder {
        client_order_id: ClientOrderId,
        price: Option<Decimal>,
        quantity: Option<Decimal>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscribe_start_ts_round_trips_and_legacy_payloads_default() {
        let with_start = ClientMessage::Subscribe {
            symbols: vec!["X".into()],
            start_ts: Some(123),
        };
        let json = serde_json::to_string(&with_start).unwrap();
        let decoded: ClientMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            decoded,
            ClientMessage::Subscribe {
                symbols,
                start_ts: Some(123)
            } if symbols == vec!["X"]
        ));

        let without_start = ClientMessage::Subscribe {
            symbols: vec!["X".into()],
            start_ts: None,
        };
        let json = serde_json::to_string(&without_start).unwrap();
        let decoded: ClientMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            decoded,
            ClientMessage::Subscribe {
                symbols,
                start_ts: None
            } if symbols == vec!["X"]
        ));

        let legacy = r#"{"type":"Subscribe","symbols":["X"]}"#;
        let decoded: ClientMessage = serde_json::from_str(legacy).unwrap();
        assert!(matches!(
            decoded,
            ClientMessage::Subscribe {
                symbols,
                start_ts: None
            } if symbols == vec!["X"]
        ));
    }

    #[test]
    fn account_state_with_positions_round_trips() {
        let state = AccountState {
            balances: vec![Balance {
                currency: "USDT".into(),
                total: Decimal::from(-300),
                free: Decimal::from(-1000),
                locked: Decimal::from(700),
            }],
            positions: vec![Position {
                symbol: "BTCUSDT".into(),
                quantity: Decimal::from(3),
                avg_px: Decimal::from(100),
            }],
            ts_event: 123,
        };

        let json = serde_json::to_string(&state).unwrap();
        let decoded: AccountState = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded.balances[0].currency, state.balances[0].currency);
        assert_eq!(decoded.balances[0].total, state.balances[0].total);
        assert_eq!(decoded.balances[0].free, state.balances[0].free);
        assert_eq!(decoded.balances[0].locked, state.balances[0].locked);
        assert_eq!(decoded.positions[0].symbol, state.positions[0].symbol);
        assert_eq!(decoded.positions[0].quantity, state.positions[0].quantity);
        assert_eq!(decoded.positions[0].avg_px, state.positions[0].avg_px);
        assert_eq!(decoded.ts_event, state.ts_event);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitOrder {
    pub client_order_id: ClientOrderId,
    pub symbol: Symbol,
    pub side: Side,
    pub order_type: OrderType,
    pub quantity: Decimal,
    pub price: Option<Decimal>,
    pub time_in_force: TimeInForce,
}

/// Server → client messages (execution events + market data).
///
/// These map onto nautilus `OrderEventAny` variants on the adapter side. The
/// divergences mogwai is built to emit (partials via `leaves_qty`, rejects,
/// duplicates, delays, drops) are expressed entirely through this stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerMessage {
    OrderAccepted {
        client_order_id: ClientOrderId,
        venue_order_id: VenueOrderId,
        ts_event: u64,
    },
    OrderRejected {
        client_order_id: ClientOrderId,
        reason: String,
        ts_event: u64,
    },
    OrderCanceled {
        client_order_id: ClientOrderId,
        venue_order_id: VenueOrderId,
        ts_event: u64,
    },
    OrderFilled(OrderFilled),
    AccountState(AccountState),
    Trade(TradeTick),
    Quote(QuoteTick),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderFilled {
    pub client_order_id: ClientOrderId,
    pub venue_order_id: VenueOrderId,
    pub trade_id: String,
    pub symbol: Symbol,
    pub side: Side,
    pub last_qty: Decimal,
    pub last_px: Decimal,
    /// Remaining quantity. `> 0` ⇒ this is a partial fill.
    pub leaves_qty: Decimal,
    pub commission: Decimal,
    pub ts_event: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountState {
    pub balances: Vec<Balance>,
    pub positions: Vec<Position>,
    pub ts_event: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Balance {
    pub currency: String,
    pub total: Decimal,
    pub free: Decimal,
    pub locked: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub symbol: Symbol,
    /// Signed net quantity: positive is long, negative is short, zero is flat.
    pub quantity: Decimal,
    /// Volume-weighted average entry price of the open quantity. Zero when flat.
    pub avg_px: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeTick {
    pub symbol: Symbol,
    pub price: Decimal,
    pub size: Decimal,
    pub aggressor: AggressorSide,
    pub ts_event: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuoteTick {
    pub symbol: Symbol,
    pub bid_px: Decimal,
    pub ask_px: Decimal,
    pub bid_sz: Decimal,
    pub ask_sz: Decimal,
    pub ts_event: u64,
}

/// Out-of-band control plane: arm deterministic divergences for tests.
///
/// This is the reason mogwai exists as an external process - it can emit ugly,
/// realistic event streams an in-process matching engine never would, to drive
/// broadarrow's `classify` → brake/quarantine/restart layer.
pub mod control {
    use super::{ClientOrderId, Decimal, Deserialize, Serialize};

    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(tag = "type")]
    pub enum Divergence {
        /// Fill the next matching order only `fraction` of the way, leaving the rest open.
        PartialFillNext {
            client_order_id: ClientOrderId,
            fraction: Decimal,
        },
        /// Reject the next submitted order with `reason`.
        RejectNextSubmit { reason: String },
        /// Delay every outbound execution event by `ms`.
        DelayAcks { ms: u64 },
        /// Emit the next fill event twice.
        DuplicateNextFill,
        /// Swallow the next account-state update (induce account drift).
        DropNextAccountUpdate,
        /// Stop sending anything for `ms` (simulate a venue blackout).
        GoDark { ms: u64 },
    }
}
