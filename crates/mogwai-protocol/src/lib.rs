// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Wire protocol shared by the mogwai fake broker and its broadarrow adapter.
//!
//! This is the single source of truth for the native JSON-over-WS protocol. The
//! broadarrow-side adapter path-deps this crate so both ends serialize identical types.
//! mogwai never imports nautilus; nautilus types are mirrored here only as far as
//! the wire needs them.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

mod clock;
mod decimal;
mod havoc;
mod instruments;
mod messages;
pub mod sizing;
mod transport;

pub mod control;

pub use clock::{ServerClock, SimClock, now_unix_nanos, validate_sim_clock};
pub use decimal::{decimal_from_f64, decimal_to_f64};
pub use havoc::{
    BASELINE_LATENCY, ClientHavoc, ConnHavoc, EventKind, HavocLatency, HavocSpec,
    MAX_LATENCY_NANOS, MarketRegime, finite_in, finite_in_excl_lo, validate_client_havoc,
    validate_conn_havoc, validate_divergence, validate_market_regime,
};
pub use instruments::{InstrumentDef, default_instruments};
pub use messages::{
    ADMISSION_FRAME_MAX_BYTES, AccountId, AccountIdError, AccountState, AdmissionSubject,
    AggressorSide, Balance, ClientMessage, FillSnapshot, JSON_ESCAPE_FACTOR, MAX_ACCOUNT_ID_LEN,
    MAX_CLIENT_ID_LEN, MAX_CURRENCY_LEN, MAX_REASON_LEN, MAX_SUBSCRIBE_SYMBOLS,
    MAX_SUBSCRIPTION_ISSUES_LISTED, MAX_SYMBOL_LEN, OrderFilled, OrderStatusInfo,
    OrderStatusSnapshot, OrderType, Position, QueryKind, QuoteTick, ServerMessage, Side,
    SubmitOrder, SubscriptionIssue, SubscriptionOutcome, SubscriptionRequest, TimeInForce,
    TradeTick, WireOrderStatus, truncate_client_id, truncate_reason, validate_client_order_id,
    validate_modify_order, validate_request_id, validate_submit_order, validate_subscriptions,
    validate_symbols,
};
/// HTTP header carrying an acting account on stateful venue requests.
pub const ACCOUNT_HEADER: &str = "x-mogwai-account";
/// Websocket query parameter carrying the account bound to a session.
pub const ACCOUNT_QUERY_PARAM: &str = "account";
pub use transport::TransportProfile;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_request_timeout_secs_is_thirty() {
        assert_eq!(DEFAULT_REQUEST_TIMEOUT_SECS, 30);
    }
}
