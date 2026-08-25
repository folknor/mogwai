// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::havoc::EventKind;
use crate::{ClientOrderId, Symbol, VenueOrderId};

/// Maximum byte length of any consumer-supplied identifier the venue echoes back
/// into its own output: `client_order_id`, `request_id`. The cap exists so a
/// produced frame has a computable upper bound - the admission reservation in
/// `mogwai-venue` sizes worst-case output against it, and an unbounded id
/// would make that bound unprovable (and let one 8 MiB order id exhaust a
/// connection's whole execution budget).
pub const MAX_ECHOED_ID_LEN: usize = 64;
/// Maximum byte length of the account identity carried by the transport.
pub const MAX_ACCOUNT_ID_LEN: usize = 64;
/// Maximum websocket frame and reassembled message accepted from a consumer.
/// Legal command frames are only a few hundred bytes; this leaves ample room
/// while preventing dependency defaults from setting the venue's memory bound.
pub const MAX_INBOUND_MESSAGE_BYTES: usize = 64 * 1024;

/// True only when a traded price is strictly through a resting limit.
///
/// The single definition of the trigger predicate. The engine applies it to
/// the acceptance-time reading and the data walk applies it to every later
/// print. A print exactly at the trigger is touching, not trading through.
/// Both sides of the seam use this copy so arrival and sweep decisions cannot
/// disagree. Deliberately a trade
/// predicate, not a quote predicate: this venue has a trades-only tape.
#[must_use]
pub fn trades_through(side: Side, limit: Decimal, traded: Decimal) -> bool {
    match side {
        Side::Buy => traded < limit,
        Side::Sell => traded > limit,
    }
}

/// True when a traded price has reached or passed a conditional order's
/// trigger. Touch, not through: `trades_through`'s strictness is a queue
/// argument - at your own limit price you are behind the resting queue, so the
/// tape merely reaching your price is not evidence flow reached you - and a
/// stop holds no queue position. Its trigger is a pure price predicate the
/// venue evaluates on its own book, and every real venue fires it on touch.
///
/// Note the sides mirror `trades_through`: a buy limit rests below the market
/// and waits for the tape to come down, a buy stop rests above and waits for it
/// to come up. For the same side and the same price the two are exact logical
/// complements, which is precisely why they must not be collapsed into one
/// function with a strictness flag - they are never handed the same price
/// (a limit is scanned against its drawn band trigger, a conditional against
/// its stated stop).
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
    /// `touches_trigger` against an untriggered stop's trigger price. The stop
    /// family, and only that.
    StopTrigger,
    /// `touches_toward` against an untriggered touched order's trigger price.
    /// The touched-order family, and only that: market-if-touched and
    /// limit-if-touched.
    ///
    /// The complement of `StopTrigger` in direction and its twin in strictness.
    /// A stop protects - buy above the market, sell below - so it fires when
    /// price runs away from where you are. A touched order enters - buy below,
    /// sell above - so it fires when price comes toward its level. Same
    /// machinery, opposite comparison; collapsing them into one predicate with a
    /// flag would put the two most easily confused behaviours in the venue
    /// behind one boolean.
    TouchedTrigger,
}

impl ScanKind {
    #[must_use]
    pub fn hit(self, side: Side, px: Decimal, traded: Decimal) -> bool {
        match self {
            Self::FillThrough => trades_through(side, px, traded),
            Self::StopTrigger => touches_trigger(side, px, traded),
            Self::TouchedTrigger => touches_toward(side, px, traded),
        }
    }
}

/// True when a traded price has reached a touched order's trigger from the
/// entry side: a buy waits for the tape to come down to it, a sell for it to
/// come up.
///
/// Touch rather than through, exactly like `touches_trigger`, and for the same
/// reason: a conditional holds no queue position, so its trigger is a pure price
/// predicate the venue evaluates on its own book.
#[must_use]
pub fn touches_toward(side: Side, trigger: Decimal, traded: Decimal) -> bool {
    match side {
        Side::Buy => traded <= trigger,
        Side::Sell => traded >= trigger,
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
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct AccountId(String);

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

impl<'de> Deserialize<'de> for AccountId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

/// Maximum byte length of a symbol on the wire, same reasoning as
/// `MAX_ECHOED_ID_LEN`.
pub const MAX_SYMBOL_LEN: usize = 32;

/// Validate an inbound symbol, wherever one arrives.
///
/// The alphabet was chosen for the URL ingresses - it needs no percent encoding,
/// and is shared by the adapter constructing the URL and the venue validating
/// its decoded value - but the rule is no longer scoped to them. As of
/// 2026-08-19 [`validate_submit_order`] calls this too, so order entry and the
/// `/trades`, `/quotes` and `source` query strings judge a symbol by one rule.
/// That is what makes "a symbol is 1 to 32 bytes of the URL-safe alphabet" a
/// sentence about the venue rather than about two of its three doors.
///
/// Relaxing this for a URL reason would therefore relax order entry with it.
/// The callers are `validate_submit_order` (and `validate_submit_group` through
/// it), `mogwai-venue`'s `http.rs` and `source.rs`, `config.rs` for an
/// instrument's `index_symbol`, and `mogwai-adapter`'s config check. `config.rs`
/// does not apply it to an instrument's own `symbol`, which is a recorded
/// asymmetry
/// rather than an oversight.
pub fn validate_wire_symbol(symbol: &str) -> Result<(), &'static str> {
    if symbol.is_empty() || symbol.len() > MAX_SYMBOL_LEN {
        // Bytes, not characters: the check is `symbol.len()`. The two agree
        // only because the alphabet arm below admits ASCII alone, and saying
        // characters would be a lie the moment that arm relaxed.
        return Err("symbols are 1 to 32 bytes");
    }
    if !symbol
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err("symbols use only ASCII letters, digits, dot, dash or underscore");
    }
    Ok(())
}

/// Maximum byte length of a callsign on the wire. Same reasoning as
/// `MAX_SYMBOL_LEN`, and generous enough for a uuid-shaped value.
pub const MAX_CALLSIGN_LEN: usize = 64;

/// Validate the `/ws?callsign=` socket identity, which shares the URL alphabet
/// with a wire symbol and is bounded for the same reason: it is carried in a
/// query string with no percent encoding, and it is retained per socket for the
/// life of the connection.
///
/// Empty is refused rather than treated as absent. A consumer that sends
/// `callsign=` has said something, and reading an empty string as "no opinion"
/// would silently give it the always-evict behaviour it was trying to leave.
pub fn validate_callsign(callsign: &str) -> Result<(), &'static str> {
    if callsign.is_empty() || callsign.len() > MAX_CALLSIGN_LEN {
        return Err("callsigns are 1 to 64 bytes");
    }
    if !callsign
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err("callsigns use only ASCII letters, digits, dot, dash or underscore");
    }
    Ok(())
}

/// Maximum byte length of a venue-generated `reason` string. Constructors
/// truncate to this on a char boundary rather than rejecting: a reason is
/// diagnostic prose, and a truncated diagnostic is still truthful about what
/// happened, whereas a refused frame would not be.
pub const MAX_REASON_LEN: usize = 512;

/// The refusal a `post_only` order on the wrong type earns, at the wire gate
/// and in the engine's own validator alike. One string, because the two gates
/// carried two copies of it and a consumer cannot tell which of them spoke.
///
/// It names both sets - the legal types and then the refused ones - rather than
/// stating a rule the consumer has to apply. The rule it used to state, "legal
/// only on orders that rest as a limit", is false for `MarketToLimit`, which
/// rests its remainder as a limit and is refused anyway. The legal half is what
/// `mogwai-engine`'s admission-table test parses, up to the first " orders", so
/// the second half may grow or change freely.
pub const POST_ONLY_REFUSAL: &str = "post_only is legal only on Limit, StopLimit, LimitIfTouched and TrailingStopLimit orders; not on Market, StopMarket, TrailingStopMarket, MarketIfTouched or MarketToLimit";

/// Maximum byte length of a currency code, an instrument base or an instrument
/// quote as configured. Operator-supplied config strings reach the wire through
/// `AccountState`'s balance rows and every position row, so
/// `sizing::BALANCE_ROW_MAX_BYTES` / `sizing::POSITION_ROW_MAX_BYTES` are only
/// upper bounds if these are capped too. Enforced where the config is loaded
/// (`mogwai-venue/src/config.rs`), which fails startup rather than a
/// connection.
pub const MAX_CURRENCY_LEN: usize = 16;

/// Validate a currency code wherever one is configured.
///
/// A currency code is a lookup key and nothing else: an account's equity is
/// summed over the balances carrying exactly this string, and a margin or
/// commission row is denominated by matching it. So a code that no balance can
/// ever equal is not a cosmetic defect - it freezes a policed account's equity
/// at zero, or denominates a charge in a currency the account does not hold,
/// and it does so silently because every layer downstream is doing an honest
/// comparison against a string that simply never matches.
///
/// Blank is refused for that reason, and so is a padded code: `" USD "` reads
/// to a human as USD and matches no balance funded as `USD`. Refusing anything
/// differing from its own trimmed form is deliberately stricter than
/// normalizing it would be. Normalizing would make the config file and the
/// venue's behaviour disagree about what the operator wrote, and the whole
/// point of failing at load is that the operator learns before a run rather
/// than from an equity figure hours later.
///
/// The length cap is what makes `sizing::BALANCE_ROW_MAX_BYTES` and its kin
/// upper bounds rather than typical cases; see [`MAX_CURRENCY_LEN`].
pub fn validate_currency_code(code: &str) -> Result<(), String> {
    if code.is_empty() {
        return Err("currency code must not be blank".to_owned());
    }
    if code != code.trim() {
        return Err(format!(
            "currency code {code:?} carries leading or trailing whitespace, so it would match no \
             balance"
        ));
    }
    if code.len() > MAX_CURRENCY_LEN {
        return Err(format!(
            "currency code {code:?} exceeds MAX_CURRENCY_LEN ({MAX_CURRENCY_LEN} bytes)"
        ));
    }
    Ok(())
}

/// Worst-case expansion factor `serde_json` applies to an arbitrary string of
/// N bytes: a byte that must be escaped as `\uXXXX` costs six output bytes.
/// Every `*_MAX_BYTES` constant is stated in serialized bytes, so each embedded
/// string contributes `JSON_ESCAPE_FACTOR * cap`, never its raw cap. Sizing
/// against raw lengths - which an implementer measuring with ordinary ASCII
/// test strings would never catch - makes a reservation a typical case rather
/// than an upper bound.
pub const JSON_ESCAPE_FACTOR: usize = 6;

/// Upper bound on the serialized bytes of any `EventKind::Admission` frame -
/// `AdmissionRejected` and `ProtocolError`, since both ride the venue's
/// priority lane. `AdmissionRejected` is the widest: one capped client id, one
/// capped reason and its fixed envelope. This bound is
/// what makes the priority lane's frame count a memory bound, so every
/// `ProtocolError` construction site must route its reason through
/// `truncate_reason`.
///
/// The figure is the next power of two above `JSON_ESCAPE_FACTOR *
/// (MAX_ECHOED_ID_LEN + MAX_REASON_LEN) + ADMISSION_ENVELOPE_BYTES` - 3712,
/// so 4096 - and `admission_frames_fit_their_ceiling` runs that derivation
/// rather than trusting this comment. No symbol term: neither admission frame
/// carries a symbol. `AdmissionSubject` names the refused command by ID, never
/// by instrument, and every one of its id-shaped fields is truncated to
/// `MAX_ECHOED_ID_LEN` by the hand-written `Serialize` below, so one capped id
/// is the only unbounded subject contribution. The variants are not otherwise
/// identical and this bound does not need them to be: they differ in key name,
/// in the width of the `kind` tag, and - for `Query` alone - by a serialized
/// `QueryKind` value. Those deltas are fixed scaffolding, charged to
/// `ADMISSION_ENVELOPE_BYTES`, which is why that constant is deliberately
/// generous. `admission_frames_fit_their_ceiling` measures EVERY variant, so
/// "the widest" is a measurement rather than a claim.
pub const ADMISSION_FRAME_MAX_BYTES: usize = 4096;

/// Fixed JSON scaffolding of an `AdmissionRejected`: the envelope, the key
/// names, the subject tag, the `ts_event` digits, and any fixed-alphabet field
/// value a subject variant adds beyond its capped id - today that is `Query`'s
/// `QueryKind`, whose two spellings are `Orders` and `Fills`. Generous by
/// design - it is the constant term of an upper bound, so over-stating it can
/// only make the bound safer.
pub const ADMISSION_ENVELOPE_BYTES: usize = 256;

/// Truncate a venue-generated reason to `MAX_REASON_LEN` bytes on a char
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

/// Truncate a consumer-supplied identifier to `MAX_ECHOED_ID_LEN` bytes on a
/// char boundary, for echoing back in a refusal. An over-length id is never
/// accepted, so a truncated echo cannot be mistaken for a live correlation: a
/// consumer matching on it finds no order, which is the truth. Echoing the id at
/// full length would recreate the unbounded frame the cap exists to prevent.
#[must_use]
pub fn truncate_echoed_id(mut id: String) -> String {
    if id.len() <= MAX_ECHOED_ID_LEN {
        return id;
    }
    let mut end = MAX_ECHOED_ID_LEN;
    while !id.is_char_boundary(end) {
        end -= 1;
    }
    id.truncate(end);
    id
}

/// Boundary guard for a client order id: over-length is a malformed request,
/// refused with the existing rejection mechanism, never with
/// `AdmissionRejected` (which reads as a capacity signal).
pub fn validate_client_order_id(id: &ClientOrderId) -> Result<(), &'static str> {
    (id.len() <= MAX_ECHOED_ID_LEN)
        .then_some(())
        .ok_or("client_order_id exceeds MAX_ECHOED_ID_LEN")
}

/// Boundary guard for a venue-truth query's `request_id`, which the venue
/// echoes on its reply and on a refusal.
pub fn validate_request_id(id: &str) -> Result<(), &'static str> {
    (id.len() <= MAX_ECHOED_ID_LEN)
        .then_some(())
        .ok_or("request_id exceeds MAX_ECHOED_ID_LEN")
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

