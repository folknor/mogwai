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
/// adapter sources every occurrence from this constant (`clock.rs`,
/// `client.rs`'s `request_timeout_secs`) rather than repeating the literal, so
/// the honest-transport default lives in exactly one spot.
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

/// Affine wall-to-simulated time map shared by the server, adapter, and the
/// nautilus clock injected into broadarrow's live node.
///
/// The scaling step (`offset as f64 * speed` / `offset as f64 / speed` in
/// `sim_ns`/`wall_ns`/`wall_span`) goes through `f64`, so it is only exact
/// while the elapsed nanosecond offset (`wall_ns - wall_anchor_ns` or
/// `sim_ns - sim_epoch_ns`) stays under 2^53 (~104 days of continuous span
/// from the anchor). Past that the `as f64` cast drops low-order nanoseconds
/// before scaling, and `sim_ns`/`wall_ns` stop being exact inverses of each
/// other. Harmless for any realistic session length; noted here because the
/// anchors themselves (`sim_epoch_ns`, `wall_anchor_ns`) stay `u64` end to end
/// specifically to dodge this loss, so only the *offset* from the anchor is
/// exposed to it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SimClock {
    pub sim_epoch_ns: u64,
    pub wall_anchor_ns: u64,
    pub speed: f64,
}

impl SimClock {
    /// The identity map: simulated time equals wall time.
    #[must_use]
    pub const fn identity() -> Self {
        Self {
            sim_epoch_ns: 0,
            wall_anchor_ns: 0,
            speed: 1.0,
        }
    }

    /// Map a wall-clock nanosecond read onto the simulated axis.
    #[must_use]
    pub fn sim_ns(&self, wall_ns: u64) -> u64 {
        if wall_ns <= self.wall_anchor_ns {
            return self.sim_epoch_ns;
        }
        if !self.speed.is_finite() || self.speed <= 0.0 {
            return self.sim_epoch_ns;
        }
        let offset = wall_ns - self.wall_anchor_ns;
        let scaled = scaled_f64_to_u64(offset as f64 * self.speed);
        self.sim_epoch_ns.saturating_add(scaled)
    }

    /// Return the wall instant at which the clock reaches `sim_ns`.
    #[must_use]
    pub fn wall_ns(&self, sim_ns: u64) -> u64 {
        if sim_ns <= self.sim_epoch_ns {
            return self.wall_anchor_ns;
        }
        if !self.speed.is_finite() || self.speed <= 0.0 {
            return u64::MAX;
        }
        let offset = sim_ns - self.sim_epoch_ns;
        let scaled = scaled_f64_to_u64(offset as f64 / self.speed);
        self.wall_anchor_ns.saturating_add(scaled)
    }

    /// Return the wall duration that realizes `sim_dur_ns` simulated nanos.
    #[must_use]
    pub fn wall_span(&self, sim_dur_ns: u64) -> u64 {
        if !self.speed.is_finite() || self.speed <= 0.0 {
            return u64::MAX;
        }
        scaled_f64_to_u64(sim_dur_ns as f64 / self.speed)
    }

    /// `wall_span` as a `Duration`, the form every caller actually sleeps or
    /// intervals on. The single place the wall floor is applied: a span that
    /// scales to zero nanos is clamped to 1ns so a `tokio::time::sleep` /
    /// `interval` derived from a configured (sim-intended) duration never
    /// degenerates to a zero-delay busy loop. This 1ns is only the code floor;
    /// the EFFECTIVE floor is the tokio timer granularity (~1ms), below which a
    /// scaled duration coalesces regardless. See `reference/clock.md`.
    #[must_use]
    pub fn wall_duration(&self, sim_dur_ns: u64) -> std::time::Duration {
        std::time::Duration::from_nanos(self.wall_span(sim_dur_ns).max(1))
    }

    /// True when this clock is the default wall-time map.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        *self == Self::identity()
    }
}

