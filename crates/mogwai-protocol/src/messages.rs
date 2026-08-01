// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::havoc::{EventKind, MarketRegime};
use crate::{ClientOrderId, Symbol, VenueOrderId};

/// Maximum byte length of any client-supplied identifier the venue echoes back
/// into its own output: `client_order_id`, `request_id`. The cap exists so a
/// produced frame has a computable upper bound - the admission reservation in
/// `mogwai-server` sizes worst-case output against it, and an unbounded id
/// would make that bound unprovable (and let one 8 MiB order id exhaust a
/// connection's whole execution budget).
pub const MAX_CLIENT_ID_LEN: usize = 64;
/// Maximum byte length of the account identity carried by the transport.
pub const MAX_ACCOUNT_ID_LEN: usize = 64;

/// A venue account identity. Kept deliberately small and log-safe because it
/// is accepted at every stateful transport boundary.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AccountId(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountIdError {
    Empty,
    TooLong,
    IllegalChar(char),
}

impl std::fmt::Display for AccountIdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => f.write_str("must not be empty"),
            Self::TooLong => write!(f, "exceeds MAX_ACCOUNT_ID_LEN ({MAX_ACCOUNT_ID_LEN})"),
            Self::IllegalChar(ch) => write!(f, "contains illegal character {ch:?}"),
        }
    }
}

impl std::error::Error for AccountIdError {}