/// The order types this venue serves.
///
/// The surface is complete by definition rather than sized against any one
/// consumer's catalog. An exchange serves stocks, futures, forex and crypto,
/// and the order types, time-in-force values and account types that go with
/// each, so this enum is finished when that set is covered and not when the
/// current caller stops asking.
///
/// Completeness is load-bearing rather than tidy. For any shape this venue does
/// not model there is no forward test anywhere: a bar-close backtest
/// structurally cannot see resting-order timing, conditional fills, partial
/// fills or latency. A curated subset does not make those strategy shapes
/// inconvenient to test, it makes them untestable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderType {
    Market,
    Limit,
    /// Untriggered conditional carrying a trigger price and no price: the fill
    /// comes from the print that triggered it and the reservation from the
    /// trigger, so a stamped price would be a number nothing reads.
    StopMarket,
    /// Untriggered conditional carrying both. `price` is the limit price the
    /// order takes after it triggers.
    StopLimit,
    /// A stop whose trigger ratchets with the tape and fires on touch like any
    /// other stop.
    ///
    /// The trigger follows the extreme the tape has reached since the order
    /// rested, held `trail_offset` away from it, and never retreats: a sell
    /// stop rises with the high and stays put when price falls back. That
    /// one-way movement is what makes it a trailing stop rather than a stop
    /// somebody keeps amending.
    ///
    /// It exists because a Pine `strategy.exit` with a native trailing leg
    /// compiles into exactly this, and refusing it halted the run - so an entire
    /// dealt exit family was untestable anywhere, forward being the only place
    /// resting-order timing can be validated at all. Re-placing it as a fixed
    /// stop each bar is a real workaround for the doctrines whose trail level is
    /// an indicator value, and covers neither `trail_points` nor `trail_offset`.
    TrailingStopMarket,
    /// The mirror of a stop: fires when the tape touches `trigger_price` coming
    /// from the other side, then takes liquidity.
    ///
    /// A stop protects a position (sell below, buy above); a touched order
    /// enters on strength or weakness (buy below, sell above). The trigger
    /// arithmetic differs only in which direction closes the gap, which is why
    /// the two share a state machine.
    MarketIfTouched,
    /// `MarketIfTouched` that rests as a limit at `price` once touched, exactly
    /// as `StopLimit` is to `StopMarket`.
    LimitIfTouched,
    /// A market order that rests as a limit at the price it could not fill at,
    /// rather than sweeping through the book.
    ///
    /// The venue's fill model has no book to sweep, so what this expresses here
    /// is "take what is available at the touch and rest the remainder" - which
    /// is the behaviour a real market-to-limit gives and which an IOC market
    /// cannot, since IOC cancels its remainder instead of resting it.
    ///
    /// That argument is about why the type exists, not a refusal of `Ioc` on
    /// it, and the wire admits the combination deliberately. The precedence,
    /// which the crate previously left unstated: the time in force governs the
    /// remainder. Where a remainder exists, `Fok` rejects the order before
    /// acceptance, `Ioc` cancels the remainder, and `Gtc`/`Day`/`Gtd` keep it.
    /// Pinned by `mogwai-engine`'s test
    /// `a_market_to_limit_remainder_is_governed_by_its_time_in_force`.
    ///
    /// The engine implements both halves of that model: the executing part
    /// takes the market's drawn price, bounded by the stated limit rather than
    /// filled at it, and a kept remainder rests as a scannable limit at that
    /// price. Both halves were broken until 2026-08-19 - the whole quantity
    /// filled at the order's own limit with no reference to the tape, and a
    /// divergence-manufactured remainder rested inert - and this comment
    /// recorded the defect; it is closed, and the model above is the behaviour.
    MarketToLimit,
    /// A trailing stop that rests as a limit once it fires, rather than taking
    /// liquidity - `TrailingStopMarket` is to this what `StopMarket` is to
    /// `StopLimit`.
    ///
    /// It carries two distances. `trail_offset` holds the trigger away from the
    /// extreme the tape has reached, exactly as on `TrailingStopMarket`.
    /// `limit_offset` holds the limit away from that trigger, on the fillable
    /// side of it: a sell rests at `trigger - limit_offset`, a buy at
    /// `trigger + limit_offset`. The limit is derived and re-derived on every
    /// ratchet, so it follows the trigger rather than drifting away from it.
    ///
    /// What it buys over `TrailingStopMarket`: a bound on the price the exit
    /// accepts. A trailing stop that takes liquidity fills at whatever the
    /// triggering print slipped to, which is the shape a consumer uses when
    /// certainty of exit beats price; this one refuses to fill through its
    /// limit, and rests instead. That is a real strategy choice and the venue
    /// does not make it for anyone.
    TrailingStopLimit,
}

impl OrderType {
    /// Whether this type waits for the tape to reach a trigger before it can
    /// fill or rest.
    #[must_use]
    pub const fn is_conditional(self) -> bool {
        matches!(
            self,
            Self::StopMarket
                | Self::StopLimit
                | Self::TrailingStopMarket
                | Self::TrailingStopLimit
                | Self::MarketIfTouched
                | Self::LimitIfTouched
        )
    }

    /// Whether the order becomes a limit once its trigger fires, rather than
    /// taking liquidity.
    #[must_use]
    pub const fn rests_after_trigger(self) -> bool {
        matches!(
            self,
            Self::StopLimit | Self::LimitIfTouched | Self::TrailingStopLimit
        )
    }

    /// Whether `post_only` means anything on this type - the one rule, read by
    /// the wire gate and by the engine's own validator, which used to spell it
    /// twice and disagree about `TrailingStopLimit`.
    ///
    /// `post_only` says "reject rather than take liquidity", so it is legal
    /// exactly where the order's whole purpose is to rest: a `Limit`, and the
    /// three types that become a limit once their trigger fires. It is refused
    /// on `MarketToLimit` even though that type does rest a remainder as a
    /// limit, and the exclusion is deliberate rather than an oversight: its
    /// first act is to take what the touch offers, which is the thing
    /// `post_only` forbids, so the two together ask for an order that must not
    /// do the one thing the type exists to do.
    #[must_use]
    pub const fn may_be_post_only(self) -> bool {
        matches!(self, Self::Limit) || self.rests_after_trigger()
    }

    /// Whether the trigger fires when the tape reaches it from below for a buy
    /// and from above for a sell - the touched family - rather than the stop
    /// family's opposite convention.
    ///
    /// A stop buy triggers when price rises to it and a stop sell when price
    /// falls to it: that is protection. A touched buy triggers when price falls
    /// to it and a touched sell when price rises to it: that is entry. Same
    /// machinery, opposite comparison, and getting it backwards turns every
    /// protective order into an entry.
    #[must_use]
    pub const fn triggers_toward(self) -> bool {
        matches!(self, Self::MarketIfTouched | Self::LimitIfTouched)
    }

    /// Whether the trigger moves with the tape instead of staying where it was
    /// placed.
    #[must_use]
    pub const fn trails(self) -> bool {
        matches!(self, Self::TrailingStopMarket | Self::TrailingStopLimit)
    }

    /// The limit price a trailing stop limit rests at, given where its trigger
    /// now sits. One implementation, called at acceptance and again on every
    /// ratchet, so the two can never disagree about which side of the trigger
    /// the limit belongs on.
    ///
    /// The limit sits on the fillable side: a sell triggers as price falls, so
    /// resting below the trigger is what makes it reachable, and the offset is
    /// the slippage the consumer will accept before it would rather not trade.
    /// Putting it on the other side would rest a limit the tape has already
    /// passed through.
    #[must_use]
    pub fn trailing_limit_px(
        side: Side,
        trigger: Decimal,
        limit_offset: Decimal,
    ) -> Option<Decimal> {
        match side {
            Side::Sell => trigger.checked_sub(limit_offset),
            Side::Buy => trigger.checked_add(limit_offset),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeInForce {
    Gtc,
    Ioc,
    Fok,
    /// Rests until the end of the trading day, then expires.
    ///
    /// Not optional trivia: this is the default on equity venues, so a surface
    /// offering only Gtc, Ioc and Fok is not an equity surface. The day boundary
    /// comes from the instrument's calendar - a session close is a real instant
    /// the venue already knows - rather than from a wall clock.
    Day,
    /// Rests until `expire_time`, then expires. Common enough across all four
    /// asset classes to belong beside `Day`.
    Gtd,
}

impl TimeInForce {
    /// Whether this order expires on its own rather than resting until it is
    /// cancelled or filled.
    #[must_use]
    pub const fn expires(self) -> bool {
        matches!(self, Self::Day | Self::Gtd)
    }
}

/// Consumer → venue order-entry messages. Market data is streamed immediately
/// when the websocket is upgraded; there is no subscription command.
///
/// Named for what it carries rather than for who sent it, which is why it does
/// not mirror `VenueMessage`. The crate-level doc states the reason: the venue
/// is one party and names its own frames, while the inbound side has no
/// singular party to be named after.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Command {
    SubmitOrder(SubmitOrder),
    /// Submit a linked group in one step: every member accepted, or the whole
    /// group rejected and nothing on the book.
    ///
    /// Why the wire needs this at all: a consumer can obviously send the
    /// legs one at a time and the venue will take them. It can, and that is the
    /// hazard. A two-leg `Ouo` bracket dispatched per leg lets leg one fill
    /// before leg two has been admitted: the shrink runs against a sibling that
    /// is not on the book yet, so leg two arrives at full size and the pair's
    /// aggregate fill is unbounded at twice the intended quantity. For a
    /// crossed slice that is an account reversal, which is exactly what a
    /// consumer consumes bracket linkage to prevent, so the guarantee it needs
    /// is not "the venue serves Ouo" but "the venue admits the group before any
    /// member can fill". Per-leg submission cannot state that and this can.
    ///
    /// What is guaranteed, stated precisely so a consumer can cite it:
    ///
    /// 1. Atomic admission. Every member is validated against the book and
    ///    against the rest of the group before any of them is accepted. One
    ///    unacceptable member rejects the whole group, and the consumer sees one
    ///    `OrderRejected` per member and no `OrderAccepted` at all.
    /// 2. No tape advance between members. The whole group is one engine call at
    ///    one instant against one market reading, so no member meets a market
    ///    a sibling did not.
    /// 3. Fill-atomic linkage. A member that fills during the group has its rule
    ///    applied to every sibling, including the ones admitted after it, before
    ///    the group returns and therefore before any sweep can look at them.
    ///
    /// The one carve-out, and it is funds. The dry pass judges every member
    /// against the book as it is before the group runs, so it cannot see money
    /// an earlier member's fill is about to spend. A member the venue can no
    /// longer fund when its own turn comes is rejected on the second pass, with
    /// its earlier siblings already accepted - so on this one axis guarantee 1
    /// holds for everything the venue can decide in advance and not for a
    /// balance the group's own fills moved.
    ///
    /// Whether your group can meet it is a question about your own orders. A
    /// reduce-only member places no hold and cannot meet it; a member without
    /// that flag takes a hold like any other order, and whether an exit can be
    /// reduce-only depends on the run's `oms_type` and is on your side of the
    /// wire. Size a group so its members are jointly affordable against the
    /// balance the venue holds at submission, and the carve-out is unreachable.
    ///
    /// The group is self-contained: every id a member names must be another
    /// member, which is what makes "admit the group" and "admit every sibling"
    /// the same statement.
    SubmitOrderGroup {
        orders: Vec<SubmitOrder>,
    },
    CancelOrder {
        client_order_id: ClientOrderId,
    },
    ModifyOrder {
        client_order_id: ClientOrderId,
        #[serde(default, with = "crate::decimal::str_option")]
        price: Option<Decimal>,
        #[serde(default, with = "crate::decimal::str_option")]
        quantity: Option<Decimal>,
        /// Amending the trigger of an untriggered conditional restarts its
        /// trigger window; on anything else it is rejected.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "crate::decimal::str_option"
        )]
        trigger_price: Option<Decimal>,
    },
    /// Reconciliation query: ask the venue for the current status of its
    /// orders, answered from the engine's own book - not from any event the
    /// consumer may or may not have received. This is the second, independent
    /// witness Nautilus' reconciliation (startup mass-status and the
    /// continuous open-order poll) consumes: after a havoc scenario cancels a
    /// resting order venue-side and drops the lifecycle event, this query
    /// still reports the truth.
    ///
    /// Honest-content invariant: the reply's content is always a truthful
    /// read of the venue book. Havoc may delay or drop the reply's delivery
    /// (the snapshot classifies as execution, so `DelayAcks` holds it and
    /// `GoDark` drops it - transport faults are fair game and exercise the
    /// consumer's query-timeout path), but no divergence may ever alter what
    /// it says. A venue that lies on the reconciliation channel collapses the
    /// two witnesses into one adversary and makes any poll-heal test
    /// unprovable; a lying venue-truth source is a different fault class that
    /// would need its own explicitly-named havoc, never a side effect here.
    QueryOrders {
        /// Consumer-chosen correlation id echoed verbatim on the reply, so a
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
    /// twin of [`Command::QueryOrders`] with the same honest-content /
    /// havoc-able-delivery contract. The venue records each fill once as it
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
    /// Read this passenger's own history, one bounded page at a time.
    ///
    /// It carries no symbol, and that absence is the point. The upgrade already
    /// resolved this connection to one river, so a request made here names
    /// nothing and cannot name it wrong. Over HTTP a poll names a symbol and no
    /// passenger, which once a label names several rivers names none of them: a
    /// passenger reading surged water would backfill the clean river's prints
    /// and fold bars from a market it is not in, silently. Every selector that
    /// would fix that restates at the history call what the upgrade settled,
    /// which is a second place for identity to live and therefore to drift.
    ///
    /// One page per request, pulled. The consumer asks for the next page only
    /// after accepting the previous one, which bounds resident output per
    /// connection, work wasted after a disconnect, and how far a slow reader can
    /// fall behind. A server-pushed multi-page response would need its own
    /// queue and cancellation protocol and could still outrun the reader.
    QueryHistory {
        /// Correlation id echoed verbatim on the reply, so a requester sharing
        /// the socket with unsolicited market and execution frames can match
        /// replies to requests. A history refusal is correlated for the same
        /// reason: an untargeted error would leave a consumer unable to resolve
        /// which of its own requests failed.
        request_id: String,
        kind: HistoryKind,
        /// The window's inclusive start. Absent means the river's origin.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start: Option<u64>,
        /// The window's requested end, clamped to the cutoff the venue fixes at
        /// the first page. Absent means "everything up to now".
        #[serde(default, skip_serializing_if = "Option::is_none")]
        end: Option<u64>,
        /// The token from the previous page of this session, echoed verbatim.
        ///
        /// Opaque on purpose. It is the venue's own bookkeeping - which river,
        /// which kind, the immutable cutoff, the next position - and a consumer
        /// that treats it as anything but a token to hand back is relying on
        /// something the venue has not promised. Keeping it opaque is what lets
        /// the position move from a timestamp to something stronger if the
        /// generator ever permits two rows at one instant, without a wire
        /// change and without a consumer having encoded the old assumption.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        continuation: Option<String>,
    },
}

/// Which of a river's two streams a history page reads.
///
/// Separate streams with separate cutoffs, which is what makes a shared instant
/// between a quote and its first child trade harmless: a tie across the kinds
/// cannot cut either page. Within one kind a river never prints two rows at one
/// instant, and that is gated in `mogwai-data` by
/// `a_river_never_prints_two_trades_at_one_instant` and
/// `a_river_never_prints_two_quotes_at_one_instant`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum HistoryKind {
    Trades,
    Quotes,
}

