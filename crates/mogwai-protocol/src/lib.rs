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

/// Selects which transport carries order entry and which carries live market
/// data, so one mogwai-server can present itself as different venue archetypes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TransportProfile {
    /// WS carries both order entry and a server-pushed market-data stream.
    #[default]
    WsStreaming,
    /// Order entry over HTTP request/response; market data remains pushed WS.
    HttpOrders,
    /// Order entry over HTTP request/response; market data is polled over HTTP.
    HttpPolling,
}

impl TransportProfile {
    /// Order entry travels over HTTP rather than the `/ws` socket.
    #[must_use]
    pub fn orders_over_http(self) -> bool {
        matches!(self, Self::HttpOrders | Self::HttpPolling)
    }

    /// Live market data is obtained by polling `GET /trades`.
    #[must_use]
    pub fn data_by_polling(self) -> bool {
        matches!(self, Self::HttpPolling)
    }
}

/// One config object that arms mogwai's havoc surfaces.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct HavocSpec {
    /// Transport-level corruption the adapter applies to its own inbound stream.
    #[serde(default)]
    pub client: ClientHavoc,
    /// Execution divergences the adapter relays to mogwai-server on connect.
    #[serde(default)]
    pub server: Vec<control::Divergence>,
    /// Generator-level market regime applied before market-data ticks exist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<MarketRegime>,
    /// Connection-lifecycle corruption applied to adapter transport machinery.
    #[serde(default)]
    pub conn: ConnHavoc,
}

/// Connection-lifecycle havoc: corrupts the transport's connect / reconnect /
/// heartbeat / quota machinery rather than the event stream the other havoc
/// surfaces target. Each field mirrors a nautilus adapter config knob
/// (`WebSocketConfig` reconnect/idle/heartbeat fields and per-adapter
/// heartbeat / idle / request-timeout / quota fields). A clean default is a
/// production-shaped reconnecting transport; hostile values drive realistic
/// transport pathologies.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ConnHavoc {
    /// Idle read timeout in ms. If no inbound application-data frame arrives
    /// within this window, the socket is declared dead and reconnected. Ping
    /// and Pong frames do not reset the idle clock. `0` disables detection.
    pub idle_timeout_ms: u64,
    /// Heartbeat ping interval in ms. `0` disables heartbeat.
    pub heartbeat_interval_ms: u64,
    /// Initial reconnect backoff in ms.
    pub reconnect_delay_initial_ms: u64,
    /// Reconnect backoff ceiling in ms.
    pub reconnect_delay_max_ms: u64,
    /// Exponential backoff growth factor.
    pub reconnect_backoff_factor: f64,
    /// Max uniform jitter in ms added to each reconnect backoff.
    pub reconnect_jitter_ms: u64,
    /// Reconnect attempt cap. `None` is unlimited.
    pub reconnect_max_attempts: Option<u32>,
    /// HTTP request quota in requests per second. `None` is unlimited.
    pub max_requests_per_second: Option<u32>,
    /// Per-request timeout in secs for HTTP order entry. `0` keeps 30s.
    pub request_timeout_secs: u64,
}

impl Default for ConnHavoc {
    fn default() -> Self {
        Self {
            idle_timeout_ms: 0,
            heartbeat_interval_ms: 0,
            reconnect_delay_initial_ms: 1_000,
            reconnect_delay_max_ms: 10_000,
            reconnect_backoff_factor: 2.0,
            reconnect_jitter_ms: 0,
            reconnect_max_attempts: None,
            max_requests_per_second: None,
            request_timeout_secs: 0,
        }
    }
}

pub fn validate_conn_havoc(conn: &ConnHavoc) -> Result<(), &'static str> {
    if !conn.reconnect_backoff_factor.is_finite() || conn.reconnect_backoff_factor < 1.0 {
        return Err("reconnect_backoff_factor must be finite and >= 1.0");
    }
    if conn.reconnect_delay_initial_ms > 0
        && conn.reconnect_delay_max_ms > 0
        && conn.reconnect_delay_max_ms < conn.reconnect_delay_initial_ms
    {
        return Err("reconnect_delay_max_ms must be >= reconnect_delay_initial_ms");
    }
    if conn.max_requests_per_second == Some(0) {
        return Err("max_requests_per_second must be > 0");
    }
    Ok(())
}