impl AccountId {
    pub fn parse(raw: &str) -> Result<Self, AccountIdError> {
        if raw.is_empty() {
            return Err(AccountIdError::Empty);
        }
        if raw.len() > MAX_ACCOUNT_ID_LEN {
            return Err(AccountIdError::TooLong);
        }
        for ch in raw.chars() {
            if !ch.is_ascii_alphanumeric() && !matches!(ch, '.' | '_' | ':' | '-') {
                return Err(AccountIdError::IllegalChar(ch));
            }
        }
        Ok(Self(raw.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Maximum byte length of a symbol on the wire, same reasoning as
/// `MAX_CLIENT_ID_LEN`.
pub const MAX_SYMBOL_LEN: usize = 32;

/// Maximum byte length of a server-generated `reason` string. Constructors
/// truncate to this on a char boundary rather than rejecting: a reason is
/// diagnostic prose, and a truncated diagnostic is still truthful about what
/// happened, whereas a refused frame would not be.
pub const MAX_REASON_LEN: usize = 512;

/// Maximum byte length of a currency code, an instrument base or an instrument
/// quote as configured. Operator-supplied config strings reach the wire through
/// `AccountState`'s balance rows and every position row, so
/// `sizing::BALANCE_ROW_MAX_BYTES` / `sizing::POSITION_ROW_MAX_BYTES` are only
/// upper bounds if these are capped too. Enforced where the config is loaded
/// (`mogwai-server/src/config.rs`), which fails startup rather than a
/// connection.
pub const MAX_CURRENCY_LEN: usize = 16;

/// Maximum number of symbols one `Subscribe`/`Unsubscribe` may name, so a
/// single read-loop iteration cannot be made to demand 100k per-symbol
/// reservations.
pub const MAX_SUBSCRIBE_SYMBOLS: usize = 256;

/// Maximum outcomes listed on one coalesced `SubscriptionIssues` frame.
///
/// The cap is LOSSY and deliberately so. A subscribe producing more than this
/// many outcomes drops the surplus, and those entries get no `(symbol,
/// generation, issue)` at all - a genuine attribution hole that can swallow the
/// news that a feed is dead. It is kept because the cap is what makes
/// `ADMISSION_FRAME_MAX_BYTES` provable and what keeps a `MAX_SUBSCRIBE_SYMBOLS`
/// subscribe to ONE priority frame, both load-bearing.
///
/// Two things pay for it: `entries` is filled with REFUSALS first, so the
/// outcomes that kill a feed are the ones that survive truncation (a lost
/// degradation costs a log line, a lost refusal costs a feed); and the frame
/// carries `issues_total` AND `refusals_total`, so a client seeing more refusals
/// counted than listed knows attributably that some feed it asked for is dead
/// and can resubscribe the set rather than guess.
///
/// Pagination and a larger frame were both rejected: pagination puts multi-frame
/// sequencing state on the priority lane the coalescing exists to protect, and a
/// larger frame reopens a currently-proven ceiling.
pub const MAX_SUBSCRIPTION_ISSUES_LISTED: usize = 16;

/// Worst-case expansion factor `serde_json` applies to an arbitrary string of
/// N bytes: a byte that must be escaped as `\uXXXX` costs six output bytes.
/// Every `*_MAX_BYTES` constant is stated in SERIALIZED bytes, so each embedded
/// string contributes `JSON_ESCAPE_FACTOR * cap`, never its raw cap. Sizing
/// against raw lengths - which an implementer measuring with ordinary ASCII
/// test strings would never catch - makes a reservation a typical case rather
/// than an upper bound.
pub const JSON_ESCAPE_FACTOR: usize = 6;

/// Upper bound on the serialized bytes of any `EventKind::Admission` frame -
/// `AdmissionRejected`, `ProtocolError`, and `SubscriptionIssues`, since all ride the server's
/// priority lane. Proven by `admission_frames_fit_their_ceiling`: the widest
/// widest frame is `SubscriptionIssues` with
/// `MAX_SUBSCRIPTION_ISSUES_LISTED` entries. Each charges its symbol at
/// `JSON_ESCAPE_FACTOR * MAX_SYMBOL_LEN`, two u64 values and fixed issue JSON,
/// keeping the list below 5600 bytes plus envelope. This bound is
/// what makes the priority lane's FRAME count a memory bound, so every
/// `ProtocolError` construction site must route its reason through
/// `truncate_reason`.
pub const ADMISSION_FRAME_MAX_BYTES: usize = 8192;

/// Truncate a server-generated reason to `MAX_REASON_LEN` bytes on a char
/// boundary, appending nothing (the truncation is visible as an abrupt end).
#[must_use]
pub fn truncate_reason(mut reason: String) -> String {
    if reason.len() <= MAX_REASON_LEN {
        return reason;
    }
    let mut end = MAX_REASON_LEN;
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    reason.truncate(end);
    reason
}

/// Truncate a client-supplied identifier to `MAX_CLIENT_ID_LEN` bytes on a
/// char boundary, for ECHOING back in a refusal. An over-length id is never
/// accepted, so a truncated echo cannot be mistaken for a live correlation: a
/// client matching on it finds no order, which is the truth. Echoing the id at
/// full length would recreate the unbounded frame the cap exists to prevent.
#[must_use]
pub fn truncate_client_id(mut id: String) -> String {
    if id.len() <= MAX_CLIENT_ID_LEN {
        return id;
    }
    let mut end = MAX_CLIENT_ID_LEN;
    while !id.is_char_boundary(end) {
        end -= 1;
    }
    id.truncate(end);
    id
}

/// Boundary guard for a client order id: over-length is a MALFORMED request,
/// refused with the existing rejection mechanism, never with
/// `AdmissionRejected` (which reads as a capacity signal).
pub fn validate_client_order_id(id: &ClientOrderId) -> Result<(), &'static str> {
    (id.len() <= MAX_CLIENT_ID_LEN)
        .then_some(())
        .ok_or("client_order_id exceeds MAX_CLIENT_ID_LEN")
}

/// Boundary guard for a venue-truth query's `request_id`, which the venue
/// echoes on its reply and on a refusal.
pub fn validate_request_id(id: &str) -> Result<(), &'static str> {
    (id.len() <= MAX_CLIENT_ID_LEN)
        .then_some(())
        .ok_or("request_id exceeds MAX_CLIENT_ID_LEN")
}

/// Boundary guard for a `Subscribe`/`Unsubscribe` symbol list: both the
/// per-symbol length and the CARDINALITY are capped. Called from the websocket
/// read loop, not from the order path - subscribes never reach the order path.
pub fn validate_symbols(symbols: &[Symbol]) -> Result<(), &'static str> {
    if symbols.len() > MAX_SUBSCRIBE_SYMBOLS {
        return Err("symbols exceeds MAX_SUBSCRIBE_SYMBOLS");
    }
    if symbols.iter().any(|s| s.len() > MAX_SYMBOL_LEN) {
        return Err("symbol exceeds MAX_SYMBOL_LEN");
    }
    Ok(())
}

/// Boundary guard for a `Subscribe`'s entry list: cardinality, per-symbol
/// length, and DUPLICATE SYMBOLS. Two entries naming one symbol with different
/// generations have no defined meaning - which cursor wins, which generation is
/// current - so the whole frame is refused rather than silently deduplicated.
/// `dedup_symbols` was the old answer and it silently discarded a cursor; a
/// refusal is the honest one. (`dedup_symbols` survives on the `Unsubscribe`
/// path, where a duplicate genuinely is a no-op.)
pub fn validate_subscriptions(subs: &[SubscriptionRequest]) -> Result<(), &'static str> {
    if subs.len() > MAX_SUBSCRIBE_SYMBOLS {
        return Err("subscriptions exceeds MAX_SUBSCRIBE_SYMBOLS");
    }
    if subs.iter().any(|entry| entry.symbol.len() > MAX_SYMBOL_LEN) {
        return Err("symbol exceeds MAX_SYMBOL_LEN");
    }
    let mut symbols: Vec<&str> = subs.iter().map(|entry| entry.symbol.as_str()).collect();
    symbols.sort_unstable();
    if symbols.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err("subscriptions names a symbol twice");
    }
    Ok(())
}