/// One row of a history page: the same tick shape the live stream carries.
///
/// Untagged, because a page already states its `kind` and repeating it per row
/// would put the same fact in two places that could disagree. A page never
/// mixes the two.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum HistoryRow {
    Trade(TradeTick),
    Quote(QuoteTick),
}
impl HistoryRow {
    #[must_use]
    pub fn ts_event(&self) -> u64 {
        match self {
            Self::Trade(trade) => trade.ts_event,
            Self::Quote(quote) => quote.ts_event,
        }
    }
}

/// Which order command produced an execution frame, so the outbound path can
/// apply that command class's ack latency. `None` on the wire-diagnostic and
/// query paths, which carry no per-command latency.
///
/// Never serialized. It lives here, next to `Command` and `EventKind`, for
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
    ///
    /// Exhaustive rather than wildcarded, for the same reason
    /// `EventKind::is_execution` is: the compiler carries the claim. A wildcard
    /// arm let a new order-entry command compile clean as classless, which is
    /// silently outside every `CommandLatency` window. Here a new variant must
    /// opt in or out explicitly.
    #[must_use]
    pub fn of(cmd: &Command) -> Option<Self> {
        match cmd {
            Command::SubmitOrder(_) | Command::SubmitOrderGroup { .. } => Some(Self::Submit),
            Command::ModifyOrder { .. } => Some(Self::Modify),
            Command::CancelOrder { .. } => Some(Self::Cancel),
            Command::QueryOrders { .. }
            | Command::QueryFills { .. }
            | Command::QueryHistory { .. } => None,
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
    /// Terminal: canceled (consumer cancel, IOC remainder, or a venue-side
    /// havoc cancel).
    Canceled,
    /// Terminal: the order's own time in force ended it - a `Gtd` reaching its
    /// instant, or a `Day` whose session closed. Distinct from `Canceled`
    /// because nobody cancelled it: a cancel is an actor's decision and an
    /// expiry is the clock, and a venue that reports one as the other tells a
    /// consumer its order was pulled when the consumer's own stated lifetime
    /// simply ran out. Nautilus carries the same distinction as
    /// `OrderStatus::Expired` and an `OrderExpired` event, so the fidelity is
    /// available end to end rather than collapsing at the adapter.
    Expired,
    /// Terminal: refused after acceptance - today only a post-only stop-limit
    /// that would take liquidity against its own triggering print. A
    /// pre-acceptance refusal never becomes a truth-store row at all.
    Rejected,
}

impl WireOrderStatus {
    #[must_use]
    pub fn is_open(self) -> bool {
        // `Triggered` is open: a triggered stop-limit is resting and fillable,
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
    #[serde(with = "rust_decimal::serde::str")]
    pub quantity: Decimal,
    /// Quantity filled so far.
    #[serde(with = "rust_decimal::serde::str")]
    pub filled_qty: Decimal,
    /// Current order price. Always present in practice (the venue stamps
    /// Market orders before the engine sees them), optional on the wire to
    /// mirror `SubmitOrder`.
    #[serde(default, with = "crate::decimal::str_option")]
    pub price: Option<Decimal>,
    /// The conditional's stop price, `None` for a non-conditional order.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::decimal::str_option"
    )]
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

/// Reply to [`Command::QueryOrders`]: the venue's truthful order book
/// read at `ts_event`. An empty `orders` for a targeted query means the venue
/// never accepted that id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderStatusSnapshot {
    /// The request's correlation id, echoed verbatim.
    pub request_id: String,
    pub orders: Vec<OrderStatusInfo>,
    pub ts_event: u64,
}

/// Reply to [`Command::QueryFills`]: the venue's booked fills in the
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
    #[serde(with = "rust_decimal::serde::str")]
    pub quantity: Decimal,
    #[serde(default, with = "crate::decimal::str_option")]
    pub price: Option<Decimal>,
    /// The price the tape must touch for a conditional to become live.
    /// Required on `StopMarket`/`StopLimit`, refused on `Market`/`Limit`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::decimal::str_option"
    )]
    pub trigger_price: Option<Decimal>,
    /// How far a trailing stop's trigger sits from the extreme the tape has
    /// reached. Required on `TrailingStopMarket` and `TrailingStopLimit`, and
    /// refused on every other type.
    ///
    /// An absolute price distance, not a fraction: Pine states a trail in points
    /// or in ticks, and converting a fraction back would need a reference price
    /// nothing here agrees on.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::decimal::str_option"
    )]
    pub trail_offset: Option<Decimal>,
    /// How far a `TrailingStopLimit`'s limit sits from its own trigger, on the
    /// fillable side of it. Required on that type and refused on every other.
    ///
    /// Distinct from `trail_offset`, which holds the trigger away from the
    /// tape's extreme: this one holds the limit away from the trigger, so a
    /// trailing stop limit carries two independent distances and collapsing
    /// them would silently tie how far the stop trails to how much slippage the
    /// consumer tolerates.
    ///
    /// The limit price is derived from it rather than stated, at acceptance and
    /// again on every ratchet, which is why `price` is refused on this type: a
    /// trigger that moves and a limit that does not would drift apart until the
    /// limit is unreachable, and nautilus models the same materialization.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::decimal::str_option"
    )]
    pub limit_offset: Option<Decimal>,
    pub time_in_force: TimeInForce,
    /// Sim instant a `Gtd` order expires at. Required on `Gtd` and refused on
    /// every other time-in-force, including `Day` - a day order's expiry comes
    /// from the instrument's calendar, not from the consumer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expire_time: Option<u64>,
    /// Fills are clamped to the position this order would close, and the order
    /// is canceled rather than filled when that position is gone. Exempt from
    /// the funded-admission check and from `held_balances`: it can only
    /// shrink an exposure the position itself already represents.
    #[serde(default)]
    pub reduce_only: bool,
    /// An order that would take liquidity is rejected rather than filled.
    ///
    /// Legal on `Limit`, `StopLimit`, `LimitIfTouched` and
    /// `TrailingStopLimit`. See [`OrderType::may_be_post_only`], which is the
    /// rule both gates read. This doc said "Limit and StopLimit only" while the
    /// code admitted four types, so take the predicate as the contract and this
    /// sentence as its spelling.
    #[serde(default)]
    pub post_only: bool,
    /// The order list this order belongs to, and what its membership means.
    /// Absent for a standalone order, which is every order this venue served
    /// before linkage existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<OrderLink>,
}

/// How many orders one linkage may name. A bracket needs two; the cap exists
/// because every named sibling is a frame the fill that cancels or shrinks it
/// can emit, and `sizing` has to bound that in advance.
pub const MAX_LINKED_ORDERS: usize = 8;

/// How many orders one `SubmitOrderGroup` may carry.
///
/// A bracket is two or three. The cap exists for the same reason
/// `MAX_LINKED_ORDERS` does: a group is admitted and executed as one batch, so
/// its worst-case output has to be bounded before the batch runs, and the bound
/// is per member. One more than `MAX_LINKED_ORDERS`, so a parent naming the
/// maximum number of siblings can travel with all of them.
pub const MAX_GROUP_ORDERS: usize = MAX_LINKED_ORDERS + 1;

/// The wire-shape guard for a `SubmitOrderGroup`, which is
/// `validate_submit_order` for every member plus the rules that only exist
/// because the members arrived together.
///
/// Those rules are what make the atomicity guarantee statable:
///
/// - The group is self-contained. Every `linked_order_ids` entry and every
///   `parent_order_id` must name another member. A group naming an outsider
///   could not promise that admitting the group admits every sibling, which is
///   the whole guarantee - the outsider might be rejected, or might already be
///   filled, and the consumer would have a bracket with a leg missing.
/// - Every member is linked, and to the same list. An unlinked order in a group
///   frame is a standalone order asking for a guarantee that means nothing for
///   it; two list ids in one frame are two groups, and admitting them together
///   would promise an atomicity neither one asked for.
/// - Ids are unique within the group. Two members under one id cannot both be
///   admitted, and deciding which one wins is a choice the venue must not make.
/// - One symbol. Atomic admission is a property of one book at one instant, and
///   a cross-symbol group would need two, so it is refused rather than served
///   with a guarantee that quietly does not hold across the pair.
/// - No `Ioc` or `Fok`. A now-or-never order's fate is decided by the market
///   rather than by admission, so it cannot be part of a promise about
///   admission; and it is the one verdict the venue cannot reach before it has
///   already accepted the members beside it.
pub fn validate_submit_group(orders: &[SubmitOrder]) -> Result<(), &'static str> {
    if orders.is_empty() {
        return Err("an order group must carry at least one order");
    }
    if orders.len() > MAX_GROUP_ORDERS {
        return Err("order group exceeds MAX_GROUP_ORDERS");
    }
    for order in orders {
        validate_submit_order(order)?;
        if matches!(order.time_in_force, TimeInForce::Ioc | TimeInForce::Fok) {
            return Err(
                "an order-group member cannot be immediate-or-cancel: a now-or-never order's fate \
                 is not decided by admission",
            );
        }
    }
    let Some(list_id) = orders[0].link.as_ref().map(|link| &link.order_list_id) else {
        return Err("every order-group member must carry a link");
    };
    let symbol = &orders[0].symbol;
    for order in orders {
        let Some(link) = order.link.as_ref() else {
            return Err("every order-group member must carry a link");
        };
        if &link.order_list_id != list_id {
            return Err("every order-group member must name the same order_list_id");
        }
        if &order.symbol != symbol {
            return Err("every order-group member must name the same symbol");
        }
    }
    for (index, order) in orders.iter().enumerate() {
        if orders[..index]
            .iter()
            .any(|earlier| earlier.client_order_id == order.client_order_id)
        {
            return Err("duplicate client_order_id within the order group");
        }
    }
    let names_member =
        |id: &ClientOrderId| orders.iter().any(|member| &member.client_order_id == id);
    for order in orders {
        let link = order.link.as_ref().expect("checked above");
        if !link.linked_order_ids.iter().all(&names_member) {
            return Err("an order group may only link its own members");
        }
        if link
            .parent_order_id
            .as_ref()
            .is_some_and(|parent| !names_member(parent))
        {
            return Err("an order group's parent must be a member of the group");
        }
    }
    Ok(())
}

/// What one order's membership of an order list means.
///
/// A linkage is a group id plus a rule, and nothing else: the venue holds no
/// tree of orders, it holds a rule each member carries and applies at the
/// instant a member fills. That is what makes a genuine bracket expressible -
/// the sibling is reaped where the fill is committed, in the same batch, rather
/// than on a later sweep that a second fill could beat.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OrderLink {
    /// The list's identity, shared by every member. Carried for the consumer's
    /// benefit (nautilus keys an `OrderList` by it); the venue's own linkage
    /// arithmetic runs off `contingency`, `linked_order_ids` and
    /// `parent_order_id`.
    pub order_list_id: String,
    /// What a fill of this order does to the orders it names.
    pub contingency: Contingency,
    /// The siblings this order's rule acts on. Empty is legal only for a pure
    /// child (one carrying `parent_order_id` and `NoContingency`).
    #[serde(default)]
    pub linked_order_ids: Vec<ClientOrderId>,
    /// The order this one waits for. A child rests inert - unscanned, holding
    /// no hold - until its parent fills, which is the whole of
    /// one-triggers-the-other. `None` for a parent or a standalone member.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_order_id: Option<ClientOrderId>,
}

/// The rule a linked order carries, matching nautilus's `ContingencyType` so a
/// host's order list maps across without reinterpretation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum Contingency {
    /// The order names siblings for the venue's records but a fill of it does
    /// nothing to them. What a bare `Oto` parent carries: its children are named
    /// by their own `parent_order_id`, and releasing them is not a rule about
    /// cancellation.
    #[default]
    NoContingency,
    /// One-cancels-the-other: a fill of this order cancels every sibling it
    /// names that is still resting. Any fill, not only a full one - a venue
    /// that let a partially-filled take-profit leave its stop live would let a
    /// bracket hold two live exits for one position.
    Oco,
    /// One-triggers-the-other. Carried by a parent whose children wait on it;
    /// the release itself is driven by each child's `parent_order_id`, so this
    /// value is a declaration of intent rather than a second mechanism.
    Oto,
    /// One-updates-the-other: a fill of this order shrinks every sibling it
    /// names by the filled quantity, cancelling a sibling the shrink would take
    /// to zero. This is the bracket that survives partial fills - the stop
    /// tracks how much of the position is left.
    Ouo,
}

