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

/// Default per-request timeout in seconds for HTTP order entry. This is the
/// value `ConnHavoc.request_timeout_secs == 0` documents as "keeps 30s"; the
/// adapter hardcodes it in several places and should source it from here so the
/// honest-transport default lives in exactly one spot.
pub const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 30;

/// Maximum number of trades a single `/trades` history page returns. The server
/// enforces this cap (it clamps every request to it), and the adapter requests
/// within it - sourcing both from here keeps the two in lockstep, so the adapter
/// never advertises a ceiling larger than the server will honor.
pub const MAX_HISTORY_LIMIT: usize = 1_000;

/// Saturating UNIX-nanoseconds clock reader: the single source of truth for
/// "now" on the wire's `ts_event` axis, shared by the server (its `now_ns`) and
/// the adapter (its `now_unix_nanos`, which wraps the result in `UnixNanos`).
///
/// A backward clock step (NTP correction, leap second) yields an `Err` from
/// `duration_since`, which we saturate to `0` rather than panic on - the prior
/// duplicated readers `.expect("clock before epoch")`ed and would kill their
/// host task on any skew. The nanosecond count is a `u128`; we clamp it to
/// `u64::MAX` rather than truncate with `as u64`, which would silently wrap
/// past year 2554. `map_or` (not `map(...).unwrap_or(...)`) keeps clippy's
/// `map_unwrap_or` lint quiet.
#[must_use]
pub fn now_unix_nanos() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
}

/// Saturating `Decimal` -> `f64`: returns `0.0` when the conversion is `None`
/// (the magnitude does not fit an `f64`). The data crate already carries a
/// private reader with this exact contract; the adapter currently
/// `.to_f64().expect(...)`s on the hot inbound fill/balance path, so a
/// pathological wire `Decimal` panics the runtime instead of degrading. Hoisted
/// here so both can converge on a non-panicking conversion.
#[must_use]
pub fn decimal_to_f64(d: Decimal) -> f64 {
    use rust_decimal::prelude::ToPrimitive;
    d.to_f64().unwrap_or(0.0)
}

/// Saturating `f64` -> `Decimal`: clamps to `Decimal::MAX` / `Decimal::MIN` for
/// out-of-range finite inputs and maps any non-finite input (NaN, +/-inf) to
/// `Decimal::ZERO`. Mirrors the data crate's private writer so the two can be
/// unified, and gives the adapter a total conversion in place of a panicking one.
#[must_use]
pub fn decimal_from_f64(x: f64) -> Decimal {
    use rust_decimal::prelude::FromPrimitive;
    if !x.is_finite() {
        return Decimal::ZERO;
    }
    Decimal::from_f64(x).unwrap_or(if x > 0.0 { Decimal::MAX } else { Decimal::MIN })
}

/// The canonical default instrument set the venue seeds when none is supplied.
///
/// Today this is the single BTCUSDT instrument the engine seeds inline in
/// `Engine::new`; the server's price-grid scalars and the adapter test fixtures
/// re-pin the same precisions/increments by hand. Sourcing all three from this
/// one function ends the duplication that the engine's
/// `btcusdt_uses_engine_price_grid` test exists only to police. The field values
/// match the engine seed exactly: price precision 2, size precision 8, with
/// `1e-2` / `1e-8` increments.
#[must_use]
pub fn default_instruments() -> Vec<InstrumentDef> {
    vec![InstrumentDef {
        symbol: "BTCUSDT".into(),
        base: "BTC".into(),
        quote: "USDT".into(),
        price_precision: 2,
        size_precision: 8,
        price_increment: Decimal::new(1, 2),
        size_increment: Decimal::new(1, 8),
    }]
}

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

/// `value` is finite and lies in the inclusive range `[lo, hi]`.
///
/// The finite-range idiom (`is_finite() && (lo..=hi).contains(&value)`) was
/// open-coded across every numeric validator below; this names the bounds and
/// makes the `NaN`/inf guard uniform, so a non-finite input can never slip
/// through a forgotten `is_finite()`.
#[must_use]
pub fn finite_in(value: f64, lo: f64, hi: f64) -> bool {
    value.is_finite() && (lo..=hi).contains(&value)
}