/// One symbol's subscription within a `Subscribe`. Self-contained: identity,
/// cursor and regime all per entry, so a client may resubscribe one symbol
/// without restating any other's state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubscriptionRequest {
    /// Client-chosen identity for THIS generation of this symbol's stream.
    ///
    /// The client is OBLIGED to make it strictly increasing per symbol within a
    /// connection; the venue enforces the obligation and refuses any entry that
    /// breaks it with `SubscriptionIssue::StaleGeneration`, because a reused
    /// generation makes a stale diagnostic look current. A monotone counter that
    /// never resets - not even across reconnects - trivially conforms.
    ///
    /// The venue never interprets it beyond ordering it against the generation
    /// it recorded for the symbol, and echoing it on every diagnostic about this
    /// subscription. Ordering is what lets a client tell a diagnostic about a
    /// superseded generation from one about a generation it never issued, which
    /// an opaque identifier cannot.
    pub generation: u64,
    pub symbol: Symbol,
    /// Replay from this unix-nanosecond instant forward. `None` positions the
    /// stream at sim-now (a live subscribe, the shape that avoids a catch-up
    /// dump); `Some` is a historical window or a resume cursor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_ts: Option<u64>,
    /// Generator-level market regime for THIS symbol's stream only. The regime
    /// is per entry on the wire; the adapter today sends one value for every
    /// entry, so a genuinely per-symbol regime is a client-side change only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regime: Option<MarketRegime>,
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
        subscriptions: Vec<SubscriptionRequest>,
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
    /// Reconciliation query: ask the venue for the CURRENT status of its
    /// orders, answered from the engine's own book - not from any event the
    /// client may or may not have received. This is the second, independent
    /// witness Nautilus' reconciliation (startup mass-status and the
    /// continuous open-order poll) consumes: after a havoc scenario cancels a
    /// resting order server-side and drops the lifecycle event, this query
    /// still reports the truth.
    ///
    /// Honest-content invariant: the reply's CONTENT is always a truthful
    /// read of the venue book. Havoc may delay or drop the reply's DELIVERY
    /// (the snapshot classifies as execution, so `DelayAcks` holds it and
    /// `GoDark` drops it - transport faults are fair game and exercise the
    /// consumer's query-timeout path), but no divergence may ever alter what
    /// it says. A venue that lies on the reconciliation channel collapses the
    /// two witnesses into one adversary and makes any poll-heal test
    /// unprovable; a lying venue-truth source is a different fault class that
    /// would need its own explicitly-named havoc, never a side effect here.
    QueryOrders {
        /// Client-chosen correlation id echoed verbatim on the reply, so a
        /// requester sharing the socket with unsolicited events can match
        /// replies to requests.
        request_id: String,
        /// Restrict the reply to this one order. `None` reports every order
        /// the venue has ever accepted (open and terminal).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_order_id: Option<ClientOrderId>,
        /// Restrict the reply to currently-open orders. Terminal orders are
        /// omitted; an id queried directly still reports its terminal state.
        #[serde(default)]
        open_only: bool,
    },
    /// Reconciliation query for the venue's fill history, the fill-report
    /// twin of [`ClientMessage::QueryOrders`] with the same honest-content /
    /// havoc-able-delivery contract. The venue records each fill ONCE as it
    /// books - a `DuplicateNextFill` doubles the wire event, not the truth -
    /// so this reply is the ground truth a dropped or duplicated
    /// `OrderFilled` stream can be reconciled against.
    QueryFills {
        /// Correlation id echoed verbatim on the reply.
        request_id: String,
        /// Restrict the reply to fills of this one order. `None` reports all.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_order_id: Option<ClientOrderId>,
    },
}

/// Venue-side order status as reported on an [`OrderStatusSnapshot`]. Only
/// states the venue itself can attest to: a submit that never passed the
/// accept gate leaves no record (its id is absent from the snapshot), so
/// there is no `Rejected` variant - "absent" is the truthful answer for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireOrderStatus {
    /// Accepted and resting, nothing filled yet.
    Accepted,
    /// Resting with some quantity filled.
    PartiallyFilled,
    /// Terminal: fully filled.
    Filled,
    /// Terminal: canceled (client cancel, IOC remainder, or a server-side
    /// havoc cancel).
    Canceled,
}

impl WireOrderStatus {
    #[must_use]
    pub fn is_open(self) -> bool {
        matches!(self, Self::Accepted | Self::PartiallyFilled)
    }
}

