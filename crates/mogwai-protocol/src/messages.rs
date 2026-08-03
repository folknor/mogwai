// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::havoc::EventKind;
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

/// True only when a traded price is strictly through a resting limit.
///
/// The single definition of the trigger predicate. The engine applies it to
/// the acceptance-time reading and the data walk applies it to every later
/// print. A print AT the trigger is touching, not trading through. Both sides
/// of the seam use this copy so arrival and sweep decisions cannot disagree.
/// Deliberately a TRADE
/// predicate, not a quote predicate: this venue has a trades-only tape.
#[must_use]
pub fn trades_through(side: Side, limit: Decimal, traded: Decimal) -> bool {
    match side {
        Side::Buy => traded < limit,
        Side::Sell => traded > limit,
    }
}

/// True when a traded price has reached or passed a conditional order's
/// trigger. TOUCH, not through: `trades_through`'s strictness is a QUEUE
/// argument - at your own limit price you are behind the resting queue, so the
/// tape merely reaching your price is not evidence flow reached YOU - and a
/// stop holds no queue position. Its trigger is a pure price predicate the
/// venue evaluates on its own book, and every real venue fires it on touch.
///
/// Note the sides mirror `trades_through`: a buy LIMIT rests below the market
/// and waits for the tape to come DOWN, a buy STOP rests above and waits for it
/// to come UP. For the SAME side and the SAME price the two are exact logical
/// complements, which is precisely why they must not be collapsed into one
/// function with a strictness flag - they are never handed the same price
/// (a limit is scanned against its DRAWN band trigger, a conditional against
/// its STATED stop).
#[must_use]
pub fn touches_trigger(side: Side, trigger: Decimal, traded: Decimal) -> bool {
    match side {
        Side::Buy => traded >= trigger,
        Side::Sell => traded <= trigger,
    }
}

/// Which predicate a tape walk applies to one resting order. The engine
/// classifies, the data walk evaluates, and neither owns the enum - it lives
/// with the two predicate functions so the classification and the predicates
/// cannot drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanKind {
    /// `trades_through` against a live limit's drawn band trigger.
    FillThrough,
    /// `touches_trigger` against an untriggered conditional's stop price.
    TriggerTouch,
}

impl ScanKind {
    #[must_use]
    pub fn hit(self, side: Side, px: Decimal, traded: Decimal) -> bool {
        match self {
            Self::FillThrough => trades_through(side, px, traded),
            Self::TriggerTouch => touches_trigger(side, px, traded),
        }
    }
}

/// The print that satisfied a scan: both its instant and its price, because a
/// triggered stop-market prices its fill off exactly this print.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    pub ts_ns: u64,
    pub px: Decimal,
}

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

/// Worst-case expansion factor `serde_json` applies to an arbitrary string of
/// N bytes: a byte that must be escaped as `\uXXXX` costs six output bytes.
/// Every `*_MAX_BYTES` constant is stated in SERIALIZED bytes, so each embedded
/// string contributes `JSON_ESCAPE_FACTOR * cap`, never its raw cap. Sizing
/// against raw lengths - which an implementer measuring with ordinary ASCII
/// test strings would never catch - makes a reservation a typical case rather
/// than an upper bound.
pub const JSON_ESCAPE_FACTOR: usize = 6;

/// Upper bound on the serialized bytes of any `EventKind::Admission` frame -
/// `AdmissionRejected` and `ProtocolError`, since both ride the server's
/// priority lane. `AdmissionRejected` is the widest: one capped client id, one
/// capped reason, one capped symbol and its fixed envelope. This bound is
/// what makes the priority lane's FRAME count a memory bound, so every
/// `ProtocolError` construction site must route its reason through
/// `truncate_reason`.
///
/// The figure is the next power of two above `JSON_ESCAPE_FACTOR * (
/// MAX_CLIENT_ID_LEN + MAX_REASON_LEN + MAX_SYMBOL_LEN) +
/// ADMISSION_ENVELOPE_BYTES`, and `admission_frames_fit_their_ceiling` runs
/// that derivation rather than trusting this comment.
pub const ADMISSION_FRAME_MAX_BYTES: usize = 4096;

