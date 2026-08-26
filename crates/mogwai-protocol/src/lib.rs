// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Wire protocol shared by the mogwai fake broker and its nautilus adapter.
//!
//! This is the single source of truth for the native JSON-over-WS protocol. The
//! adapter side depends on this crate so both ends serialize identical types.
//! mogwai never imports nautilus; nautilus types are mirrored here only as far as
//! the wire needs them.
//!
//! The two directions are named asymmetrically, and the asymmetry is the point.
//! Outbound frames are `VenueMessage`, named for the party that sends them,
//! because the venue is one thing and every frame comes from it. Inbound frames
//! are `Command`, named for what they carry, because there is no singular party
//! to name them after: a consumer may be one process, several sharing nothing
//! but the wire, or the program the venue is embedded in, and what the venue
//! perceives is one connection under one account rather than a consumer. A
//! `ConsumerMessage` would name something the venue cannot see. See the
//! Consumer entry in `reference/glossary.md`.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

mod clock;
/// `Decimal` conversions, and the serde glue that makes a wire decimal a JSON
/// string rather than a JSON number. Public for the one decode path outside
/// this crate that carries money: `mogwai-venue`'s `POST /accounts` balances.
pub mod decimal;
mod havoc;
mod instruments;
mod messages;
mod ready;
pub mod seeds;
pub mod sizing;

pub mod close;
pub mod control;
/// Launching a venue and learning its endpoint - the launcher half of the
/// readiness handshake, shipped so consumers do not re-derive it from prose.
pub mod launch;
pub mod risk;

pub use clock::{SimClock, VenueClock, now_unix_nanos, validate_sim_clock};
pub use decimal::{decimal_from_f64, decimal_to_f64};
pub use havoc::{
    BASELINE_LATENCY, ConnHavoc, EventKind, HavocLatency, HavocSpec, InboundHavoc, MAX_HALT_SECS,
    MAX_LATENCY_NANOS, MarketRegime, REFUSE_HALT_SECS, finite_in, finite_in_excl_lo,
    validate_conn_havoc, validate_divergence, validate_inbound_havoc, validate_market_regime,
};
pub use instruments::{
    FundingTerms, InstrumentClass, InstrumentDef, OmsType, WireAssetClass, default_instruments,
};
pub use messages::{
    ADMISSION_ENVELOPE_BYTES, ADMISSION_FRAME_MAX_BYTES, AccountId, AccountIdError, AccountState,
    AdmissionSubject, AggressorSide, Balance, Command, CommandClass, Contingency, FillSnapshot,
    HistoryKind, HistoryRow, Hit, JSON_ESCAPE_FACTOR, LiquiditySide, MAX_ACCOUNT_ID_LEN,
    MAX_CALLSIGN_LEN, MAX_CURRENCY_LEN, MAX_ECHOED_ID_LEN, MAX_GROUP_ORDERS,
    MAX_INBOUND_MESSAGE_BYTES, MAX_LINKED_ORDERS, MAX_REASON_LEN, MAX_SYMBOL_LEN, OrderFilled,
    OrderLink, OrderStatusInfo, OrderStatusSnapshot, OrderType, POST_ONLY_REFUSAL, Position,
    PostedMargin, QueryKind, QuoteTick, ScanKind, Side, SubmitOrder, SubmitPhase, TimeInForce,
    TradeTick, VenueMessage, WireOrderStatus, touches_toward, touches_trigger, trades_through,
    truncate_echoed_id, truncate_reason, validate_callsign, validate_client_order_id,
    validate_currency_code, validate_modify_order, validate_request_id, validate_submit_group,
    validate_submit_order, validate_wire_symbol,
};
pub use ready::ReadyRecord;
pub use seeds::RunSeeds;

pub type Symbol = std::sync::Arc<str>;
/// Consumer-assigned order id (nautilus `ClientOrderId`).
pub type ClientOrderId = String;
/// Venue-assigned order id (mogwai-assigned `VenueOrderId`).
pub type VenueOrderId = String;