/// One order's venue-truth status row on an [`OrderStatusSnapshot`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderStatusInfo {
    pub client_order_id: ClientOrderId,
    pub venue_order_id: VenueOrderId,
    pub symbol: Symbol,
    pub side: Side,
    pub order_type: OrderType,
    pub time_in_force: TimeInForce,
    pub status: WireOrderStatus,
    /// Current total order quantity (post-amend, if any).
    pub quantity: Decimal,
    /// Quantity filled so far.
    pub filled_qty: Decimal,
    /// Current order price. Always present in practice (the server stamps
    /// Market orders before the engine sees them), optional on the wire to
    /// mirror `SubmitOrder`.
    pub price: Option<Decimal>,
    /// When the venue accepted the order (sim unix-ns).
    pub ts_accepted: u64,
    /// Last lifecycle activity: accept, fill, amend, or terminal transition.
    pub ts_last: u64,
}

/// Reply to [`ClientMessage::QueryOrders`]: the venue's truthful order book
/// read at `ts_event`. An empty `orders` for a targeted query means the venue
/// never accepted that id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderStatusSnapshot {
    /// The request's correlation id, echoed verbatim.
    pub request_id: String,
    pub orders: Vec<OrderStatusInfo>,
    pub ts_event: u64,
}

/// Reply to [`ClientMessage::QueryFills`]: the venue's booked fills in the
/// order they booked. Each fill appears exactly once regardless of how many
/// `OrderFilled` events the wire carried for it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FillSnapshot {
    /// The request's correlation id, echoed verbatim.
    pub request_id: String,
    pub fills: Vec<OrderFilled>,
    pub ts_event: u64,
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
    validate_client_order_id(&order.client_order_id)?;
    if order.symbol.len() > MAX_SYMBOL_LEN {
        return Err("symbol exceeds MAX_SYMBOL_LEN");
    }
    if order.quantity <= Decimal::ZERO {
        return Err("quantity must be > 0");
    }
    match order.price {
        Some(price) if price <= Decimal::ZERO => Err("price must be > 0"),
        None if order.order_type == OrderType::Limit => Err("Limit order must carry a price"),
        _ => Ok(()),
    }
}

/// What an `AdmissionRejected` refers to. Present because the refusal must be
/// translatable: the adapter turns a refused submit into nautilus
/// `OrderRejected` but a refused cancel into `OrderCancelRejected` - flipping a
/// live order to Rejected because its CANCEL was refused would be an invalid
/// transition (see `ServerMessage::OrderCancelRejected`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind")]
pub enum AdmissionSubject {
    Submit {
        client_order_id: ClientOrderId,
    },
    Cancel {
        client_order_id: ClientOrderId,
    },
    Modify {
        client_order_id: ClientOrderId,
    },
    /// A `QueryOrders` or `QueryFills`; the id is the one that would have been
    /// echoed on the reply (bounded by `validate_request_id`, which is what
    /// makes this subject's contribution to `ADMISSION_FRAME_MAX_BYTES`
    /// computable), so a waiting requester can fail its own wait instead of
    /// timing out. `query` names WHICH query, because a consumer keeps two
    /// separate waiter maps keyed by request id and the protocol nowhere
    /// requires ids to be unique across the two.
    Query {
        request_id: String,
        query: QueryKind,
    },
    /// A frame the venue could not decode, or could not attribute at all.
    Frame,
}

/// Which venue-truth query a refused `Query` subject refers to. Mirrors a
/// consumer's two waiter maps one-for-one.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum QueryKind {
    Orders,
    Fills,
}

/// What the venue did to one requested subscription that it did not serve
/// exactly as asked. A closed set rather than prose: the client's handling is a
/// match, not a string search, and a fixed set is what keeps
/// `ADMISSION_FRAME_MAX_BYTES` provable now that issues are per entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum SubscriptionIssue {
    /// REFUSED: the venue does not list this symbol. Nothing streams.
    UnknownSymbol,
    /// REFUSED: the global replay-thread pool is exhausted. Nothing streams;
    /// running streams are untouched.
    ReplayCapacity,
    /// REFUSED: `generation` is not strictly greater than the generation the
    /// venue currently records for this symbol on this connection. The live
    /// stream is untouched - this is a client-ordering fault, and destroying a
    /// healthy replay over one would be a worse answer than refusing it.
    StaleGeneration { current: u64 },
    /// REFUSED: the positioning seek exhausted its tick budget, so the stream
    /// could not be placed. Discovered asynchronously on the replay thread,
    /// which is why this issue in particular must carry a generation.
    SeekBudgetExhausted,
    /// DEGRADED: `start_ts` preceded the tape origin; the venue clamped the
    /// request to `effective_start_ts` (the tape origin) and the stream runs
    /// from there. This value IS the position the venue used, so a client may
    /// safely adopt it as a cursor.
    StartBeforeOrigin { effective_start_ts: u64 },
    /// DEGRADED: `start_ts` exceeded sim-now; the venue discarded the requested
    /// start and the stream is live from the clock. `sim_now` is the clock
    /// reading at admission - an OBSERVATION, not a position the venue
    /// promises, because the replay seeks sim-now again on its own thread. A
    /// client must NOT adopt it as a cursor.
    StartAfterSimNow { sim_now: u64 },
    /// DEGRADED: the entry's `regime` failed `validate_market_regime` and was
    /// dropped; the clean, unhavocked tape streams.
    InvalidRegime,
    /// DEGRADED: the entry's `ReopenGap` is anchored at or before the tape
    /// origin and can never fire; it was stripped and the clean tape streams.
    ReopenGapUnfireable { at_ts: u64 },
}