/// Fixed JSON scaffolding of an `AdmissionRejected`: the envelope, the key
/// names, the subject tag and the `ts_event` digits. Generous by design - it is
/// the constant term of an upper bound, so over-stating it can only make the
/// bound safer.
pub const ADMISSION_ENVELOPE_BYTES: usize = 256;

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
    /// Untriggered conditional carrying a trigger price and NO price: the fill
    /// comes from the print that triggered it and the reservation from the
    /// trigger, so a stamped price would be a number nothing reads.
    StopMarket,
    /// Untriggered conditional carrying both. `price` is the limit price the
    /// order takes AFTER it triggers.
    StopLimit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeInForce {
    Gtc,
    Ioc,
    Fok,
}

/// Client → server order-entry messages. Market data is streamed immediately
/// when the websocket is upgraded; there is no subscription command.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientMessage {
    SubmitOrder(SubmitOrder),
    CancelOrder {
        client_order_id: ClientOrderId,
    },
    ModifyOrder {
        client_order_id: ClientOrderId,
        price: Option<Decimal>,
        quantity: Option<Decimal>,
        /// Amending the trigger of an UNTRIGGERED conditional restarts its
        /// trigger window; on anything else it is rejected.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trigger_price: Option<Decimal>,
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

/// Which order command produced an execution frame, so the outbound path can
/// apply that command class's ack latency. `None` on the wire-diagnostic and
/// query paths, which carry no per-command latency.
///
/// Never serialized. It lives here, next to `ClientMessage` and `EventKind`, for
/// the same reason `EventKind` does: the classification of a wire type belongs
/// with the wire type, so the two ends cannot disagree about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandClass {
    Submit,
    Modify,
    Cancel,
}

impl CommandClass {
    /// The class of an order-entry command, or `None` for anything else -
    /// queries. Queries are deliberately classless: the
    /// reconciliation witness is never made the slowest thing on the venue.
    #[must_use]
    pub fn of(cmd: &ClientMessage) -> Option<Self> {
        match cmd {
            ClientMessage::SubmitOrder(_) => Some(Self::Submit),
            ClientMessage::ModifyOrder { .. } => Some(Self::Modify),
            ClientMessage::CancelOrder { .. } => Some(Self::Cancel),
            _ => None,
        }
    }
}

/// Venue-side order status as reported on an [`OrderStatusSnapshot`]. Only
/// states the venue itself can attest to: a submit that never passed the
/// accept gate leaves no record (its id is absent from the snapshot), so
/// there is no `Rejected` variant - "absent" is the truthful answer for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireOrderStatus {
    /// Accepted and resting, nothing filled yet.
    Accepted,
    /// A conditional whose trigger has fired, with nothing filled yet. A
    /// triggered order with a partial fill reports `PartiallyFilled`, because a
    /// partial fill is the more specific truth.
    Triggered,
    /// Resting with some quantity filled.
    PartiallyFilled,
    /// Terminal: fully filled.
    Filled,
    /// Terminal: canceled (client cancel, IOC remainder, or a server-side
    /// havoc cancel).
    Canceled,
    /// Terminal: refused AFTER acceptance - today only a post-only stop-limit
    /// that would take liquidity against its own triggering print. A
    /// pre-acceptance refusal never becomes a truth-store row at all.
    Rejected,
}

impl WireOrderStatus {
    #[must_use]
    pub fn is_open(self) -> bool {
        // `Triggered` is OPEN: a triggered stop-limit is resting and fillable,
        // and omitting it would make it vanish from open-order reconciliation
        // between its trigger and its fill.
        matches!(
            self,
            Self::Accepted | Self::Triggered | Self::PartiallyFilled
        )
    }
}