/// Default per-request timeout in seconds for HTTP order entry. This is the
/// value `ConnHavoc.request_timeout_secs == 0` documents as "keeps 30s"; the
/// adapter sources every occurrence from this constant (`clock.rs`,
/// `client.rs`'s `request_timeout_secs`) rather than repeating the literal, so
/// the honest-transport default lives in exactly one spot.
pub const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 30;

/// How long a consumer waits for one `/ws` upgrade before abandoning it.
///
/// Sized for a cold river, not for a handshake. The venue materializes no river
/// until something names it, so the first boarding of a river pays that river's
/// whole warmup synthesis inside the upgrade - the boat cannot be placed until
/// the water it reads exists. This was five hardcoded seconds, chosen when one
/// river was warmed before readiness and every first boarding therefore found
/// its water already there; with that privilege gone, five seconds refuses any
/// warmup a consumer might reasonably configure.
///
/// It is a consumer policy and it belongs to the consumer: the venue declines to
/// promise a fast first boarding, because the honest cost of a long warmup is a
/// long wait. Raising it past a nautilus node's own `timeout_connection`, which
/// defaults to sixty seconds, buys nothing - that deadline governs the node's
/// wait for every client to report connected, and it is the host's to raise.
pub const DEFAULT_DIAL_TIMEOUT_SECS: u64 = 60;

/// What a `/trades` request that states no `limit` gets. This is the
/// no-opinion answer, deliberately split from the ceiling below: at the
/// raw-fill cadence one page at `MAX_HISTORY_LIMIT` is roughly 7 MB of JSON,
/// and serving that to a caller who expressed no opinion would be a 50x
/// regression nobody asked for. 1000 raw fills is ~20 simulated seconds.
pub const DEFAULT_HISTORY_LIMIT: usize = 1_000;

/// Maximum number of trades a single `/trades` history page returns - the
/// ceiling an explicit caller may ask for. The venue enforces it (it clamps
/// every request to it) and the adapter, which always states a limit, asks for
/// exactly this; sourcing both from here keeps the two in lockstep, so the
/// adapter never advertises a ceiling larger than the venue will honor.
///
/// Sized against a loopback venue: ~1000 simulated seconds per page, ~7 MB of
/// JSON, synthesized well inside `DEFAULT_REQUEST_TIMEOUT_SECS`. It is not
/// sized for a real network, and the adapter additionally bounds a whole paged
/// request with its own `MAX_TRADES_PER_REQUEST`.
pub const MAX_HISTORY_LIMIT: usize = 50_000;

/// Ceiling on a `QueryHistory` continuation token, in bytes.
///
/// The token is the venue's own bookkeeping handed back verbatim, so this is
/// not a limit a consumer has to reason about - it is what stops a fabricated
/// one from becoming an unbounded allocation on the decode path, the same
/// reason `MAX_ECHOED_ID_LEN` exists. Generous against the shape the venue
/// actually emits, which is a version tag and three integers.
pub const MAX_CONTINUATION_LEN: usize = 256;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trades_through_is_strict_on_both_sides() {
        let limit = Decimal::from(100);
        assert!(trades_through(Side::Buy, limit, Decimal::from(99)));
        assert!(!trades_through(Side::Buy, limit, limit));
        assert!(trades_through(Side::Sell, limit, Decimal::from(101)));
        assert!(!trades_through(Side::Sell, limit, limit));
    }

    #[test]
    fn run_complete_round_trips() {
        let message = VenueMessage::RunComplete {
            sim_now_ns: 123,
            elapsed_ns: 45,
        };
        let json = serde_json::to_string(&message).expect("serialize RunComplete");
        assert_eq!(
            json,
            r#"{"type":"RunComplete","sim_now_ns":123,"elapsed_ns":45}"#
        );
        let decoded: VenueMessage = serde_json::from_str(&json).expect("decode RunComplete");
        assert_eq!(serde_json::to_string(&decoded).unwrap(), json);
    }
}