/// Market-regime havoc: perturbs the generator before ticks are produced.
///
/// This is distinct from server divergences and client-side transport havoc,
/// which corrupt events after production. It is carried per subscription on
/// `Subscribe` and per request on `GET /trades`; it never travels the
/// `/control/divergence` control plane.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum MarketRegime {
    /// Multiply the GARCH return RMS by `vol_mult` and lift clamps with it.
    VolStorm { vol_mult: f64 },
    /// Divide arrival intensity by `thin_factor`, stretching inter-arrivals.
    LiquidityDrought { thin_factor: f64 },
    /// Inside the UTC half-open hour window `[start_hour, end_hour)`, scale the
    /// session vol curve by `1.0 + extra_vol_mult` (the extra rides the same
    /// multiplicative envelope as `VolStorm`'s `vol_mult`, so the spike is an
    /// amplification of the fitted session curve, not an additive shift of it).
    SessionEdgeSpike {
        start_hour: u8,
        end_hour: u8,
        extra_vol_mult: f64,
    },
    /// Halt once at `at_ts`, then resume with a signed latent log-return gap.
    ReopenGap {
        at_ts: u64,
        halt_secs: u64,
        gap_frac: f64,
    },
}

pub fn validate_market_regime(regime: &MarketRegime) -> Result<(), &'static str> {
    match *regime {
        MarketRegime::VolStorm { vol_mult } => {
            if vol_mult.is_finite() && (0.0..=100.0).contains(&vol_mult) && vol_mult > 0.0 {
                Ok(())
            } else {
                Err("vol_mult must be in (0.0, 100.0]")
            }
        }
        MarketRegime::LiquidityDrought { thin_factor } => {
            if thin_factor.is_finite() && (1.0..=1000.0).contains(&thin_factor) {
                Ok(())
            } else {
                Err("thin_factor must be in [1.0, 1000.0]")
            }
        }
        MarketRegime::SessionEdgeSpike {
            start_hour,
            end_hour,
            extra_vol_mult,
        } => {
            if start_hour >= end_hour || end_hour > 24 {
                return Err("session edge window must satisfy start_hour < end_hour <= 24");
            }
            if extra_vol_mult.is_finite() && (0.0..=100.0).contains(&extra_vol_mult) {
                Ok(())
            } else {
                Err("extra_vol_mult must be in [0.0, 100.0]")
            }
        }
        MarketRegime::ReopenGap {
            halt_secs,
            gap_frac,
            ..
        } => {
            if halt_secs > 86_400 {
                return Err("halt_secs must be <= 86400");
            }
            if gap_frac.is_finite() && (-1.0..=1.0).contains(&gap_frac) {
                Ok(())
            } else {
                Err("gap_frac must be in [-1.0, 1.0]")
            }
        }
    }
}

/// Client-side, in-adapter havoc knobs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ClientHavoc {
    /// Added delay before each inbound event reaches the sink.
    #[serde(default)]
    pub latency: Option<HavocLatency>,
    /// Probability in [0.0, 1.0] that an inbound event is dropped.
    #[serde(default)]
    pub drop_prob: f64,
    /// Probability in [0.0, 1.0] that an inbound event is emitted twice.
    #[serde(default)]
    pub duplicate_prob: f64,
    /// Probability in [0.0, 1.0] that adjacent inbound events are transposed.
    #[serde(default)]
    pub reorder_prob: f64,
    /// Optional deterministic RNG seed.
    #[serde(default)]
    pub seed: Option<u64>,
}

/// Static inbound-latency knobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct HavocLatency {
    /// Base delay added to every inbound event.
    #[serde(default)]
    pub base_nanos: u64,
    /// Extra delay for order-lifecycle execution events.
    #[serde(default)]
    pub exec_event_nanos: u64,
    /// Extra delay for fill events.
    #[serde(default)]
    pub fill_nanos: u64,
    /// Extra delay for market-data events and account-state snapshots.
    #[serde(default)]
    pub data_nanos: u64,
}