/// `value` is finite and lies in the half-open range `(lo, hi]` - the
/// exclusive-lower variant of [`finite_in`], for knobs (e.g. `vol_mult`) where
/// the lower bound is a degenerate "no effect" or "divide by zero" value that
/// must be rejected.
#[must_use]
pub fn finite_in_excl_lo(value: f64, lo: f64, hi: f64) -> bool {
    value.is_finite() && value > lo && value <= hi
}

pub fn validate_conn_havoc(conn: &ConnHavoc) -> Result<(), &'static str> {
    if !conn.reconnect_backoff_factor.is_finite() || conn.reconnect_backoff_factor < 1.0 {
        return Err("reconnect_backoff_factor must be finite and >= 1.0");
    }
    // A zero ceiling has no defined meaning: only `reconnect_max_attempts:
    // None` / `max_requests_per_second: None` are the documented "unlimited"
    // knobs. Reject `max == 0` whenever a real initial backoff is set, so the
    // lifecycle backoff never has to disambiguate "no clamp" from "clamp to
    // zero" (which collapses into a CPU-spinning reconnect loop). This is the
    // authoritative gate; the lifecycle layer's guard is belt-and-suspenders.
    if conn.reconnect_delay_initial_ms > 0 && conn.reconnect_delay_max_ms == 0 {
        return Err("reconnect_delay_max_ms must be > 0 when reconnect_delay_initial_ms > 0");
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
            if finite_in_excl_lo(vol_mult, 0.0, 100.0) {
                Ok(())
            } else {
                Err("vol_mult must be in (0.0, 100.0]")
            }
        }
        MarketRegime::LiquidityDrought { thin_factor } => {
            if finite_in(thin_factor, 1.0, 1000.0) {
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
            // Half-open window `[start_hour, end_hour)`: a consumer may index a
            // 24-element session curve by either edge, so both must stay in
            // bounds. `start_hour <= 23` keeps the lower edge a valid index;
            // `start_hour < end_hour <= 24` keeps the window non-empty and the
            // upper (exclusive) edge no larger than the curve length.
            if start_hour > 23 || start_hour >= end_hour || end_hour > 24 {
                return Err(
                    "session edge window must satisfy start_hour <= 23 and start_hour < end_hour <= 24",
                );
            }
            if finite_in(extra_vol_mult, 0.0, 100.0) {
                Ok(())
            } else {
                Err("extra_vol_mult must be in [0.0, 100.0]")
            }
        }
        MarketRegime::ReopenGap {
            at_ts,
            halt_secs,
            gap_frac,
        } => {
            // `at_ts` is a forward-replay UNIX-nanosecond instant. `0` is the
            // degenerate "halt at epoch" - a halt that, on any real forward
            // replay, has already passed before the first tick, so the regime
            // never actually fires. Reject it the way every sibling rejects its
            // degenerate field, rather than arm a silently inert divergence.
            if at_ts == 0 {
                return Err("at_ts must be > 0 (a forward-replay instant, not the epoch)");
            }
            if halt_secs > 86_400 {
                return Err("halt_secs must be <= 86400");
            }
            if finite_in(gap_frac, -1.0, 1.0) {
                Ok(())
            } else {
                Err("gap_frac must be in [-1.0, 1.0]")
            }
        }
    }
}

