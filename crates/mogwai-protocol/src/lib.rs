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
    AccountState, AggressorSide, Balance, ClientMessage, OrderFilled, OrderType, Position,
    QuoteTick, ServerMessage, Side, SubmitOrder, TimeInForce, TradeTick, validate_modify_order,
    validate_submit_order,
};
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
