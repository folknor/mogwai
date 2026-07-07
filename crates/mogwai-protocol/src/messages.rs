use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::havoc::{EventKind, MarketRegime};
use crate::{ClientOrderId, Symbol, VenueOrderId};

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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start_ts: Option<u64>,
        /// Optional generator-level market regime for this subscription.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        regime: Option<MarketRegime>,
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

/// API-boundary guard for a `SubmitOrder`, mirroring `validate_conn_havoc` /
/// `validate_market_regime` / `validate_divergence` / `validate_client_havoc`
/// in style and message convention. `quantity` must be strictly positive, and
/// a `Limit` order must carry a strictly positive `price` (a `Market` order's
/// price is legitimately absent - Nautilus MARKET orders carry no price).
///
/// This is the crate's own gate, not a substitute for the venue-side check:
/// `mogwai-engine`'s `validate_submit` is the authoritative, instrument-aware
/// guard (grid alignment, instrument lookup, precision) and remains the last
/// line of defense regardless of whether a caller runs this first.
///
/// The apparent disagreement with the engine - this validator ACCEPTS a
/// priceless `Market` order while `mogwai-engine`'s `validate_submit` REJECTS
/// one ("submit price required") - is a deliberate two-phase split, not a
/// drift. This gate validates the PRE-stamp wire, exactly what the adapter puts
/// on the socket: a nautilus MARKET order legitimately carries no price there.
/// The server then STAMPS a synthetic execution price onto every Market order
/// (on both the WS and HTTP carriers, failing loudly if synthesis fails) before
/// the engine ever sees it, so by the time `validate_submit` runs the order
/// always carries a price and a still-priceless one is a genuine post-stamp
/// bug. The engine is the authoritative POST-stamp gate; this is the honest
/// PRE-stamp one, and the two are consistent precisely because the stamp sits
/// between them.
pub fn validate_submit_order(order: &SubmitOrder) -> Result<(), &'static str> {
    if order.quantity <= Decimal::ZERO {
        return Err("quantity must be > 0");
    }
    match order.price {
        Some(price) if price <= Decimal::ZERO => Err("price must be > 0"),
        None if order.order_type == OrderType::Limit => Err("Limit order must carry a price"),
        _ => Ok(()),
    }
}

/// API-boundary guard for a `ClientMessage::ModifyOrder`'s `price`/`quantity`
/// pair, mirroring `validate_submit_order` in style. At least one of the two
/// must be present - both absent decodes as a no-op amend that changes
/// nothing - and whichever is present must be strictly positive.
pub fn validate_modify_order(
    price: Option<Decimal>,
    quantity: Option<Decimal>,
) -> Result<(), &'static str> {
    if price.is_none() && quantity.is_none() {
        return Err("ModifyOrder must set price and/or quantity");
    }
    if price.is_some_and(|p| p <= Decimal::ZERO) {
        return Err("price must be > 0");
    }
    if quantity.is_some_and(|q| q <= Decimal::ZERO) {
        return Err("quantity must be > 0");
    }
    Ok(())
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
    OrderUpdated {
        client_order_id: ClientOrderId,
        venue_order_id: VenueOrderId,
        /// New total order quantity after the amend.
        quantity: Decimal,
        /// New price after the amend. `None` for a still-priceless order.
        price: Option<Decimal>,
        /// Remaining quantity after the amend.
        leaves_qty: Decimal,
        ts_event: u64,
    },
    OrderModifyRejected {
        client_order_id: ClientOrderId,
        /// Present when the order is known but the amend is illegal; absent
        /// when the order id is unknown to the venue.
        venue_order_id: Option<VenueOrderId>,
        reason: String,
        ts_event: u64,
    },
    /// The venue received a `CancelOrder` it could not honor: the target is
    /// unknown, already terminal (filled or canceled), or the cancel is
    /// otherwise illegal.
    ///
    /// Distinct from `OrderRejected`, which terminates the ORDER. A rejected
    /// cancel does NOT kill the order - it is still whatever it was (Accepted,
    /// PartiallyFilled, or already terminal), and nautilus's own FSM restores
    /// the pre-cancel status on `CancelRejected`. Overloading `OrderRejected`
    /// for a cancel failure (as the engine once did) would wrongly flip a live
    /// or already-filled order to Rejected - an invalid transition. Mirrors
    /// `OrderModifyRejected`, including the `venue_order_id` presence rule.
    OrderCancelRejected {
        client_order_id: ClientOrderId,
        /// Present when the order is known but the cancel is illegal; absent
        /// when the order id is unknown to the venue.
        venue_order_id: Option<VenueOrderId>,
        reason: String,
        ts_event: u64,
    },
    OrderFilled(OrderFilled),
    AccountState(AccountState),
    Trade(TradeTick),
    Quote(QuoteTick),
    /// Server-originated liveness signal. Carries the server wall clock
    /// unix-ns so the frame is non-empty and timestamp-comparable, but no
    /// market or execution payload. Clients may ignore it; its job is to keep
    /// the socket frame-active through a `StallData` window.
    Heartbeat {
        ts_event: u64,
    },
    /// A `/ws` request the server could not decode OR could not serve: a
    /// frame that is not a `ClientMessage` (bad JSON, unknown `type`, or a
    /// known `type` missing a required field, e.g. `{"type":"Subscribe"}`
    /// with no `symbols`), a `Subscribe` for a symbol the venue does not
    /// list, a subscription whose positioning seek exhausted its tick budget,
    /// or a `start_ts` below the tape's data origin (diagnosed, then
    /// clamped). Emitted in place of the old silent drop: without it, an
    /// unservable live request and a healthy-but-idle feed were
    /// indistinguishable on the wire. Untargeted - the offending frame
    /// carries no `client_order_id` to echo, unlike `OrderRejected`.
    ProtocolError {
        reason: String,
        ts_event: u64,
    },
}