/// The `/clock` payload: the affine `SimClock` plus the tape boundary the
/// server derived at boot. `SimClock` stays the pure wall-to-sim map; this
/// richer envelope publishes where the synthetic tape begins so a client can
/// guard its own warmup window instead of issuing a doomed off-tape fetch.
///
/// `server_now_ns` is `sim.sim_ns(wall)` sampled when the request is served, so
/// a client gets sim-now and the tape floor from one round trip without having
/// to read its own (possibly skewed) wall clock. `data_origin_ns` is the
/// earliest `ts_event` any source can serve (`server_now_at_boot -
/// backfill_horizon_ns`); a request for a `start` below it is refused. The
/// horizon is echoed so the client can report the floor in its own terms.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ServerClock {
    /// The affine wall-to-sim map the adapter feeds to the nautilus node.
    pub sim: SimClock,
    /// `sim.sim_ns(wall)` at the instant the `/clock` request was served.
    pub server_now_ns: u64,
    /// Earliest `ts_event` the tape can serve; a `start` below it is off-tape.
    pub data_origin_ns: u64,
    /// How far behind boot sim-now the tape begins, in nanoseconds.
    pub backfill_horizon_ns: u64,
}

/// API-boundary guard for `SimClock`, mirroring `validate_conn_havoc` /
/// `validate_market_regime` / `validate_client_havoc` in style. `speed` must
/// be finite and strictly positive.
///
/// `sim_ns`/`wall_ns`/`wall_span` all tolerate a non-finite or non-positive
/// `speed` in memory (they fall back to the anchor instant rather than
/// panic), so nothing in this type stops a degenerate `SimClock` from
/// existing - but it cannot round-trip over the wire: serde_json serializes a
/// non-finite `f64` as JSON `null`, and `null` fails to decode back into the
/// bare `f64` field, wedging whichever end tries to parse it.
/// `mogwai-server`'s own config-time check (`build_sim_clock`) already guards
/// the configured speed before a `SimClock` is ever constructed there; this
/// validator exists so any other sender of a `SimClock` - present or future -
/// has the same one-line gate to call before serializing one, instead of
/// reproducing the check ad hoc.
pub fn validate_sim_clock(clock: &SimClock) -> Result<(), &'static str> {
    if !clock.speed.is_finite() || clock.speed <= 0.0 {
        return Err("speed must be finite and > 0.0");
    }
    Ok(())
}

fn scaled_f64_to_u64(value: f64) -> u64 {
    if value.is_nan() || value <= 0.0 {
        0
    } else if value >= u64::MAX as f64 {
        u64::MAX
    } else {
        value as u64
    }
}

/// Saturating `Decimal` -> `f64`. `Decimal`'s max magnitude (~7.9e28) sits
/// nowhere near `f64::MAX` (~1.8e308), so `to_f64()` cannot actually fail for
/// any value the type can hold - the `unwrap_or(0.0)` fallback is defensive
/// completeness, not a live safety net today. It is kept anyway because
/// `0.0` is the worst possible sentinel for a price or quantity on the hot
/// fill/balance path (a huge magnitude would silently read as zero rather
/// than surface as an error), so if `Decimal`'s range or `f64`'s ever changed
/// underneath this, this is the one place that assumption needs revisiting.
/// The data crate carries a private reader with this exact contract; the
/// adapter's `convert.rs` already calls this helper directly rather than the
/// panicking `.to_f64().expect(...)` a pathological wire `Decimal` would
/// otherwise take the runtime down with.
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
/// Today this is the single BTCUSDT instrument. The engine seeds from this
/// function, and the server derives its default generator grid from the same
/// definition, so order validation and generated prices agree on tick size and
/// precision. The field values are price precision 2, size precision 8, with
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
///
/// `#[serde(default)]` at the container fills any OMITTED field from
/// `ConnHavoc::default()`, so a partial `[havoc.conn]` table (arming one knob,
/// e.g. only `heartbeat_interval_ms`) loads the way partial `[havoc.client]`
/// and `[havoc.data]` tables already do. It must be the CONTAINER default, not
/// per-field: per-field `#[serde(default)]` pulls each field type's own
/// `Default` (`0.0` for `reconnect_backoff_factor`, `0` for the delays), and a
/// zeroed `reconnect_backoff_factor` fails `validate_conn_havoc`. The container
/// default routes every omission through this struct's `Default` impl, which
/// carries the real production-shaped values (`1.0`s/`2.0` factor).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
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

