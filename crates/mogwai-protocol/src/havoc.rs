// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::control;

/// One config object that arms mogwai's havoc surfaces.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct HavocSpec {
    /// Transport-level corruption the adapter applies to its own inbound stream.
    #[serde(default)]
    pub client: ClientHavoc,
    /// Execution divergences the adapter relays to mogwai-venue on connect.
    #[serde(default)]
    pub venue: Vec<control::Divergence>,
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
    // The mirror image of the gate above: a zero initial under a positive
    // ceiling is the SAME spin loop from the other side. The lifecycle backoff
    // is multiplicative (`initial * factor^attempt`), so `initial == 0` yields
    // a zero delay on every attempt no matter how large the ceiling - while
    // the venue is down the reconnect loop redials with `Duration::ZERO`, an
    // unthrottled CPU spin the positive max appears to rule out but cannot.
    // Disabling backoff is a deliberate two-knob act: BOTH bounds must be
    // zero. This also fails loud for a partial `[havoc.conn]` table that arms
    // only `reconnect_delay_initial_ms = 0` and inherits the default ceiling
    // (10000): add `reconnect_delay_max_ms = 0` to actually disable backoff.
    if conn.reconnect_delay_initial_ms == 0 && conn.reconnect_delay_max_ms > 0 {
        return Err(
            "reconnect_delay_initial_ms must be > 0 when reconnect_delay_max_ms > 0 (backoff is initial * factor^attempt, so a zero initial spins at zero delay forever; set BOTH to 0 to disable backoff)",
        );
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
/// This is distinct from venue divergences and client-side transport havoc,
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
/// The client-order-id targets must pass `validate_client_order_id`: the engine
/// rejects every submit carrying an invalid id, so an invalid target can never
/// match an order and would sit in the armed queue forever as a dead entry.
/// `PartialFillNext.fraction` must lie in the half-open `(0, 1]`: a fill must
/// move some quantity (`> 0`) and cannot exceed the order (`<= 1`).
/// `DelayAcks.ms`, `GoDark.ms`, and `StallData.ms` are bounded by
/// `MAX_DIVERGENCE_MS` so a control-plane request cannot arm an effectively
/// permanent window.
/// `RejectNextSubmit.reason` is capped here because the engine echoes it into an
/// order event whose reservation is derived from `MAX_REASON_LEN`.
///
/// The engine also applies a defensive runtime clamp (a `fraction > 1` becomes a
/// full fill, a `fraction <= 0` becomes a full fill with a warning), but that is
/// a last-line safety net; this validator is the authoritative guard that
/// rejects the misconfiguration early, before it is armed.
pub fn validate_divergence(div: &control::Divergence) -> Result<(), &'static str> {
    match div {
        control::Divergence::PartialFillNext {
            client_order_id,
            fraction,
        } => {
            if client_order_id.trim().is_empty() {
                return Err("PartialFillNext client_order_id is invalid");
            }
            crate::validate_client_order_id(client_order_id)
                .map_err(|_| "PartialFillNext client_order_id is invalid")?;
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
        // Bounded by `MAX_DIVERGENCE_MS`, the same ceiling as every other ms
        // window, and deliberately NOT `MAX_LATENCY_NANOS`: that 60 s ceiling is
        // argued from a per-event network delay on the INBOUND stream, while
        // these are venue windows on the same axis as `DelayAcks`.
        control::Divergence::CommandLatency {
            submit_act_ms,
            modify_act_ms,
            cancel_act_ms,
            submit_ack_ms,
            modify_ack_ms,
            cancel_ack_ms,
        } => {
            if [
                *submit_act_ms,
                *modify_act_ms,
                *cancel_act_ms,
                *submit_ack_ms,
                *modify_ack_ms,
                *cancel_ack_ms,
            ]
            .iter()
            .any(|ms| *ms > control::MAX_DIVERGENCE_MS)
            {
                return Err("CommandLatency fields must each be <= 3600000 (one hour)");
            }
            Ok(())
        }
        control::Divergence::FlowSurge {
            rate_mult,
            children_mult,
            duration_ms,
        } => {
            if !rate_mult.is_finite() || !(1.0..=1000.0).contains(rate_mult) || *rate_mult == 1.0 {
                return Err("FlowSurge rate_mult must be in (1, 1000]");
            }
            if !children_mult.is_finite() || !(1.0..=100.0).contains(children_mult) {
                return Err("FlowSurge children_mult must be in [1, 100]");
            }
            if *duration_ms > control::MAX_DIVERGENCE_MS {
                return Err("FlowSurge duration_ms must be <= 3600000");
            }
            Ok(())
        }
        control::Divergence::FeeSurcharge { mult, window_ms } => {
            if *mult <= Decimal::ZERO || *mult > Decimal::from(100) {
                return Err("FeeSurcharge mult must be in (0, 100]");
            }
            if *window_ms > control::MAX_DIVERGENCE_MS {
                return Err("FeeSurcharge window_ms must be <= 3600000");
            }
            Ok(())
        }
        control::Divergence::CancelOpenOrderSilently { client_order_id } => {
            if client_order_id.trim().is_empty() {
                return Err("CancelOpenOrderSilently client_order_id is invalid");
            }
            crate::validate_client_order_id(client_order_id)
                .map_err(|_| "CancelOpenOrderSilently client_order_id is invalid")?;
            Ok(())
        }
        control::Divergence::RejectNextSubmit { reason } => {
            if reason.len() > crate::MAX_REASON_LEN {
                return Err("RejectNextSubmit reason exceeds MAX_REASON_LEN");
            }
            Ok(())
        }
        control::Divergence::RejectNextCancel { reason } => {
            if reason.len() > crate::MAX_REASON_LEN {
                return Err("RejectNextCancel reason exceeds MAX_REASON_LEN");
            }
            Ok(())
        }
        control::Divergence::DuplicateNextFill
        | control::Divergence::DropNextAccountUpdate
        | control::Divergence::ClearDivergences
        // Nothing to validate: it carries no field, and it is legal against any
        // venue at any moment - a venue can always die.
        | control::Divergence::FaultTape => Ok(()),
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
            // Admission frames bucket with exec: the simulated INBOUND latency
            // is an adapter-side consumer knob, not a venue contract, and a
            // refusal is exec-adjacent from the consumer's point of view.
            // Giving it its own configured field would be a knob nobody asked
            // for. This is unrelated to the venue-side `DelayAcks` hold, which
            // admission traffic is exempt from.
            EventKind::Exec | EventKind::Admission => self.exec_event_nanos,
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
    /// Transport/admission truth: what the venue's request handling refused,
    /// clamped, or could not decode. Never engine output, so the knob that
    /// holds engine output (`DelayAcks`) legitimately does not reach it - see
    /// `docs/havoc.md`. `is_execution()` is FALSE for this kind, which is
    /// what implements that exemption in one place.
    Admission,
}

impl EventKind {
    /// Whether this category is an account/execution event rather than market
    /// data. `Exec` and `Fill` are both order-lifecycle (execution) traffic;
    /// `Data` is market data and `Admission` is neither - it is transport truth
    /// about a request the venue would not serve. The venue's outbound delay
    /// path keys off this split, while the adapter's latency bucketing uses the
    /// full `EventKind` - both consult [`VenueMessage::category`] so the two
    /// ends can never disagree about which side of the seam a variant sits on
    /// (the split-brain that classified `AccountState` as data on one end and
    /// execution on the other).
    ///
    /// Written as an EXHAUSTIVE MATCH rather than `matches!` so the compiler
    /// carries the claim: a new kind must opt IN to being delayed, and a
    /// `matches!` would silently opt it out (or, phrased as `!matches!(Data)`,
    /// silently opt it in) the day it is added. Here the crate does not build
    /// until the new variant is classified deliberately.
    #[must_use]
    pub fn is_execution(self) -> bool {
        match self {
            EventKind::Exec | EventKind::Fill => true,
            EventKind::Data | EventKind::Admission => false,
        }
    }
    /// Whether this kind rides the priority lane on the venue and bypasses the
    /// execution delay pump. Exhaustive for the same reason as
    /// [`Self::is_execution`].
    #[must_use]
    pub fn is_admission(self) -> bool {
        match self {
            EventKind::Admission => true,
            EventKind::Exec | EventKind::Fill | EventKind::Data => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            venue: vec![
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
            r#"{"client":{"latency":null,"drop_prob":0.0,"duplicate_prob":0.0,"reorder_prob":0.0,"seed":null},"venue":[],"conn":{"idle_timeout_ms":0,"heartbeat_interval_ms":0,"reconnect_delay_initial_ms":1000,"reconnect_delay_max_ms":10000,"reconnect_backoff_factor":2.0,"reconnect_jitter_ms":0,"reconnect_max_attempts":null,"max_requests_per_second":null,"request_timeout_secs":0}}"#
        );
    }

    #[test]
    fn havoc_spec_defaults_from_empty_object() {
        let decoded: HavocSpec = serde_json::from_str("{}").unwrap();
        assert_eq!(decoded.client, ClientHavoc::default());
        assert!(decoded.venue.is_empty());
        assert_eq!(decoded.data, None);
        assert_eq!(decoded.conn, ConnHavoc::default());

        let decoded: HavocSpec = serde_json::from_str(r#"{"venue":[]}"#).unwrap();
        assert_eq!(decoded.client, ClientHavoc::default());
        assert!(decoded.venue.is_empty());
        assert_eq!(decoded.data, None);
        assert_eq!(decoded.conn, ConnHavoc::default());

        let retired = serde_json::from_str::<HavocSpec>(r#"{"server":[]}"#)
            .expect_err("the retired server field must not be silently ignored");
        assert!(retired.to_string().contains("unknown field `server`"));
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

        // A partial table arming ONLY `reconnect_delay_initial_ms = 0` still
        // decodes (the container default fills the rest), but it inherits the
        // default ceiling (10000) and the validator rejects the combination
        // loudly - a zero initial under a positive max is a zero-delay
        // reconnect spin, and the error tells the operator the way out (zero
        // BOTH bounds to disable backoff) rather than silently arming it.
        let zero_initial: ConnHavoc =
            serde_json::from_str(r#"{"reconnect_delay_initial_ms":0}"#).unwrap();
        assert_eq!(
            zero_initial,
            ConnHavoc {
                reconnect_delay_initial_ms: 0,
                ..ConnHavoc::default()
            }
        );
        assert!(validate_conn_havoc(&zero_initial).is_err());
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

        // The mirror image: a zero initial under a positive ceiling is the
        // same spin loop from the other side - the lifecycle backoff is
        // `initial * factor^attempt`, so a zero initial stays zero on every
        // attempt regardless of the ceiling.
        invalid = conn;
        invalid.reconnect_delay_initial_ms = 0;
        invalid.reconnect_delay_max_ms = 5_000;
        assert_eq!(
            validate_conn_havoc(&invalid),
            Err(
                "reconnect_delay_initial_ms must be > 0 when reconnect_delay_max_ms > 0 (backoff is initial * factor^attempt, so a zero initial spins at zero delay forever; set BOTH to 0 to disable backoff)"
            )
        );

        // But a zero initial with a zero max is still fine (backoff disabled
        // takes BOTH knobs), as is the honest default.
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
        // Admission buckets with Exec by design (the doc on `delay_for` argues
        // it); without this line moving it to `data_nanos` moves nothing red.
        assert_eq!(
            latency.delay_for(EventKind::Admission).as_nanos(),
            11,
            "admission frames ride exec_event_nanos, not a knob of their own"
        );
    }

    /// The whole classification table, enumerated. The exhaustive `match` below
    /// is what makes this a claim about EVERY kind rather than a spot check:
    /// adding a variant fails to compile here and in `is_execution` /
    /// `is_admission`, so the "a new kind must opt IN to being delayed" comment
    /// cannot outlive its implementation.
    #[test]
    fn every_event_kind_is_classified_deliberately() {
        let latency = HavocLatency {
            base_nanos: 0,
            exec_event_nanos: 1,
            fill_nanos: 2,
            data_nanos: 3,
        };
        for kind in [
            EventKind::Exec,
            EventKind::Fill,
            EventKind::Data,
            EventKind::Admission,
        ] {
            // (is_execution, is_admission, latency bucket)
            let expected: (bool, bool, u128) = match kind {
                EventKind::Exec => (true, false, 1),
                EventKind::Fill => (true, false, 2),
                EventKind::Data => (false, false, 3),
                // Admission is neither execution nor data: it is transport
                // truth, and being outside `is_execution` is exactly what
                // exempts it from the venue's `DelayAcks` hold.
                EventKind::Admission => (false, true, 1),
            };
            assert_eq!(
                (
                    kind.is_execution(),
                    kind.is_admission(),
                    latency.delay_for(kind).as_nanos(),
                ),
                expected,
                "{kind:?} is misclassified"
            );
        }
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

        // Venue-owned delay/dark windows are bounded, while `0` remains valid
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

        // Each of the six `CommandLatency` fields carries the same one-hour
        // ceiling, checked independently: a single over-range field refuses the
        // whole arm, and an all-zero arm is the disarm.
        validate_divergence(&control::Divergence::CommandLatency {
            submit_act_ms: 0,
            modify_act_ms: 0,
            cancel_act_ms: 0,
            submit_ack_ms: 0,
            modify_ack_ms: 0,
            cancel_ack_ms: 0,
        })
        .expect("an all-zero arm is the disarm");
        let at_ceiling = control::Divergence::CommandLatency {
            submit_act_ms: control::MAX_DIVERGENCE_MS,
            modify_act_ms: control::MAX_DIVERGENCE_MS,
            cancel_act_ms: control::MAX_DIVERGENCE_MS,
            submit_ack_ms: control::MAX_DIVERGENCE_MS,
            modify_ack_ms: control::MAX_DIVERGENCE_MS,
            cancel_ack_ms: control::MAX_DIVERGENCE_MS,
        };
        validate_divergence(&at_ceiling).expect("the ceiling itself is valid");
        for over in 0..6 {
            let mut fields = [0u64; 6];
            fields[over] = control::MAX_DIVERGENCE_MS + 1;
            assert_eq!(
                validate_divergence(&control::Divergence::CommandLatency {
                    submit_act_ms: fields[0],
                    modify_act_ms: fields[1],
                    cancel_act_ms: fields[2],
                    submit_ack_ms: fields[3],
                    modify_ack_ms: fields[4],
                    cancel_ack_ms: fields[5],
                }),
                Err("CommandLatency fields must each be <= 3600000 (one hour)")
            );
        }

        // Every non-numeric variant is unconditionally valid. The two reason
        // carriers and the id carrier are here at legal values because the
        // loop used to skip them entirely, so nothing established that a
        // well-formed arm of theirs is ACCEPTED - only that a malformed one is
        // refused, which a validator refusing everything also satisfies.
        for div in [
            control::Divergence::RejectNextSubmit {
                reason: "nope".into(),
            },
            control::Divergence::RejectNextCancel {
                reason: "nope".into(),
            },
            control::Divergence::CancelOpenOrderSilently {
                client_order_id: "O-1".into(),
            },
            control::Divergence::DuplicateNextFill,
            control::Divergence::DropNextAccountUpdate,
            control::Divergence::ClearDivergences,
        ] {
            validate_divergence(&div).expect("non-numeric variants are always valid");
        }

        // The oversized-reason ceiling is per-variant code, so the cancel side
        // owes its own case; only the submit side had one.
        assert_eq!(
            validate_divergence(&control::Divergence::RejectNextCancel {
                reason: "R".repeat(crate::MAX_REASON_LEN + 1),
            }),
            Err("RejectNextCancel reason exceeds MAX_REASON_LEN")
        );
        for blank in ["", " ", "\t\n  "] {
            assert_eq!(
                validate_divergence(&control::Divergence::CancelOpenOrderSilently {
                    client_order_id: blank.into(),
                }),
                Err("CancelOpenOrderSilently client_order_id is invalid")
            );
        }
    }

    /// `FlowSurge` carries three independent bounds and had none of them
    /// pinned. `rate_mult`'s is the odd one - an inclusive `[1, 1000]` range
    /// MINUS the point 1.0, because a surge that multiplies by one is an armed
    /// divergence that does nothing.
    #[test]
    fn validate_divergence_bounds_flow_surge() {
        let surge =
            |rate_mult: f64, children_mult: f64, duration_ms: u64| control::Divergence::FlowSurge {
                rate_mult,
                children_mult,
                duration_ms,
            };

        // In range on every axis, including both inclusive edges.
        for div in [
            surge(1.000_001, 1.0, 0),
            surge(2.0, 3.0, 1_000),
            surge(1_000.0, 100.0, control::MAX_DIVERGENCE_MS),
        ] {
            validate_divergence(&div).expect("an in-range surge is valid");
        }

        // rate_mult: the excluded lower point, below it, above the ceiling,
        // and non-finite.
        for bad in [1.0, 0.999, 0.0, -1.0, 1_000.1, f64::NAN, f64::INFINITY] {
            assert_eq!(
                validate_divergence(&surge(bad, 2.0, 0)),
                Err("FlowSurge rate_mult must be in (1, 1000]"),
                "rate_mult {bad} must be refused"
            );
        }

        // children_mult: inclusive at 1, so only below it and above 100 fail.
        for bad in [0.999, 0.0, -1.0, 100.1, f64::NAN, f64::INFINITY] {
            assert_eq!(
                validate_divergence(&surge(2.0, bad, 0)),
                Err("FlowSurge children_mult must be in [1, 100]"),
                "children_mult {bad} must be refused"
            );
        }

        assert_eq!(
            validate_divergence(&surge(2.0, 2.0, control::MAX_DIVERGENCE_MS + 1)),
            Err("FlowSurge duration_ms must be <= 3600000")
        );
    }

    /// `FeeSurcharge` is the `Decimal` twin of the surge bounds, and was
    /// likewise untested. `mult` is half-open `(0, 100]`: a zero multiplier
    /// would make every fill free, which is not a surcharge.
    #[test]
    fn validate_divergence_bounds_fee_surcharge() {
        let fee =
            |mult: Decimal, window_ms: u64| control::Divergence::FeeSurcharge { mult, window_ms };

        for div in [
            fee(Decimal::new(1, 2), 0),
            fee(Decimal::ONE, 1_000),
            fee(Decimal::from(100), control::MAX_DIVERGENCE_MS),
        ] {
            validate_divergence(&div).expect("an in-range surcharge is valid");
        }

        for bad in [
            Decimal::ZERO,
            Decimal::new(-1, 2),
            Decimal::from(-100),
            Decimal::new(1_0001, 2),
        ] {
            assert_eq!(
                validate_divergence(&fee(bad, 0)),
                Err("FeeSurcharge mult must be in (0, 100]"),
                "mult {bad} must be refused"
            );
        }

        assert_eq!(
            validate_divergence(&fee(Decimal::ONE, control::MAX_DIVERGENCE_MS + 1)),
            Err("FeeSurcharge window_ms must be <= 3600000")
        );
    }

    #[test]
    fn validate_divergence_rejects_blank_partial_fill_target() {
        // The engine rejects every submit whose client_order_id trims to
        // empty, so a PartialFillNext targeting a blank id can never match an
        // order - it would sit armed forever as a dead queue entry. The same
        // trim-based emptiness check the engine applies to submits guards the
        // divergence target here, so whitespace-only ids reject too.
        for blank in ["", " ", "\t\n  "] {
            assert_eq!(
                validate_divergence(&control::Divergence::PartialFillNext {
                    client_order_id: blank.into(),
                    fraction: Decimal::new(5, 1),
                }),
                Err("PartialFillNext client_order_id is invalid")
            );
        }

        // A real, submit-valid id stays valid.
        for ok in ["O-1", "O_1"] {
            validate_divergence(&control::Divergence::PartialFillNext {
                client_order_id: ok.into(),
                fraction: Decimal::new(5, 1),
            })
            .expect("non-blank target id is valid");
        }
    }

    #[test]
    fn validate_divergence_rejects_unmatchable_targets_and_oversized_reason() {
        let oversized_id = "X".repeat(crate::MAX_CLIENT_ID_LEN + 1);
        assert_eq!(
            validate_divergence(&control::Divergence::PartialFillNext {
                client_order_id: oversized_id.clone(),
                fraction: Decimal::ONE,
            }),
            Err("PartialFillNext client_order_id is invalid")
        );
        assert_eq!(
            validate_divergence(&control::Divergence::CancelOpenOrderSilently {
                client_order_id: oversized_id,
            }),
            Err("CancelOpenOrderSilently client_order_id is invalid")
        );
        assert_eq!(
            validate_divergence(&control::Divergence::RejectNextSubmit {
                reason: "R".repeat(crate::MAX_REASON_LEN + 1),
            }),
            Err("RejectNextSubmit reason exceeds MAX_REASON_LEN")
        );
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
}