impl ServerMessage {
    /// The single source of truth for how each wire variant is classified into
    /// the exec / fill / data buckets that both ends key their havoc off.
    ///
    /// The server's outbound delay path (`DelayAcks`) delays every execution
    /// event ([`EventKind::is_execution`], i.e. everything but `Data`), and the
    /// adapter's inbound latency knob buckets each variant with the full
    /// three-way split. Both consult this one classifier, so a variant can
    /// never be data on one end and execution on the other.
    ///
    /// `AccountState` is an account/execution event: it reports balances and
    /// positions that move only as orders fill, so it rides the execution path
    /// on both ends. Classifying it as `Data` (as the adapter once did) split
    /// the two ends' views of the same frame.
    #[must_use]
    pub fn category(&self) -> EventKind {
        match self {
            ServerMessage::OrderFilled(_) => EventKind::Fill,
            // Heartbeat is a liveness signal, not execution traffic: `DelayAcks`
            // must not perturb its cadence. It also must survive `StallData`,
            // so writer gates use `is_market_data()` rather than this category.
            ServerMessage::Trade(_) | ServerMessage::Quote(_) | ServerMessage::Heartbeat { .. } => {
                EventKind::Data
            }
            ServerMessage::AccountState(_)
            | ServerMessage::OrderAccepted { .. }
            | ServerMessage::OrderRejected { .. }
            | ServerMessage::OrderCanceled { .. }
            | ServerMessage::OrderUpdated { .. }
            | ServerMessage::OrderModifyRejected { .. }
            | ServerMessage::OrderCancelRejected { .. }
            | ServerMessage::ProtocolError { .. } => EventKind::Exec,
        }
    }

    /// Whether this frame is market channel data, the payload a
    /// per-subscription data watchdog keys off. This is deliberately narrower
    /// than `category() == Data`: the server heartbeat rides the data latency
    /// bucket but is a liveness signal, not channel data.
    #[must_use]
    pub fn is_market_data(&self) -> bool {
        matches!(self, ServerMessage::Trade(_) | ServerMessage::Quote(_))
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscribe_start_ts_round_trips_and_legacy_payloads_default() {
        let with_start = ClientMessage::Subscribe {
            symbols: vec!["X".into()],
            start_ts: Some(123),
            regime: None,
        };
        let json = serde_json::to_string(&with_start).unwrap();
        let decoded: ClientMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            decoded,
            ClientMessage::Subscribe {
                symbols,
                start_ts: Some(123),
                regime: None
            } if symbols == vec!["X"]
        ));