/// API-boundary guard for a `SubmitOrder`, mirroring `validate_conn_havoc` /
/// `validate_market_regime` / `validate_divergence` / `validate_inbound_havoc`
/// in style and message convention. `quantity` must be strictly positive, and
/// a `Limit` order must carry a strictly positive `price` (a `Market` order's
/// price is legitimately absent - nautilus `Market` orders carry no price).
///
/// This is the crate's own gate, not a substitute for the venue-side check:
/// `mogwai-engine`'s `validate_submit` is the authoritative, instrument-aware
/// guard (grid alignment, instrument lookup, precision) and remains the last
/// line of defense regardless of whether a caller runs this first.
///
/// The apparent disagreement with the engine - this validator accepts a
/// priceless `Market` order while `mogwai-engine`'s `validate_submit` rejects
/// one ("submit price required") - is a deliberate two-phase split, not a
/// drift. This gate validates the pre-stamp wire, exactly what the adapter puts
/// on the socket: a nautilus `Market` order legitimately carries no price there.
/// The venue then stamps a synthetic execution price onto every Market order
/// (on both the WS and HTTP carriers, failing loudly if synthesis fails) before
/// the engine ever sees it, so by the time `validate_submit` runs the order
/// always carries a price and a still-priceless one is a genuine post-stamp
/// bug. The engine is the authoritative post-stamp gate; this is the honest
/// pre-stamp one, and the two are consistent precisely because the stamp sits
/// between them.
pub fn validate_submit_order(order: &SubmitOrder) -> Result<(), &'static str> {
    validate_client_order_id(&order.client_order_id)?;
    // The same rule the URL-carried ingresses use, not a length check of its
    // own. An order-entry symbol used to be bounded only by `MAX_SYMBOL_LEN`,
    // so the empty string and any byte outside the wire alphabet - a newline, a
    // control character, markup - reached the engine's instrument lookup and
    // came back through the rejection path as a reason string. Nothing
    // downstream depended on that latitude: every symbol the venue can actually
    // serve comes from a config instrument or a preset, and the engine refuses
    // an unknown one anyway. One alphabet across every inbound symbol is
    // what makes "a symbol is 1 to 32 bytes of the URL-safe alphabet" a true
    // sentence rather than one that holds at two ingresses out of three.
    validate_wire_symbol(&order.symbol)?;
    if order
        .position_id
        .as_ref()
        .is_some_and(|id| id.len() > MAX_ECHOED_ID_LEN)
    {
        return Err("position_id exceeds MAX_ECHOED_ID_LEN");
    }
    if order.quantity <= Decimal::ZERO {
        return Err("quantity must be > 0");
    }
    if order.price.is_some_and(|price| price <= Decimal::ZERO) {
        return Err("price must be > 0");
    }
    if let Some(link) = &order.link {
        validate_order_link(order, link)?;
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
        OrderType::StopMarket | OrderType::MarketIfTouched | OrderType::TrailingStopMarket
            if order.price.is_some() =>
        {
            Err("a market-on-trigger order must not carry a price")
        }
        _ if order.order_type.is_conditional() && order.trigger_price.is_none() => {
            Err("conditional order must carry trigger_price")
        }
        OrderType::StopLimit | OrderType::LimitIfTouched if order.price.is_none() => {
            Err("a limit-on-trigger order must carry a price")
        }
        OrderType::MarketToLimit if order.price.is_none() => {
            Err("MarketToLimit order must carry a price: it is the limit the remainder rests at")
        }
        OrderType::MarketToLimit if order.trigger_price.is_some() => {
            Err("MarketToLimit order must not carry trigger_price")
        }
        // The trail offset is what makes a trailing stop one, and it is
        // meaningless anywhere else - accepting it silently on a fixed stop
        // would leave a consumer believing its stop moves.
        _ if order.order_type.trails() && order.trail_offset.is_none() => {
            Err("a trailing order must carry trail_offset")
        }
        _ if order.trail_offset.is_some() && !order.order_type.trails() => {
            Err("trail_offset is legal only on a trailing order")
        }
        // The limit price is derived from the trigger and this offset, and is
        // re-derived on every ratchet. A consumer-stated price would be
        // overwritten by the first trail, so accepting one would be a lie
        // rather than a harmless redundancy.
        OrderType::TrailingStopLimit if order.limit_offset.is_none() => {
            Err("TrailingStopLimit order must carry limit_offset")
        }
        OrderType::TrailingStopLimit if order.price.is_some() => {
            Err("TrailingStopLimit order must not carry a price: it is derived from limit_offset")
        }
        _ if order.limit_offset.is_some() && order.order_type != OrderType::TrailingStopLimit => {
            Err("limit_offset is legal only on TrailingStopLimit")
        }
        _ if order
            .limit_offset
            .is_some_and(|offset| offset <= Decimal::ZERO) =>
        {
            Err("limit_offset must be > 0")
        }
        _ if order
            .trail_offset
            .is_some_and(|offset| offset <= Decimal::ZERO) =>
        {
            Err("trail_offset must be > 0")
        }
        // Names the legal set rather than stating a rule. The message used to
        // read "legal only on orders that rest as a limit", which is false for
        // `MarketToLimit` - a type that does rest its remainder as a limit and
        // is refused here anyway, for the reason on `may_be_post_only`. A
        // refusal whose stated reason does not hold for one of the orders it
        // refuses is how the rule gets "corrected" wrongly later.
        //
        // It sits ahead of the conditional-IOC arm deliberately, and
        // `Engine::validate_submit` checks the two in the same order. An order
        // can break both rules at once - a post-only `StopMarket` marked
        // `Ioc` - and if the two gates reached them in opposite orders they
        // would name different reasons for one order, which is the exact
        // defect the shared predicate exists to remove.
        _ if order.post_only && !order.order_type.may_be_post_only() => Err(POST_ONLY_REFUSAL),
        // A conditional cannot be now-or-never: an order that must fill
        // immediately cannot also wait for a trigger. Day and Gtd can wait, so
        // they are admitted where Ioc and Fok are not.
        //
        // `MarketToLimit` is not conditional and is deliberately not caught
        // here. It waits for nothing - it acts at once and only its remainder
        // is in question - so the type and the time in force are not in
        // conflict, and the precedence between them is stated on the variant.
        _ if order.order_type.is_conditional()
            && matches!(order.time_in_force, TimeInForce::Ioc | TimeInForce::Fok) =>
        {
            Err(
                "conditional orders cannot be immediate-or-cancel: a now-or-never order cannot wait for a trigger",
            )
        }
        // `expire_time` is Gtd's whole content, and it belongs to nothing else -
        // a Day order's expiry comes from the instrument's calendar, so a consumer
        // stating one would be stating a deadline the venue ignores.
        _ if order.time_in_force == TimeInForce::Gtd && order.expire_time.is_none() => {
            Err("Gtd order must carry expire_time")
        }
        _ if order.expire_time.is_some() && order.time_in_force != TimeInForce::Gtd => {
            Err("expire_time is legal only on Gtd")
        }
        _ => Ok(()),
    }
}

/// The wire-shape half of the linkage contract: bounded, self-consistent, and
/// refusing the shapes whose meaning the venue would have to invent.
///
/// The rules that are not arbitrary:
///
/// - A rule that acts on siblings must NAME some. `Oco` and `Ouo` with nothing
///   linked is an order that silently behaves like a standalone one, which is
///   the failure a consumer would discover only by watching a stop it thought was
///   reaped go on to fill.
/// - An order may not link itself. A self-cancelling `Oco` would try to cancel
///   the order whose fill triggered it.
/// - A child must be able to wait. A market child released by its parent's fill
///   would have to execute against a reading the release path does not take, and
///   a now-or-never child would expire at submit, before its parent ever fills.
///   Both are refused rather than reinterpreted.
fn validate_order_link(order: &SubmitOrder, link: &OrderLink) -> Result<(), &'static str> {
    if link.order_list_id.is_empty() {
        return Err("order_list_id must not be empty");
    }
    if link.order_list_id.len() > MAX_ECHOED_ID_LEN {
        return Err("order_list_id exceeds MAX_ECHOED_ID_LEN");
    }
    if link.linked_order_ids.len() > MAX_LINKED_ORDERS {
        return Err("linked_order_ids exceeds MAX_LINKED_ORDERS");
    }
    for id in &link.linked_order_ids {
        validate_client_order_id(id)?;
        if *id == order.client_order_id {
            return Err("an order may not link itself");
        }
    }
    if let Some(parent) = &link.parent_order_id {
        validate_client_order_id(parent)?;
        if *parent == order.client_order_id {
            return Err("an order may not be its own parent");
        }
        if order.order_type == OrderType::Market {
            return Err(
                "a Market order cannot be an order-list child: a released child rests, and a market order has nothing to rest on",
            );
        }
        if matches!(order.time_in_force, TimeInForce::Ioc | TimeInForce::Fok) {
            return Err(
                "an order-list child cannot be immediate-or-cancel: it must outlive the submit that placed it to be released at all",
            );
        }
    }
    if matches!(link.contingency, Contingency::Oco | Contingency::Ouo)
        && link.linked_order_ids.is_empty()
    {
        return Err("Oco and Ouo must name at least one linked order");
    }
    Ok(())
}

/// What an `AdmissionRejected` refers to. Present because the refusal must be
/// translatable: the adapter turns a refused submit into nautilus
/// `OrderRejected` but a refused cancel into `OrderCancelRejected` - flipping a
/// live order to Rejected because its cancel was refused would be an invalid
/// transition (see `VenueMessage::OrderCancelRejected`).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind")]
pub enum AdmissionSubject {
    Submit {
        client_order_id: ClientOrderId,
    },
    /// A whole `SubmitOrderGroup`, named by its list id rather than by its
    /// members. One id keeps the frame bounded the way every other subject is,
    /// and it loses nothing: a group is admitted or refused whole, so naming
    /// one member would be as wrong as naming none, and the consumer knows which
    /// orders it sent under that list id.
    SubmitGroup {
        order_list_id: String,
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
    /// timing out. `query` names which query, because a consumer keeps two
    /// separate waiter maps keyed by request id and the protocol nowhere
    /// requires ids to be unique across the two.
    Query {
        request_id: String,
        query: QueryKind,
    },
    /// A frame the venue could not decode, or could not attribute at all.
    Frame,
}

/// Hand-written so every embedded id is truncated to `MAX_ECHOED_ID_LEN` on a
/// char boundary. Without it the derived impl would echo a consumer-supplied id
/// of any length, which makes `ADMISSION_FRAME_MAX_BYTES` - and therefore the
/// priority lane's frame-count memory bound - fictional.
///
/// The invariant is held at serialization, not at construction, deliberately.
/// Making the variants unconstructible from a raw `String` was proposed in the
/// 2026-08 hunt and declined: the subject is built at many refusal sites from
/// ids the venue has already refused, so a fallible constructor would put an
/// error path inside error handling. The residual is disclosed rather than
/// fixed - an in-memory `AdmissionSubject` may hold an over-length id, and only
/// what reaches the wire is bounded.
impl Serialize for AdmissionSubject {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        #[derive(Serialize)]
        #[serde(tag = "kind")]
        enum BoundedSubject<'a> {
            Submit {
                client_order_id: &'a str,
            },
            SubmitGroup {
                order_list_id: &'a str,
            },
            Cancel {
                client_order_id: &'a str,
            },
            Modify {
                client_order_id: &'a str,
            },
            Query {
                request_id: &'a str,
                query: QueryKind,
            },
            Frame,
        }
        fn bounded(value: &str) -> &str {
            if value.len() <= MAX_ECHOED_ID_LEN {
                return value;
            }
            let mut end = MAX_ECHOED_ID_LEN;
            while !value.is_char_boundary(end) {
                end -= 1;
            }
            &value[..end]
        }
        match self {
            Self::Submit { client_order_id } => BoundedSubject::Submit {
                client_order_id: bounded(client_order_id),
            },
            Self::SubmitGroup { order_list_id } => BoundedSubject::SubmitGroup {
                order_list_id: bounded(order_list_id),
            },
            Self::Cancel { client_order_id } => BoundedSubject::Cancel {
                client_order_id: bounded(client_order_id),
            },
            Self::Modify { client_order_id } => BoundedSubject::Modify {
                client_order_id: bounded(client_order_id),
            },
            Self::Query { request_id, query } => BoundedSubject::Query {
                request_id: bounded(request_id),
                query: *query,
            },
            Self::Frame => BoundedSubject::Frame,
        }
        .serialize(serializer)
    }
}

/// Which venue-truth query a refused `Query` subject refers to. Mirrors a
/// consumer's waiter maps one-for-one.
///
/// The two history spellings are here rather than in a subject of their own so
/// that every correlated refusal reaches a consumer through one vocabulary. A
/// history request that is refused for capacity has to name which request it
/// refused, exactly as an order query does - an untargeted diagnostic would
/// leave the consumer holding a pending backfill it can never resolve, which is
/// the same quiet-window failure an empty page would be.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum QueryKind {
    Orders,
    Fills,
    HistoryTrades,
    HistoryQuotes,
}
impl QueryKind {
    #[must_use]
    pub fn of_history(kind: HistoryKind) -> Self {
        match kind {
            HistoryKind::Trades => Self::HistoryTrades,
            HistoryKind::Quotes => Self::HistoryQuotes,
        }
    }
}

/// API-boundary guard for a `Command::ModifyOrder`'s `price`/`quantity`
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