/// API-boundary guard for an armed divergence, mirroring `validate_conn_havoc`
/// and `validate_market_regime` in style and message convention.
///
/// `PartialFillNext.fraction` must lie in the half-open `(0, 1]`: a fill must
/// move some quantity (`> 0`) and cannot exceed the order (`<= 1`).
/// `DelayAcks.ms`, `GoDark.ms`, and `StallData.ms` are bounded by
/// `MAX_DIVERGENCE_MS` so a control-plane request cannot arm an effectively
/// permanent window.
/// `ClearDivergences` and the engine-side single-shot variants are otherwise
/// unconstrained.
///
/// The engine also applies a defensive runtime clamp (a `fraction > 1` becomes a
/// full fill, a `fraction <= 0` becomes a full fill with a warning), but that is
/// a last-line safety net; this validator is the authoritative guard that
/// rejects the misconfiguration early, before it is armed.
pub fn validate_divergence(div: &control::Divergence) -> Result<(), &'static str> {
    match div {
        control::Divergence::PartialFillNext { fraction, .. } => {
            if *fraction <= Decimal::ZERO || *fraction > Decimal::ONE {
                return Err("PartialFillNext fraction must be in (0, 1]");
            }
            Ok(())
        }
        control::Divergence::DelayAcks { ms }
        | control::Divergence::GoDark { ms }
        | control::Divergence::StallData { ms } => {
            if *ms > control::MAX_DIVERGENCE_MS {
                return Err("DelayAcks/GoDark/StallData ms must be <= 3600000 (one hour)");
            }
            Ok(())
        }
        control::Divergence::RejectNextSubmit { .. }
        | control::Divergence::DuplicateNextFill
        | control::Divergence::DropNextAccountUpdate
        | control::Divergence::ClearDivergences => Ok(()),
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
    /// Extra delay for market-data events (trades and quotes). Account-state
    /// snapshots are execution traffic and ride `exec_event_nanos`, not this.
    #[serde(default)]
    pub data_nanos: u64,
}

/// Always-on baseline inbound network latency for the honest default feed.
///
/// A modest one-way delay carried by every inbound event regardless of armed
/// havoc, so the no-havoc path still has realistic network delay. Armed
/// `ClientHavoc.latency`, when present, adds on top of this baseline instead of
/// replacing it: the network's own latency is always present, and havoc latency
/// is an additional perturbation above the honest floor.
pub const BASELINE_LATENCY: HavocLatency = HavocLatency {
    base_nanos: 30_000_000,
    exec_event_nanos: 0,
    fill_nanos: 0,
    data_nanos: 0,
};

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

impl EventKind {
    /// Whether this category is an account/execution event rather than market
    /// data. `Exec` and `Fill` are both order-lifecycle (execution) traffic;
    /// only `Data` is market data. The server's outbound delay path keys off
    /// this two-way split, while the adapter's latency bucketing uses the full
    /// three-way `EventKind` - both consult [`ServerMessage::category`] so the
    /// two ends can never disagree about which side of the seam a variant sits
    /// on (the split-brain that classified `AccountState` as data on one end
    /// and execution on the other).
    #[must_use]
    pub fn is_execution(self) -> bool {
        !matches!(self, EventKind::Data)
    }
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
        // VolStorm rejects non-finite and the inclusive upper edge round-trips.
        assert!(validate_market_regime(&MarketRegime::VolStorm { vol_mult: f64::NAN }).is_err());
        assert!(
            validate_market_regime(&MarketRegime::VolStorm {
                vol_mult: f64::INFINITY
            })
            .is_err()
        );
        validate_market_regime(&MarketRegime::VolStorm { vol_mult: 100.0 })
            .expect("100.0 is the inclusive upper bound");
        assert!(
            validate_market_regime(&MarketRegime::VolStorm { vol_mult: 100.1 }).is_err(),
            "above the inclusive upper bound"
        );

        assert!(
            validate_market_regime(&MarketRegime::LiquidityDrought { thin_factor: 0.5 }).is_err()
        );