        let without_start = ClientMessage::Subscribe {
            symbols: vec!["X".into()],
            start_ts: None,
            regime: None,
        };
        let json = serde_json::to_string(&without_start).unwrap();
        assert_eq!(json, r#"{"type":"Subscribe","symbols":["X"]}"#);
        let decoded: ClientMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            decoded,
            ClientMessage::Subscribe {
                symbols,
                start_ts: None,
                regime: None
            } if symbols == vec!["X"]
        ));

        let legacy = r#"{"type":"Subscribe","symbols":["X"]}"#;
        let decoded: ClientMessage = serde_json::from_str(legacy).unwrap();
        assert!(matches!(
            decoded,
            ClientMessage::Subscribe {
                symbols,
                start_ts: None,
                regime: None
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

    #[test]
    fn server_message_category_is_shared_source_of_truth() {
        // The classifier both ends consult. `AccountState` is exec, not data:
        // the split-brain this test pins is the adapter once bucketing it as
        // data while the server delayed it as execution. Trades and quotes are
        // the only `Data`; fills are `Fill`; every order-lifecycle event and
        // the account snapshot are `Exec`.
        let exec = [
            ServerMessage::AccountState(AccountState {
                balances: Vec::new(),
                positions: Vec::new(),
                ts_event: 1,
            }),
            ServerMessage::OrderAccepted {
                client_order_id: "O".into(),
                venue_order_id: "V".into(),
                ts_event: 1,
            },
            ServerMessage::OrderRejected {
                client_order_id: "O".into(),
                reason: "no".into(),
                ts_event: 1,
            },
            ServerMessage::OrderCanceled {
                client_order_id: "O".into(),
                venue_order_id: "V".into(),
                ts_event: 1,
            },
            ServerMessage::OrderUpdated {
                client_order_id: "O".into(),
                venue_order_id: "V".into(),
                quantity: Decimal::from(1),
                price: None,
                leaves_qty: Decimal::from(1),
                ts_event: 1,
            },
            ServerMessage::OrderModifyRejected {
                client_order_id: "O".into(),
                venue_order_id: None,
                reason: "no".into(),
                ts_event: 1,
            },
            ServerMessage::OrderCancelRejected {
                client_order_id: "O".into(),
                venue_order_id: None,
                reason: "no".into(),
                ts_event: 1,
            },
        ];
        for msg in &exec {
            assert_eq!(msg.category(), EventKind::Exec, "{msg:?} is execution");
            assert!(msg.category().is_execution(), "{msg:?} delays as execution");
        }

        let fill = ServerMessage::OrderFilled(OrderFilled {
            client_order_id: "O".into(),
            venue_order_id: "V".into(),
            trade_id: "T".into(),
            symbol: "BTCUSDT".into(),
            side: Side::Buy,
            last_qty: Decimal::from(1),
            last_px: Decimal::from(1),
            leaves_qty: Decimal::ZERO,
            commission: Decimal::ZERO,
            ts_event: 1,
        });
        assert_eq!(fill.category(), EventKind::Fill);
        assert!(
            fill.category().is_execution(),
            "fills delay as execution too"
        );

        let data = [
            ServerMessage::Trade(TradeTick {
                symbol: "BTCUSDT".into(),
                price: Decimal::from(1),
                size: Decimal::from(1),
                aggressor: AggressorSide::NoAggressor,
                ts_event: 1,
            }),
            ServerMessage::Quote(QuoteTick {
                symbol: "BTCUSDT".into(),
                bid_px: Decimal::from(1),
                ask_px: Decimal::from(1),
                bid_sz: Decimal::from(1),
                ask_sz: Decimal::from(1),
                ts_event: 1,
            }),
        ];
        for msg in &data {
            assert_eq!(msg.category(), EventKind::Data, "{msg:?} is market data");
            assert!(msg.is_market_data(), "{msg:?} is channel data");
            assert!(
                !msg.category().is_execution(),
                "{msg:?} is not delayed as execution"
            );
        }

        let heartbeat = ServerMessage::Heartbeat { ts_event: 1 };
        assert_eq!(heartbeat.category(), EventKind::Data);
        assert!(!heartbeat.category().is_execution());
        assert!(!heartbeat.is_market_data());
    }

    #[test]
    fn heartbeat_round_trips() {
        let heartbeat = ServerMessage::Heartbeat { ts_event: 123 };
        let json = serde_json::to_string(&heartbeat).unwrap();
        assert_eq!(json, r#"{"type":"Heartbeat","ts_event":123}"#);
        let decoded: ServerMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            decoded,
            ServerMessage::Heartbeat { ts_event: 123 }
        ));
    }

    #[test]
    fn order_updated_and_modify_rejected_round_trip() {
        let updated = ServerMessage::OrderUpdated {
            client_order_id: "O1".into(),
            venue_order_id: "V1".into(),
            quantity: Decimal::from(20),
            price: Some(Decimal::from(200)),
            leaves_qty: Decimal::from(17),
            ts_event: 123,
        };
        let json = serde_json::to_string(&updated).unwrap();
        let decoded: ServerMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            decoded,
            ServerMessage::OrderUpdated {
                client_order_id,
                venue_order_id,
                quantity,
                price: Some(price),
                leaves_qty,
                ts_event: 123,
            } if client_order_id == "O1"
                && venue_order_id == "V1"
                && quantity == Decimal::from(20)
                && price == Decimal::from(200)
                && leaves_qty == Decimal::from(17)
        ));

        let known_reject = ServerMessage::OrderModifyRejected {
            client_order_id: "O2".into(),
            venue_order_id: Some("V2".into()),
            reason: "modify to non-positive price".into(),
            ts_event: 456,
        };
        let json = serde_json::to_string(&known_reject).unwrap();
        let decoded: ServerMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            decoded,
            ServerMessage::OrderModifyRejected {
                client_order_id,
                venue_order_id: Some(venue_order_id),
                reason,
                ts_event: 456,
            } if client_order_id == "O2"
                && venue_order_id == "V2"
                && reason == "modify to non-positive price"
        ));

        let unknown_reject = ServerMessage::OrderModifyRejected {
            client_order_id: "GHOST".into(),
            venue_order_id: None,
            reason: "unknown order".into(),
            ts_event: 789,
        };
        let json = serde_json::to_string(&unknown_reject).unwrap();
        let decoded: ServerMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            decoded,
            ServerMessage::OrderModifyRejected {
                client_order_id,
                venue_order_id: None,
                reason,
                ts_event: 789,
            } if client_order_id == "GHOST" && reason == "unknown order"
        ));
    }

    #[test]
    fn validate_submit_order_bounds_quantity_and_limit_price() {
        let base = SubmitOrder {
            client_order_id: "O-1".into(),
            symbol: "BTCUSDT".into(),
            side: Side::Buy,
            order_type: OrderType::Limit,
            quantity: Decimal::ONE,
            price: Some(Decimal::from(100)),
            time_in_force: TimeInForce::Gtc,
        };
        validate_submit_order(&base).expect("well-formed limit order is valid");

        let mut zero_qty = base.clone();
        zero_qty.quantity = Decimal::ZERO;
        assert_eq!(
            validate_submit_order(&zero_qty),
            Err("quantity must be > 0")
        );

        let mut negative_qty = base.clone();
        negative_qty.quantity = Decimal::from(-1);
        assert_eq!(
            validate_submit_order(&negative_qty),
            Err("quantity must be > 0")
        );

        let mut priceless_limit = base.clone();
        priceless_limit.price = None;
        assert_eq!(
            validate_submit_order(&priceless_limit),
            Err("Limit order must carry a price")
        );

        let mut zero_price = base.clone();
        zero_price.price = Some(Decimal::ZERO);
        assert_eq!(validate_submit_order(&zero_price), Err("price must be > 0"));

        // A priceless Market order is legitimate (Nautilus MARKET orders carry
        // no price).
        let mut market = base;
        market.order_type = OrderType::Market;
        market.price = None;
        validate_submit_order(&market).expect("priceless market order is valid");
    }

    #[test]
    fn validate_modify_order_rejects_empty_and_nonpositive() {
        assert_eq!(
            validate_modify_order(None, None),
            Err("ModifyOrder must set price and/or quantity")
        );
        assert_eq!(
            validate_modify_order(Some(Decimal::ZERO), None),
            Err("price must be > 0")
        );
        assert_eq!(
            validate_modify_order(None, Some(Decimal::from(-1))),
            Err("quantity must be > 0")
        );
        validate_modify_order(Some(Decimal::from(100)), None).expect("price-only amend is valid");
        validate_modify_order(None, Some(Decimal::from(1))).expect("quantity-only amend is valid");
        validate_modify_order(Some(Decimal::from(100)), Some(Decimal::from(1)))
            .expect("both present and positive is valid");
    }
}