/// Venue → consumer messages (execution events + market data).
///
/// These map onto nautilus `OrderEventAny` variants on the adapter side. The
/// divergences mogwai is built to emit (partials via `leaves_qty`, rejects,
/// duplicates, delays, drops) are expressed entirely through this stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum VenueMessage {
    /// The run reached its declared simulated duration. Venue-wide: it is true
    /// for every passenger at once and nothing further comes from this venue.
    /// Sent immediately before a normal close, which makes a planned exit
    /// distinguishable from a failed connection.
    ///
    /// A passenger boarding a run that has already completed is told at once,
    /// and that is the same event rather than a third one - it is learning a
    /// run-wide fact, not causing a new kind of completion.
    ///
    /// Both fields are this passenger's observation of a run-wide fact, read on
    /// its own boat clock. `elapsed_ns` is therefore the span that boat has
    /// covered, not the run's declared duration: a boat placed late, or a
    /// passenger that boards after completion, reports a shorter span for the
    /// same event. See `reference/clock.md`.
    RunComplete {
        sim_now_ns: u64,
        elapsed_ns: u64,
    },
    /// This passenger's own declared duration elapsed. The run continues for
    /// everyone else; for this socket it is over, and redialling would start a
    /// fresh duration rather than resume anything.
    ///
    /// A separate variant rather than a field, deliberately. Both events end a
    /// socket with WS 1000, and they were once the same frame - so a consumer
    /// classifying on the frame called a passenger's own deadline a finished
    /// run and stopped, while the close reason that could have told it apart
    /// was documented as unreliable. A new field would not have cured that: an
    /// older decoder ignores an unknown field and commits the same false
    /// transition, where an unknown tag fails outright and makes version skew
    /// visible instead of silent.
    ///
    /// The run wins a tie: if the run's own deadline has passed, the venue
    /// announces `RunComplete` even when this passenger's timer fired in the
    /// same instant: the run-wide fact is the stronger one, and it is the one
    /// that stays true for a consumer deciding whether to redial.
    ///
    /// `elapsed_ns` is the span observed on the boat clock since this passenger
    /// boarded, which is the same kind of quantity `RunComplete` carries and is
    /// not the same as the deadline that fired - a boat can predate its
    /// passenger, and a timer can wake late. `declared_duration_ns` states the
    /// deadline itself, so a consumer can see the difference rather than infer
    /// it.
    PassengerDurationComplete {
        sim_now_ns: u64,
        elapsed_ns: u64,
        declared_duration_ns: u64,
    },
    /// The venue could not admit a command or a frame, and said so instead of
    /// dropping it: it refused to do the work before any engine state was
    /// touched, because its per-connection outbound capacity could not cover the
    /// command's worst-case output, or because the request could not be decoded
    /// at all. `subject` names what was refused so the refusal is translatable
    /// per command (a refused cancel is not a rejected order), and `reason` is
    /// venue-generated and truncated to `MAX_REASON_LEN`, which with the
    /// identifier caps is what bounds this frame by
    /// `ADMISSION_FRAME_MAX_BYTES`.
    ///
    /// Admission truth, not engine output: it classifies `EventKind::Admission`,
    /// rides the venue's priority lane, and is deliberately not held by a
    /// `DelayAcks` window - the knob that holds engine output does not reach
    /// something the engine never produced. See `docs/havoc.md`.
    ///
    /// Every admission refusal is backpressure, which is what `retryable` says
    /// as data rather than as prose. It matters because of what happens to this
    /// frame downstream: a consumer's adapter has to map it onto its own stack's
    /// event for the same subject, and nautilus's `OrderRejected` carries a
    /// reason string and nothing else - so a refused submit reaches a strategy
    /// looking exactly like a business rejection ("insufficient balance",
    /// "market closed"), terminal, with the two distinguishable only by reading
    /// the venue's wording. A consumer will not, and should not, hang its
    /// quarantine decision on our prose, so the honest thing is to make the
    /// distinction a field.
    ///
    /// `true` on every variant this venue constructs today, and the field is not
    /// therefore pointless: it is the contract that an admission refusal means
    /// "the venue was full, not that it said no", stated where a consumer can
    /// read it. A future refusal on this frame that is genuinely not retryable
    /// sets it `false` and no consumer has to be told.
    ///
    /// `#[serde(default)]` because absent must mean the safe reading, which is
    /// not retryable: a consumer that infers retryability from a venue predating
    /// the field would retry against a refusal nobody promised was transient.
    AdmissionRejected {
        subject: AdmissionSubject,
        reason: String,
        /// Whether the same command, sent again later, could succeed. See the
        /// variant's own note for why this is a field and not a prose contract.
        #[serde(default)]
        retryable: bool,
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
    /// and a duplicated trigger would be a transition the consumer's FSM has no
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
    /// A resting order's own time in force ended it: a `Gtd` reaching its
    /// instant, or a `Day` whose session closed. Carries no reason string,
    /// because the order's own `time_in_force` already says which clock ran
    /// out and the venue has nothing to add.
    ///
    /// A separate frame rather than an `OrderCanceled` with a flag: a consumer
    /// matching on the frame is the one that has to act differently, and a
    /// flag on the cancel arm is a distinction every consumer must remember to
    /// read. It is not duplicated by `DuplicateNextFill` for the same reason
    /// `OrderTriggered` is not - it is not a fill, and a duplicated expiry is
    /// a transition no consumer FSM has an arm for.
    OrderExpired {
        client_order_id: ClientOrderId,
        venue_order_id: VenueOrderId,
        ts_event: u64,
    },
    OrderUpdated {
        client_order_id: ClientOrderId,
        venue_order_id: VenueOrderId,
        /// New total order quantity after the amend.
        #[serde(with = "rust_decimal::serde::str")]
        quantity: Decimal,
        /// New price after the amend. `None` for a still-priceless order.
        #[serde(default, with = "crate::decimal::str_option")]
        price: Option<Decimal>,
        /// New trigger price after the amend. `None` for a non-conditional
        /// order, and for an amend that did not touch the trigger.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "crate::decimal::str_option"
        )]
        trigger_price: Option<Decimal>,
        /// Remaining quantity after the amend.
        #[serde(with = "rust_decimal::serde::str")]
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
    /// unknown, already terminal (filled, canceled, expired or rejected), or
    /// the cancel is otherwise illegal.
    ///
    /// Distinct from `OrderRejected`, which terminates the order. A rejected
    /// cancel does not kill the order - it is still whatever it was (Accepted,
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
    /// One bounded page of this passenger's own history, answering a
    /// `QueryHistory`.
    ///
    /// The rows are this connection's river, never a label's. That is the whole
    /// reason history moved onto the socket, and it is why the page names no
    /// symbol: the connection already did.
    ///
    /// Splicing onto live, which is the part a consumer has to get right. A
    /// socket is attached to the live tape at upgrade, so live frames keep
    /// crossing while a history session paginates. The venue fixes `cutoff` at
    /// the first page and every later page of that session uses the same one,
    /// so pagination cannot chase a moving present and never finish. The
    /// consumer starts buffering live rows before it sends the first
    /// `QueryHistory` - otherwise a frame can arrive between deciding to
    /// backfill and the command reaching the venue - and once the session
    /// completes it drops buffered rows of that kind at or below `cutoff` and
    /// keeps the rest. That deliberately admits overlap and forbids gaps:
    /// overlap is removable because a river never prints two rows of one kind at
    /// one instant, and a gap is not recoverable at all.
    HistoryPage {
        request_id: String,
        kind: HistoryKind,
        /// Trades or quotes, ascending, never both. Empty only when the window
        /// genuinely holds nothing, never because the venue gave up - a short
        /// success would be indistinguishable from a quiet market, which is the
        /// failure this whole shape exists to avoid.
        rows: Vec<HistoryRow>,
        /// The session's immutable inclusive end, fixed at the first page. Per
        /// kind, because a combined market cutoff would name an instant at
        /// which one stream had a row and the other did not.
        cutoff: u64,
        /// Hand this back to read the next page. Absent when `complete`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        continuation: Option<String>,
        /// Every row through `cutoff` has now been delivered.
        complete: bool,
    },
    /// A `QueryHistory` that will not be answered, correlated to the request
    /// that asked. Distinct from an empty page, deliberately: representing
    /// unavailable history as a quiet market is the defect, not the fallback.
    HistoryRejected {
        request_id: String,
        reason: String,
        /// Whether asking again could succeed - a capacity refusal can, a
        /// malformed window cannot.
        retryable: bool,
    },
    AccountState(AccountState),
    Trade(TradeTick),
    Quote(QuoteTick),
    /// Venue-originated liveness signal. Carries the venue wall clock
    /// unix-ns so the frame is non-empty and timestamp-comparable, but no
    /// market or execution payload. Consumers may ignore it; its job is to keep
    /// the socket frame-active through a `StallData` window.
    Heartbeat {
        ts_event: u64,
    },
    /// The bounded tape fanout overwrote market frames before they reached this
    /// connection, so this passenger's view of the water has a hole. Advisory:
    /// the venue declares the loss and keeps serving, and the reader decides its
    /// own response.
    ///
    /// Positional, which is why it rides the market lane rather than the
    /// priority lane. It is written immediately before the first market frame
    /// delivered after the loss, so the hole it names is the one the reader is
    /// about to read across. A diagnostic that overtook the market backlog would
    /// name a boundary the reader had not reached.
    ///
    /// The boundaries are aggregation bounds, not frame identities. Several
    /// trades and quotes can share one `ts_event`, so these delimit the affected
    /// span for a reader folding bars or advancing a cursor; they do not say
    /// which frames were lost. `after_ts_event` is the last market frame that
    /// actually crossed this socket, absent when the loss preceded the first
    /// one; `resumed_ts_event` is the first frame delivered after it, absent
    /// when the tape ended inside the hole.
    ///
    /// Every declared loss is unarmed. A frame the venue deliberately withheld
    /// under `GoDark` or `StallData` is not loss - the consumer armed it - and a
    /// hole discovered while such a window is open is held back until delivery
    /// resumes rather than announced into a blackout. The venue does not claim
    /// the converse: it cannot know whether a frame the ring overwrote would
    /// have been suppressed, because suppression is a question about a delivery
    /// attempt that never happened, so a hole spanning a blackout is declared
    /// rather than assumed away.
    ///
    /// The counters are what distinguish a passenger that fell behind once from
    /// one whose sustainable read rate is below the boat's publish rate:
    /// `episode` rises per declared hole and `skipped_total` accumulates over
    /// the passenger's life. The venue never ends a connection for either - what
    /// to do about a lossy view is the consumer's decision, not the venue's.
    ///
    /// Delivery is not guaranteed when the transport fails. This declares
    /// recoverable loss while the connection is still writable; a sink failure
    /// can leave an already-owed declaration undeliverable, and socket
    /// termination is then the only observable.
    FeedLagged {
        episode: u64,
        skipped: u64,
        skipped_total: u64,
        after_ts_event: Option<u64>,
        resumed_ts_event: Option<u64>,
    },
    /// A non-fatal run-level havoc observation. It replaces the old
    /// subscription-attributed diagnostic because a run-level observation has
    /// no one subscription to attribute.
    HavocDiagnostic {
        reason: String,
        sim_now_ns: u64,
    },
    /// A whole frame the venue could not decode or attribute: a
    /// frame that is not a `Command` (bad JSON, unknown `type`, or a
    /// known `type` missing required fields), or a request on a carrier that
    /// does not support it. Emitted in
    /// place of a silent drop: without it, an unservable request and a
    /// healthy-but-idle feed were indistinguishable on the wire.
    ///
    /// Untargeted, and now only where untargetedness is honest: a whole-frame
    /// fault has no target to name.
    ///
    /// Classifies `EventKind::Admission`, not `Exec`: it reports what the
    /// venue's request handling refused, which is never something the matching
    /// engine produced, so `DelayAcks` (a hold on engine output) does not reach
    /// it and it rides the venue's priority lane ahead of held traffic.
    ///
    /// `reason` is venue-generated prose and must be routed through
    /// `truncate_reason` at every construction site: serde's decode-error text
    /// echoes consumer-controlled field names, and without the truncation
    /// `ADMISSION_FRAME_MAX_BYTES` - hence the priority lane's frame count as a
    /// memory bound - is unproven.
    ProtocolError {
        reason: String,
        ts_event: u64,
    },
}

/// Probe of the internally tagged discriminator, used to pick a payload decoder
/// without buffering the frame.
///
/// The tag is a `Cow`, not a `&str`, and that is load-bearing rather than
/// stylistic: serde_json can only hand out a borrowed string when the JSON
/// scalar contains no escape sequence, so a borrowed probe would refuse
/// a tag written with a `\uXXXX` escape - a valid, if noncanonical, spelling that
/// the fully general enum decoder accepts. Refusing it would narrow what this
/// crate decodes relative to what it documents, on the one type both ends
/// serialize against. `#[serde(borrow)]` keeps the zero-copy path for the
/// canonical unescaped spelling the venue emits and falls back to an owned
/// `String` only for an escaped one.
#[derive(Deserialize)]
struct TagProbe<'a> {
    #[serde(rename = "type", borrow)]
    kind: std::borrow::Cow<'a, str>,
}