        // SessionEdgeSpike: the half-open window must be [start, end) with
        // start <= 23, start < end <= 24. The previous validator let
        // start_hour=24/end_hour=24 through the `>=` gate only because they
        // were equal; an empty/degenerate or out-of-bounds start must reject.
        assert!(
            validate_market_regime(&MarketRegime::SessionEdgeSpike {
                start_hour: 24,
                end_hour: 24,
                extra_vol_mult: 1.0,
            })
            .is_err()
        );
        // The tightened bound: start_hour at 24 with a larger end_hour would
        // still index past a 24-element curve. (The old check `start_hour >=
        // end_hour || end_hour > 24` would have ACCEPTED start_hour=24 had
        // end_hour been representable above it - here we make start_hour=24
        // reject outright.)
        assert!(
            validate_market_regime(&MarketRegime::SessionEdgeSpike {
                start_hour: 24,
                end_hour: 25,
                extra_vol_mult: 1.0,
            })
            .is_err(),
            "start_hour must be <= 23"
        );
        // A boundary-valid window: last full hour as a half-open [23, 24).
        validate_market_regime(&MarketRegime::SessionEdgeSpike {
            start_hour: 23,
            end_hour: 24,
            extra_vol_mult: 0.0,
        })
        .expect("[23, 24) with extra_vol_mult at the inclusive lower bound is valid");