impl SubscriptionIssue {
    /// `true` when nothing streams for this entry; `false` when the stream
    /// runs, altered. The one bit a client needs to decide whether to keep
    /// waiting for data.
    #[must_use]
    pub fn is_refusal(self) -> bool {
        matches!(
            self,
            Self::UnknownSymbol
                | Self::ReplayCapacity
                | Self::StaleGeneration { .. }
                | Self::SeekBudgetExhausted
        )
    }
}

/// One entry's outcome on a `SubscriptionIssues` frame. The `generation` echoes
/// the `SubscriptionRequest` this describes, which is what lets a client discard
/// a diagnostic about a generation it has already superseded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriptionOutcome {
    pub generation: u64,
    pub symbol: Symbol,
    pub issue: SubscriptionIssue,
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
    /// The venue REFUSED to do the work, before any engine state was touched:
    /// its per-connection outbound capacity could not cover the command's
    /// worst-case output, or the request could not be decoded at all.
    /// `subject` names what was refused so the refusal is translatable per
    /// command (a refused cancel is not a rejected order).
    ///
    /// Admission truth, not engine output: it classifies `EventKind::Admission`,
    /// rides the server's priority lane, and is deliberately NOT held by a
    /// `DelayAcks` window - the knob that holds engine output does not reach
    /// something the engine never produced. See `reference/havoc.md`.
    /// `reason` is server-generated and truncated to `MAX_REASON_LEN`, which
    /// with the identifier caps is what bounds this frame by
    /// `ADMISSION_FRAME_MAX_BYTES`.
    AdmissionRejected {
        subject: AdmissionSubject,
        reason: String,
        ts_event: u64,
    },
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
    /// Truthful venue book read answering a `QueryOrders` - see that variant
    /// for the honest-content / havoc-able-delivery contract.
    OrderStatusSnapshot(OrderStatusSnapshot),
    /// Truthful venue fill history answering a `QueryFills`.
    FillSnapshot(FillSnapshot),
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
    /// A whole frame the server could not decode or attribute to an entry: a
    /// frame that is not a `ClientMessage` (bad JSON, unknown `type`, or a
    /// known `type` missing required fields), a `Subscribe`/`Unsubscribe`
    /// refused whole at the `validate_subscriptions`/`validate_symbols`
    /// boundary, or a request on a carrier that does not support it. Emitted in
    /// place of a silent drop: without it, an unservable request and a
    /// healthy-but-idle feed were indistinguishable on the wire.
    ///
    /// Untargeted, and now only where untargetedness is honest: a whole-frame
    /// fault HAS no entry to name. Everything per-entry - unknown symbols, dead
    /// seeks, clamped starts, dropped regimes - moved to `SubscriptionIssues`,
    /// which echoes the `(symbol, generation)` it describes. The correlation
    /// field that was future work is that generation, and it exists.
    ///
    /// Classifies `EventKind::Admission`, not `Exec`: it reports what the
    /// venue's REQUEST HANDLING refused, which is never something the matching
    /// engine produced, so `DelayAcks` (a hold on engine output) does not reach
    /// it and it rides the server's priority lane ahead of held traffic.
    ///
    /// `reason` is server-generated prose and MUST be routed through
    /// `truncate_reason` at every construction site: serde's decode-error text
    /// echoes client-controlled field names, and without the truncation
    /// `ADMISSION_FRAME_MAX_BYTES` - hence the priority lane's frame count as a
    /// memory bound - is unproven.
    ProtocolError {
        reason: String,
        ts_event: u64,
    },
    /// Per-entry truth about a `Subscribe` the venue could not serve exactly as
    /// asked. COALESCED: one frame per `Subscribe` for every synchronously
    /// discovered issue, so a `MAX_SUBSCRIBE_SYMBOLS`-entry subscribe of unknown
    /// symbols costs one priority frame, not 256. The asynchronous
    /// `SeekBudgetExhausted` arrives later in its own single-entry frame, paid
    /// for by the replay's promise ticket.
    ///
    /// `entries` lists at most `MAX_SUBSCRIPTION_ISSUES_LISTED`, REFUSALS
    /// FIRST; `issues_total` is the true count and `refusals_total` the count
    /// of those that killed a feed, so a client can tell "some feed I asked for
    /// is dead but was truncated away" from "everything listed is what
    /// happened".
    ///
    /// One entry may produce more than one outcome (a clamped start on an entry
    /// whose regime was also dropped), so `issues_total >= entries.len()` says
    /// nothing about how many ENTRIES were affected.
    ///
    /// Classifies `EventKind::Admission`, rides the priority lane, exempt from
    /// `DelayAcks` - it reports what request handling did, never engine output.
    ///
    /// Successful subscriptions gain NO acknowledgment frame: a client's own
    /// record of the last generation it issued per symbol is the whole decision
    /// table, and a success frame would cost a priority-lane frame per subscribe
    /// on the healthy path. What would reopen that: a client needing positive
    /// confirmation during a tape arrival drought, where the first tick - the
    /// implicit success signal - can be hours of sim time away.
    SubscriptionIssues {
        entries: Vec<SubscriptionOutcome>,
        issues_total: usize,
        refusals_total: usize,
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
            // The query replies are execution-channel traffic: `DelayAcks`
            // holds them and `GoDark` drops them (delivery is havoc-able),
            // while their content stays a truthful book read - the invariant
            // documented on `ClientMessage::QueryOrders`.
            ServerMessage::AccountState(_)
            | ServerMessage::OrderStatusSnapshot(_)
            | ServerMessage::FillSnapshot(_)
            | ServerMessage::OrderAccepted { .. }
            | ServerMessage::OrderRejected { .. }
            | ServerMessage::OrderCanceled { .. }
            | ServerMessage::OrderUpdated { .. }
            | ServerMessage::OrderModifyRejected { .. }
            | ServerMessage::OrderCancelRejected { .. } => EventKind::Exec,
            ServerMessage::AdmissionRejected { .. }
            | ServerMessage::ProtocolError { .. }
            | ServerMessage::SubscriptionIssues { .. } => EventKind::Admission,
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
    pub account_id: AccountId,
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
    fn account_id_round_trips_as_bare_string() {
        let id = AccountId::parse("ACC-1").expect("valid account id");
        let json = serde_json::to_string(&id).expect("serialize account id");
        assert_eq!(json, r#""ACC-1""#);
        assert_eq!(serde_json::from_str::<AccountId>(&json).unwrap(), id);
    }

    #[test]
    fn account_id_parse_rejects_empty_overlong_and_illegal() {
        for raw in ["", &"A".repeat(MAX_ACCOUNT_ID_LEN + 1), "a/b", "a b"] {
            assert!(AccountId::parse(raw).is_err(), "{raw:?} must be refused");
        }
        assert!(AccountId::parse("WYRD-042:BTCUSDT").is_ok());
    }

    #[test]
    fn account_state_carries_its_account_id() {
        let json = r#"{"account_id":"ACC-42","balances":[],"positions":[],"ts_event":7}"#;
        let state: AccountState = serde_json::from_str(json).expect("decode snapshot");
        assert_eq!(state.account_id.as_str(), "ACC-42");
    }

    #[test]
    fn subscribe_round_trips_per_entry_generations() {
        let with_start = ClientMessage::Subscribe {
            subscriptions: vec![
                SubscriptionRequest {
                    generation: 1,
                    symbol: "X".into(),
                    start_ts: None,
                    regime: None,
                },
                SubscriptionRequest {
                    generation: 2,
                    symbol: "Y".into(),
                    start_ts: Some(123),
                    regime: None,
                },
            ],
        };
        let json = serde_json::to_string(&with_start).unwrap();
        assert_eq!(
            json,
            r#"{"type":"Subscribe","subscriptions":[{"generation":1,"symbol":"X"},{"generation":2,"symbol":"Y","start_ts":123}]}"#
        );
        let decoded: ClientMessage = serde_json::from_str(&json).unwrap();
        // `ClientMessage` is not `PartialEq`, so the round trip is proven by
        // re-serializing to the identical bytes.
        assert_eq!(serde_json::to_string(&decoded).unwrap(), json);

        // Every issue variant, both directions: the tags and payload field
        // names are wire contract, and a client's handling is a match on them.
        let issues = [
            SubscriptionIssue::UnknownSymbol,
            SubscriptionIssue::ReplayCapacity,
            SubscriptionIssue::StaleGeneration { current: 7 },
            SubscriptionIssue::SeekBudgetExhausted,
            SubscriptionIssue::StartBeforeOrigin {
                effective_start_ts: 11,
            },
            SubscriptionIssue::StartAfterSimNow { sim_now: 13 },
            SubscriptionIssue::InvalidRegime,
            SubscriptionIssue::ReopenGapUnfireable { at_ts: 17 },
        ];
        let refusals_total = issues.iter().filter(|issue| issue.is_refusal()).count();
        assert_eq!(refusals_total, 4, "four of the eight kill a feed");
        let frame = ServerMessage::SubscriptionIssues {
            entries: issues
                .iter()
                .enumerate()
                .map(|(i, issue)| SubscriptionOutcome {
                    generation: i as u64 + 1,
                    symbol: format!("SYM{i}"),
                    issue: *issue,
                })
                .collect(),
            issues_total: issues.len(),
            refusals_total,
            ts_event: 99,
        };
        let json = serde_json::to_string(&frame).unwrap();
        assert!(
            json.contains(r#"{"kind":"StartAfterSimNow","sim_now":13}"#),
            "the issue tag and its payload name are wire contract: {json}"
        );
        let decoded: ServerMessage = serde_json::from_str(&json).unwrap();
        let ServerMessage::SubscriptionIssues { entries, .. } = &decoded else {
            panic!("expected SubscriptionIssues, got {decoded:?}")
        };
        assert_eq!(
            entries.iter().map(|e| e.issue).collect::<Vec<_>>(),
            issues.to_vec(),
            "every variant survives the round trip"
        );
        assert_eq!(serde_json::to_string(&decoded).unwrap(), json);
    }

    #[test]
    fn account_state_with_positions_round_trips() {
        let state = AccountState {
            account_id: AccountId::parse("ACC-1").unwrap(),
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
                account_id: AccountId::parse("ACC-1").unwrap(),
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
    fn admission_rejected_round_trips() {
        let escaped = "\u{0001}".repeat(MAX_CLIENT_ID_LEN);
        let subjects = [
            AdmissionSubject::Submit {
                client_order_id: escaped.clone(),
            },
            AdmissionSubject::Cancel {
                client_order_id: escaped.clone(),
            },
            AdmissionSubject::Modify {
                client_order_id: escaped.clone(),
            },
            AdmissionSubject::Query {
                request_id: escaped.clone(),
                query: QueryKind::Orders,
            },
            AdmissionSubject::Query {
                request_id: escaped.clone(),
                query: QueryKind::Fills,
            },
            AdmissionSubject::Frame,
        ];
        for subject in subjects {
            let message = ServerMessage::AdmissionRejected {
                subject,
                reason: "\u{0001}".repeat(MAX_REASON_LEN),
                ts_event: u64::MAX,
            };
            let wire = serde_json::to_vec(&message).expect("serialize admission rejection");
            let decoded: ServerMessage =
                serde_json::from_slice(&wire).expect("deserialize admission rejection");
            assert_eq!(
                serde_json::to_vec(&decoded).expect("serialize decoded admission rejection"),
                wire
            );
        }
    }

    #[test]
    fn admission_frames_fit_their_ceiling() {
        let escaped_id = "\u{0001}".repeat(MAX_CLIENT_ID_LEN);
        let escaped_symbol = "\u{0001}".repeat(MAX_SYMBOL_LEN);
        let subjects = [
            AdmissionSubject::Submit {
                client_order_id: escaped_id.clone(),
            },
            AdmissionSubject::Cancel {
                client_order_id: escaped_id.clone(),
            },
            AdmissionSubject::Modify {
                client_order_id: escaped_id.clone(),
            },
            AdmissionSubject::Query {
                request_id: escaped_id.clone(),
                query: QueryKind::Orders,
            },
            AdmissionSubject::Query {
                request_id: escaped_id.clone(),
                query: QueryKind::Fills,
            },
            AdmissionSubject::Frame,
        ];
        for subject in subjects {
            let msg = ServerMessage::AdmissionRejected {
                subject,
                reason: "\u{0001}".repeat(MAX_REASON_LEN),
                ts_event: u64::MAX,
            };
            assert!(
                serde_json::to_vec(&msg)
                    .expect("serialize admission rejection")
                    .len()
                    <= ADMISSION_FRAME_MAX_BYTES,
                "{msg:?} exceeds the admission-frame ceiling"
            );
        }
        let issues = ServerMessage::SubscriptionIssues {
            entries: vec![
                SubscriptionOutcome {
                    generation: u64::MAX,
                    symbol: escaped_symbol,
                    issue: SubscriptionIssue::StartBeforeOrigin {
                        effective_start_ts: u64::MAX
                    }
                };
                MAX_SUBSCRIPTION_ISSUES_LISTED
            ],
            issues_total: MAX_SUBSCRIBE_SYMBOLS,
            refusals_total: MAX_SUBSCRIBE_SYMBOLS,
            ts_event: u64::MAX,
        };
        assert!(
            serde_json::to_vec(&issues).expect("serialize issues").len()
                <= ADMISSION_FRAME_MAX_BYTES
        );
    }

    #[test]
    fn admission_is_not_execution_for_delay_purposes() {
        for msg in [
            ServerMessage::AdmissionRejected {
                subject: AdmissionSubject::Frame,
                reason: "bad frame".into(),
                ts_event: 1,
            },
            ServerMessage::ProtocolError {
                reason: "bad frame".into(),
                ts_event: 1,
            },
        ] {
            assert_eq!(msg.category(), EventKind::Admission);
            assert!(!msg.category().is_execution());
            assert!(!msg.is_market_data());
        }
    }

    #[test]
    fn oversized_ids_do_not_echo_at_full_length() {
        let overlong = "x".repeat(8 * 1024 * 1024);
        let echoed = truncate_client_id(overlong);
        let msg = ServerMessage::OrderRejected {
            client_order_id: echoed,
            reason: truncate_reason("x".repeat(8 * 1024 * 1024)),
            ts_event: 1,
        };
        assert!(
            serde_json::to_vec(&msg)
                .expect("serialize bounded rejection")
                .len()
                <= crate::sizing::ORDER_EVENT_MAX_BYTES
        );
    }

    #[test]
    fn oversized_client_ids_are_refused_at_the_boundary() {
        let overlong = "x".repeat(MAX_CLIENT_ID_LEN + 1);
        assert_eq!(
            validate_client_order_id(&overlong),
            Err("client_order_id exceeds MAX_CLIENT_ID_LEN")
        );
    }

    #[test]
    fn query_messages_round_trip_and_default_their_filters() {
        let targeted = ClientMessage::QueryOrders {
            request_id: "Q1".into(),
            client_order_id: Some("O1".into()),
            open_only: true,
        };
        let json = serde_json::to_string(&targeted).unwrap();
        let decoded: ClientMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            decoded,
            ClientMessage::QueryOrders {
                request_id,
                client_order_id: Some(id),
                open_only: true,
            } if request_id == "Q1" && id == "O1"
        ));

        // A bare query decodes with both filters defaulted: all orders.
        let bare = r#"{"type":"QueryOrders","request_id":"Q2"}"#;
        let decoded: ClientMessage = serde_json::from_str(bare).unwrap();
        assert!(matches!(
            decoded,
            ClientMessage::QueryOrders {
                request_id,
                client_order_id: None,
                open_only: false,
            } if request_id == "Q2"
        ));

        let fills = r#"{"type":"QueryFills","request_id":"Q3"}"#;
        let decoded: ClientMessage = serde_json::from_str(fills).unwrap();
        assert!(matches!(
            decoded,
            ClientMessage::QueryFills {
                request_id,
                client_order_id: None,
            } if request_id == "Q3"
        ));
    }

    #[test]
    fn snapshot_replies_round_trip_and_classify_as_execution() {
        let snapshot = ServerMessage::OrderStatusSnapshot(OrderStatusSnapshot {
            request_id: "Q1".into(),
            orders: vec![OrderStatusInfo {
                client_order_id: "O1".into(),
                venue_order_id: "V1".into(),
                symbol: "BTCUSDT".into(),
                side: Side::Buy,
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::Gtc,
                status: WireOrderStatus::PartiallyFilled,
                quantity: Decimal::from(10),
                filled_qty: Decimal::from(3),
                price: Some(Decimal::from(100)),
                ts_accepted: 5,
                ts_last: 7,
            }],
            ts_event: 9,
        });
        let json = serde_json::to_string(&snapshot).unwrap();
        let decoded: ServerMessage = serde_json::from_str(&json).unwrap();
        let ServerMessage::OrderStatusSnapshot(decoded) = decoded else {
            panic!("expected order status snapshot")
        };
        assert_eq!(decoded.request_id, "Q1");
        assert_eq!(decoded.orders.len(), 1);
        assert_eq!(decoded.orders[0].status, WireOrderStatus::PartiallyFilled);
        assert_eq!(decoded.orders[0].filled_qty, Decimal::from(3));

        // Both replies ride the execution channel: DelayAcks holds them and
        // GoDark drops them (havoc-able delivery), and they are not market
        // data, so StallData leaves them alone.
        assert_eq!(snapshot.category(), EventKind::Exec);
        assert!(snapshot.category().is_execution());
        assert!(!snapshot.is_market_data());

        let fills = ServerMessage::FillSnapshot(FillSnapshot {
            request_id: "Q2".into(),
            fills: Vec::new(),
            ts_event: 9,
        });
        assert_eq!(fills.category(), EventKind::Exec);
        assert!(!fills.is_market_data());
    }

    #[test]
    fn wire_order_status_open_matches_variants() {
        assert!(WireOrderStatus::Accepted.is_open());
        assert!(WireOrderStatus::PartiallyFilled.is_open());
        assert!(!WireOrderStatus::Filled.is_open());
        assert!(!WireOrderStatus::Canceled.is_open());
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