impl VenueMessage {
    /// Decode a venue frame without serde's internally-tagged content buffer
    /// on the market-data hot path. The small tag probe borrows from `json`,
    /// then the selected payload struct streams directly from the same bytes.
    /// Cold execution and control variants retain serde's fully general
    /// order-independent decoder.
    ///
    /// Accepts exactly what `serde_json::from_str::<VenueMessage>` accepts;
    /// see the private `TagProbe` for the escape case that makes that non-obvious.
    pub fn from_json_str(json: &str) -> serde_json::Result<Self> {
        match serde_json::from_str::<TagProbe<'_>>(json)?.kind.as_ref() {
            "Trade" => serde_json::from_str(json).map(Self::Trade),
            "Quote" => serde_json::from_str(json).map(Self::Quote),
            _ => serde_json::from_str(json),
        }
    }

    /// Byte-slice twin of [`Self::from_json_str`].
    pub fn from_json_slice(json: &[u8]) -> serde_json::Result<Self> {
        match serde_json::from_slice::<TagProbe<'_>>(json)?.kind.as_ref() {
            "Trade" => serde_json::from_slice(json).map(Self::Trade),
            "Quote" => serde_json::from_slice(json).map(Self::Quote),
            _ => serde_json::from_slice(json),
        }
    }

    /// The single source of truth for how each wire variant is classified into
    /// the exec / fill / data buckets that both ends key their havoc off.
    ///
    /// The venue's outbound delay path (`DelayAcks`) delays every execution
    /// event ([`EventKind::is_execution`], i.e. `Exec` and `Fill` only -
    /// `Admission` is transport truth and is exempt, as is `Data`), and the
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
            VenueMessage::OrderFilled(_) => EventKind::Fill,
            // Heartbeat is a liveness signal, not execution traffic: `DelayAcks`
            // must not perturb its cadence. It also must survive `StallData`,
            // so writer gates use `is_market_data()` rather than this category.
            VenueMessage::Trade(_) | VenueMessage::Quote(_) | VenueMessage::Heartbeat { .. } => {
                EventKind::Data
            }
            // The query replies are execution-channel traffic: `DelayAcks`
            // holds them and `GoDark` drops them (delivery is havoc-able),
            // while their content stays a truthful book read - the invariant
            // documented on `Command::QueryOrders`.
            VenueMessage::AccountState(_)
            | VenueMessage::OrderStatusSnapshot(_)
            | VenueMessage::FillSnapshot(_)
            | VenueMessage::OrderAccepted { .. }
            | VenueMessage::OrderTriggered { .. }
            | VenueMessage::OrderRejected { .. }
            | VenueMessage::OrderCanceled { .. }
            | VenueMessage::OrderExpired { .. }
            | VenueMessage::OrderUpdated { .. }
            | VenueMessage::OrderModifyRejected { .. }
            | VenueMessage::OrderCancelRejected { .. } => EventKind::Exec,
            // Transport classification, not a claim that these are admissions
            // in the ordinary sense. This bucket is what `DelayAcks` is exempt
            // from and what rides the priority lane; the two terminal
            // announcements sit here because a completion must not be held
            // behind engine output, not because completing is an admission.
            VenueMessage::AdmissionRejected { .. }
            | VenueMessage::ProtocolError { .. }
            | VenueMessage::FeedLagged { .. }
            | VenueMessage::HavocDiagnostic { .. }
            | VenueMessage::RunComplete { .. }
            | VenueMessage::PassengerDurationComplete { .. } => EventKind::Admission,
            // Its own class, so neither of the two obvious homes decides a
            // contract by accident - see `EventKind::History`.
            VenueMessage::HistoryPage { .. } | VenueMessage::HistoryRejected { .. } => {
                EventKind::History
            }
        }
    }

    /// Whether this frame is market channel data, the payload a
    /// per-subscription data watchdog keys off. This is deliberately narrower
    /// than `category() == Data`: the venue heartbeat rides the data latency
    /// bucket but is a liveness signal, not channel data.
    #[must_use]
    pub fn is_market_data(&self) -> bool {
        matches!(self, VenueMessage::Trade(_) | VenueMessage::Quote(_))
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
    #[serde(with = "rust_decimal::serde::str")]
    pub last_qty: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub last_px: Decimal,
    /// Remaining quantity. `> 0` ⇒ this is a partial fill.
    #[serde(with = "rust_decimal::serde::str")]
    pub leaves_qty: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
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
    #[serde(with = "rust_decimal::serde::str")]
    pub initial: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub maintenance: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Balance {
    pub currency: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub total: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub free: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub locked: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub symbol: Symbol,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_id: Option<String>,
    /// Signed net quantity: positive is long, negative is short, zero is flat.
    #[serde(with = "rust_decimal::serde::str")]
    pub quantity: Decimal,
    /// Volume-weighted average entry price of the open quantity. Zero when flat.
    #[serde(with = "rust_decimal::serde::str")]
    pub avg_px: Decimal,
    #[serde(
        default,
        skip_serializing_if = "Decimal::is_zero",
        with = "rust_decimal::serde::str"
    )]
    pub mark_px: Decimal,
    #[serde(
        default,
        skip_serializing_if = "Decimal::is_zero",
        with = "rust_decimal::serde::str"
    )]
    pub unrealized_pnl: Decimal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeTick {
    pub symbol: Symbol,
    #[serde(with = "rust_decimal::serde::str")]
    pub price: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub size: Decimal,
    pub aggressor: AggressorSide,
    pub ts_event: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuoteTick {
    pub symbol: Symbol,
    #[serde(with = "rust_decimal::serde::str")]
    pub bid_px: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub ask_px: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub bid_sz: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub ask_sz: Decimal,
    pub ts_event: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one alphabet both ends judge a URL-carried symbol by. Written here
    /// rather than only at the two call sites because a drift in this function
    /// is a consumer that builds a URL the venue then refuses.
    #[test]
    fn wire_symbols_are_the_url_safe_alphabet() {
        for legal in [
            "MNQ",
            "BTCUSDT",
            "ES.c.0",
            "a-b_c",
            &"X".repeat(MAX_SYMBOL_LEN),
        ] {
            assert!(validate_wire_symbol(legal).is_ok(), "{legal} is legal");
        }
        for illegal in [
            "",
            " ",
            " MNQ",
            "MNQ ",
            "MN Q",
            "MNQ/1",
            "MNQ%20",
            "MNQ?symbol=X",
            "MNQ&speed=2",
            "MNQ\u{00e9}",
            &"X".repeat(MAX_SYMBOL_LEN + 1),
        ] {
            assert!(
                validate_wire_symbol(illegal).is_err(),
                "{illegal:?} must be refused"
            );
        }
        // Same guard the callsign cap carries, and for the same reason: the
        // refusal text spells its bound out as a literal while the check reads
        // `MAX_SYMBOL_LEN`, so moving the constant would leave a refusal
        // stating a number the venue no longer enforces. This is the front
        // door - order entry routes through this function - so the refusal is
        // what a consumer reads when its symbol is rejected.
        let over = "X".repeat(MAX_SYMBOL_LEN + 1);
        let refusal = validate_wire_symbol(&over).unwrap_err();
        assert!(
            refusal.contains(&MAX_SYMBOL_LEN.to_string()),
            "the refusal {refusal:?} must state the cap {MAX_SYMBOL_LEN} it enforces"
        );
    }

    /// The currency-code rule, which is stricter than blank-rejection: a padded
    /// code reads as a currency and matches no balance, so it is refused rather
    /// than normalized. Both consumers - the risk policy here and
    /// `mogwai-venue`'s config loader - go through this one function.
    #[test]
    fn currency_codes_are_unpadded_and_capped() {
        for legal in ["USD", "USDT", "XBT", &"C".repeat(MAX_CURRENCY_LEN)] {
            assert!(
                validate_currency_code(legal).is_ok(),
                "{legal:?} is a usable code"
            );
        }
        for illegal in [
            "",
            " ",
            "\t",
            " USD",
            "USD ",
            " USD ",
            "USD\n",
            &"C".repeat(MAX_CURRENCY_LEN + 1),
        ] {
            assert!(
                validate_currency_code(illegal).is_err(),
                "{illegal:?} must be refused"
            );
        }
        let over = "C".repeat(MAX_CURRENCY_LEN + 1);
        let refusal = validate_currency_code(&over).unwrap_err();
        assert!(
            refusal.contains(&MAX_CURRENCY_LEN.to_string()),
            "the refusal {refusal:?} must state the cap {MAX_CURRENCY_LEN} it enforces"
        );
    }

    /// Order entry judges a symbol by the same alphabet the URL ingresses do.
    /// It did not until 2026-08-19: `validate_submit_order` carried a bare
    /// `symbol.len() > MAX_SYMBOL_LEN` check, so the empty string and any byte
    /// outside the wire alphabet were admitted at the one inbound symbol
    /// ingress this workspace has, while a 2026-08 audit's record asserted
    /// that all three ingresses validated. The claim was wrong at exactly
    /// this call.
    ///
    /// Both ingress shapes are covered, because `SubmitOrderGroup` carries up
    /// to `MAX_GROUP_ORDERS` symbols of its own and reaches this validator only
    /// through `validate_submit_group`. A fix applied to the single-order path
    /// alone would leave the group path open and this test would say so.
    #[test]
    fn an_order_entry_symbol_is_judged_by_the_wire_alphabet() {
        let submit = |symbol: &str| SubmitOrder {
            client_order_id: "O-1".to_string(),
            symbol: Symbol::from(symbol),
            position_id: None,
            side: Side::Buy,
            order_type: OrderType::Market,
            quantity: Decimal::ONE,
            price: None,
            trigger_price: None,
            trail_offset: None,
            limit_offset: None,
            time_in_force: TimeInForce::Gtc,
            expire_time: None,
            reduce_only: false,
            post_only: false,
            link: None,
        };

        for legal in ["MNQ", "BTCUSDT", "BTCUSDT.P", "BTCUSD-INV", "ES.c.0"] {
            assert!(
                validate_submit_order(&submit(legal)).is_ok(),
                "{legal} names an instrument this venue can serve and must be admitted"
            );
        }
        for illegal in [
            "",
            " ",
            "MNQ ",
            "MN Q",
            "MNQ\n",
            "MNQ\u{7f}",
            "MNQ/1",
            "<script>",
            &"X".repeat(MAX_SYMBOL_LEN + 1),
        ] {
            let order = submit(illegal);
            let refusal = validate_submit_order(&order)
                .expect_err(&format!("{illegal:?} must be refused at order entry"));
            // The group assertion is an equality, not an `is_err`. A group of
            // one unlinked order is refused for want of a link whatever its
            // symbol says, so `is_err` would pass over a group path that never
            // looked at the symbol at all. `validate_submit_group` runs the
            // per-member validator before any linkage rule, so the symbol's
            // refusal is the one that must come back.
            assert_eq!(
                validate_submit_group(std::slice::from_ref(&order)),
                Err(refusal),
                "{illegal:?} must be refused through the group carrier for the same reason"
            );
        }
    }

    /// The callsign's alphabet and bounds, which nothing held before this
    /// test. `validate_callsign` has two production callers - `ws.rs`'s
    /// upgrade refusal and the adapter's config check - and one use in
    /// `adapter_smoke.rs` as an oracle on a minted callsign, so every existing
    /// use is a positive case. A body of `Ok(())` passed the whole workspace:
    /// the refusals are what those two call sites exist for, and nothing
    /// exercised one.
    ///
    /// It is a separate function from `validate_wire_symbol` with the same
    /// alphabet and a different cap, so a drift between them is representable;
    /// this pins the half that is not a symbol. The empty case is called out
    /// because the function makes an explicit ruling on it: `callsign=` with
    /// nothing after it is a consumer that has spoken, and reading it as absent
    /// would silently hand back the always-evict behaviour it was trying to
    /// leave.
    #[test]
    fn callsigns_use_the_url_safe_alphabet() {
        for legal in [
            "mogwai-4242-1",
            "a",
            "0.1_2-3",
            &"s".repeat(MAX_CALLSIGN_LEN),
        ] {
            assert!(
                validate_callsign(legal).is_ok(),
                "{legal:?} must be accepted"
            );
        }
        for illegal in [
            "",
            " ",
            " abc",
            "abc ",
            "a b",
            "a/b",
            "a%20b",
            "a?x=1",
            "a&speed=2",
            "a:b",
            "caf\u{e9}",
            &"s".repeat(MAX_CALLSIGN_LEN + 1),
        ] {
            assert!(
                validate_callsign(illegal).is_err(),
                "{illegal:?} must be refused"
            );
        }
        // The message and the constant are checked against each other, not the
        // constant against a literal. The refusal text is a hardcoded
        // "callsigns are 1 to 64 characters" and nothing else would notice the cap
        // moving out from under it - the durable-prose-asserting-a-live-fact
        // shape, in a string literal.
        let over = "s".repeat(MAX_CALLSIGN_LEN + 1);
        let refusal = validate_callsign(&over).unwrap_err();
        assert!(
            refusal.contains(&MAX_CALLSIGN_LEN.to_string()),
            "the refusal {refusal:?} must state the cap {MAX_CALLSIGN_LEN} it enforces"
        );
    }

    /// The two echo guards, which bound what a refusal frame may carry back.
    /// Both are one-liners and both are exercised only indirectly today -
    /// `validate_client_order_id` through `validate_submit_order`, and
    /// `validate_request_id` through the venue's query path - so neither had a
    /// case at its own boundary.
    #[test]
    fn the_echo_id_guards_admit_exactly_their_cap() {
        let at_cap = "x".repeat(MAX_ECHOED_ID_LEN);
        let over = "x".repeat(MAX_ECHOED_ID_LEN + 1);
        assert!(validate_client_order_id(&at_cap).is_ok());
        assert!(validate_client_order_id(&over).is_err());
        assert!(validate_request_id(&at_cap).is_ok());
        assert!(validate_request_id(&over).is_err());
        // Empty is accepted by both, deliberately rather than by omission:
        // these are length guards for an echo, and the emptiness rules live
        // where they mean something (the engine refuses an empty
        // `client_order_id` on submit, with its own message). Pinned so a
        // reader does not infer a rule these functions do not make.
        assert!(validate_client_order_id(&ClientOrderId::new()).is_ok());
        assert!(validate_request_id("").is_ok());
    }

    /// The two truncations, whose whole content is the char-boundary walk.
    /// Both run on the echo path of a refusal, so a panic here is a panic on
    /// the frame that reports someone else's error - and a multi-byte
    /// character straddling the cap is the only input that reaches the loop at
    /// all.
    #[test]
    fn truncation_cuts_on_a_char_boundary_and_leaves_short_values_alone() {
        // Untouched below the cap, and at it: the guard is `<=`.
        let at_cap = "x".repeat(MAX_ECHOED_ID_LEN);
        assert_eq!(truncate_echoed_id(at_cap.clone()), at_cap);
        let short = "O-1".to_string();
        assert_eq!(truncate_reason(short.clone()), short);

        // A four-byte character straddling the cap: the cut lands before it,
        // never inside it, so the result is short of the cap rather than at it.
        let straddling = format!("{}\u{1f600}", "x".repeat(MAX_ECHOED_ID_LEN - 2));
        let cut = truncate_echoed_id(straddling);
        assert_eq!(
            cut.len(),
            MAX_ECHOED_ID_LEN - 2,
            "the cut must fall before the straddling character, not inside it"
        );
        assert!(cut.chars().all(|ch| ch == 'x'));

        let reason = format!("{}\u{1f600}", "y".repeat(MAX_REASON_LEN - 1));
        let cut = truncate_reason(reason);
        assert_eq!(cut.len(), MAX_REASON_LEN - 1);
        assert!(cut.chars().all(|ch| ch == 'y'));

        // An ASCII overflow cuts exactly at the cap on both, which is the term
        // `ORDER_EVENT_MAX_BYTES` charges `MAX_REASON_LEN` and
        // `2 * MAX_ECHOED_ID_LEN` for. Asserted on both because the straddling
        // case above lands short of the cap and so cannot pin the ceiling the
        // reservation is derived from.
        assert_eq!(
            truncate_reason("z".repeat(MAX_REASON_LEN * 3)).len(),
            MAX_REASON_LEN
        );
        assert_eq!(
            truncate_echoed_id("z".repeat(MAX_ECHOED_ID_LEN * 3)).len(),
            MAX_ECHOED_ID_LEN
        );
    }

    /// The three tape predicates at the price, which is the only argument that
    /// separates them and the one a fixture built from a market away from the
    /// level cannot reach. They are exercised through the engine and the data
    /// walk today, never directly, and the strictness split is the whole
    /// design: a limit is behind the queue at its own price, a conditional
    /// holds no queue position at all.
    ///
    /// The complement claim is `trades_through`'s doc comment made runnable -
    /// it states that for the same side and the same price the two are exact
    /// logical complements, and that is what silently stops being true if
    /// either comparison is relaxed.
    #[test]
    fn the_scan_predicates_split_exactly_at_the_price() {
        let px = Decimal::from(100);
        let below = Decimal::from(99);
        let above = Decimal::from(101);

        for side in [Side::Buy, Side::Sell] {
            for traded in [below, px, above] {
                assert_eq!(
                    trades_through(side, px, traded),
                    !touches_trigger(side, px, traded),
                    "{side:?} at {traded}: the limit and the stop predicate must \
                     be exact complements at one price"
                );
            }
        }

        // A print at the level: through for neither side, touching for both.
        assert!(!trades_through(Side::Buy, px, px));
        assert!(!trades_through(Side::Sell, px, px));
        assert!(touches_trigger(Side::Buy, px, px));
        assert!(touches_trigger(Side::Sell, px, px));
        assert!(touches_toward(Side::Buy, px, px));
        assert!(touches_toward(Side::Sell, px, px));

        // ...and the two conditional predicates are direction-opposite, which
        // is what stops them being collapsed behind a strictness flag.
        assert!(touches_trigger(Side::Buy, px, above) && !touches_toward(Side::Buy, px, above));
        assert!(touches_toward(Side::Buy, px, below) && !touches_trigger(Side::Buy, px, below));
        assert!(touches_trigger(Side::Sell, px, below) && !touches_toward(Side::Sell, px, below));
        assert!(touches_toward(Side::Sell, px, above) && !touches_trigger(Side::Sell, px, above));

        // `ScanKind::hit` is a three-arm match over exactly these three
        // functions, so what this holds is narrow and worth stating as such:
        // the arms are not transposed. It cannot see the engine assigning the
        // wrong `ScanKind` to an order, which is the drift that would actually
        // bite - that classification is held where an order is built, not here.
        for side in [Side::Buy, Side::Sell] {
            for traded in [below, px, above] {
                assert_eq!(
                    ScanKind::FillThrough.hit(side, px, traded),
                    trades_through(side, px, traded)
                );
                assert_eq!(
                    ScanKind::StopTrigger.hit(side, px, traded),
                    touches_trigger(side, px, traded)
                );
                assert_eq!(
                    ScanKind::TouchedTrigger.hit(side, px, traded),
                    touches_toward(side, px, traded)
                );
            }
        }
    }

    /// The hole the refusal fix opened one layer up, and the reason
    /// `crate::decimal::str_option` exists instead of
    /// `rust_decimal::serde::str_option`.
    ///
    /// An optional wire decimal has two legal spellings for "no value" - the
    /// field absent, or present and `null` - and the first cut of the numeric
    /// refusal broke both of them, each by a different mechanism. (An earlier
    /// draft of this comment counted three by listing "absent" and "omitted"
    /// as separate spellings; they are one.) The dependency's
    /// `str_option` refuses an explicit `null`, which is exactly what the venue
    /// and the adapter emit for a priceless order - a stop-market submit, an
    /// amend that does not touch the price - and it made
    /// `adapter_submits_a_stop_market_and_sees_triggered_then_filled` and
    /// `a_trigger_amend_on_a_triggered_stop_limit_keeps_it_triggered` fail with
    /// no execution event at all, because the stub decodes with a silent
    /// `if let Ok`. And a `with = ...` field loses serde's implicit
    /// Option-is-optional handling, so an absent `price` - which every
    /// `Market`-order fixture in the serving suite sends - became a
    /// missing-field error until the `default`s went back on.
    ///
    /// Both are pinned here rather than left to the socket suites, which the
    /// changed-files check lane does not run.
    #[test]
    fn an_absent_or_null_optional_wire_decimal_is_still_none() {
        let modify_null =
            r#"{"type":"ModifyOrder","client_order_id":"O-1","price":null,"quantity":"2"}"#;
        let modify_absent = r#"{"type":"ModifyOrder","client_order_id":"O-1","quantity":"2"}"#;
        for frame in [modify_null, modify_absent] {
            let Ok(Command::ModifyOrder {
                price, quantity, ..
            }) = serde_json::from_str::<Command>(frame)
            else {
                panic!("an absent or null optional decimal must decode as None: {frame}");
            };
            assert_eq!(price, None, "{frame}");
            assert_eq!(quantity, Some(Decimal::from(2)), "{frame}");
        }

        // The two order shapes that actually reach the venue without a price.
        // The Market one omits the field, exactly as the serving suite's
        // fixtures do; the StopMarket one spells it null, exactly as the
        // adapter's `Command` serialization does.
        for frame in [
            r#"{"type":"SubmitOrder","client_order_id":"O-1","symbol":"BTCUSDT","side":"Buy","order_type":"Market","quantity":"1","time_in_force":"Gtc"}"#,
            r#"{"type":"SubmitOrder","client_order_id":"O-2","symbol":"BTCUSDT","side":"Sell","order_type":"StopMarket","quantity":"1","price":null,"trigger_price":"95.00","time_in_force":"Gtc"}"#,
        ] {
            let Ok(Command::SubmitOrder(order)) = serde_json::from_str::<Command>(frame) else {
                panic!("a priceless submit must decode: {frame}");
            };
            assert_eq!(order.price, None, "{frame}");
        }

        // And `null` still round-trips as `null` rather than vanishing: the
        // byte form the wire test pins is unchanged by the annotations.
        let reserialized =
            serde_json::to_string(&serde_json::from_str::<Command>(modify_null).expect("decode"))
                .expect("re-serialize");
        assert_eq!(reserialized, modify_null);
    }

    /// Every `Decimal` on the wire is a JSON string, in both directions, and a
    /// numeric spelling is refused rather than rounded.
    ///
    /// `rust_decimal`'s default `Deserialize` accepts a JSON number as well as
    /// a string, and the number goes through `f64`. Measured before the fix,
    /// against these same types: `{"price": 12345678901234567890.123}` decoded
    /// to `12345678901234567000`, `0.1234567890123456789` to
    /// `0.12345678901234568`, and - the asymmetry that shows the two grammars
    /// are not even nested - `1e-30` decoded as a number to `Decimal::ZERO`
    /// while the same text as a string was refused outright. A price or a
    /// quantity whose value depends on how the peer spelled it is not a source
    /// of truth, so the `with = "rust_decimal::serde::str"` annotations make
    /// the string spelling the only one.
    ///
    /// The line is money, not "Decimal everywhere", and the split is by what
    /// the number means rather than by which module it lives in. Prices,
    /// quantities, balances and equity are string-only wherever they decode;
    /// operator-supplied fractions and thresholds stay tolerant.
    ///
    /// String-only, and this table is only part of that set: the frames here
    /// (execution, account and market data), `risk::RiskState` and its nested
    /// `Breach` (published on `GET /account`, pinned by
    /// `a_published_risk_state_refuses_a_numeric_decimal`), and
    /// `mogwai-venue`'s `OpenAccountRequest.balances` (the `POST /accounts`
    /// opening balances, pinned by
    /// `an_opening_balance_must_be_spelled_as_a_string`).
    ///
    /// Tolerant, deliberately, and the list is exhaustive as of this round:
    /// `control::Divergence` (`POST /control/divergence`),
    /// `risk::AccountPolicy` together with the rule structs it nests
    /// (`TrailingDrawdown`, `DailyLossLimit`, `OverallDrawdown`,
    /// `MaxPosition`), and `instruments::InstrumentDef`. The last two are also
    /// TOML config, where `multiplier = 2` is the natural spelling, and all of
    /// them carry fractions and thresholds rather than a booked quantity.
    ///
    /// Nothing detects a new `Decimal` field that forgets the annotation. The
    /// table below is exhaustive by hand over this module's 35 serde `Decimal`
    /// fields across ten struct shapes - `SubmitOrder` 5, `ModifyOrder` 3,
    /// `OrderUpdated` 4, `OrderFilled` 4, `OrderStatusInfo` 4, `Balance` 3,
    /// `Position` 4, `PostedMargin` 2, `TradeTick` 2, `QuoteTick` 4. (`Hit.px`
    /// is excluded because that struct is not serde.) A new one owes its
    /// annotation and a row here, or it silently reopens the hole.
    #[test]
    fn every_wire_decimal_refuses_a_numeric_spelling() {
        // The two market-data frames, shared by the hot-path block below and by
        // the `venue` table, so the two cannot end up checking different
        // frames.
        const TRADE: &str = r#"{"type":"Trade","symbol":"BTCUSDT","price":"100.5","size":"2","aggressor":"Buyer","ts_event":1}"#;
        const QUOTE: &str = r#"{"type":"Quote","symbol":"BTCUSDT","bid_px":"99.5","ask_px":"100.5","bid_sz":"2","ask_sz":"3","ts_event":1}"#;

        // First, because the hot path has its own decoder and this is the
        // only block that reaches it: `from_json_str` short-circuits the
        // internally-tagged content buffer for Trade and Quote, so it is a
        // second decode path over the same fields. Placed after the tables it
        // could never fail first, which would make it unbite-checkable.
        //
        // Every decimal field of both frames, not a sample of them: the fields
        // do share one `Deserialize`, but this block's whole claim is that the
        // second path refuses what the first does, and a claim stated over
        // "the same fields" has to be checked over all of them.
        for (frame, fields) in [
            (TRADE, ["price", "size"].as_slice()),
            (QUOTE, ["bid_px", "ask_px", "bid_sz", "ask_sz"].as_slice()),
        ] {
            assert!(
                VenueMessage::from_json_str(frame).is_ok(),
                "the tag-probe decoder must still take the string spelling: {frame}"
            );
            for field in fields {
                let numeric = unquote(frame, field);
                assert!(
                    VenueMessage::from_json_str(&numeric).is_err(),
                    "the tag-probe decoder must refuse a numeric spelling too: {numeric}"
                );
            }
        }

        // (label, a fully valid frame, every Decimal field in it).
        //
        // The submit shape needs two rows because no single order type carries
        // all five of its decimals legally - `TrailingStopLimit` refuses
        // `price`, whose value it derives from `limit_offset`. A one-row
        // fixture spelling all five was a frame `validate_submit_order` would
        // reject; decode-only tests never call the validator, so it passed
        // while pinning a shape the venue does not serve. The loop below runs
        // the validator over every decoded `SubmitOrder`, so this cannot
        // silently happen again.
        let commands: &[(&str, &str, &[&str])] = &[
            (
                "SubmitOrder StopLimit",
                r#"{"type":"SubmitOrder","client_order_id":"O-1","symbol":"BTCUSDT","side":"Buy","order_type":"StopLimit","quantity":"2","price":"100.5","trigger_price":"99.5","time_in_force":"Gtc"}"#,
                &["quantity", "price", "trigger_price"],
            ),
            (
                "SubmitOrder TrailingStopLimit",
                r#"{"type":"SubmitOrder","client_order_id":"O-2","symbol":"BTCUSDT","side":"Buy","order_type":"TrailingStopLimit","quantity":"2","trigger_price":"99.5","trail_offset":"1.5","limit_offset":"0.5","time_in_force":"Gtc"}"#,
                &["quantity", "trigger_price", "trail_offset", "limit_offset"],
            ),
            (
                "ModifyOrder",
                r#"{"type":"ModifyOrder","client_order_id":"O-1","price":"100.5","quantity":"2","trigger_price":"99.5"}"#,
                &["price", "quantity", "trigger_price"],
            ),
        ];
        let venue: &[(&str, &str, &[&str])] = &[
            (
                "OrderUpdated",
                r#"{"type":"OrderUpdated","client_order_id":"O-1","venue_order_id":"V-1","quantity":"2","price":"100.5","trigger_price":"99.5","leaves_qty":"1","ts_event":1}"#,
                &["quantity", "price", "trigger_price", "leaves_qty"],
            ),
            (
                "OrderFilled",
                r#"{"type":"OrderFilled","client_order_id":"O-1","venue_order_id":"V-1","trade_id":"T-1","symbol":"BTCUSDT","side":"Buy","last_qty":"1","last_px":"100.5","leaves_qty":"0","commission":"0.02","commission_currency":"USDT","liquidity_side":"taker","ts_event":1}"#,
                &["last_qty", "last_px", "leaves_qty", "commission"],
            ),
            (
                "AccountState",
                r#"{"type":"AccountState","account_id":"MOGWAI-001","balances":[{"currency":"USDT","total":"9900","free":"9800","locked":"100"}],"positions":[{"symbol":"BTCUSDT","quantity":"1","avg_px":"100.5","mark_px":"101.5","unrealized_pnl":"1"}],"margins":[{"symbol":"BTCUSDT","currency":"USDT","initial":"50","maintenance":"25"}],"ts_event":1}"#,
                &[
                    "total",
                    "free",
                    "locked",
                    "quantity",
                    "avg_px",
                    "mark_px",
                    "unrealized_pnl",
                    "initial",
                    "maintenance",
                ],
            ),
            (
                "OrderStatusSnapshot",
                r#"{"type":"OrderStatusSnapshot","request_id":"Q-1","orders":[{"client_order_id":"O-1","venue_order_id":"V-1","symbol":"BTCUSDT","side":"Buy","order_type":"StopLimit","time_in_force":"Gtc","status":"Accepted","quantity":"2","filled_qty":"0.5","price":"100.5","trigger_price":"99.5","ts_accepted":1,"ts_last":2}],"ts_event":3}"#,
                &["quantity", "filled_qty", "price", "trigger_price"],
            ),
            ("Trade", TRADE, &["price", "size"]),
            ("Quote", QUOTE, &["bid_px", "ask_px", "bid_sz", "ask_sz"]),
        ];

        // Strip the quotes off one field's value, leaving the rest of the frame
        // untouched, so a refusal can only be about that field's spelling.
        //
        // The rename case - a field renamed out from under this table - is
        // caught by the `find` panic below and by nothing else. An earlier
        // draft added a trailing `assert_ne!(out, frame)` claiming to guard it;
        // `out` is `frame` minus two quote characters and so always differs, so
        // that assertion could not fail and the guard it advertised did not
        // exist.
        fn unquote(frame: &str, field: &str) -> String {
            let needle = format!("\"{field}\":\"");
            let at = frame
                .find(&needle)
                .unwrap_or_else(|| panic!("{field} is not spelled as a string in {frame}"));
            let value_start = at + needle.len();
            let value_end = value_start
                + frame[value_start..]
                    .find('"')
                    .expect("the value's closing quote");
            let mut out = String::with_capacity(frame.len());
            out.push_str(&frame[..value_start - 1]);
            out.push_str(&frame[value_start..value_end]);
            out.push_str(&frame[value_end + 1..]);
            out
        }

        for (label, frame, fields) in commands {
            let decoded = serde_json::from_str::<Command>(frame)
                .unwrap_or_else(|e| panic!("{label}: the string spelling must still decode: {e}"));
            // Every fixture is a frame the venue would actually admit. Decode
            // tests skip validation, so without this a fixture can pin an
            // illegal shape and a reader auditing the table for exhaustiveness
            // will believe that shape is legal.
            if let Command::SubmitOrder(order) = &decoded {
                validate_submit_order(order).unwrap_or_else(|e| {
                    panic!("{label}: the fixture must be a submit the venue admits: {e}")
                });
            }
            assert!(!fields.is_empty(), "{label}: no fields listed");
            for field in *fields {
                let numeric = unquote(frame, field);
                let decoded = serde_json::from_str::<Command>(&numeric);
                assert!(
                    decoded.is_err(),
                    "{label}.{field}: a numeric spelling must be refused, got {decoded:?}"
                );
            }
        }
        for (label, frame, fields) in venue {
            assert!(
                serde_json::from_str::<VenueMessage>(frame).is_ok(),
                "{label}: the string spelling must still decode"
            );
            assert!(!fields.is_empty(), "{label}: no fields listed");
            for field in *fields {
                let numeric = unquote(frame, field);
                let decoded = serde_json::from_str::<VenueMessage>(&numeric);
                assert!(
                    decoded.is_err(),
                    "{label}.{field}: a numeric spelling must be refused, got {decoded:?}"
                );
            }
        }
    }

    /// The post-subscription-retirement wire surface, pinned by byte form.
    ///
    /// `Subscribe`, `Unsubscribe` and the nine `SubscriptionIssue` variants are
    /// gone; the two that carried surviving meaning became top-level frames.
    /// Pinning the text rather than only `from(to(x)) == x` is what stops a
    /// field rename from passing here and failing in the launcher or the
    /// adapter instead.
    #[test]
    fn client_and_venue_messages_round_trip() {
        let client_frames = [
            (
                Command::CancelOrder {
                    client_order_id: "O-1".into(),
                },
                r#"{"type":"CancelOrder","client_order_id":"O-1"}"#,
            ),
            (
                Command::ModifyOrder {
                    client_order_id: "O-1".into(),
                    price: None,
                    quantity: Some(Decimal::from(2)),
                    trigger_price: None,
                },
                r#"{"type":"ModifyOrder","client_order_id":"O-1","price":null,"quantity":"2"}"#,
            ),
            (
                Command::QueryOrders {
                    request_id: "Q-1".into(),
                    client_order_id: None,
                    open_only: true,
                },
                r#"{"type":"QueryOrders","request_id":"Q-1","open_only":true}"#,
            ),
            (
                Command::QueryFills {
                    request_id: "Q-2".into(),
                    client_order_id: None,
                },
                r#"{"type":"QueryFills","request_id":"Q-2"}"#,
            ),
        ];
        for (frame, expected) in client_frames {
            let json = serde_json::to_string(&frame).expect("serialize");
            assert_eq!(json, expected);
            let decoded: Command = serde_json::from_str(&json).expect("decode");
            assert_eq!(serde_json::to_string(&decoded).expect("re-serialize"), json);
        }

        // There is no Subscribe frame left to send: the venue pushes the run's
        // one tape unbidden, so a consumer that still sends one is refused by the
        // decoder rather than silently ignored.
        assert!(
            serde_json::from_str::<Command>(r#"{"type":"Subscribe","symbols":["BTCUSDT"]}"#)
                .is_err(),
            "Subscribe was retired with the subscription model"
        );
        assert!(
            serde_json::from_str::<Command>(r#"{"type":"Unsubscribe"}"#).is_err(),
            "Unsubscribe was retired with the subscription model"
        );

        let venue_frames = [
            (
                VenueMessage::Quote(QuoteTick {
                    symbol: "BTCUSDT".into(),
                    bid_px: Decimal::from(99),
                    ask_px: Decimal::from(100),
                    bid_sz: Decimal::from(2),
                    ask_sz: Decimal::from(3),
                    ts_event: 7,
                }),
                r#"{"type":"Quote","symbol":"BTCUSDT","bid_px":"99","ask_px":"100","bid_sz":"2","ask_sz":"3","ts_event":7}"#,
            ),
            (
                // The other half of the tag-probe fast path. Both hot variants
                // belong in this table, because it is the only thing proving
                // the direct payload decoders produce byte-identical frames.
                VenueMessage::Trade(TradeTick {
                    symbol: "BTCUSDT".into(),
                    price: Decimal::from(99),
                    size: Decimal::from(2),
                    aggressor: AggressorSide::Buyer,
                    ts_event: 11,
                }),
                r#"{"type":"Trade","symbol":"BTCUSDT","price":"99","size":"2","aggressor":"Buyer","ts_event":11}"#,
            ),
            (
                VenueMessage::RunComplete {
                    sim_now_ns: 123,
                    elapsed_ns: 45,
                },
                r#"{"type":"RunComplete","sim_now_ns":123,"elapsed_ns":45}"#,
            ),
            (
                // A distinct tag, which is the whole point: an older consumer
                // fails to decode this rather than reading it as a finished run.
                VenueMessage::PassengerDurationComplete {
                    sim_now_ns: 123,
                    elapsed_ns: 45,
                    declared_duration_ns: 40,
                },
                r#"{"type":"PassengerDurationComplete","sim_now_ns":123,"elapsed_ns":45,"declared_duration_ns":40}"#,
            ),
            (
                // Formerly SubscriptionIssue::FeedLagged. There is no
                // subscription to attribute it to, so it is a top-level frame.
                // Both boundaries present is the ordinary resumption shape; the
                // absent cases are pinned separately below.
                VenueMessage::FeedLagged {
                    episode: 2,
                    skipped: 7,
                    skipped_total: 19,
                    after_ts_event: Some(100),
                    resumed_ts_event: Some(400),
                },
                r#"{"type":"FeedLagged","episode":2,"skipped":7,"skipped_total":19,"after_ts_event":100,"resumed_ts_event":400}"#,
            ),
            (
                // Formerly SubscriptionIssue::ReopenGapUnfireable.
                VenueMessage::HavocDiagnostic {
                    reason: "reopen gap at or before the tape origin".into(),
                    sim_now_ns: 9,
                },
                r#"{"type":"HavocDiagnostic","reason":"reopen gap at or before the tape origin","sim_now_ns":9}"#,
            ),
            (
                VenueMessage::Heartbeat { ts_event: 1 },
                r#"{"type":"Heartbeat","ts_event":1}"#,
            ),
            (
                VenueMessage::ProtocolError {
                    reason: "invalid command frame".into(),
                    ts_event: 2,
                },
                r#"{"type":"ProtocolError","reason":"invalid command frame","ts_event":2}"#,
            ),
        ];
        for (frame, expected) in venue_frames {
            let json = serde_json::to_string(&frame).expect("serialize");
            assert_eq!(json, expected);
            let decoded = VenueMessage::from_json_str(&json).expect("decode");
            assert_eq!(serde_json::to_string(&decoded).expect("re-serialize"), json);
            let decoded = VenueMessage::from_json_slice(json.as_bytes()).expect("decode bytes");
            assert_eq!(serde_json::to_string(&decoded).expect("re-serialize"), json);
        }
    }

    /// Both `FeedLagged` boundaries are optional, and each absence means a
    /// distinct thing a consumer has to be able to tell apart: no lower bound
    /// says the loss preceded the first frame this passenger was ever given, so
    /// there is no instant to aggregate back to; no upper bound says the tape
    /// ended inside the hole, so nothing resumed and the span is open.
    ///
    /// Pinned as wire shape rather than left to the round-trip list above,
    /// because `null` versus an absent key is the kind of thing a serde
    /// attribute changes silently. A consumer branching on the absence would
    /// read a skipped key as "no gap boundary" and a `null` as the same, but
    /// only if the venue keeps emitting the key at all.
    #[test]
    fn a_feed_lagged_boundary_is_absent_rather_than_dropped() {
        let unbounded_below = serde_json::to_string(&VenueMessage::FeedLagged {
            episode: 1,
            skipped: 3,
            skipped_total: 3,
            after_ts_event: None,
            resumed_ts_event: Some(70),
        })
        .expect("serialize");
        assert_eq!(
            unbounded_below,
            r#"{"type":"FeedLagged","episode":1,"skipped":3,"skipped_total":3,"after_ts_event":null,"resumed_ts_event":70}"#
        );

        let unbounded_above = serde_json::to_string(&VenueMessage::FeedLagged {
            episode: 4,
            skipped: 9,
            skipped_total: 12,
            after_ts_event: Some(70),
            resumed_ts_event: None,
        })
        .expect("serialize");
        assert_eq!(
            unbounded_above,
            r#"{"type":"FeedLagged","episode":4,"skipped":9,"skipped_total":12,"after_ts_event":70,"resumed_ts_event":null}"#
        );

        for json in [&unbounded_below, &unbounded_above] {
            let decoded = VenueMessage::from_json_str(json).expect("decode");
            assert_eq!(
                &serde_json::to_string(&decoded).expect("re-serialize"),
                json
            );
        }
    }

    /// The tag-probe decoders must accept exactly what the fully general
    /// internally tagged decoder accepts. A JSON string may spell any character
    /// as a `\uXXXX` escape, so `"type"` carrying an escaped tag is a valid
    /// frame from a noncanonical but conforming peer. A probe deserialized as
    /// a borrowed `&str` refuses those outright, which would make the public
    /// helpers narrower than the type they decode - a silent compatibility
    /// regression rather than a visible one.
    #[test]
    fn tag_probe_accepts_escaped_tags_exactly_as_the_general_decoder_does() {
        // Rewrite a canonical frame's tag so its first character is spelled as
        // a JSON `\uXXXX` escape. Built rather than written literally so the
        // escape introducer - ASCII 92 - cannot be lost to a source-level
        // escape of its own.
        let escape_tag = |canonical: &str, tag: &str| {
            let mut chars = tag.chars();
            let first = chars.next().expect("tag is non-empty") as u32;
            let escaped = format!("{}u{:04X}{}", char::from(92), first, chars.as_str());
            let rewritten = canonical.replacen(&format!("\"{tag}\""), &format!("\"{escaped}\""), 1);
            assert_ne!(rewritten, canonical, "the tag rewrite must actually apply");
            rewritten
        };

        let canonical_frames = [
            // Hot fast-path variants, which the probe dispatches directly.
            (
                "Trade",
                r#"{"type":"Trade","symbol":"BTCUSDT","price":"99","size":"2","aggressor":"Buyer","ts_event":11}"#,
            ),
            (
                "Quote",
                r#"{"type":"Quote","symbol":"BTCUSDT","bid_px":"99","ask_px":"100","bid_sz":"2","ask_sz":"3","ts_event":7}"#,
            ),
            // A cold variant, which reaches the general decoder through the
            // probe's fallback arm and must survive the same escape.
            ("Heartbeat", r#"{"type":"Heartbeat","ts_event":1}"#),
        ];
        for (tag, canonical) in canonical_frames {
            let wire = escape_tag(canonical, tag);
            let wire = wire.as_str();
            // The general decoder is the reference: it accepts these today.
            let reference = serde_json::from_str::<VenueMessage>(wire).expect("general decode");
            assert_eq!(
                serde_json::to_string(&reference).expect("re-serialize"),
                canonical
            );

            for decoded in [
                VenueMessage::from_json_str(wire).expect("escaped tag, str"),
                VenueMessage::from_json_slice(wire.as_bytes()).expect("escaped tag, slice"),
            ] {
                assert_eq!(
                    serde_json::to_string(&decoded).expect("re-serialize"),
                    canonical
                );
            }
        }

        // Symmetry: what the general decoder refuses, the helpers refuse too.
        for bad in [
            r#"{"type":"Nonesuch"}"#,
            r#"{"symbol":"BTCUSDT"}"#,
            r#"{"type":"Trade","symbol":"BTCUSDT"}"#,
        ] {
            assert!(serde_json::from_str::<VenueMessage>(bad).is_err(), "{bad}");
            assert!(VenueMessage::from_json_str(bad).is_err(), "{bad}");
            assert!(
                VenueMessage::from_json_slice(bad.as_bytes()).is_err(),
                "{bad}"
            );
        }
    }

    /// `ADMISSION_FRAME_MAX_BYTES` is what makes the priority lane's frame
    /// count a memory bound, so it must be proven rather than asserted.
    ///
    /// The old bound was 8192, sized by a list of `MAX_SUBSCRIPTION_ISSUES_LISTED`
    /// rows. With `SubscriptionIssues` retired the widest admission frame is a
    /// single `AdmissionRejected` - one capped client id, one capped reason
    /// plus a fixed envelope - so the bound was recomputed
    /// from those caps and rounded up to the next power of two. This test is
    /// the recomputation, run.
    #[test]
    fn admission_frames_fit_their_ceiling() {
        // The worst case is every capped field at its cap, in characters that
        // JSON escapes maximally - which is what JSON_ESCAPE_FACTOR prices.
        let worst_id = "\u{7}".repeat(MAX_ECHOED_ID_LEN);
        let worst_reason = "\u{7}".repeat(MAX_REASON_LEN);

        // Every subject variant, not just `Submit`. The variants differ in key
        // name, in `kind` tag width, and `Query` carries a serialized
        // `QueryKind` on top of its capped id - so which one is widest is a
        // measurement, and taking the max is what makes the constant's
        // "AdmissionRejected is the widest admission frame" claim run rather
        // than be asserted about one arbitrarily chosen variant.
        let subjects = [
            AdmissionSubject::Submit {
                client_order_id: worst_id.clone(),
            },
            AdmissionSubject::SubmitGroup {
                order_list_id: worst_id.clone(),
            },
            AdmissionSubject::Cancel {
                client_order_id: worst_id.clone(),
            },
            AdmissionSubject::Modify {
                client_order_id: worst_id.clone(),
            },
            AdmissionSubject::Query {
                request_id: worst_id.clone(),
                query: QueryKind::Orders,
            },
            AdmissionSubject::Query {
                request_id: worst_id.clone(),
                query: QueryKind::Fills,
            },
            AdmissionSubject::Frame,
        ];
        let mut widest_len = 0usize;
        for subject in subjects {
            let frame = VenueMessage::AdmissionRejected {
                subject,
                reason: worst_reason.clone(),
                retryable: true,
                ts_event: u64::MAX,
            };
            let len = serde_json::to_string(&frame).expect("serialize").len();
            assert!(
                len <= ADMISSION_FRAME_MAX_BYTES,
                "a maximal admission frame is {len} bytes, over the {ADMISSION_FRAME_MAX_BYTES} ceiling"
            );
            widest_len = widest_len.max(len);
        }

        let error = VenueMessage::ProtocolError {
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

        // The analytic bound the constant is derived from, so the constant is
        // not merely large enough for the case above by luck.
        let analytic =
            JSON_ESCAPE_FACTOR * (MAX_ECHOED_ID_LEN + MAX_REASON_LEN) + ADMISSION_ENVELOPE_BYTES;
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

    #[test]
    fn admission_subject_serialization_bounds_raw_client_ids() {
        let frame = VenueMessage::AdmissionRejected {
            subject: AdmissionSubject::Submit {
                client_order_id: "x".repeat(MAX_ECHOED_ID_LEN + 10_000),
            },
            reason: "capacity exhausted".into(),
            retryable: true,
            ts_event: 1,
        };
        let json = serde_json::to_string(&frame).expect("serialize");
        assert!(json.len() <= ADMISSION_FRAME_MAX_BYTES);
        let decoded: VenueMessage = serde_json::from_str(&json).expect("decode");
        let VenueMessage::AdmissionRejected {
            subject: AdmissionSubject::Submit { client_order_id },
            ..
        } = decoded
        else {
            panic!("wrong frame shape")
        };
        assert_eq!(client_order_id.len(), MAX_ECHOED_ID_LEN);
    }
}