/// One order's venue-truth status row on an [`OrderStatusSnapshot`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderStatusInfo {
    pub client_order_id: ClientOrderId,
    pub venue_order_id: VenueOrderId,
    pub symbol: Symbol,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_id: Option<String>,
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
    /// The conditional's stop price, `None` for a non-conditional order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_price: Option<Decimal>,
    /// Sim unix-ns the trigger fired, `None` while untriggered or for a
    /// non-conditional order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ts_triggered: Option<u64>,
    #[serde(default)]
    pub reduce_only: bool,
    #[serde(default)]
    pub post_only: bool,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_id: Option<String>,
    pub side: Side,
    pub order_type: OrderType,
    pub quantity: Decimal,
    pub price: Option<Decimal>,
    /// The price the tape must touch for a conditional to become live.
    /// REQUIRED on StopMarket/StopLimit, refused on Market/Limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_price: Option<Decimal>,
    pub time_in_force: TimeInForce,
    /// Fills are clamped to the position this order would close, and the order
    /// is canceled rather than filled when that position is gone. Exempt from
    /// the funded-admission check and from `locked_balances`: it can only
    /// shrink an exposure the position itself already represents.
    #[serde(default)]
    pub reduce_only: bool,
    /// An order that would take liquidity is rejected rather than filled.
    /// Legal on Limit and StopLimit only.
    #[serde(default)]
    pub post_only: bool,
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
    if order
        .position_id
        .as_ref()
        .is_some_and(|id| id.len() > MAX_CLIENT_ID_LEN)
    {
        return Err("position_id exceeds MAX_CLIENT_ID_LEN");
    }
    if order.quantity <= Decimal::ZERO {
        return Err("quantity must be > 0");
    }
    if order.price.is_some_and(|price| price <= Decimal::ZERO) {
        return Err("price must be > 0");
    }
    if order
        .trigger_price
        .is_some_and(|price| price <= Decimal::ZERO)
    {
        return Err("trigger_price must be > 0");
    }
    match order.order_type {
        OrderType::Market if order.trigger_price.is_some() => {
            Err("Market order must not carry trigger_price")
        }
        OrderType::Limit if order.price.is_none() => Err("Limit order must carry a price"),
        OrderType::Limit if order.trigger_price.is_some() => {
            Err("Limit order must not carry trigger_price")
        }
        OrderType::StopMarket if order.price.is_some() => {
            Err("StopMarket order must not carry a price")
        }
        OrderType::StopMarket | OrderType::StopLimit if order.trigger_price.is_none() => {
            Err("conditional order must carry trigger_price")
        }
        OrderType::StopLimit if order.price.is_none() => Err("StopLimit order must carry a price"),
        _ if order.post_only
            && !matches!(order.order_type, OrderType::Limit | OrderType::StopLimit) =>
        {
            Err("post_only is legal only on Limit and StopLimit")
        }
        _ if matches!(
            order.order_type,
            OrderType::StopMarket | OrderType::StopLimit
        ) && order.time_in_force != TimeInForce::Gtc =>
        {
            Err(
                "conditional orders are good-till-cancel only: a now-or-never order cannot wait for a trigger",
            )
        }
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

/// API-boundary guard for a `ClientMessage::ModifyOrder`'s `price`/`quantity`
/// pair, mirroring `validate_submit_order` in style. At least one of the two
/// must be present - both absent decodes as a no-op amend that changes
/// nothing - and whichever is present must be strictly positive.
pub fn validate_modify_order(
    price: Option<Decimal>,
    quantity: Option<Decimal>,
    trigger_price: Option<Decimal>,
) -> Result<(), &'static str> {
    if price.is_none() && quantity.is_none() && trigger_price.is_none() {
        return Err("ModifyOrder must set price, quantity and/or trigger_price");
    }
    if price.is_some_and(|p| p <= Decimal::ZERO) {
        return Err("price must be > 0");
    }
    if quantity.is_some_and(|q| q <= Decimal::ZERO) {
        return Err("quantity must be > 0");
    }
    if trigger_price.is_some_and(|p| p <= Decimal::ZERO) {
        return Err("trigger_price must be > 0");
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
    /// The declared simulated run duration elapsed. This is sent immediately
    /// before the venue closes normally, making a planned exit distinguishable
    /// from a failed connection.
    RunComplete {
        sim_now_ns: u64,
        elapsed_ns: u64,
    },
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
    /// A conditional order's trigger fired. Always precedes whatever the
    /// trigger produced (a fill, or the order resting as a live limit), in the
    /// same batch. Never duplicated by `DuplicateNextFill`: it is not a fill,
    /// and a duplicated trigger would be a transition the client's FSM has no
    /// arm for.
    OrderTriggered {
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
        /// New trigger price after the amend. `None` for a non-conditional
        /// order, and for an amend that did not touch the trigger.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trigger_price: Option<Decimal>,
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
    /// The bounded tape fanout overwrote frames for this connection. This is a
    /// venue fault, not a client refusal; the server closes with WS 1011 after
    /// delivering it.
    FeedLagged {
        skipped: u64,
        sim_now_ns: u64,
    },
    /// A non-fatal run-level havoc observation. It replaces the old
    /// subscription-attributed diagnostic because a run has one tape.
    HavocDiagnostic {
        reason: String,
        sim_now_ns: u64,
    },
    /// A whole frame the server could not decode or attribute: a
    /// frame that is not a `ClientMessage` (bad JSON, unknown `type`, or a
    /// known `type` missing required fields), or a request on a carrier that
    /// does not support it. Emitted in
    /// place of a silent drop: without it, an unservable request and a
    /// healthy-but-idle feed were indistinguishable on the wire.
    ///
    /// Untargeted, and now only where untargetedness is honest: a whole-frame
    /// fault has no target to name.
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
            | ServerMessage::OrderTriggered { .. }
            | ServerMessage::OrderRejected { .. }
            | ServerMessage::OrderCanceled { .. }
            | ServerMessage::OrderUpdated { .. }
            | ServerMessage::OrderModifyRejected { .. }
            | ServerMessage::OrderCancelRejected { .. } => EventKind::Exec,
            ServerMessage::AdmissionRejected { .. }
            | ServerMessage::ProtocolError { .. }
            | ServerMessage::FeedLagged { .. }
            | ServerMessage::HavocDiagnostic { .. }
            | ServerMessage::RunComplete { .. } => EventKind::Admission,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_id: Option<String>,
    pub side: Side,
    pub last_qty: Decimal,
    pub last_px: Decimal,
    /// Remaining quantity. `> 0` ⇒ this is a partial fill.
    pub leaves_qty: Decimal,
    pub commission: Decimal,
    pub commission_currency: String,
    pub liquidity_side: LiquiditySide,
    pub ts_event: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiquiditySide {
    Maker,
    #[default]
    Taker,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountState {
    pub account_id: AccountId,
    pub balances: Vec<Balance>,
    pub positions: Vec<Position>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub margins: Vec<PostedMargin>,
    pub ts_event: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostedMargin {
    pub symbol: Symbol,
    pub currency: String,
    pub initial: Decimal,
    pub maintenance: Decimal,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_id: Option<String>,
    /// Signed net quantity: positive is long, negative is short, zero is flat.
    pub quantity: Decimal,
    /// Volume-weighted average entry price of the open quantity. Zero when flat.
    pub avg_px: Decimal,
    #[serde(default, skip_serializing_if = "Decimal::is_zero")]
    pub mark_px: Decimal,
    #[serde(default, skip_serializing_if = "Decimal::is_zero")]
    pub unrealized_pnl: Decimal,
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

    /// The post-subscription-retirement wire surface, pinned by BYTE form.
    ///
    /// `Subscribe`, `Unsubscribe` and the nine `SubscriptionIssue` variants are
    /// gone; the two that carried surviving meaning became top-level frames.
    /// Pinning the text rather than only `from(to(x)) == x` is what stops a
    /// field rename from passing here and failing in the launcher or the
    /// adapter instead.
    #[test]
    fn client_and_server_messages_round_trip() {
        let client_frames = [
            (
                ClientMessage::CancelOrder {
                    client_order_id: "O-1".into(),
                },
                r#"{"type":"CancelOrder","client_order_id":"O-1"}"#,
            ),
            (
                ClientMessage::ModifyOrder {
                    client_order_id: "O-1".into(),
                    price: None,
                    quantity: Some(Decimal::from(2)),
                    trigger_price: None,
                },
                r#"{"type":"ModifyOrder","client_order_id":"O-1","price":null,"quantity":"2"}"#,
            ),
            (
                ClientMessage::QueryOrders {
                    request_id: "Q-1".into(),
                    client_order_id: None,
                    open_only: true,
                },
                r#"{"type":"QueryOrders","request_id":"Q-1","open_only":true}"#,
            ),
            (
                ClientMessage::QueryFills {
                    request_id: "Q-2".into(),
                    client_order_id: None,
                },
                r#"{"type":"QueryFills","request_id":"Q-2"}"#,
            ),
        ];
        for (frame, expected) in client_frames {
            let json = serde_json::to_string(&frame).expect("serialize");
            assert_eq!(json, expected);
            let decoded: ClientMessage = serde_json::from_str(&json).expect("decode");
            assert_eq!(serde_json::to_string(&decoded).expect("re-serialize"), json);
        }

        // There is no Subscribe frame left to send: the venue pushes the run's
        // one tape unbidden, so a client that still sends one is refused by the
        // decoder rather than silently ignored.
        assert!(
            serde_json::from_str::<ClientMessage>(r#"{"type":"Subscribe","symbols":["BTCUSDT"]}"#)
                .is_err(),
            "Subscribe was retired with the subscription model"
        );
        assert!(
            serde_json::from_str::<ClientMessage>(r#"{"type":"Unsubscribe"}"#).is_err(),
            "Unsubscribe was retired with the subscription model"
        );

        let server_frames = [
            (
                ServerMessage::RunComplete {
                    sim_now_ns: 123,
                    elapsed_ns: 45,
                },
                r#"{"type":"RunComplete","sim_now_ns":123,"elapsed_ns":45}"#,
            ),
            (
                // Formerly SubscriptionIssue::FeedLagged. There is no
                // subscription to attribute it to, so it is a top-level frame.
                ServerMessage::FeedLagged {
                    skipped: 7,
                    sim_now_ns: 8,
                },
                r#"{"type":"FeedLagged","skipped":7,"sim_now_ns":8}"#,
            ),
            (
                // Formerly SubscriptionIssue::ReopenGapUnfireable.
                ServerMessage::HavocDiagnostic {
                    reason: "reopen gap at or before the tape origin".into(),
                    sim_now_ns: 9,
                },
                r#"{"type":"HavocDiagnostic","reason":"reopen gap at or before the tape origin","sim_now_ns":9}"#,
            ),
            (
                ServerMessage::Heartbeat { ts_event: 1 },
                r#"{"type":"Heartbeat","ts_event":1}"#,
            ),
            (
                ServerMessage::ProtocolError {
                    reason: "invalid client frame".into(),
                    ts_event: 2,
                },
                r#"{"type":"ProtocolError","reason":"invalid client frame","ts_event":2}"#,
            ),
        ];
        for (frame, expected) in server_frames {
            let json = serde_json::to_string(&frame).expect("serialize");
            assert_eq!(json, expected);
            let decoded: ServerMessage = serde_json::from_str(&json).expect("decode");
            assert_eq!(serde_json::to_string(&decoded).expect("re-serialize"), json);
        }
    }

    /// `ADMISSION_FRAME_MAX_BYTES` is what makes the priority lane's FRAME
    /// count a memory bound, so it must be PROVEN rather than asserted.
    ///
    /// The old bound was 8192, sized by a list of `MAX_SUBSCRIPTION_ISSUES_LISTED`
    /// rows. With `SubscriptionIssues` retired the widest admission frame is a
    /// single `AdmissionRejected` - one capped client id, one capped reason,
    /// one capped symbol, plus a fixed envelope - so the bound was recomputed
    /// from those caps and rounded up to the next power of two. This test is
    /// the recomputation, run.
    #[test]
    fn admission_frames_fit_their_ceiling() {
        // The worst case is every capped field at its cap, in characters that
        // JSON escapes maximally - which is what JSON_ESCAPE_FACTOR prices.
        let worst_id = "\u{7}".repeat(MAX_CLIENT_ID_LEN);
        let worst_reason = "\u{7}".repeat(MAX_REASON_LEN);

        let widest = ServerMessage::AdmissionRejected {
            subject: AdmissionSubject::Submit {
                client_order_id: worst_id.clone(),
            },
            reason: worst_reason.clone(),
            ts_event: u64::MAX,
        };
        let widest_len = serde_json::to_string(&widest).expect("serialize").len();

        let error = ServerMessage::ProtocolError {
            reason: worst_reason,
            ts_event: u64::MAX,
        };
        let error_len = serde_json::to_string(&error).expect("serialize").len();

        assert!(
            widest_len >= error_len,
            "AdmissionRejected is the widest admission frame: {widest_len} vs {error_len}"
        );
        assert!(
            widest_len <= ADMISSION_FRAME_MAX_BYTES,
            "the widest admission frame is {widest_len} bytes, over the {ADMISSION_FRAME_MAX_BYTES} ceiling"
        );

        // The analytic bound the constant is derived FROM, so the constant is
        // not merely large enough for the case above by luck.
        let analytic = JSON_ESCAPE_FACTOR * (MAX_CLIENT_ID_LEN + MAX_REASON_LEN + MAX_SYMBOL_LEN)
            + ADMISSION_ENVELOPE_BYTES;
        assert!(
            analytic <= ADMISSION_FRAME_MAX_BYTES,
            "the analytic worst case is {analytic} bytes, over the {ADMISSION_FRAME_MAX_BYTES} ceiling"
        );
        assert!(
            ADMISSION_FRAME_MAX_BYTES < 2 * analytic,
            "the ceiling is the next power of two above the analytic bound, not an \
             arbitrarily large number that proves nothing"
        );
    }
}