/// Upper bound on any single `HavocLatency` field, in nanoseconds: 60 seconds.
///
/// The four delay fields are otherwise raw `u64` nanos, so without a ceiling an
/// armed value up to `u64::MAX` (~584 years) would compose into `delay_for` and
/// effectively wedge the stream. mogwai's job is to mimic a real network in
/// trouble, not an impossible one: the honest baseline is 30 ms, a badly
/// degraded link (hung proxy, congestion, a retransmit storm) stretches a frame
/// to seconds or low tens of seconds, and a frame arriving a full minute late
/// already reads as a dead connection downstream. 60 s spans that whole
/// pathological-but-plausible band while rejecting fat-fingered or hostile
/// values, and sits well under the one-hour `control::MAX_DIVERGENCE_MS` window
/// cap - an in-flight per-event delay belongs far below a total blackout.
pub const MAX_LATENCY_NANOS: u64 = 60_000_000_000;

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

/// API-boundary guard for the client-side transport havoc knobs, mirroring
/// `validate_conn_havoc` / `validate_market_regime` / `validate_divergence` in
/// style and message convention. The adapter runs it at config-`validate` time
/// (via `validate_havoc`) so an out-of-range knob never constructs a client.
///
/// `drop_prob`, `duplicate_prob`, and `reorder_prob` must each be a finite
/// probability in `[0.0, 1.0]`. The four `HavocLatency` delay fields must each
/// be `<= MAX_LATENCY_NANOS` (60 s), so an armed latency stays within the
/// pathological-but-plausible network band instead of wedging the stream with a
/// multi-century delay.
pub fn validate_client_havoc(client: &ClientHavoc) -> Result<(), &'static str> {
    if !finite_in(client.drop_prob, 0.0, 1.0) {
        return Err("drop_prob must be in [0.0, 1.0]");
    }
    if !finite_in(client.duplicate_prob, 0.0, 1.0) {
        return Err("duplicate_prob must be in [0.0, 1.0]");
    }
    if !finite_in(client.reorder_prob, 0.0, 1.0) {
        return Err("reorder_prob must be in [0.0, 1.0]");
    }
    if let Some(latency) = client.latency
        && (latency.base_nanos > MAX_LATENCY_NANOS
            || latency.exec_event_nanos > MAX_LATENCY_NANOS
            || latency.fill_nanos > MAX_LATENCY_NANOS
            || latency.data_nanos > MAX_LATENCY_NANOS)
    {
        return Err("HavocLatency fields must each be <= MAX_LATENCY_NANOS (60s)");
    }
    Ok(())
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
    fn sim_clock_identity_maps_wall_unchanged() {
        let clock = SimClock::identity();

        assert_eq!(clock.sim_ns(123), 123);
        assert_eq!(clock.wall_ns(456), 456);
        assert_eq!(clock.wall_span(789), 789);
        assert!(clock.is_identity());
    }

    #[test]
    fn sim_clock_preserves_epoch_scale_nanosecond_precision() {
        let clock = SimClock {
            sim_epoch_ns: 1_900_000_000_000_000_001,
            wall_anchor_ns: 10_000_000,
            speed: 1.0,
        };

        assert_eq!(
            clock.sim_ns(clock.wall_anchor_ns + 17),
            clock.sim_epoch_ns + 17
        );
        assert_eq!(
            clock.wall_ns(clock.sim_epoch_ns + 17),
            clock.wall_anchor_ns + 17
        );
    }

    #[test]
    fn sim_clock_wall_duration_scales_and_floors_at_one_nano() {
        use std::time::Duration;

        // Identity: a sim duration is its own wall duration.
        assert_eq!(
            SimClock::identity().wall_duration(5_000_000),
            Duration::from_nanos(5_000_000)
        );

        // Accelerated: a 100 ms sim duration realizes as 1 ms wall at 100x.
        let fast = SimClock {
            sim_epoch_ns: 1,
            wall_anchor_ns: 1,
            speed: 100.0,
        };
        assert_eq!(fast.wall_duration(100_000_000), Duration::from_millis(1));

        // A span that scales below one nanosecond is clamped to the 1ns code
        // floor, never zero (the tokio granularity floor is the real bound).
        assert_eq!(fast.wall_duration(0), Duration::from_nanos(1));
        assert_eq!(fast.wall_duration(50), Duration::from_nanos(1));
    }

    #[test]
    fn sim_clock_scales_and_inverts_with_rounding_bound() {
        let clock = SimClock {
            sim_epoch_ns: 86_400_000_000_000,
            wall_anchor_ns: 1_000_000_000,
            speed: 3.5,
        };
        let wall = clock.wall_anchor_ns + 20_000;
        let sim = clock.sim_ns(wall);

        assert_eq!(sim, clock.sim_epoch_ns + 70_000);
        assert_eq!(clock.wall_ns(sim), wall);
        assert_eq!(clock.wall_span(35_000), 10_000);
    }

    #[test]
    fn sim_clock_saturates_underflow_and_overflow() {
        let clock = SimClock {
            sim_epoch_ns: 1_000,
            wall_anchor_ns: 2_000,
            speed: 2.0,
        };
        assert_eq!(clock.sim_ns(1_999), 1_000);
        assert_eq!(clock.wall_ns(999), 2_000);

        let overflow_sim = SimClock {
            sim_epoch_ns: u64::MAX - 10,
            wall_anchor_ns: 0,
            speed: 100.0,
        };
        assert_eq!(overflow_sim.sim_ns(1), u64::MAX);

        let overflow_wall = SimClock {
            sim_epoch_ns: 0,
            wall_anchor_ns: u64::MAX - 10,
            speed: 1.0,
        };
        assert_eq!(overflow_wall.wall_ns(20), u64::MAX);
    }

    #[test]
    fn sim_clock_round_trips_over_json() {
        let clock = SimClock {
            sim_epoch_ns: 123,
            wall_anchor_ns: 456,
            speed: 7.5,
        };

        let json = serde_json::to_string(&clock).unwrap();
        let decoded: SimClock = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded, clock);
    }

    #[test]
    fn server_clock_serde_round_trip() {
        let clock = ServerClock {
            sim: SimClock {
                sim_epoch_ns: 1_900_000_000_000_000_000,
                wall_anchor_ns: 1_782_000_000_000_000_000,
                speed: 120.0,
            },
            server_now_ns: 1_900_000_799_000_000_000,
            data_origin_ns: 1_899_913_600_000_000_000,
            backfill_horizon_ns: 86_400_000_000_000,
        };

        let json = serde_json::to_string(&clock).unwrap();
        let decoded: ServerClock = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded, clock);
    }

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
    fn partial_conn_havoc_fills_omitted_fields_from_default() {
        // An operator arming ONE conn knob (here `heartbeat_interval_ms`) must
        // not be forced to spell out the other eight fields. A partial
        // `[havoc.conn]` table fills every omission from `ConnHavoc::default()`
        // (the container `#[serde(default)]`), matching how partial
        // `[havoc.client]` / `[havoc.data]` tables already load, and the result
        // still passes `validate_conn_havoc` - which a per-field default would
        // NOT, since it would zero `reconnect_backoff_factor` below its 1.0 floor.
        let decoded: ConnHavoc = serde_json::from_str(r#"{"heartbeat_interval_ms":2000}"#).unwrap();
        assert_eq!(
            decoded,
            ConnHavoc {
                heartbeat_interval_ms: 2000,
                ..ConnHavoc::default()
            }
        );
        assert_eq!(validate_conn_havoc(&decoded), Ok(()));

        // The same partial table nested one level up, as it arrives on a real
        // `[havoc]` scenario, resolves identically.
        let spec: HavocSpec =
            serde_json::from_str(r#"{"conn":{"heartbeat_interval_ms":2000}}"#).unwrap();
        assert_eq!(spec.conn, decoded);
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
    fn validate_client_havoc_bounds_probabilities_and_latency() {
        // A clean default and a fully-armed-but-in-range spec both pass.
        validate_client_havoc(&ClientHavoc::default()).expect("default is clean");
        validate_client_havoc(&ClientHavoc {
            latency: Some(HavocLatency {
                base_nanos: MAX_LATENCY_NANOS,
                exec_event_nanos: MAX_LATENCY_NANOS,
                fill_nanos: MAX_LATENCY_NANOS,
                data_nanos: MAX_LATENCY_NANOS,
            }),
            drop_prob: 1.0,
            duplicate_prob: 0.0,
            reorder_prob: 0.5,
            seed: Some(7),
        })
        .expect("max latency and boundary probabilities are valid");

        // Each probability is rejected out of [0.0, 1.0], including non-finite.
        for bad in [1.0001, -0.0001, f64::NAN, f64::INFINITY] {
            assert_eq!(
                validate_client_havoc(&ClientHavoc {
                    drop_prob: bad,
                    ..ClientHavoc::default()
                }),
                Err("drop_prob must be in [0.0, 1.0]")
            );
            assert_eq!(
                validate_client_havoc(&ClientHavoc {
                    duplicate_prob: bad,
                    ..ClientHavoc::default()
                }),
                Err("duplicate_prob must be in [0.0, 1.0]")
            );
            assert_eq!(
                validate_client_havoc(&ClientHavoc {
                    reorder_prob: bad,
                    ..ClientHavoc::default()
                }),
                Err("reorder_prob must be in [0.0, 1.0]")
            );
        }

        // Any single latency field over the ceiling is rejected.
        for latency in [
            HavocLatency {
                base_nanos: MAX_LATENCY_NANOS + 1,
                ..HavocLatency::default()
            },
            HavocLatency {
                exec_event_nanos: MAX_LATENCY_NANOS + 1,
                ..HavocLatency::default()
            },
            HavocLatency {
                fill_nanos: MAX_LATENCY_NANOS + 1,
                ..HavocLatency::default()
            },
            HavocLatency {
                data_nanos: MAX_LATENCY_NANOS + 1,
                ..HavocLatency::default()
            },
        ] {
            assert_eq!(
                validate_client_havoc(&ClientHavoc {
                    latency: Some(latency),
                    ..ClientHavoc::default()
                }),
                Err("HavocLatency fields must each be <= MAX_LATENCY_NANOS (60s)")
            );
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

    #[test]
    fn validate_sim_clock_rejects_non_finite_and_non_positive_speed() {
        let mut clock = SimClock::identity();
        assert_eq!(validate_sim_clock(&clock), Ok(()));

        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.0, -1.0] {
            clock.speed = bad;
            assert_eq!(
                validate_sim_clock(&clock),
                Err("speed must be finite and > 0.0")
            );
        }
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
    /// A `/ws` frame the server could not decode as a `ClientMessage` (bad
    /// JSON, unknown `type`, or a known `type` missing a required field, e.g.
    /// `{"type":"Subscribe"}` with no `symbols`). Emitted in place of the old
    /// silent drop: without it, a malformed live request and a healthy-but-idle
    /// feed were indistinguishable on the wire. Untargeted - the malformed
    /// frame carries no `client_order_id` to echo, unlike `OrderRejected`.
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