        // ReopenGap now validates at_ts: epoch (0) is rejected as a halt that
        // can never fire on a forward replay.
        assert!(
            validate_market_regime(&MarketRegime::ReopenGap {
                at_ts: 0,
                halt_secs: 60,
                gap_frac: 0.0,
            })
            .is_err(),
            "at_ts == 0 is a halt at the epoch"
        );
        assert!(
            validate_market_regime(&MarketRegime::ReopenGap {
                at_ts: 123,
                halt_secs: 86_401,
                gap_frac: 0.0,
            })
            .is_err()
        );
        assert!(
            validate_market_regime(&MarketRegime::ReopenGap {
                at_ts: 123,
                halt_secs: 60,
                gap_frac: 1.5,
            })
            .is_err(),
            "gap_frac outside [-1.0, 1.0]"
        );
    }

    #[test]
    fn finite_range_helpers_reject_non_finite_and_respect_bounds() {
        // Inclusive variant: both edges included, non-finite always rejected.
        assert!(finite_in(0.0, 0.0, 1.0));
        assert!(finite_in(1.0, 0.0, 1.0));
        assert!(!finite_in(-0.001, 0.0, 1.0));
        assert!(!finite_in(1.001, 0.0, 1.0));
        assert!(!finite_in(f64::NAN, 0.0, 1.0));
        assert!(!finite_in(f64::INFINITY, 0.0, 1.0));
        assert!(!finite_in(f64::NEG_INFINITY, 0.0, 1.0));

        // Exclusive-lower variant: lower edge excluded, upper included.
        assert!(!finite_in_excl_lo(0.0, 0.0, 100.0));
        assert!(finite_in_excl_lo(0.000_1, 0.0, 100.0));
        assert!(finite_in_excl_lo(100.0, 0.0, 100.0));
        assert!(!finite_in_excl_lo(100.001, 0.0, 100.0));
        assert!(!finite_in_excl_lo(f64::NAN, 0.0, 100.0));
        assert!(!finite_in_excl_lo(f64::INFINITY, 0.0, 100.0));
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
                control::Divergence::StallData { ms: 125 },
                control::Divergence::ClearDivergences,
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

        let clear_json = serde_json::to_string(&control::Divergence::ClearDivergences).unwrap();
        assert_eq!(clear_json, r#"{"type":"ClearDivergences"}"#);
        let clear: control::Divergence = serde_json::from_str(&clear_json).unwrap();
        assert_eq!(clear, control::Divergence::ClearDivergences);

        let stall_json =
            serde_json::to_string(&control::Divergence::StallData { ms: 500 }).unwrap();
        assert_eq!(stall_json, r#"{"type":"StallData","ms":500}"#);
        let stall: control::Divergence = serde_json::from_str(&stall_json).unwrap();
        assert_eq!(stall, control::Divergence::StallData { ms: 500 });

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

        // B.7: a zero ceiling with a nonzero initial backoff is ambiguous
        // (max == 0 is not a documented "unlimited" sentinel) and would
        // collapse the lifecycle backoff to a CPU-spinning zero delay. The
        // old guard fired only when BOTH were > 0, so this slipped through.
        invalid = conn;
        invalid.reconnect_delay_initial_ms = 5_000;
        invalid.reconnect_delay_max_ms = 0;
        assert_eq!(
            validate_conn_havoc(&invalid),
            Err("reconnect_delay_max_ms must be > 0 when reconnect_delay_initial_ms > 0")
        );

        // But a zero initial with a zero max is still fine (backoff disabled),
        // as is the honest default.
        let mut both_zero = conn;
        both_zero.reconnect_delay_initial_ms = 0;
        both_zero.reconnect_delay_max_ms = 0;
        assert_eq!(validate_conn_havoc(&both_zero), Ok(()));
        assert_eq!(validate_conn_havoc(&ConnHavoc::default()), Ok(()));
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
    fn now_unix_nanos_is_monotone_and_nonzero() {
        let a = now_unix_nanos();
        let b = now_unix_nanos();
        // A real post-epoch wall clock is well past zero and never steps back
        // across two adjacent reads in this test environment.
        assert!(a > 0);
        assert!(b >= a);
    }

    #[test]
    fn decimal_f64_round_trips_and_saturates() {
        // A representable value round-trips through both helpers.
        let d = Decimal::new(12_345, 2);
        assert!((decimal_to_f64(d) - 123.45).abs() < 1e-9);
        assert_eq!(decimal_from_f64(123.45), d);

        // Non-finite inputs collapse to zero rather than panicking.
        assert_eq!(decimal_from_f64(f64::NAN), Decimal::ZERO);
        assert_eq!(decimal_from_f64(f64::INFINITY), Decimal::ZERO);
        assert_eq!(decimal_from_f64(f64::NEG_INFINITY), Decimal::ZERO);

        // Magnitudes past Decimal's range saturate to the signed bound.
        assert_eq!(decimal_from_f64(1e40), Decimal::MAX);
        assert_eq!(decimal_from_f64(-1e40), Decimal::MIN);
    }

    #[test]
    fn validate_divergence_bounds_partial_fill_fraction() {
        // Legitimate fractions in (0, 1].
        validate_divergence(&control::Divergence::PartialFillNext {
            client_order_id: "O-1".into(),
            fraction: Decimal::new(5, 1),
        })
        .expect("0.5 is in range");
        validate_divergence(&control::Divergence::PartialFillNext {
            client_order_id: "O-1".into(),
            fraction: Decimal::ONE,
        })
        .expect("1.0 is the inclusive upper bound");

        // Zero, negative, and >1 are rejected.
        for bad in [Decimal::ZERO, Decimal::new(-1, 1), Decimal::new(11, 1)] {
            assert_eq!(
                validate_divergence(&control::Divergence::PartialFillNext {
                    client_order_id: "O-1".into(),
                    fraction: bad,
                }),
                Err("PartialFillNext fraction must be in (0, 1]")
            );
        }

        // Server-owned delay/dark windows are bounded, while `0` remains valid
        // as the disarm value.
        for div in [
            control::Divergence::DelayAcks {
                ms: control::MAX_DIVERGENCE_MS,
            },
            control::Divergence::GoDark {
                ms: control::MAX_DIVERGENCE_MS,
            },
            control::Divergence::StallData {
                ms: control::MAX_DIVERGENCE_MS,
            },
            control::Divergence::DelayAcks { ms: 0 },
            control::Divergence::GoDark { ms: 0 },
            control::Divergence::StallData { ms: 0 },
        ] {
            validate_divergence(&div).expect("bounded ms value is valid");
        }
        for div in [
            control::Divergence::DelayAcks {
                ms: control::MAX_DIVERGENCE_MS + 1,
            },
            control::Divergence::GoDark {
                ms: control::MAX_DIVERGENCE_MS + 1,
            },
            control::Divergence::StallData {
                ms: control::MAX_DIVERGENCE_MS + 1,
            },
        ] {
            assert_eq!(
                validate_divergence(&div),
                Err("DelayAcks/GoDark/StallData ms must be <= 3600000 (one hour)")
            );
        }

        // Every non-numeric variant is unconditionally valid.
        for div in [
            control::Divergence::RejectNextSubmit {
                reason: "nope".into(),
            },
            control::Divergence::DuplicateNextFill,
            control::Divergence::DropNextAccountUpdate,
            control::Divergence::ClearDivergences,
        ] {
            validate_divergence(&div).expect("non-numeric variants are always valid");
        }
    }

    #[test]
    fn default_instruments_matches_engine_btcusdt_seed() {
        let defs = default_instruments();
        assert_eq!(defs.len(), 1);
        assert_eq!(
            defs[0],
            InstrumentDef {
                symbol: "BTCUSDT".into(),
                base: "BTC".into(),
                quote: "USDT".into(),
                price_precision: 2,
                size_precision: 8,
                price_increment: Decimal::new(1, 2),
                size_increment: Decimal::new(1, 8),
            }
        );
    }

    #[test]
    fn default_request_timeout_secs_is_thirty() {
        assert_eq!(DEFAULT_REQUEST_TIMEOUT_SECS, 30);
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
    /// Server-originated liveness signal. Carries the server wall clock
    /// unix-ns so the frame is non-empty and timestamp-comparable, but no
    /// market or execution payload. Clients may ignore it; its job is to keep
    /// the socket frame-active through a `StallData` window.
    Heartbeat {
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
            | ServerMessage::OrderModifyRejected { .. } => EventKind::Exec,
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

/// Out-of-band control plane: arm deterministic divergences for tests.
///
/// This is the reason mogwai exists as an external process - it can emit ugly,
/// realistic event streams an in-process matching engine never would, to drive
/// broadarrow's `classify` → brake/quarantine/restart layer.
pub mod control {
    use super::{ClientOrderId, Decimal, Deserialize, Serialize};

    /// Upper bound on any single divergence's `ms` window, enforced by
    /// `validate_divergence`.
    ///
    /// One hour is far longer than any test blackout, data-stall, or ack-delay
    /// scenario needs, and `3_600_000 * 1_000_000` ns is well below `u64::MAX`,
    /// so validated temporal windows cannot saturate writer deadlines.
    pub const MAX_DIVERGENCE_MS: u64 = 3_600_000;

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
        /// Delay every outbound execution event by `ms`, bounded by
        /// `MAX_DIVERGENCE_MS`. Arm with `ms: 0` to clear, or post
        /// `ClearDivergences`.
        DelayAcks { ms: u64 },
        /// Emit the next fill event twice.
        DuplicateNextFill,
        /// Swallow the next fill-driven account-state update (induce account drift).
        DropNextAccountUpdate,
        /// Stop sending anything for `ms` (simulate a venue blackout), bounded
        /// by `MAX_DIVERGENCE_MS`. Frames produced during the window are
        /// dropped, not buffered. Post `ClearDivergences` to lift the window
        /// early.
        GoDark { ms: u64 },
        /// Suppress only market-data frames (`Trade` / `Quote`) for `ms`,
        /// leaving every execution frame alive. Bounded by
        /// `MAX_DIVERGENCE_MS`. Frames produced during the window are dropped,
        /// not buffered. Post `ClearDivergences` to lift the window early.
        ///
        /// Unlike `GoDark`, this keeps the socket healthy while only channel
        /// data is withheld, especially when paired with the server
        /// `Heartbeat`.
        StallData { ms: u64 },
        /// Clear the server-owned temporal windows: cancel any armed
        /// `DelayAcks`, any armed `GoDark`, and any armed `StallData`.
        ///
        /// This does not flush engine-side single-shot divergences
        /// (`PartialFillNext`, `RejectNextSubmit`, `DuplicateNextFill`,
        /// `DropNextAccountUpdate`), which self-disarm on their own trigger.
        ClearDivergences,
    }
}