impl HavocLatency {
    /// Effective delay for an inbound event, composing base into the category.
    #[must_use]
    pub fn delay_for(&self, kind: EventKind) -> std::time::Duration {
        let extra = match kind {
            EventKind::Exec => self.exec_event_nanos,
            EventKind::Fill => self.fill_nanos,
            EventKind::Data => self.data_nanos,
        };
        std::time::Duration::from_nanos(self.base_nanos.saturating_add(extra))
    }
}

/// Inbound-event categories the client-side latency knob distinguishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Exec,
    Fill,
    Data,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstrumentDef {
    pub symbol: Symbol,
    pub base: String,
    pub quote: String,
    pub price_precision: u8,
    pub size_precision: u8,
    pub price_increment: Decimal,
    pub size_increment: Decimal,
}

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
    fn market_regime_round_trips_and_validates() {
        let regimes = [
            MarketRegime::VolStorm { vol_mult: 10.0 },
            MarketRegime::LiquidityDrought { thin_factor: 5.0 },
            MarketRegime::SessionEdgeSpike {
                start_hour: 13,
                end_hour: 15,
                extra_vol_mult: 4.0,
            },
            MarketRegime::ReopenGap {
                at_ts: 123,
                halt_secs: 60,
                gap_frac: -0.2,
            },
        ];

        for regime in regimes {
            validate_market_regime(&regime).expect("regime in range");
            let json = serde_json::to_string(&regime).unwrap();
            let decoded: MarketRegime = serde_json::from_str(&json).unwrap();
            assert_eq!(decoded, regime);
        }

        assert!(validate_market_regime(&MarketRegime::VolStorm { vol_mult: 0.0 }).is_err());
        assert!(
            validate_market_regime(&MarketRegime::LiquidityDrought { thin_factor: 0.5 }).is_err()
        );
        assert!(
            validate_market_regime(&MarketRegime::SessionEdgeSpike {
                start_hour: 24,
                end_hour: 24,
                extra_vol_mult: 1.0,
            })
            .is_err()
        );
        assert!(
            validate_market_regime(&MarketRegime::ReopenGap {
                at_ts: 0,
                halt_secs: 86_401,
                gap_frac: 0.0,
            })
            .is_err()
        );
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
    fn instrument_def_round_trips() {
        let def = InstrumentDef {
            symbol: "BTCUSDT".into(),
            base: "BTC".into(),
            quote: "USDT".into(),
            price_precision: 2,
            size_precision: 8,
            price_increment: Decimal::new(1, 2),
            size_increment: Decimal::new(1, 8),
        };

        let json = serde_json::to_string(&def).unwrap();
        let decoded: InstrumentDef = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded, def);
    }

    #[test]
    fn transport_profile_round_trips_and_defaults() {
        #[derive(Debug, Deserialize)]
        struct Wrapper {
            #[serde(default)]
            profile: TransportProfile,
        }

        for profile in [
            TransportProfile::WsStreaming,
            TransportProfile::HttpOrders,
            TransportProfile::HttpPolling,
        ] {
            let json = serde_json::to_string(&profile).unwrap();
            let decoded: TransportProfile = serde_json::from_str(&json).unwrap();
            assert_eq!(decoded, profile);
        }

        let decoded: Wrapper = serde_json::from_str("{}").unwrap();
        assert_eq!(decoded.profile, TransportProfile::WsStreaming);

        // orders_over_http: true for both HTTP variants, false only for WS.
        assert!(!TransportProfile::WsStreaming.orders_over_http());
        assert!(TransportProfile::HttpOrders.orders_over_http());
        assert!(TransportProfile::HttpPolling.orders_over_http());

        // data_by_polling: true only for the fully-request/response variant.
        assert!(!TransportProfile::WsStreaming.data_by_polling());
        assert!(!TransportProfile::HttpOrders.data_by_polling());
        assert!(TransportProfile::HttpPolling.data_by_polling());
    }

    #[test]
    fn havoc_spec_round_trips() {
        let spec = HavocSpec {
            client: ClientHavoc {
                latency: Some(HavocLatency {
                    base_nanos: 10,
                    exec_event_nanos: 20,
                    fill_nanos: 30,
                    data_nanos: 40,
                }),
                drop_prob: 0.1,
                duplicate_prob: 0.2,
                reorder_prob: 0.3,
                seed: Some(42),
            },
            server: vec![
                control::Divergence::PartialFillNext {
                    client_order_id: "O-1".into(),
                    fraction: Decimal::new(5, 1),
                },
                control::Divergence::GoDark { ms: 250 },
            ],
            data: Some(MarketRegime::LiquidityDrought { thin_factor: 5.0 }),
            conn: ConnHavoc {
                idle_timeout_ms: 25,
                heartbeat_interval_ms: 50,
                reconnect_delay_initial_ms: 100,
                reconnect_delay_max_ms: 1_000,
                reconnect_backoff_factor: 1.5,
                reconnect_jitter_ms: 7,
                reconnect_max_attempts: Some(3),
                max_requests_per_second: Some(2),
                request_timeout_secs: 1,
            },
        };

        let json = serde_json::to_string(&spec).unwrap();
        let decoded: HavocSpec = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded, spec);

        let clean = HavocSpec::default();
        let json = serde_json::to_string(&clean).unwrap();
        // `conn` is always serialized because its default is the honest
        // connection lifecycle, so an omitted key decodes to this object.
        assert_eq!(
            json,
            r#"{"client":{"latency":null,"drop_prob":0.0,"duplicate_prob":0.0,"reorder_prob":0.0,"seed":null},"server":[],"conn":{"idle_timeout_ms":0,"heartbeat_interval_ms":0,"reconnect_delay_initial_ms":1000,"reconnect_delay_max_ms":10000,"reconnect_backoff_factor":2.0,"reconnect_jitter_ms":0,"reconnect_max_attempts":null,"max_requests_per_second":null,"request_timeout_secs":0}}"#
        );
    }

    #[test]
    fn havoc_spec_defaults_from_empty_object() {
        let decoded: HavocSpec = serde_json::from_str("{}").unwrap();
        assert_eq!(decoded.client, ClientHavoc::default());
        assert!(decoded.server.is_empty());
        assert_eq!(decoded.data, None);
        assert_eq!(decoded.conn, ConnHavoc::default());

        let decoded: HavocSpec = serde_json::from_str(r#"{"server":[]}"#).unwrap();
        assert_eq!(decoded.client, ClientHavoc::default());
        assert!(decoded.server.is_empty());
        assert_eq!(decoded.data, None);
        assert_eq!(decoded.conn, ConnHavoc::default());
    }

    #[test]
    fn conn_havoc_round_trips_and_validates() {
        let conn = ConnHavoc {
            idle_timeout_ms: 10,
            heartbeat_interval_ms: 20,
            reconnect_delay_initial_ms: 30,
            reconnect_delay_max_ms: 300,
            reconnect_backoff_factor: 1.25,
            reconnect_jitter_ms: 5,
            reconnect_max_attempts: Some(4),
            max_requests_per_second: Some(8),
            request_timeout_secs: 2,
        };

        let json = serde_json::to_string(&conn).unwrap();
        let decoded: ConnHavoc = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, conn);
        assert_eq!(validate_conn_havoc(&conn), Ok(()));

        let mut invalid = conn;
        invalid.reconnect_backoff_factor = 0.5;
        assert_eq!(
            validate_conn_havoc(&invalid),
            Err("reconnect_backoff_factor must be finite and >= 1.0")
        );

        invalid = conn;
        invalid.reconnect_delay_max_ms = 1;
        assert_eq!(
            validate_conn_havoc(&invalid),
            Err("reconnect_delay_max_ms must be >= reconnect_delay_initial_ms")
        );

        invalid = conn;
        invalid.max_requests_per_second = Some(0);
        assert_eq!(
            validate_conn_havoc(&invalid),
            Err("max_requests_per_second must be > 0")
        );
    }

    #[test]
    fn havoc_latency_composes_base() {
        let latency = HavocLatency {
            base_nanos: 10,
            exec_event_nanos: 1,
            fill_nanos: 2,
            data_nanos: 3,
        };

        assert_eq!(latency.delay_for(EventKind::Exec).as_nanos(), 11);
        assert_eq!(latency.delay_for(EventKind::Fill).as_nanos(), 12);
        assert_eq!(latency.delay_for(EventKind::Data).as_nanos(), 13);
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

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
        /// Swallow the next fill-driven account-state update (induce account drift).
        DropNextAccountUpdate,
        /// Stop sending anything for `ms` (simulate a venue blackout).
        GoDark { ms: u64 },
    }
}
