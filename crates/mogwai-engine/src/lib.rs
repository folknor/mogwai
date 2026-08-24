// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The venue-agnostic exchange core: open orders, accounts, and the divergence
//! injection layer. Protocol gateways (native JSON-over-WS, or a future Binance
//! facade) drive this engine and serialize whatever it emits.
//!
//! The engine is intentionally synchronous and side-effect free: `process` takes
//! a [`Command`] and returns the [`VenueMessage`]s to send. The venue
//! owns sockets, timers and the clock; the engine owns order and account state.
//! Fills are synthetic - mogwai never matches against a book or a market - so
//! the fill an order gets is whatever the armed divergences dictate, defaulting
//! to an immediate full fill at the order's own price. This keeps the divergence
//! behaviour deterministic and unit-testable.
//!
//! The implementation is split by concern: [`account`] owns the ledger and
//! `AccountState` snapshot, [`orders`] owns the submit/cancel/modify lifecycle,
//! and [`divergence`] owns the armed-divergence queue. This module keeps the
//! `Engine` type itself, its constructors, and the top-level `process` dispatch.

use std::collections::{HashMap, HashSet, VecDeque};

use mogwai_protocol::{
    AccountId, AccountState, ClientOrderId, Command, FillSnapshot, Hit, InstrumentDef, OrderFilled,
    OrderStatusInfo, OrderStatusSnapshot, OrderType, Position, ScanKind, Side, SimClock,
    SubmitOrder, Symbol, TimeInForce, VenueMessage, VenueOrderId, WireOrderStatus,
    control::Divergence,
};
// Only the test-gated constructors and the tests themselves seed from the
// default table; production configs name their instruments.
#[cfg(test)]
use mogwai_protocol::default_instruments;
use rust_decimal::Decimal;

mod account;
mod divergence;
mod orders;

use account::{Account, Warned};

/// Upper bound on the engine-side armed-divergence queue.
///
/// Single-shot divergences normally self-disarm on their own trigger, but a
/// `PartialFillNext` targeted at an order that never arrives has no trigger
/// and would sit armed forever (see `take_armed`). Without a cap a stream of
/// control-plane arms - or a scenario that keeps arming targeted partials whose
/// orders never show up - grows the queue without bound (a test-harness DoS).
/// This ceiling is far above any legitimate scenario's arm count, so reaching
/// it means the queue is leaking; `arm` always sheds the oldest entry at the cap,
/// exactly the accumulated never-triggered leftovers. `clear_armed` is the
/// explicit flush for the same leak.
///
/// Public because the control-plane ack names it: an armer that learns its
/// post evicted an older entry needs the cap in the same breath to know what
/// it hit.
pub const MAX_ARMED_DIVERGENCES: usize = 1_024;

/// What the venue is waiting for on one resting order. Replaces the filter the
/// scan planner used to carry: with three kinds of resting order, "Limit and
/// GTC" would have needed a second special case, and making the state explicit
/// removes both.
#[derive(Debug, Clone, Copy)]
pub enum Resting {
    /// Live limit. A print strictly through `fill_trigger_px` fills it at its
    /// own stated price.
    Limit { fill_trigger_px: Decimal },
    /// Untriggered conditional. A print touching `stop_px` triggers it.
    ///
    /// `toward` picks which direction closes the gap: a stop fires when price
    /// runs away from the position it protects, a touched order when price comes
    /// toward the level it is waiting at. The flag lives on the resting state
    /// rather than being re-derived from the order type at every scan, so the
    /// scan planner and the trigger handler cannot disagree about which
    /// predicate an order is waiting on.
    Conditional { stop_px: Decimal, toward: bool },
    /// Never scanned: a market remainder left by a partial fill, which has no
    /// meaningful price for the tape to reach. Ends only on a consumer cancel.
    Inert,
    /// An order-list child waiting for its parent to fill. Accepted, on the
    /// book, answerable to `QueryOrders` - and inert in every other respect: it
    /// is never scanned and it places no hold, because an order that
    /// cannot execute must not tie up funds the parent's own fill will need.
    ///
    /// A fill of the parent promotes it to the state it would have been given at
    /// submit - `Limit` with a freshly drawn trigger, or `Conditional` - and it
    /// places its hold then. That promotion is the whole of
    /// one-triggers-the-other.
    Held,
}

/// How many funding instants the half-open span `from_ns .. to_ns` crossed.
///
/// Instants sit on multiples of `interval_ns` from the unix epoch, which is the
/// convention every venue publishing an eight-hour cycle follows and makes the
/// schedule a property of the clock rather than of when a run happened to boot.
/// Half-open with `from_ns` exclusive, so a span abutting the previous one funds
/// each instant exactly once however the sweep passes are cut.
fn funding_instants(from_ns: u64, to_ns: u64, interval_ns: u64) -> u64 {
    if interval_ns == 0 || to_ns <= from_ns {
        return 0;
    }
    (to_ns / interval_ns).saturating_sub(from_ns / interval_ns)
}

/// A resting order tracked by the venue.
#[derive(Debug, Clone)]
pub struct OpenOrder {
    pub venue_order_id: VenueOrderId,
    pub submit: SubmitOrder,
    pub leaves_qty: Decimal,
    /// Sim unix-ns instant the venue accepted the order.
    pub ts_accepted: u64,
    /// Sim unix-ns instant of the last lifecycle activity (accept, fill,
    /// amend). Reported on `QueryOrders` replies as the row's `ts_last`.
    pub ts_last: u64,
    /// Sim unix-ns the conditional's trigger fired; `None` until it does and
    /// for every non-conditional order. Reported as the row's `ts_triggered`
    /// and preserved into the terminal truth store.
    pub ts_triggered: Option<u64>,
    /// Band half width this order was accepted under, in ticks. Held so a
    /// re-draw (reprice, partial-fill remainder) does not need a fresh tape
    /// reading it has no way to take.
    pub band_ticks: u32,
    /// What this order is waiting for: a print through its drawn band trigger,
    /// a print touching its stop, or nothing at all.
    pub resting: Resting,
    /// Number of draws this order has made. Part of the RNG key, so a reprice
    /// or a partial-fill remainder draws a fresh offset rather than reusing the
    /// one the previous tranche got. Deliberately NOT `revision`, which sweep
    /// passes bump: a key that moved with sweep timing would make the trigger a
    /// function of how often the sweeper ran.
    pub band_draw: u32,
    /// Sim unix-ns instant the trigger walk has already covered, the exclusive
    /// lower bound for the next pass. Advanced by the engine only when it accepts
    /// a result, never by the walker: a walk whose result is discarded must
    /// re-cover the same span rather than lose it.
    pub scanned_ns: u64,
    /// Bumped on every mutation of this order's identity for gating purposes -
    /// reprice, quantity amend, fill, frontier advance. A `ScanResult` carries
    /// the revision its walk was planned against, so a result computed against
    /// state that has since moved is always dropped rather than applied. Liveness
    /// alone is not enough: two overlapping walks can both name a still-resting
    /// order, and applying both double-counts the span they share.
    pub revision: u64,
}

impl OpenOrder {
    /// The order this one waits on, if it is an order-list child.
    #[must_use]
    pub fn parent_order_id(&self) -> Option<&str> {
        self.submit
            .link
            .as_ref()
            .and_then(|link| link.parent_order_id.as_deref())
    }
}

/// The one construction path for an engine. The core receives observations,
/// never a tape or clock; the fill seed roots its private trigger stream.
pub struct EngineConfig {
    pub account_id: AccountId,
    pub instruments: Vec<InstrumentDef>,
    pub balances: HashMap<String, Decimal>,
    /// Root of the fill-band RNG stream. Never the generator's stream: a draw
    /// that advanced the tape's state would make the tape a function of consumer
    /// behaviour, which is exactly the market impact this venue excludes.
    pub fill_seed: u64,
}

/// The band a liquidation close is judged against when nobody has told the
/// engine the run's own cap. Matches the venue's `fill_band_max_ticks`
/// default, so an engine built standalone behaves like a default venue.
pub const DEFAULT_LIQUIDATION_BAND_TICKS: u32 = 200;

/// The client order id prefixes the venue mints for itself, and therefore the
/// ones a consumer may not submit under. Both are venue-originated reduce-only
/// closes: `LQ-` is a margin-maintenance liquidation, `RISK-` an account-policy
/// flatten. The restriction is not cosmetic - a consumer that claims one of these
/// ids burns it in `seen_client_order_ids`, and the forced close that later
/// mints the same id is refused as a duplicate, so pre-claiming ids is a way to
/// make the venue unable to liquidate you. `RISK-` was minted without being
/// reserved, which is why the list is stated once here and read by both the
/// minting sites and the admission check rather than spelled at each.
pub const LIQUIDATION_ID_PREFIX: &str = "LQ-";
/// The account-policy flatten's prefix. See `RESERVED_ID_PREFIXES`.
pub const RISK_FLATTEN_ID_PREFIX: &str = "RISK-";
/// See `LIQUIDATION_ID_PREFIX`.
pub const RESERVED_ID_PREFIXES: [&str; 2] = [LIQUIDATION_ID_PREFIX, RISK_FLATTEN_ID_PREFIX];

/// Per-instrument collateral policy. The settlement schedule is not here: it
/// is the session calendar's `settlement_minute_of_day`, read in exchange-local
/// time, and the sweeper strikes each instant it names.
#[derive(Debug, Clone, Copy)]
pub struct MarginPolicy {
    /// What one contract costs to open and to hold.
    ///
    /// A fixed currency amount per contract, which is how exchange-listed
    /// futures state performance bonds: CME publishes a dollar figure per MNQ,
    /// not a ratio. `basis` decides whether these are read that way or as
    /// leverage ratios; the fields are shared because the arithmetic downstream
    /// is the same shape either way.
    pub initial_per_contract: Decimal,
    pub maintenance_per_contract: Decimal,
    pub breach_action: MarginBreachAction,
    pub basis: MarginBasis,
}

/// How a margin requirement is derived, which is the difference between a
/// futures account and a leveraged one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MarginBasis {
    /// A fixed amount of settlement currency per contract, whatever the price.
    /// Exchange-listed futures.
    #[default]
    PerContract,
    /// A fraction of notional, so the requirement moves with the price. This is
    /// what forex, crypto margin and Reg-T equity margin actually do, and it is
    /// the account type the venue had no way to express: "10x leverage" is
    /// `initial = 0.1`, and holding it as a per-contract constant would make a
    /// position that doubled in value cost the same to hold as when it opened.
    Notional,
}

impl MarginPolicy {
    /// Initial margin owed on `qty` at `px`.
    #[must_use]
    pub fn initial(&self, def: &InstrumentDef, qty: Decimal, px: Decimal) -> Decimal {
        self.required(self.initial_per_contract, def, qty, px)
    }

    /// Maintenance margin owed on `qty` at `px`.
    #[must_use]
    pub fn maintenance(&self, def: &InstrumentDef, qty: Decimal, px: Decimal) -> Decimal {
        self.required(self.maintenance_per_contract, def, qty, px)
    }

    fn required(&self, rate: Decimal, def: &InstrumentDef, qty: Decimal, px: Decimal) -> Decimal {
        match self.basis {
            MarginBasis::PerContract => rate.saturating_mul(qty.abs()),
            // Through `notional`, so an `Inverse` contract's requirement is
            // computed in its settlement asset rather than in the currency it
            // happens to be quoted in.
            MarginBasis::Notional => def
                .notional(qty.abs(), px)
                .and_then(|notional| notional.checked_mul(rate))
                .unwrap_or(Decimal::MAX),
        }
    }
}

/// One position's unrealized P and L through the instrument's OWN arithmetic,
/// `None` only when the arithmetic overflowed.
///
/// The single expression every unrealized reader in this crate uses -
/// `unrealized_pnl` (and thence the margin breach test), the `positions()` wire
/// rows, `settle`'s realized credit and `valuation_at`'s derivative
/// contribution. It exists because those readers each carried a hand-rolled
/// `(mark - avg) * qty * multiplier`, which is the linear form: for an
/// `Inverse` contract that is wrong by up to four orders of magnitude, so a
/// coin-margined book was liquidated on a number `apply_fill` disagreed with
/// while booking the same position's realized P and L through `InstrumentDef`.
/// (It is wrong in magnitude only - `1/avg - 1/mark` and `mark - avg` always
/// carry the same sign, so an earlier claim of a sign error here was false.)
/// `apply_fill`'s rule - realized and unrealized must come from the same
/// expression - now holds by construction rather than by comment.
///
/// Undefined is not overflow, and this function is where the two are separated,
/// because `InstrumentDef::unrealized` answers `None` for both.
///
/// - A flat position is always zero. A closed position is stored with `qty`,
///   `avg_px` and `mark_px` all zero and is never removed from the map, so
///   every reader would otherwise have to guard it and `valuation_at` would
///   answer `None` for a whole account holding one flat inverse row.
/// - An inverse position at a zero price is always zero. The inverse form is
///   `1/price`, which has no value at zero, and a zero price is reachable:
///   `apply_fill` warns about a zero-price fill and books it, and `settle`
///   passes the caller's settlement price through with no guard of its own.
///   Answering zero says the position contributed no mark-to-market, which is
///   the conservative reading; treating it as overflow credited `Decimal::MAX`
///   to the balance. Whether the venue should instead refuse a zero price on an inverse
///   instrument is a product question, filed in `notes/todo.md`.
fn position_unrealized_checked(
    def: &mogwai_protocol::InstrumentDef,
    qty: Decimal,
    avg_px: Decimal,
    mark_px: Decimal,
) -> Option<Decimal> {
    if qty.is_zero() {
        return Some(Decimal::ZERO);
    }
    if def.class.is_inverse() && (avg_px.is_zero() || mark_px.is_zero()) {
        return Some(Decimal::ZERO);
    }
    def.unrealized(qty, avg_px, mark_px)
}

/// [`position_unrealized_checked`], saturating in the position's direction on
/// overflow.
///
/// For the readers that must answer with a number - the margin breach test, the
/// wire rows, the settlement credit. `valuation_at` uses the checked form
/// instead, because a valuation that saturated would be a lie rather than a
/// bound.
fn position_unrealized(
    def: &mogwai_protocol::InstrumentDef,
    qty: Decimal,
    avg_px: Decimal,
    mark_px: Decimal,
) -> Decimal {
    position_unrealized_checked(def, qty, avg_px, mark_px).unwrap_or(if qty.is_sign_negative() {
        Decimal::MIN
    } else {
        Decimal::MAX
    })
}

/// What a margin breach does: refuse the order that would breach, or liquidate
/// the position that already has.
///
/// Named for margin rather than for breach because
/// `mogwai_protocol::risk::BreachAction` is the other breach in this
/// workspace, naming what an account-policy rule does when its budget is
/// spent, and the two carry disjoint variants for disjoint mechanisms. They
/// shared the bare name
/// `BreachAction` until 2026-08-23, which made a wrong import a plausible edit
/// rather than an impossible one: both derive `Default`, so a
/// `breach_action: Default::default()` under the wrong import compiles and
/// silently arms the other subsystem's default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MarginBreachAction {
    #[default]
    Refuse,
    Liquidate,
}

pub struct MarkOutcome {
    pub events: Vec<VenueMessage>,
    pub originated_orders: usize,
}

#[derive(Debug, Clone, Copy)]
pub enum FeeRate {
    BasisPoints { rate: Decimal },
    PerContract { amount: Decimal },
}

#[derive(Debug, Clone, Copy)]
pub struct FeeSchedule {
    pub maker: FeeRate,
    pub taker: FeeRate,
}

#[derive(Debug, Clone, Copy)]
struct FeeSurchargeWindow {
    mult: Decimal,
    wall_armed_ns: u64,
    sim_span_ns: u64,
}

/// What the venue read off its own clean tape at the instant a command arrived.
#[derive(Debug, Clone, Copy)]
pub struct MarketReading {
    /// Last print at or before that instant. Never a look-ahead.
    pub last_px: Decimal,
    /// Instant of that canonical last print.
    pub ts_ns: u64,
    /// Band half width in ticks, already scaled by trailing realized volatility
    /// and clamped by the venue. The engine multiplies it by the instrument's
    /// price increment, because the instrument table lives here.
    pub band_ticks: u32,
}

impl EngineConfig {
    /// A config carrying the placeholder identity `Engine::UNBOUND_ACCOUNT_ID`,
    /// for tests that never put a snapshot on the wire.
    ///
    /// Test-only on purpose: an engine built from this stamps the literal
    /// `UNBOUND` on every `AccountState.account_id` it produces, and a snapshot
    /// is only self-describing if that field is the account the ledger belongs
    /// to. Production builds an `EngineConfig` by hand, which requires the real
    /// id. The gate is what keeps the placeholder off the wire, rather than the
    /// convention that nobody calls this.
    #[cfg(test)]
    #[must_use]
    pub fn unbound(instruments: Vec<InstrumentDef>) -> Self {
        Self {
            account_id: AccountId::parse(Engine::UNBOUND_ACCOUNT_ID).expect("static account id"),
            instruments,
            balances: HashMap::new(),
            fill_seed: 0,
        }
    }
}

/// One resting order the caller must walk the tape for, and the trigger price
/// that decides it. The engine hands these out; the venue walks the tape and
/// hands back `ScanResult`s. This is the whole seam - the engine never sees a
/// tick.
#[derive(Debug, Clone)]
pub struct PendingScan {
    pub client_order_id: ClientOrderId,
    pub symbol: Symbol,
    pub side: Side,
    /// The price the predicate is applied against: a live limit's drawn band
    /// trigger, or an untriggered conditional's stop price. The stated price a
    /// fill books at stays inside the engine.
    pub px: Decimal,
    /// Which predicate the walk applies to `px`.
    pub kind: ScanKind,
    /// Exclusive lower bound of the span still to walk.
    pub from_ns: u64,
    /// The order state this scan was planned against. Echoed back on the
    /// `ScanResult` and checked under the engine lock.
    pub revision: u64,
}

/// Result of one walk, handed back for the engine to apply under the lock.
#[derive(Debug, Clone)]
pub struct ScanResult {
    pub client_order_id: ClientOrderId,
    /// Echoed from the `PendingScan`: the span's exclusive lower bound and the
    /// order revision the walk assumed. Both are checked before the result is
    /// applied.
    pub from_ns: u64,
    pub revision: u64,
    /// The print that satisfied the scan in `(from_ns, scanned_to_ns]`, or
    /// `None` if the span held nothing. The first such print - there is no
    /// accumulation - and a triggered stop-market prices its fill off it.
    pub hit: Option<Hit>,
    /// The instant the walk actually reached, which its drain budget may have
    /// cut short of the pass's target. The frontier advances to exactly this,
    /// never past it.
    pub scanned_to_ns: u64,
}

/// Resting orders carry stable acceptance metadata while command and
/// sweep-result lookups use the id index. Removal may swap slots; consumers
/// sort by that metadata before order reaches a wire event or snapshot.
#[derive(Debug, Default)]
struct OpenBook {
    orders: Vec<OpenOrder>,
    by_client_id: HashMap<ClientOrderId, usize>,
}

impl OpenBook {
    fn push(&mut self, order: OpenOrder) {
        let pos = self.orders.len();
        let replaced = self
            .by_client_id
            .insert(order.submit.client_order_id.clone(), pos);
        debug_assert!(replaced.is_none(), "open client order ids are unique");
        self.orders.push(order);
    }

    fn remove(&mut self, pos: usize) -> OpenOrder {
        let order = self.orders.swap_remove(pos);
        self.by_client_id.remove(&order.submit.client_order_id);
        if pos < self.orders.len() {
            self.by_client_id
                .insert(self.orders[pos].submit.client_order_id.clone(), pos);
        }
        order
    }

    fn position(&self, client_order_id: &str) -> Option<usize> {
        self.by_client_id.get(client_order_id).copied()
    }

    fn iter(&self) -> std::slice::Iter<'_, OpenOrder> {
        self.orders.iter()
    }

    fn len(&self) -> usize {
        self.orders.len()
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.orders.is_empty()
    }
}

impl std::ops::Index<usize> for OpenBook {
    type Output = OpenOrder;

    fn index(&self, index: usize) -> &Self::Output {
        &self.orders[index]
    }
}

impl std::ops::IndexMut<usize> for OpenBook {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        &mut self.orders[index]
    }
}

impl<'a> IntoIterator for &'a OpenBook {
    type Item = &'a OpenOrder;
    type IntoIter = std::slice::Iter<'a, OpenOrder>;

    fn into_iter(self) -> Self::IntoIter {
        self.orders.iter()
    }
}

#[derive(Debug)]
pub struct Engine {
    account_id: AccountId,
    open: OpenBook,
    /// Aggregate resting-order holds by currency. Position maintenance
    /// is folded separately because position count is independent of book
    /// depth; the hot funds path must not walk every resting order.
    order_holds: HashMap<String, Decimal>,
    /// A saturated aggregate cannot be decremented soundly. The next removal
    /// rebuilds the cache from the authoritative orders instead.
    order_holds_clipped: HashSet<String>,
    account: Account,
    /// Whether submits and amends are checked against free balance. Set once
    /// at construction: a funded account (non-empty seed) is an honest cash
    /// venue, so an order the account cannot cover is rejected like a real
    /// exchange would - otherwise the ledger goes negative and a nautilus
    /// cash-account consumer refuses every snapshot after it, silently
    /// desyncing. An unfunded account keeps the permissive delta-off-zero
    /// ledger with no funds checks: its documented purpose is exercising
    /// exactly that negative-balance path, which enforcement would make
    /// unreachable. Constructor-time, not derived from the live balance map:
    /// fills create balance entries as they book, so a dynamic check would
    /// silently flip an unfunded account into enforcing after its first fill.
    enforce_funds: bool,
    /// `InstrumentDef` (from `mogwai-protocol`) is used directly as the engine's
    /// instrument representation - it carries exactly the base/quote and
    /// precision/increment fields the fill and hold path needs, so the
    /// engine keeps no parallel struct that could drift from the wire type.
    instruments: HashMap<Symbol, InstrumentDef>,
    /// Every accepted client order id, mapped to the venue order id it was
    /// assigned. Never cleared (a deliberate, unbounded retention): key
    /// presence distinguishes "was once a real order, now terminal" from
    /// "never accepted at all", and the retained venue id lets a cancel/modify
    /// reject for a terminal order still name the order it targets - the wire
    /// contract says `venue_order_id` is absent only for genuinely unknown ids.
    seen_client_order_ids: HashMap<ClientOrderId, VenueOrderId>,
    /// Terminal order records, the closed half of the `QueryOrders` truth
    /// store: every order that reached `Filled` or `Canceled`, frozen at its
    /// terminal transition. Retention is unbounded on purpose, matching
    /// `seen_client_order_ids`: reconciliation must be able to ask about an
    /// order regardless of how long ago it closed, and a test-lifetime venue
    /// accumulates orders at test scale, not exchange scale.
    closed: HashMap<ClientOrderId, OrderStatusInfo>,
    /// Every fill as it booked, in booking order - the `QueryFills` truth
    /// store. One entry per booked fill regardless of wire duplication (a
    /// `DuplicateNextFill` doubles the event, not this record), so the reply
    /// is the ground truth a corrupted `OrderFilled` stream reconciles
    /// against. Unbounded for the same reason as `closed`.
    fills: Vec<OrderFilled>,
    /// Armed divergences, consumed as their trigger fires.
    armed: VecDeque<Divergence>,
    venue_order_seq: u64,
    trade_seq: u64,
    position_seq: u64,
    /// The last price each symbol was marked at, kept for every class rather
    /// than only the ones that post margin. This is what lets a spot holding be
    /// valued: the base asset sits in the ledger as a currency balance and the
    /// pair that quotes it is the only thing that can price it.
    ///
    /// Keyed by symbol, and that is not a limitation to route around. One
    /// account holds one position in an instrument, so one symbol has one mark
    /// by construction.
    ///
    /// The gotcha it produces, recorded so it is not re-filed as a defect and
    /// not closed by a refusal. River identity carries the generator arm, so
    /// clean MNQ and surged MNQ are two rivers wearing one symbol. An account
    /// riding both has two boats at two simulated instants writing this one
    /// entry in turn, so the position is marked from whichever swept last.
    ///
    /// That shape is allowed and the venue serves it. It is not refused at
    /// admission and must not become one: a symbol is a request parameter and
    /// the venue gates on nothing. It simply has no coherent answer to give -
    /// the request asks one book to hold one instrument at two prices, and one
    /// account holds one position in an instrument. Keying a mark by river
    /// instead would mean an account with two MNQ positions, which is a worse
    /// ledger model than the ambiguity it removes. So the consequence belongs to
    /// whoever asked for it, and nothing here is owed a fix. Owner, 2026-08-23.
    ///
    /// Two rivers wearing two different symbols are not this, and carry no
    /// ambiguity at all: their marks land in disjoint entries, each position is
    /// marked from its own river, and a valuation summing them is stale by the
    /// mark cadence rather than wrong.
    last_marks: HashMap<Symbol, Decimal>,
    liquidation_seq: u64,
    warned: Warned,
    fill_seed: u64,
    margin: HashMap<Symbol, MarginPolicy>,
    fees: HashMap<Symbol, FeeSchedule>,
    margin_breached: HashSet<Symbol>,
    liquidation_band_ticks: u32,
    fee_surcharge: Option<FeeSurchargeWindow>,
    /// The clock of the boat whose pass is currently being processed.
    ///
    /// The engine is per-account, and an account's sockets may sit on several
    /// boats, but every pass through it belongs to exactly one of them. The fee
    /// surcharge is the only state that must be judged on that boat's axis. Carrying it as one field set at each pass entry
    /// (`process_with_market_on_clock`, `apply_scans_on_clock`) rather than as
    /// a parameter threaded through the fill-booking helpers is a deliberate
    /// narrowing: those helpers are reached from a dozen places and none of the
    /// others has any business knowing a clock.
    ///
    /// The invariant that keeps it honest: every entry point that can book a
    /// fill sets this first. `settle` and `mark` do not, because they are only
    /// ever called inside a pass that already did, and their venue-originated
    /// fills pay no surcharge. A new entry point that books fills owes an
    /// assignment here; without one it silently bills on the previous pass's
    /// boat.
    event_sim: SimClock,
    oms_type: mogwai_protocol::OmsType,
}

impl Engine {
    /// Register an unseen instrument without disturbing existing venue state.
    pub fn ensure_instrument(&mut self, def: InstrumentDef) -> bool {
        match self.instruments.entry(std::sync::Arc::clone(&def.symbol)) {
            std::collections::hash_map::Entry::Occupied(_) => false,
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(def);
                true
            }
        }
    }

    pub fn set_margin_policy(&mut self, symbol: Symbol, policy: MarginPolicy) {
        self.set_margin_policies(std::iter::once((symbol, policy)));
    }

    /// Install several margin policies and rebuild the resting-order hold
    /// aggregate once.
    ///
    /// Public callers may replace policy after orders already rest. Their holds
    /// still derive from `order_hold`; rebuild the aggregate rather than
    /// teaching this setter a second margin formula. That reasoning is why the
    /// single-symbol setter rebuilds, and it is unchanged here - only the
    /// number of rebuilds is.
    ///
    /// One rebuild after all the installs is identical to one rebuild per
    /// install. `rebuild_order_holds_excluding(None)` discards the cache and
    /// recomputes it from `compute_order_holds`, which folds the open book and
    /// reads `self.margin` at that moment; it carries nothing over from the
    /// previous cache. So the result is a function of the resting orders and
    /// the policy map as they stand when it runs, and neither is touched
    /// between the installs. Every intermediate rebuild is therefore a value
    /// that no caller can observe - nothing between the installs reads
    /// `order_holds`, and the debug `reconcile_order_holds` invariant compares
    /// the cache against that same fold, so it is satisfied by the final state
    /// exactly as it would have been by each intermediate one.
    ///
    /// A caller installing one policy per instrument is the reason this exists:
    /// through the single setter that walk is quadratic in the symbol count,
    /// since each install refolds every open order. No caller in this workspace
    /// installs more than one policy at a time today, so this exists for the
    /// one that does it next rather than to fix a live cost.
    pub fn set_margin_policies<I>(&mut self, policies: I)
    where
        I: IntoIterator<Item = (Symbol, MarginPolicy)>,
    {
        for (symbol, policy) in policies {
            self.margin.insert(symbol, policy);
        }
        self.rebuild_order_holds_excluding(None);
    }

    pub fn set_fee_schedule(&mut self, symbol: Symbol, schedule: FeeSchedule) {
        self.fees.insert(symbol, schedule);
    }

    /// The band a venue-originated liquidation close is judged against, in
    /// ticks. The venue sets it from `fill_band_max_ticks`; an engine built
    /// without one keeps [`DEFAULT_LIQUIDATION_BAND_TICKS`].
    pub fn set_liquidation_band_ticks(&mut self, band_ticks: u32) {
        self.liquidation_band_ticks = band_ticks;
    }

    pub fn set_oms_type(&mut self, oms_type: mogwai_protocol::OmsType) {
        self.oms_type = oms_type;
    }

    fn position_key_id(&self, position_id: Option<&String>) -> Option<String> {
        match self.oms_type {
            mogwai_protocol::OmsType::Netting => None,
            mogwai_protocol::OmsType::Hedging => position_id.cloned(),
        }
    }

    /// Arm the surcharge for a simulated span, stamped at a wall instant.
    ///
    /// Deliberately not an absolute `start..end` on one sim axis: the venue has
    /// no single such axis, and an interval on boat A's axis applied verbatim
    /// to boat B's orders fires for the wrong span or never.
    pub fn arm_fee_surcharge(&mut self, mult: Decimal, wall_armed_ns: u64, sim_span_ns: u64) {
        self.fee_surcharge = Some(FeeSurchargeWindow {
            mult,
            wall_armed_ns,
            sim_span_ns,
        });
    }

    pub fn clear_fee_surcharge(&mut self) {
        self.fee_surcharge = None;
    }

    /// Whether `ts` - a timestamp sampled on `sim` - falls inside the armed
    /// surcharge window, judged ON `sim`.
    ///
    /// The window names no axis of its own: it is a wall arming instant plus a
    /// simulated span, so one arm means the same number of simulated
    /// milliseconds to a slow boat and a fast one. The late-boarder rule opens
    /// it at `max(sim.sim_ns(armed), sim.sim_epoch_ns)`, so a boat whose anchor
    /// is later than the arm gets the full span from its own epoch instead of a
    /// window that already closed in its past.
    pub(crate) fn fee_surcharge_multiplier_for(&self, sim: SimClock, ts: u64) -> Decimal {
        self.fee_surcharge.map_or(Decimal::ONE, |window| {
            let opening = sim.window_opening(window.wall_armed_ns);
            if ts >= opening && ts < opening.saturating_add(window.sim_span_ns) {
                window.mult
            } else {
                Decimal::ONE
            }
        })
    }

    /// The river a resting order belongs to, so a control targeting that order
    /// by id can resolve the clock its timestamps live on. `None` for an id
    /// that is unknown or already terminal - exactly the ids
    /// `cancel_open_order_silently` refuses, and resolved through the same
    /// index so the two can never disagree about what is open.
    #[must_use]
    pub fn open_order_symbol(&self, client_order_id: &str) -> Option<Symbol> {
        self.open
            .position(client_order_id)
            .map(|pos| std::sync::Arc::clone(&self.open[pos].submit.symbol))
    }

    #[must_use]
    pub fn futures_mark_symbols(&self) -> Vec<Symbol> {
        let mut symbols: Vec<_> = self
            .account
            .positions
            .keys()
            .map(|(symbol, _)| symbol)
            .chain(self.open.iter().map(|order| &order.submit.symbol))
            .filter(|symbol| self.margin.contains_key(*symbol))
            .cloned()
            .collect();
        symbols.sort();
        symbols.dedup();
        symbols
    }

    /// Whether this ledger holds a balance line in `currency` at all.
    ///
    /// Presence, not sufficiency. An account funded in a currency can still run
    /// out of it, and that is depletion - a trading outcome. An account with no
    /// line at all was never funded for the shape it is asking to trade, which
    /// is a configuration mistake knowable with no order at all. Collapsing the
    /// two would make a typo look like a market result and waste a whole run.
    #[must_use]
    pub fn is_funded_in(&self, currency: &str) -> bool {
        self.account.balances.contains_key(currency)
    }

    /// Every symbol this account needs a price for in order to state its worth.
    ///
    /// A superset of `futures_mark_symbols`: that answers "what must be marked
    /// to run the margin ledger", this answers "what must be priced to value the
    /// account". The difference is spot. A spot fill credits the base asset as a
    /// currency balance, so an account holding BTC is worth nothing statable
    /// until something prices BTC, and the instrument that prices it is the pair
    /// whose base it is.
    ///
    /// Derived from the balances rather than from the positions, because the
    /// balance is what the account actually holds: a position may be closed
    /// while the asset it was opened with is still sitting in the ledger.
    pub fn valuation_symbols(&self) -> Vec<Symbol> {
        let mut symbols = self.futures_mark_symbols();
        // Every marked position, not only the margin-posting ones: an equity or
        // a perpetual is worth nothing statable until its own symbol is priced,
        // and `futures_mark_symbols` only answers what the margin ledger needs.
        for (symbol, _) in self
            .account
            .positions
            .keys()
            .filter(|(symbol, _)| {
                self.instruments
                    .get(symbol)
                    .is_some_and(|def| def.class.is_marked())
            })
            .cloned()
        {
            symbols.push(symbol);
        }
        for (symbol, def) in &self.instruments {
            let Some(base) = def.class.base_currency() else {
                continue;
            };
            if self
                .account
                .balances
                .get(base)
                .is_some_and(|held| !held.is_zero())
            {
                symbols.push(Symbol::clone(symbol));
            }
        }
        // A perpetual that funds against another symbol needs that symbol
        // priced, even when the account does not hold it. The sweeper still
        // refuses to materialize a river nobody asked for; this only names
        // what to read if the index river already exists.
        for def in self.instruments.values() {
            if let Some(index) = def.class.funding().and_then(|terms| terms.index_symbol) {
                symbols.push(index.into());
            }
        }
        symbols.sort();
        symbols.dedup();
        symbols
    }

    /// What this account is worth, stated in `currency`.
    ///
    /// The balance in `currency`, plus every other currency balance valued at
    /// the last price of an instrument quoting it in `currency`, plus the
    /// unrealized on positions - which futures carry in their settlement
    /// currency.
    ///
    /// `None` when the account holds something nothing prices in `currency`.
    /// Refusing to answer is the point: a risk threshold judged against a number
    /// that silently omitted part of the account would look enforced while
    /// enforcing the wrong thing.
    ///
    /// Why equity is not a sum of balances, which is what this replaced. A spot
    /// fill credits the base asset as a currency balance and debits the quote
    /// (see `apply_fill`), so buying 1 BTC at 60,000 leaves `BTC: 1` beside
    /// `USDT: -60,000`. Adding those totals values one unit of any asset at one
    /// unit of any other, so the purchase reads as a 59,999 loss and fires a
    /// trailing drawdown on the account's first buy. That version shipped for
    /// one commit. Futures never had the problem: a future moves only its
    /// settlement currency and carries its own unrealized.
    ///
    /// Hence the three standing rules. A policy must name the currency it is
    /// stated in. An order that would leave the account holding something
    /// nothing prices is refused at entry. An account that reaches an
    /// unvaluable state some other way is warned about and not enforced
    /// against, because enforcing on a wrong number is worse than not
    /// enforcing while looking enforced.
    ///
    /// A last mark, not a live quote. The value is only as fresh as the last
    /// `mark` this engine saw, which the fill sweeper drives once per pass, so
    /// this inherits exactly the staleness the margin ledger already runs on.
    /// [`Engine::valuation_at`] is how a caller holding a fresher price - the
    /// extremes the tape reached between two passes - asks the same question at
    /// that price instead.
    pub fn valuation_in(&self, currency: &str) -> Option<Decimal> {
        self.valuation_at(currency, &[])
    }

    /// What this account would be worth in `currency` if the named symbols were
    /// priced at the given prices instead of at their last marks.
    ///
    /// This is what makes tick-resolution risk possible without a tick-resolution
    /// evaluation. Equity is monotone in the price of a held instrument, so its
    /// extreme over a span is attained at a price extreme; asking this question
    /// at the span's high and its low answers what a per-tick walk would have
    /// found, at two evaluations rather than thousands. The engine lock is taken
    /// once, by the sweeper, exactly as before.
    ///
    /// Monotone, not linear, and the distinction is load-bearing now that the
    /// derivative contribution runs the instrument's own arithmetic. A linear
    /// class contributes `(mark - avg) * qty * multiplier`, which is affine in
    /// `mark`; an `Inverse` contributes `(1/avg - 1/mark) * qty * multiplier`,
    /// which is strictly convex or concave and never affine. What the two-point
    /// evaluation actually needs is only that the contribution never turns
    /// around inside the span, and `-1/mark` is monotonically increasing over
    /// every positive price, so it does not. The property fails only at a price
    /// of zero, which is not a price the tape's extremes can take and which
    /// [`position_unrealized_checked`] answers as zero in any case.
    ///
    /// An override for a symbol the account does not hold changes nothing, and a
    /// symbol with no override keeps its last mark - so a span on one river
    /// leaves every other symbol's contribution exactly where the last pass put
    /// it.
    pub fn valuation_at(&self, currency: &str, at: &[(Symbol, Decimal)]) -> Option<Decimal> {
        let overridden = |symbol: &Symbol| -> Option<Decimal> {
            at.iter()
                .find(|(candidate, _)| candidate == symbol)
                .map(|(_, px)| *px)
        };
        let priced = |symbol: &Symbol| -> Option<Decimal> {
            overridden(symbol).or_else(|| self.last_marks.get(symbol).copied())
        };
        let mut total = self
            .account
            .balances
            .get(currency)
            .copied()
            .unwrap_or(Decimal::ZERO);
        for (held_currency, amount) in &self.account.balances {
            if held_currency == currency || amount.is_zero() {
                continue;
            }
            let price = self.instruments.iter().find_map(|(symbol, def)| {
                (def.class.base_currency() == Some(held_currency.as_str())
                    && def.class.settlement_currency() == currency)
                    .then(|| priced(symbol))
                    .flatten()
            })?;
            total = total.checked_add(amount.checked_mul(price)?)?;
        }
        for ((symbol, _), position) in &self.account.positions {
            let Some(def) = self.instruments.get(symbol) else {
                continue;
            };
            if !def.class.is_marked() || def.class.settlement_currency() != currency {
                continue;
            }
            // Equity contributes its market value, every other marked class its
            // unrealized. The difference is what the cash leg already did: a
            // share purchase debited the whole notional, so the shares must be
            // added back at their current worth or the account reads as having
            // spent the money and received nothing. A derivative's cash never
            // moved on open, so only the gain since entry is outstanding.
            // A position carries its own mark, which is not always the last mark
            // of its symbol - a fresh position is marked at its fill price
            // before any pass has marked it - so only an explicit override
            // displaces it.
            // Through the same one expression the other unrealized readers use,
            // in its checked form: a flat inverse row (stored with every field
            // zero and never removed from the map) made the raw
            // `InstrumentDef::unrealized` answer `None`, and this `?` turned
            // that into a `None` for the whole account - the risk sweep silently
            // declining to value an account that held one closed coin-margined
            // position. `None` survives here for genuine overflow only, which is
            // the case this function's refusal-to-answer contract is about.
            let mark_px = overridden(symbol).unwrap_or(position.mark_px);
            let contribution =
                if matches!(def.class, mogwai_protocol::InstrumentClass::Equity { .. }) {
                    def.notional(position.qty, mark_px)?
                } else {
                    position_unrealized_checked(def, position.qty, position.avg_px, mark_px)?
                };
            total = total.checked_add(contribution)?;
        }
        Some(total)
    }

    #[must_use]
    pub fn unrealized_pnl(&self, symbol: &str) -> Decimal {
        let Some(def) = self
            .instruments
            .get(symbol)
            .filter(|def| def.class.is_future())
        else {
            return Decimal::ZERO;
        };
        self.account
            .positions
            .iter()
            .filter(|((position_symbol, _), _)| position_symbol.as_ref() == symbol)
            .fold(Decimal::ZERO, |sum, (_, position)| {
                sum.saturating_add(position_unrealized(
                    def,
                    position.qty,
                    position.avg_px,
                    position.mark_px,
                ))
            })
    }

    pub fn mark(&mut self, marks: &[(Symbol, Decimal)], ts: u64) -> MarkOutcome {
        self.mark_over(marks, &[], ts)
    }

    /// As `mark`, told what the tape's high and low were over the span this mark
    /// closes.
    ///
    /// The mark itself is still the span's closing price - that is what a
    /// position is worth now, and nothing about the extremes changes it. What
    /// the extremes decide is the trailing stop: a trail follows the running
    /// extreme rather than the close, so a spike between two passes drags it
    /// even though the price came back. Passing an empty slice is the
    /// pre-extremes behaviour and is what every caller with no tape under it
    /// does.
    pub fn mark_over(
        &mut self,
        marks: &[(Symbol, Decimal)],
        extremes: &[(Symbol, Decimal, Decimal)],
        ts: u64,
    ) -> MarkOutcome {
        let mut moved = false;
        // Recorded for every class, before the futures-only position update
        // below. A spot pair posts no margin and holds no marked position, so
        // nothing else here would remember its price - and without it the base
        // asset sitting in the ledger cannot be valued.
        for (symbol, mark) in marks {
            self.last_marks.insert(Symbol::clone(symbol), *mark);
        }
        for (symbol, mark) in marks {
            for position in
                self.account
                    .positions
                    .iter_mut()
                    .filter_map(|((position_symbol, _), position)| {
                        (position_symbol == symbol).then_some(position)
                    })
            {
                if self
                    .instruments
                    .get(symbol)
                    .is_some_and(|def| def.class.is_marked())
                    && position.mark_px != *mark
                {
                    position.mark_px = *mark;
                    moved = true;
                }
            }
        }
        // The trail follows the marks this pass saw, before the margin walk, so
        // a stop that just ratcheted is the one a breach liquidation sees.
        self.ratchet_trailing_stops(marks, extremes, ts);
        let mut events = Vec::new();
        let originated_orders = self.apply_margin_breaches(marks, ts, &mut events);
        if moved || originated_orders > 0 {
            events.retain(|event| !matches!(event, VenueMessage::AccountState(_)));
            // Venue maintenance, so this consults no `DropNextAccountUpdate`
            // and calls `push_account_snapshot` not at all - one of the three
            // sites that helper's doc names. A re-mark is not a consumer command
            // and originates its own liquidations, so spending an arm the
            // consumer aimed at its own next fill would burn it on the venue's
            // act. `apply_margin_breaches` is entered with
            // `apply_divergences` false throughout for the same reason.
            events.push(VenueMessage::AccountState(self.snapshot(ts)));
        }
        MarkOutcome {
            events,
            originated_orders,
        }
    }

    /// Posted collateral, one row per symbol - never one per position. Two
    /// hedged positions in the same symbol post against one instrument, and
    /// `book_shape().margins` counts symbols, so a per-position row would both
    /// misreport the requirement and under-reserve the admission budget.
    ///
    /// `maintenance` is what the open positions require; `initial` is what the
    /// resting non-reduce-only orders require. Ordinary orders contribute their
    /// own hold; margin-equity sells contribute the same aggregate allocation
    /// used by the balance cache. Reduce-only orders place no hold and appear
    /// here as nothing.
    ///
    /// What reconciles, exactly. Every `initial` row here is a settlement-
    /// currency term `held_balances` also folds, computed by the same
    /// expression, so no `initial` row can disagree with the hold it reports.
    /// The reported `locked` is nonetheless a wider sum than
    /// `sum(initial) + sum(maintenance)`, and the difference is not a defect on
    /// either side - it is three deliberate carve-outs:
    ///
    /// - `held_balances` also folds `account.unsettled` sale proceeds, which
    ///   are not collateral and have no margin row;
    /// - it also folds holds on symbols carrying no margin policy, and on
    ///   unmarked classes, which this function skips because a spot symbol posts
    ///   no collateral;
    /// - a `Hold::Base` hold - the spot sell's base-currency hold - is
    ///   skipped here, because a margin row is denominated in the settlement
    ///   currency and a base-currency amount cannot be added to it.
    ///
    /// So the reconciliation is an equality only on an account whose whole
    /// locked balance comes from margined, marked symbols with nothing
    /// unsettled, and it is a `<=` in general. Do not read the tests that pin
    /// the equality as pinning more than that case.
    ///
    /// Both halves read the derivations `held_balances` reads rather than
    /// restating them.
    /// This function used to multiply `maintenance_per_contract` by a contract
    /// count and `initial_per_contract` by a leaves quantity - the raw fields,
    /// not `policy.maintenance` and `policy.initial` - so under
    /// `MarginBasis::Notional`, where those fields are fractions, a 40 percent
    /// requirement on two contracts was reported as eighty cents while
    /// `held_balances` correctly held the notional fraction. The reported
    /// `margins` and the reported `locked` then contradicted each other on every
    /// notional-basis account, and the invariant this comment asserts was simply
    /// false. It is the same defect `apply_margin_breaches` was already fixed
    /// for.
    #[must_use]
    pub(crate) fn margin_requirement(&self) -> Vec<mogwai_protocol::PostedMargin> {
        let mut rows: HashMap<&Symbol, (Decimal, Decimal)> = HashMap::new();
        for ((symbol, _), position) in &self.account.positions {
            let Some(policy) = self.margin.get(symbol) else {
                continue;
            };
            // Only a marked class posts maintenance collateral, the same gate
            // `held_balances` applies: a margin policy attached to a spot
            // symbol must move no number on either side of the reconciliation.
            let Some(def) = self
                .instruments
                .get(symbol)
                .filter(|def| def.class.is_marked())
            else {
                continue;
            };
            let row = rows.entry(symbol).or_default();
            row.1 = row
                .1
                .saturating_add(policy.maintenance(def, position.qty, position.mark_px));
        }
        for order in &self.open {
            let symbol = &order.submit.symbol;
            if !self.margin.contains_key(symbol) {
                continue;
            }
            let Some(def) = self
                .instruments
                .get(symbol)
                .filter(|def| def.class.is_marked())
            else {
                continue;
            };
            // The order's own hold, not a second derivation of it. This
            // is what makes the stated reconciliation true for reduce-only
            // orders, held order-list children and the equity sell's
            // already-covered portion alike: each of those places no hold, so
            // each posts nothing, and neither rule has to be repeated here.
            let Some((currency, amount, _)) = self.order_hold_entry(order) else {
                continue;
            };
            if currency != def.class.settlement_currency() {
                continue;
            }
            let row = rows.entry(symbol).or_default();
            row.0 = row.0.saturating_add(amount);
        }
        for (symbol, def) in &self.instruments {
            if !def.class.is_equity() || !self.margin.contains_key(symbol) {
                continue;
            }
            let aggregate = self.margin_equity_sell_hold_with_pending(symbol, &[]);
            if !aggregate.is_zero() {
                // Added, never assigned. The loop above has already folded every
                // ordinary hold on this symbol - a resting margin-equity BUY
                // posts its own initial requirement there - and the sell
                // aggregate is a further term of the same sum, not a
                // replacement for it. Assigning here reported only the sell
                // requirement on an account holding both, while `held_balances`
                // locked both, so the reconciliation this function documents
                // was false on exactly the mixed case.
                let row = rows.entry(symbol).or_default();
                row.0 = row.0.saturating_add(aggregate);
            }
        }
        let mut margins: Vec<_> = rows
            .into_iter()
            .filter_map(|(symbol, (initial, maintenance))| {
                let def = self.instruments.get(symbol)?;
                Some(mogwai_protocol::PostedMargin {
                    symbol: std::sync::Arc::clone(symbol),
                    currency: def.class.settlement_currency().to_owned(),
                    initial,
                    maintenance,
                })
            })
            .collect();
        margins.sort_by(|a, b| a.symbol.cmp(&b.symbol));
        margins
    }

    /// What one symbol's position contributes to the collateral the maintenance
    /// test measures, on top of the settlement cash balance.
    ///
    /// The split is the one `valuation_in` makes, and for the same reason. A
    /// derivative's cash never moved when it opened, so only the gain since
    /// entry is outstanding: unrealized. An equity's cash moved by the whole
    /// notional, so what is outstanding is the shares' market value - and
    /// counting only its unrealized would read a margin buy as an account that
    /// spent the money and received nothing, breaching it on the spot.
    fn collateral_contribution(&self, symbol: &str) -> Decimal {
        let Some(def) = self.instruments.get(symbol) else {
            return Decimal::ZERO;
        };
        if !def.class.is_equity() {
            return self.unrealized_pnl(symbol);
        }
        let qty = self.net_position(symbol);
        let mark = self
            .account
            .positions
            .iter()
            .find(|((position_symbol, _), _)| position_symbol.as_ref() == symbol)
            .map_or(Decimal::ZERO, |(_, position)| position.mark_px);
        def.notional(qty, mark)
            .unwrap_or(if qty.is_sign_negative() {
                Decimal::MIN
            } else {
                Decimal::MAX
            })
    }

    fn apply_margin_breaches(
        &mut self,
        marks: &[(Symbol, Decimal)],
        ts: u64,
        events: &mut Vec<VenueMessage>,
    ) -> usize {
        let mut liquidate = Vec::new();
        let symbols: Vec<_> = self.margin.keys().cloned().collect();
        for symbol in symbols {
            let Some(policy) = self.margin.get(&symbol).copied() else {
                continue;
            };
            let Some(def) = self.instruments.get(&symbol) else {
                continue;
            };
            let currency = def.class.settlement_currency();
            let total = *self
                .account
                .balances
                .get(currency)
                .unwrap_or(&Decimal::ZERO);
            // Deduplicated by symbol: `unrealized_pnl` already sums every
            // position keyed under a symbol, so folding it once per position
            // key would count a hedged book's two legs twice over and declare
            // a breach (or clear one) on twice the real P&L.
            let settled_symbols: HashSet<&Symbol> = self
                .account
                .positions
                .keys()
                .map(|(other, _)| other)
                .filter(|other| {
                    self.instruments
                        .get(*other)
                        .is_some_and(|other_def| other_def.class.settlement_currency() == currency)
                })
                .collect();
            let unrealized = settled_symbols.iter().fold(Decimal::ZERO, |sum, other| {
                sum.saturating_add(self.collateral_contribution(other))
            });
            let maintenance = self
                .account
                .positions
                .iter()
                .filter_map(|((other, _), position)| {
                    let other_policy = self.margin.get(other)?;
                    let other_def = self.instruments.get(other)?;
                    (other_def.class.settlement_currency() == currency).then(|| {
                        // Through the policy, which is what honours `basis`. A
                        // notional policy states its maintenance as a fraction,
                        // so multiplying it by a contract count read a 40
                        // percent requirement on two contracts as eighty cents -
                        // a leveraged account that could never breach. The
                        // per-contract case is unchanged, since that is exactly
                        // what `maintenance` computes for it.
                        other_policy.maintenance(other_def, position.qty, position.mark_px)
                    })
                })
                .fold(Decimal::ZERO, Decimal::saturating_add);
            let breached = total.saturating_add(unrealized) < maintenance;
            match (breached, policy.breach_action) {
                (true, MarginBreachAction::Refuse) => {
                    self.margin_breached.insert(symbol);
                }
                (true, MarginBreachAction::Liquidate) => {
                    self.margin_breached.insert(std::sync::Arc::clone(&symbol));
                    // One order per open position, not one per symbol: under
                    // hedging a symbol carries several, and closing only the
                    // first leaves the account still breached after the
                    // cascade it was supposed to end.
                    if let Some((_, mark)) = marks.iter().find(|(marked, _)| marked == &symbol) {
                        for (key, position) in &self.account.positions {
                            if key.0 == symbol {
                                liquidate.push((
                                    std::sync::Arc::clone(&symbol),
                                    key.1.clone(),
                                    position.qty,
                                    *mark,
                                ));
                            }
                        }
                    }
                }
                (false, _) => {
                    self.margin_breached.remove(&symbol);
                }
            }
        }
        let mut originated = 0;
        for (symbol, position_id, qty, mark) in liquidate {
            self.liquidation_seq = self.liquidation_seq.saturating_add(1);
            let order = SubmitOrder {
                client_order_id: format!(
                    "{LIQUIDATION_ID_PREFIX}{}-{}",
                    symbol, self.liquidation_seq
                ),
                symbol,
                position_id,
                side: if qty > Decimal::ZERO {
                    Side::Sell
                } else {
                    Side::Buy
                },
                order_type: OrderType::Market,
                quantity: qty.abs(),
                price: Some(mark),
                trigger_price: None,
                trail_offset: None,
                limit_offset: None,
                time_in_force: TimeInForce::Ioc,
                expire_time: None,
                reduce_only: true,
                post_only: false,
                // A venue-originated close belongs to no order list: it is the
                // venue acting on its own account, not a leg of anybody's plan.
                link: None,
            };
            events.extend(self.on_submit_from(
                order,
                ts,
                Some(MarketReading {
                    last_px: mark,
                    ts_ns: ts,
                    // A venue-originated close has no consumer reading to inherit,
                    // so it is judged against the run's configured band cap
                    // rather than an invented constant. That is deliberately
                    // pessimistic: a forced close is the one moment a venue is
                    // least likely to do better than its worst advertised
                    // slippage.
                    band_ticks: self.liquidation_band_ticks,
                }),
                false,
                &[],
            ));
            originated += 1;
        }
        originated
    }

    /// Close every open position and cancel every resting order, as the venue
    /// rather than as the consumer.
    ///
    /// This is what enforcing an account policy does on breach: a strategy that
    /// would have been liquidated must actually be liquidated, or the forward
    /// claim is worth nothing. It is the same close the margin ledger performs
    /// under `MarginBreachAction::Liquidate` - reduce-only IOC market orders at the
    /// mark, judged against the configured liquidation band - applied to the
    /// whole book instead of to one breached symbol.
    ///
    /// Resting orders go first. A flatten that left them would leave the
    /// account able to re-open the position it was just closed out of, through
    /// a trigger nobody is watching.
    pub fn liquidate_all(&mut self, ts: u64) -> MarkOutcome {
        let mut events = Vec::new();
        let resting: Vec<String> = self
            .open
            .iter()
            .map(|order| order.submit.client_order_id.clone())
            .collect();
        for client_order_id in resting {
            events.extend(self.on_cancel(client_order_id, ts, false));
        }
        let positions: Vec<_> = self
            .account
            .positions
            .iter()
            .filter(|(_, state)| !state.qty.is_zero())
            .map(|((symbol, position_id), state)| {
                (
                    Symbol::clone(symbol),
                    position_id.clone(),
                    state.qty,
                    state.mark_px,
                )
            })
            .collect();
        let mut originated = 0;
        for (symbol, position_id, qty, mark) in positions {
            self.liquidation_seq = self.liquidation_seq.saturating_add(1);
            let order = SubmitOrder {
                client_order_id: format!(
                    "{RISK_FLATTEN_ID_PREFIX}{}-{}",
                    symbol, self.liquidation_seq
                ),
                symbol,
                position_id,
                side: if qty > Decimal::ZERO {
                    Side::Sell
                } else {
                    Side::Buy
                },
                order_type: OrderType::Market,
                quantity: qty.abs(),
                price: Some(mark),
                trigger_price: None,
                trail_offset: None,
                limit_offset: None,
                time_in_force: TimeInForce::Ioc,
                expire_time: None,
                reduce_only: true,
                post_only: false,
                // A venue-originated close belongs to no order list: it is the
                // venue acting on its own account, not a leg of anybody's plan.
                link: None,
            };
            events.extend(self.on_submit_from(
                order,
                ts,
                Some(MarketReading {
                    last_px: mark,
                    ts_ns: ts,
                    band_ticks: self.liquidation_band_ticks,
                }),
                false,
                &[],
            ));
            originated += 1;
        }
        MarkOutcome {
            events,
            originated_orders: originated,
        }
    }

    /// Retire everything this account holds that is not on `symbol`: cancel the
    /// resting orders, close the positions at their last mark.
    ///
    /// This is the freeze rule's return half, and only that. An account rides
    /// as many rivers as it has sockets, so boarding a second river while the
    /// account is live retires nothing; the sole caller (`resume` in
    /// `mogwai-venue`) runs this on a frozen account's return, when the
    /// returning passenger is the only one reading the book. That passenger may
    /// name a different symbol than the account was trading before it froze,
    /// and a position carried across would be one it can neither see nor close:
    /// nothing prices it, no sweep marks it, and no order can reach it. The
    /// same is true of a resting order, with the sharper edge that it would sit
    /// on a river no boat is reading, where nothing would ever sweep it and
    /// nothing would say so.
    ///
    /// Closed at the last mark, which is the best price that exists: the river
    /// it traded on has no cursor, so there is no fresher one to be had.
    pub fn retire_off_river(&mut self, symbol: &str, ts: u64) -> Vec<VenueMessage> {
        let mut events = Vec::new();
        let stranded: Vec<String> = self
            .open
            .iter()
            .filter(|order| order.submit.symbol.as_ref() != symbol)
            .map(|order| order.submit.client_order_id.clone())
            .collect();
        for client_order_id in stranded {
            tracing::info!(
                %client_order_id,
                bound = %symbol,
                "cancelling a resting order off the river this account is bound to",
            );
            events.extend(self.on_cancel(client_order_id, ts, false));
        }
        let positions: Vec<_> = self
            .account
            .positions
            .iter()
            .filter(|((position_symbol, _), state)| {
                position_symbol.as_ref() != symbol && !state.qty.is_zero()
            })
            .map(|((position_symbol, position_id), state)| {
                (
                    Symbol::clone(position_symbol),
                    position_id.clone(),
                    state.qty,
                    state.mark_px,
                )
            })
            .collect();
        for (position_symbol, position_id, qty, mark) in positions {
            tracing::info!(
                symbol = %position_symbol,
                bound = %symbol,
                "flattening a position off the river this account is bound to",
            );
            events.extend(self.close_at_mark(&position_symbol, position_id, qty, mark, ts));
        }
        events
    }

    /// Release every sale credit whose settlement instant the venue has now
    /// reached, making the money spendable.
    ///
    /// Driven by the sweep pass rather than by a timer, for the same reason
    /// expiry is: the instants are on the simulated clock, and the sweeper is
    /// what advances the venue's notion of now. Returns whether anything moved,
    /// so the caller can decide whether the pass owes a snapshot - a released
    /// credit changes `free` and `locked` without changing `total`, which a
    /// consumer watching its buying power very much notices.
    pub fn release_settled_cash(&mut self, now_ns: u64) -> bool {
        let before = self.account.unsettled.len();
        self.account
            .unsettled
            .retain(|credit| credit.settles_at_ns > now_ns);
        before != self.account.unsettled.len()
    }

    /// Cancel every resting order on a symbol nobody is reading.
    ///
    /// `readable` is the set of symbols a cursor is currently walking. An order
    /// outside it rests on a river with no clock: there is no instant to sweep
    /// it to, so it cannot fill, cannot expire and cannot be told apart from an
    /// order the tape simply has not reached. Leaving it there is the one
    /// outcome that is neither of the two honest ones - so the venue refuses to
    /// leave it, and the consumer is told with an ordinary `OrderCanceled`.
    ///
    /// A frozen account never reaches this: it is skipped by the sweeper
    /// wholesale, and its book survives untouched for the socket that returns to
    /// it. The two rules are the same statement from opposite sides - an order
    /// nobody is reading either belongs to an account that will come back for
    /// it, or it should not be resting.
    pub fn cancel_unreadable_orders(&mut self, readable: &[Symbol], ts: u64) -> Vec<VenueMessage> {
        let stranded: Vec<String> = self
            .open
            .iter()
            .filter(|order| !readable.contains(&order.submit.symbol))
            .map(|order| order.submit.client_order_id.clone())
            .collect();
        let mut events = Vec::new();
        for client_order_id in stranded {
            tracing::info!(
                %client_order_id,
                "cancelling a resting order on a river no cursor is reading",
            );
            events.extend(self.on_cancel(client_order_id, ts, false));
        }
        events
    }

    /// Re-base every resting order's scan frontier onto `now_ns`.
    ///
    /// A frozen account's orders carry the frontier of the boat that departed,
    /// which sits in a returning boat's future: a cursor is placed at its
    /// river's origin, so without this the order waits for the new cursor to
    /// reach an instant the old one had already passed, which is as long as the
    /// previous session ran.
    ///
    /// Nothing is owed for the span in between. Nobody was reading the account,
    /// so no pass watched that water on its behalf - which is the same statement
    /// the freeze itself makes, applied to the frontier rather than to the
    /// sweep.
    pub fn rebase_scans(&mut self, now_ns: u64) {
        for pos in 0..self.open.len() {
            let order = &mut self.open[pos];
            if order.scanned_ns == now_ns {
                continue;
            }
            order.scanned_ns = now_ns;
            // A walk planned against the old frontier says nothing about the new
            // one, exactly as after an amend.
            order.revision = order.revision.saturating_add(1);
        }
    }

    /// Re-base only the frontiers that sit in `now_ns`'s future, and report how
    /// many there were.
    ///
    /// A frontier ahead of the cursor is never legitimate. An order's frontier
    /// is set to the instant it was accepted, or to how far a sweep has walked,
    /// and both are sampled on the cursor that is serving it - so under ordinary
    /// operation it trails `now_ns` and never leads it. When it leads, the
    /// order's scan window is empty on every pass and it rests unfillable until
    /// the cursor catches up, which is silent and looks exactly like an order
    /// that has not been hit yet.
    ///
    /// This is the state itself rather than a proxy for it, which is why it is
    /// preferred over identifying the cursor. [`Self::rebase_scans`] is applied
    /// when an account is found frozen, and freezing was always standing in for
    /// "the cursor this book was marked on is gone". That proxy has a hole: a
    /// newcomer claiming an existing account is counted on before the incumbent is
    /// closed, deliberately, so the account never freezes - and if the newcomer
    /// boards a different river, the departed one's boat is torn down with its
    /// worker, and a boat placed over that river again starts at the yard's
    /// origin. A placement nonce on the boat would let the proxy be repaired;
    /// asking whether any frontier is in the future needs no new identity, and
    /// closes the case whatever produced it.
    ///
    /// A trailing frontier is left exactly where it is. That span is water the
    /// account is owed a scan over, and moving it forward would be the
    /// unconditional re-base wearing a different name - which is the whole
    /// thing this method exists not to be.
    ///
    /// Nothing is owed for the span skipped, on the same reasoning as
    /// `rebase_scans`: no pass watched that water on this account's behalf.
    pub fn rebase_future_scans(&mut self, now_ns: u64) -> usize {
        let mut rebased = 0;
        for pos in 0..self.open.len() {
            let order = &mut self.open[pos];
            if order.scanned_ns <= now_ns {
                continue;
            }
            order.scanned_ns = now_ns;
            // A walk planned against the old frontier says nothing about the new
            // one, exactly as after an amend.
            order.revision = order.revision.saturating_add(1);
            rebased += 1;
        }
        rebased
    }

    /// One venue-originated reduce-only close at the mark, the shape both the
    /// margin breach and the risk breach use.
    fn close_at_mark(
        &mut self,
        symbol: &Symbol,
        position_id: Option<String>,
        qty: Decimal,
        mark: Decimal,
        ts: u64,
    ) -> Vec<VenueMessage> {
        self.liquidation_seq = self.liquidation_seq.saturating_add(1);
        let order = SubmitOrder {
            client_order_id: format!("{LIQUIDATION_ID_PREFIX}{}-{}", symbol, self.liquidation_seq),
            symbol: Symbol::clone(symbol),
            position_id,
            side: if qty > Decimal::ZERO {
                Side::Sell
            } else {
                Side::Buy
            },
            order_type: OrderType::Market,
            quantity: qty.abs(),
            price: Some(mark),
            trigger_price: None,
            trail_offset: None,
            limit_offset: None,
            time_in_force: TimeInForce::Ioc,
            expire_time: None,
            reduce_only: true,
            post_only: false,
            link: None,
        };
        self.on_submit_from(
            order,
            ts,
            Some(MarketReading {
                last_px: mark,
                ts_ns: ts,
                band_ticks: self.liquidation_band_ticks,
            }),
            false,
            &[],
        )
    }

    /// Exchange funding on every perpetual position, for each funding instant
    /// the span `from_ns .. to_ns` crossed.
    ///
    /// A perpetual has no expiry to converge at, so funding is the only thing
    /// tying it to spot: the long pays the short a fraction of notional at a
    /// fixed interval, or the reverse when the rate is negative. A strategy
    /// holding a perp across funding instants has a real cash flow, so a venue
    /// without this reports P and L that is wrong by construction rather than by
    /// approximation.
    ///
    /// Paid on notional at the mark, not at entry: funding is a payment on what
    /// the position is worth now, which is why a position that has moved against
    /// its holder pays less as it shrinks.
    ///
    /// The span is half-open, `from_ns` exclusive and `to_ns` inclusive, matching
    /// the settlement walk - so an instant is funded exactly once however the
    /// sweep passes are cut.
    pub fn apply_funding(&mut self, from_ns: u64, to_ns: u64, ts: u64) -> MarkOutcome {
        let mut paid = false;
        let symbols: Vec<Symbol> = self.instruments.keys().cloned().collect();
        for symbol in symbols {
            let Some(def) = self.instruments.get(&symbol) else {
                continue;
            };
            let Some(terms) = def.class.funding() else {
                continue;
            };
            if terms.interval_ns == 0 {
                continue;
            }
            let instants = funding_instants(from_ns, to_ns, terms.interval_ns);
            if instants == 0 {
                continue;
            }
            let index = terms
                .index_symbol
                .as_ref()
                .and_then(|index| self.last_marks.get(index.as_str()).copied());
            let currency = def.class.settlement_currency().to_owned();
            let def = def.clone();
            let mut owed = Decimal::ZERO;
            for position in self
                .account
                .positions
                .iter()
                .filter(|((position_symbol, _), _)| *position_symbol == symbol)
                .map(|(_, position)| position)
            {
                if position.qty.is_zero() {
                    continue;
                }
                let Some(notional) = def.notional(position.qty, position.mark_px) else {
                    continue;
                };
                // The rate is computed at this mark against the index, if any.
                // A long pays a positive rate, so the sign follows the
                // position: the same amount debits a long and credits a short,
                // which is what makes funding a transfer rather than a fee.
                let rate = terms.rate(position.mark_px, index);
                let Some(payment) = notional
                    .checked_mul(rate)
                    .and_then(|per| per.checked_mul(Decimal::from(instants)))
                else {
                    continue;
                };
                owed = owed.saturating_sub(payment);
            }
            if owed.is_zero() {
                continue;
            }
            let total = self.account.balances.entry(currency).or_default();
            *total = total.saturating_add(owed);
            paid = true;
        }
        let mut events = Vec::new();
        if paid {
            // Venue maintenance, the funding exchange: reports unconditionally
            // and consults no arm, on the same ruling as `on_mark` above.
            events.push(VenueMessage::AccountState(self.snapshot(ts)));
        }
        MarkOutcome {
            events,
            originated_orders: 0,
        }
    }

    /// Realize every futures position in `marks` at the settlement price given.
    ///
    /// An inverse instrument always refuses a non-positive settlement price, and the
    /// refusal is to leave the position marked where it was rather than to
    /// settle it somewhere unpriceable. An inverse contract's value is
    /// `multiplier * qty / price`, which has no value at zero, and settlement
    /// writes the price it was given into both `avg_px` and `mark_px` - so a
    /// zero here does not merely produce one bad number, it poisons the
    /// position for the rest of the run, and every later reader answers on a
    /// price that cannot be inverted.
    ///
    /// Ruled 2026-08-20 on what a venue can actually receive rather than on
    /// arithmetic. A real settlement price is a TWAP of an index built by
    /// median-with-outlier-rejection across several spot venues; a zero would
    /// require every constituent to print zero across the whole window, and an
    /// unavailable component is dropped and reweighted rather than fed in as a
    /// zero. So a caller handing one over is supplying something no index
    /// construction produces, which is a caller defect and not a market event.
    ///
    /// Linear classes are always left alone deliberately: their value is
    /// `multiplier * qty * price`, which is perfectly defined at zero and means
    /// what it says. The rule is about invertibility, not about zero being
    /// distasteful.
    ///
    /// The same ruling closed the other two prices a non-positive value could
    /// enter by. Order entry already refused a non-positive `price` and
    /// `trigger_price` in `validate_submit_order`, so nothing was owed there.
    /// Refusing at the fill was considered and rejected: by the time a fill is
    /// booked the tape has already produced the print, and aborting the serving
    /// path over a price the market printed is the one thing no venue does.
    ///
    /// `Engine::position_unrealized_checked` still answers zero for an
    /// unpriceable position. That stays a backstop under the fill case, and
    /// under a case this guard makes unreachable from here, rather than the
    /// policy it was before.
    pub fn settle(&mut self, marks: &[(Symbol, Decimal)], ts: u64) -> MarkOutcome {
        let mut settled = false;
        for (symbol, settle_px) in marks {
            let Some(def) = self.instruments.get(symbol) else {
                continue;
            };
            if !def.class.is_future() {
                continue;
            }
            if def.class.is_inverse() && *settle_px <= Decimal::ZERO {
                self.warn_unpriceable_settlement(symbol);
                continue;
            }
            for position in
                self.account
                    .positions
                    .iter_mut()
                    .filter_map(|((position_symbol, _), position)| {
                        (position_symbol == symbol).then_some(position)
                    })
            {
                // Through the instrument's own arithmetic, exactly as the
                // unrealized readers and `apply_fill` do: settlement realizes
                // the position at `settle_px`, so booking it linearly while the
                // mark-to-market it replaces was inverse would move the
                // account's value at the instant of settlement.
                let pnl = position_unrealized(def, position.qty, position.avg_px, *settle_px);
                let total = self
                    .account
                    .balances
                    .entry(def.class.settlement_currency().to_owned())
                    .or_default();
                *total = total.saturating_add(pnl);
                position.avg_px = *settle_px;
                position.mark_px = *settle_px;
                settled = true;
            }
        }
        let mut events = Vec::new();
        let originated_orders = self.apply_margin_breaches(marks, ts, &mut events);
        if settled || originated_orders > 0 {
            events.retain(|event| !matches!(event, VenueMessage::AccountState(_)));
            // Venue maintenance, settlement: reports unconditionally and
            // consults no arm, on the same ruling as `on_mark` above.
            events.push(VenueMessage::AccountState(self.snapshot(ts)));
        }
        MarkOutcome {
            events,
            originated_orders,
        }
    }
    /// Cheap facts used to reserve a bounded wire response before mutation.
    #[must_use]
    pub fn book_shape(&self) -> mogwai_protocol::sizing::BookShape {
        mogwai_protocol::sizing::BookShape {
            balances: self.account.balances.len(),
            positions: self.account.positions.len(),
            margins: self
                .account
                .positions
                .keys()
                .map(|(symbol, _)| symbol)
                .chain(self.open.iter().map(|order| &order.submit.symbol))
                .filter(|symbol| self.margin.contains_key(*symbol))
                .collect::<std::collections::HashSet<_>>()
                .len(),
            open_orders: self.open.len(),
            closed_orders: self.closed.len(),
            recorded_fills: self.fills.len(),
        }
    }
    /// The default-instrument, unfunded engine the unit tests build on.
    ///
    /// Test-only, and gated rather than merely documented: it carries the
    /// placeholder account id, so a production caller would stamp `UNBOUND` on
    /// every snapshot it sent. Production calls `build` with a real
    /// `EngineConfig`. See `EngineConfig::unbound`.
    ///
    /// No `Default`: a derived one would yield an empty instrument table whose
    /// fill accounting silently diverges (every fill warns, books
    /// position-only); a delegating one is dead surface nothing calls.
    #[cfg(test)]
    #[expect(
        clippy::new_without_default,
        reason = "new() seeds the instrument table; a Default impl would diverge or be dead surface"
    )]
    pub fn new() -> Self {
        Self::build(EngineConfig::unbound(default_instruments()))
    }

    /// Placeholder identity for `EngineConfig::unbound` and `new()`, both of
    /// which are `cfg(test)` so this string cannot reach the wire. Production
    /// always builds an `EngineConfig` by hand, which requires the real id:
    /// an engine that guessed its own identity would stamp a wrong
    /// `AccountState.account_id` on the wire, and a snapshot is only
    /// self-describing if that field is the account the ledger belongs to.
    pub const UNBOUND_ACCOUNT_ID: &'static str = "UNBOUND";

    /// Constructs the engine with the account pre-funded per currency, the
    /// venue's equivalent of a deposit made before the run. The ledger itself
    /// only ever books fill deltas, so without a seed the first buy drives the
    /// quote leg negative - which a nautilus cash account (the adapter's
    /// default) refuses to apply, silently desyncing the consumer's account
    /// from the venue's. Funding is initial state, not a mutation: there is no
    /// deposit surface at runtime, so a scenario's capital is fixed at boot
    /// and every balance the venue ever reports is explained by fills alone.
    ///
    /// A non-empty seed also arms funds enforcement: a funded venue rejects
    /// submits and amends the free balance cannot cover, like a real cash
    /// exchange. An empty seed keeps the permissive unfunded ledger - see
    /// `enforce_funds`.
    ///
    /// `account_id` is stored, not threaded through `account_snapshot`, so the
    /// engine is the single source of truth for the identity it stamps on every
    /// `AccountState` it produces.
    pub fn build(config: EngineConfig) -> Self {
        let enforce_funds = !config.balances.is_empty();
        let instruments = config
            .instruments
            .into_iter()
            .map(|instrument| (std::sync::Arc::clone(&instrument.symbol), instrument))
            .collect();

        Self {
            account_id: config.account_id,
            open: OpenBook::default(),
            order_holds: HashMap::new(),
            order_holds_clipped: HashSet::new(),
            enforce_funds,
            account: Account {
                balances: config.balances,
                positions: HashMap::new(),
                unsettled: Vec::new(),
            },
            instruments,
            seen_client_order_ids: HashMap::new(),
            closed: HashMap::new(),
            fills: Vec::new(),
            armed: VecDeque::new(),
            venue_order_seq: 0,
            trade_seq: 0,
            position_seq: 0,
            last_marks: HashMap::new(),
            liquidation_seq: 0,
            warned: Warned::default(),
            fill_seed: config.fill_seed,
            margin: HashMap::new(),
            fees: HashMap::new(),
            margin_breached: HashSet::new(),
            liquidation_band_ticks: DEFAULT_LIQUIDATION_BAND_TICKS,
            fee_surcharge: None,
            event_sim: SimClock::identity(),
            oms_type: mogwai_protocol::OmsType::Netting,
        }
    }

    pub fn instrument_defs(&self) -> Vec<InstrumentDef> {
        let mut defs: Vec<_> = self.instruments.values().cloned().collect();
        defs.sort_by(|a, b| a.symbol.cmp(&b.symbol));
        defs
    }

    /// Monotonic id source; the venue stamps real timestamps.
    fn next_venue_order_id(&mut self) -> String {
        self.venue_order_seq = self.venue_order_seq.saturating_add(1);
        format!("V-{}", self.venue_order_seq)
    }

    fn next_trade_id(&mut self) -> String {
        self.trade_seq = self.trade_seq.saturating_add(1);
        format!("T-{}", self.trade_seq)
    }

    /// Process one consumer message, emitting the resulting execution events.
    ///
    /// `ts` is supplied by the caller (the venue's clock) so the engine stays
    /// free of wall-clock access and remains deterministic in tests.
    pub fn process(&mut self, msg: Command, ts: u64) -> Vec<VenueMessage> {
        self.process_with_market(msg, ts, None)
    }

    /// As `process`, with the tape reading the venue took at `ts`.
    ///
    /// A submit needs it to size its band and to judge marketability; a price
    /// amend needs it so a re-draw adopts the current regime rather than the one
    /// the order was accepted under. `None` is a legitimate answer - the venue's
    /// estimator can be cold or its walk can be truncated - and every path here
    /// has a defined behaviour without one: a limit rests untriggerable until a
    /// later walk has evidence, an amend keeps the band it had, and a market
    /// order fills unslipped at its stated price.
    ///
    /// Runs on the identity clock, which makes it a tests-and-benches
    /// convenience rather than a serving path: an armed fee surcharge is then
    /// judged with simulated time equal to wall time. The venue calls
    /// `process_with_market_on_clock` with the commanding socket's boat clock.
    pub fn process_with_market(
        &mut self,
        msg: Command,
        ts: u64,
        reading: Option<MarketReading>,
    ) -> Vec<VenueMessage> {
        self.process_with_market_on_clock(msg, ts, reading, SimClock::identity())
    }

    /// As `process_with_market`, on the clock of the boat this pass belongs to.
    /// Every timestamp the pass produces was sampled on that clock, so the fee
    /// surcharge is judged on it too.
    pub fn process_with_market_on_clock(
        &mut self,
        msg: Command,
        ts: u64,
        reading: Option<MarketReading>,
        sim: SimClock,
    ) -> Vec<VenueMessage> {
        self.event_sim = sim;
        if cfg!(debug_assertions) {
            self.reconcile_order_holds();
        }
        let events = match msg {
            Command::SubmitOrder(order) => self.on_submit(order, ts, reading),
            Command::SubmitOrderGroup { orders } => self.on_submit_group(&orders, ts, reading),
            Command::CancelOrder { client_order_id } => self.on_cancel(client_order_id, ts, true),
            Command::ModifyOrder {
                client_order_id,
                price,
                quantity,
                trigger_price,
            } => self.on_modify(client_order_id, price, quantity, trigger_price, ts, reading),
            Command::QueryOrders {
                request_id,
                client_order_id,
                open_only,
            } => vec![VenueMessage::OrderStatusSnapshot(
                self.order_status_snapshot(request_id, client_order_id.as_deref(), open_only, ts),
            )],
            Command::QueryFills {
                request_id,
                client_order_id,
            } => vec![VenueMessage::FillSnapshot(self.fill_snapshot(
                request_id,
                client_order_id.as_deref(),
                ts,
            ))],
            // Venue-owned and never routed here, like the transport controls.
            // A history request reads the passenger's river, and the engine
            // holds a book rather than water - it has no river to read and no
            // key to read one with. Enumerated rather than caught by a wildcard
            // so that a future command classified venue-owned at the routing
            // site and forwarded here fails this crate's build instead of
            // silently producing nothing.
            Command::QueryHistory { .. } => Vec::new(),
        };
        if cfg!(debug_assertions) {
            self.reconcile_order_holds();
        }
        events
    }

    /// Every resting order the tape can decide, each carrying the price the
    /// walk applies its predicate to and which predicate that is. There is no
    /// off switch: the band is always on, so a venue always sweeps.
    ///
    /// The dispatch is a match on `Resting`, and each arm is load-bearing:
    ///
    /// - `Limit` yields a `FillThrough` scan against the order's drawn band
    ///   trigger. A print strictly through it fills the order at its own stated
    ///   price.
    /// - `Conditional` yields a `StopTrigger` scan, or a `TouchedTrigger` one
    ///   for the touched family, against the consumer's stated stop price. A
    ///   print merely reaching it triggers, because a conditional holds no
    ///   queue position and so needs none of the strictness the limit case
    ///   does.
    /// - `Inert` yields nothing, which is the naming of what an `order_type`
    ///   filter used to express here: an armed `PartialFillNext` can leave a
    ///   market remainder resting with a stamped price, and handing that to the
    ///   tape walk would hold it until the market traded through a price the
    ///   venue itself synthesized. A market remainder, and a triggered
    ///   stop-market's remainder, have no meaningful price for the tape to
    ///   reach; they rest, are never scanned, and end only on a consumer cancel.
    #[must_use]
    pub fn pending_scans(&self) -> Vec<PendingScan> {
        // The slot order is never stable - `OpenBook::remove` swaps - so the
        // acceptance identity is carried alongside each scan and sorted on
        // explicitly. It is decorated here rather than looked up inside the
        // comparator: a comparator doing two hash lookups and two integer
        // parses per comparison would put `O(n log n)` of both on the sweeper's
        // hottest call.
        let mut scans: Vec<_> = self
            .open
            .iter()
            .filter_map(|order| {
                let (kind, px) = match order.resting {
                    Resting::Limit { fill_trigger_px } => (ScanKind::FillThrough, fill_trigger_px),
                    Resting::Conditional { stop_px, toward } => (
                        if toward {
                            ScanKind::TouchedTrigger
                        } else {
                            ScanKind::StopTrigger
                        },
                        stop_px,
                    ),
                    // Neither has a price the tape can decide: an inert
                    // remainder never had one, and a held child has one its
                    // parent has not yet released it to use.
                    Resting::Inert | Resting::Held => return None,
                };
                Some((
                    (
                        order.scanned_ns,
                        order.ts_accepted,
                        venue_order_sequence(&order.venue_order_id),
                    ),
                    PendingScan {
                        client_order_id: order.submit.client_order_id.clone(),
                        symbol: std::sync::Arc::clone(&order.submit.symbol),
                        side: order.submit.side,
                        px,
                        kind,
                        from_ns: order.scanned_ns,
                        revision: order.revision,
                    },
                ))
            })
            .collect();
        scans.sort_by_key(|(key, _)| *key);
        let scans: Vec<PendingScan> = scans.into_iter().map(|(_, scan)| scan).collect();
        scans
    }

    /// Answer a `QueryOrders` truthfully from the book: the currently-open
    /// orders plus the retained terminal records. This is the reconciliation
    /// witness, so its content is never touched by divergences - havoc may
    /// only delay or drop the reply's delivery (the venue's writer windows),
    /// per the honest-content contract on `Command::QueryOrders`.
    ///
    /// A targeted query (`client_order_id: Some`) ignores `open_only`: asking
    /// about one specific order deserves its terminal state, not an empty
    /// reply that would misread as "unknown order".
    pub fn order_status_snapshot(
        &self,
        request_id: String,
        client_order_id: Option<&str>,
        open_only: bool,
        ts: u64,
    ) -> OrderStatusSnapshot {
        let mut orders: Vec<OrderStatusInfo> = self
            .open
            .iter()
            .map(open_order_status)
            .chain(self.closed.values().cloned())
            .filter(|info| match client_order_id {
                Some(id) => info.client_order_id == id,
                None => !open_only || info.status.is_open(),
            })
            .collect();
        // `open` and the `closed` map iterate in unrelated orders; sort so
        // the reply is deterministic for goldens and diffable in logs.
        orders.sort_by(|a, b| {
            a.ts_accepted
                .cmp(&b.ts_accepted)
                .then_with(|| a.client_order_id.cmp(&b.client_order_id))
        });
        OrderStatusSnapshot {
            request_id,
            orders,
            ts_event: ts,
        }
    }

    /// Answer a `QueryFills` truthfully from the booking-order fill store.
    /// Same honest-content contract as `order_status_snapshot`.
    pub fn fill_snapshot(
        &self,
        request_id: String,
        client_order_id: Option<&str>,
        ts: u64,
    ) -> FillSnapshot {
        let fills = self
            .fills
            .iter()
            .filter(|fill| client_order_id.is_none_or(|id| fill.client_order_id == id))
            .cloned()
            .collect();
        FillSnapshot {
            request_id,
            fills,
            ts_event: ts,
        }
    }

    /// Why `cancel_open_order_silently` would refuse `client_order_id`, or
    /// `None` when it is resting and the cancel would succeed.
    ///
    /// A query, and it exists because the control plane's miss path needs one.
    /// That path ran the cancel itself to obtain its diagnosis and read the
    /// `Err`, which performs the operation it is explaining: on the ledger that
    /// happened to hold the id, the "diagnosis" silently cancelled a resting
    /// order and its held children and then reported `Ok` as "unknown order".
    /// A diagnosis that mutates is not a diagnosis.
    ///
    /// The wording is shared with the cancel through `not_resting_reason`, so
    /// the two can only agree.
    pub fn silent_cancel_refusal(&self, client_order_id: &str) -> Option<String> {
        self.open
            .position(client_order_id)
            .is_none()
            .then(|| self.not_resting_reason(client_order_id))
    }

    /// Why an id is not currently resting: unknown to this ledger, or seen and
    /// already terminal. One wording, two readers.
    ///
    /// Terminal is left unqualified because the gate is `!status.is_open()`,
    /// which an expiry and a post-acceptance rejection reach as surely as a
    /// fill or a cancel. Naming a pair of them in the text told a consumer
    /// cancelling an expired `Gtd` something false about its own order.
    fn not_resting_reason(&self, client_order_id: &str) -> String {
        match self.seen_client_order_ids.get(client_order_id) {
            Some(_) => "order already terminal".into(),
            None => "unknown order".into(),
        }
    }

    /// The control-plane out-of-band cancel (`CancelOpenOrderSilently`):
    /// remove a resting order from the book and free its hold,
    /// emitting no lifecycle event - the fault class where the venue
    /// cancelled and the consumer never heard. The truth store records the
    /// order `Canceled` at `ts`, so a later `QueryOrders` reports it
    /// honestly while the event stream stays silent. Errs when the id is not
    /// currently resting (unknown or already terminal), so the control plane
    /// can refuse a no-op arm loudly.
    pub fn cancel_open_order_silently(
        &mut self,
        client_order_id: &str,
        ts: u64,
    ) -> Result<(), String> {
        let Some(pos) = self.open.position(client_order_id) else {
            return Err(self.not_resting_reason(client_order_id));
        };
        let order = self.open[pos].clone();
        // Causally monotone, and clamped here rather than in `record_closed`.
        // The caller stamps this with the run clock, which is the honest
        // request-time instant but is not on the same axis as the boat clocks
        // that dated this order's acceptance and amends - so a raw stamp could
        // record a cancellation before the amend it follows, inside one ledger,
        // and `QueryOrders` reports both as `ts_last`. The floor covers the
        // children too, because they go terminal in the same call and each has
        // its own history; clamping only against the parent would still backdate
        // a child amended later.
        //
        // Not moved into `record_closed`, though every terminal transition runs
        // through it: a fill sweep closes an order at the instant of the print
        // that closed it, and forcing that forward would misreport when the
        // market hit. Only this control-plane path has no market instant of its
        // own to be truthful about.
        let ts = std::iter::once(order.ts_last)
            .chain(
                self.held_children_of(&order.submit.client_order_id)
                    .into_iter()
                    .filter_map(|child| {
                        self.open
                            .position(child.as_str())
                            .map(|pos| self.open[pos].ts_last)
                    }),
            )
            .fold(ts, u64::max);
        // The children go with it, and they go silently too. A held child of an
        // order that will now never fill is waiting on a release nothing can
        // perform; the fault class here is "the venue cancelled and the consumer
        // never heard", so the reaped cancellations are recorded in the truth
        // store and always dropped rather than emitted, exactly as the parent's own is.
        //
        // This widens the fault, deliberately, and a scenario author should
        // know by how much: the divergence stops meaning "the consumer's view of
        // one order is stale" and starts meaning "and of every held child of
        // it", up to `MAX_LINKED_ORDERS` of them. That is the honest
        // simulation. The alternative - reap the children but emit their
        // cancels - would tell a consumer its bracket's exits were pulled while
        // the entry it hung them on still reads live, which is a state no real
        // venue produces and which no reconciliation could make sense of.
        // Aim this arm at a parent only if you mean the whole bracket.
        let _silent = self.close_out(pos, &order, WireOrderStatus::Canceled, ts);
        Ok(())
    }

    /// Freeze a just-removed open order into the terminal truth store.
    pub(crate) fn record_closed(&mut self, order: &OpenOrder, status: WireOrderStatus, ts: u64) {
        debug_assert!(!status.is_open(), "closed records are terminal only");
        let mut info = open_order_status(order);
        info.status = status;
        info.ts_last = ts;
        self.closed
            .insert(order.submit.client_order_id.clone(), info);
    }

    /// Record a booked fill into the `QueryFills` truth store. Called once
    /// per booking, never per wire event.
    pub(crate) fn record_fill(&mut self, fill: &OrderFilled) {
        self.fills.push(fill.clone());
    }

    // `&mut self` only for the saturation warning bookkeeping in `snapshot`.
    pub fn account_snapshot(&mut self, ts: u64) -> AccountState {
        self.snapshot(ts)
    }

    /// The largest quantity of `symbol` that can execute on `side` in the worst
    /// fill order, over the working book plus `pending`.
    ///
    /// The one home of "worst fill order" for this crate, because two consumers
    /// ask it - `projected_qty`'s magnitude cap and `validate_submit`'s equity
    /// short check - and a naive sum is wrong for both in the same two ways:
    ///
    /// - A `Resting::Held` order-list child cannot execute until its parent
    ///   fills, so it contributes nothing. This is the same rule
    ///   `order_hold_entry` applies when it declines to hold funds
    ///   against a held child, and for the same reason; a bracket's two exit
    ///   legs must not be counted against the entry that has to fill first.
    /// - Mutually exclusive legs contribute their max, not their sum. An `Oco`
    ///   group cancels its siblings on any fill and an `Ouo` group shrinks them
    ///   by the filled quantity, so in both cases the whole group can execute at
    ///   most its largest leg. Summing them is what made a plain bracket - a
    ///   take-profit sell 100 and a stop sell 100 against 100 held shares - read
    ///   as a 100-share short and get refused by name on a cash equity account.
    ///   The group is keyed by `order_list_id`, and the sides are already
    ///   separated by the caller, so a parent and its children never share a
    ///   slot.
    ///
    /// `pending` is the orders admitted in the same frame that are not resting
    /// yet - a `SubmitOrderGroup`'s members. A resting order whose
    /// `client_order_id` appears in `pending` is counted from `pending` and not
    /// from the book, which is what makes this answer independent of how many
    /// members have already rested. Without that, the group's dry pass (nothing
    /// resting) and its real pass (earlier members resting) would compute
    /// different numbers, and a check reading it could admit a member on pass
    /// one and refuse it on pass two - an admission mismatch
    /// `report_group_member_refusal` would then have misfiled as the disclosed
    /// funds carve-out, because re-running the dry question after the refusal
    /// refuses again.
    ///
    /// Reduce-only orders are excluded on both sides; see `projected_qty` for
    /// why that exclusion is what makes an oversized reduce-only leave safe.
    pub(crate) fn worst_case_leaves(
        &self,
        symbol: &str,
        side: Side,
        pending: &[SubmitOrder],
    ) -> Decimal {
        // Additive legs accumulate into `total`; each exclusive group keeps only
        // its largest leg in `exclusive`, keyed by `order_list_id`.
        fn count<'a>(
            total: &mut Decimal,
            exclusive: &mut HashMap<&'a str, Decimal>,
            qty: Decimal,
            link: Option<&'a mogwai_protocol::OrderLink>,
        ) {
            match link.filter(|link| {
                matches!(
                    link.contingency,
                    mogwai_protocol::Contingency::Oco | mogwai_protocol::Contingency::Ouo
                )
            }) {
                Some(link) => {
                    let slot = exclusive
                        .entry(link.order_list_id.as_str())
                        .or_insert(Decimal::ZERO);
                    if qty > *slot {
                        *slot = qty;
                    }
                }
                None => *total = total.saturating_add(qty),
            }
        }
        let mut total = Decimal::ZERO;
        let mut exclusive: HashMap<&str, Decimal> = HashMap::new();
        for order in &self.open.orders {
            if order.submit.symbol.as_ref() != symbol
                || order.submit.side != side
                || order.submit.reduce_only
                || matches!(order.resting, Resting::Held)
                || pending
                    .iter()
                    .any(|member| member.client_order_id == order.submit.client_order_id)
            {
                continue;
            }
            count(
                &mut total,
                &mut exclusive,
                order.leaves_qty,
                order.submit.link.as_ref(),
            );
        }
        for member in pending {
            // A pending member's held-ness is read off its link rather than off
            // a `Resting` it does not have yet: a child naming a parent is
            // exactly what `on_submit_from` rests as `Resting::Held`.
            if member.symbol.as_ref() != symbol
                || member.side != side
                || member.reduce_only
                || member
                    .link
                    .as_ref()
                    .is_some_and(|link| link.parent_order_id.is_some())
            {
                continue;
            }
            // A member already resting has been partially filled at most; its
            // remaining leaves are what can still execute, and its full
            // quantity is the bound on that. Using the quantity keeps the two
            // passes identical, which is the whole point of `pending`.
            count(
                &mut total,
                &mut exclusive,
                member.quantity,
                member.link.as_ref(),
            );
        }
        exclusive
            .values()
            .fold(total, |sum, qty| sum.saturating_add(*qty))
    }

    /// How large a position in `symbol` this account can carry if `pending`
    /// joins the live book.
    ///
    /// The cap is a size, not a net. A long ten plus a working sell ten plus
    /// an incoming buy ten can reach twenty long if the buy fills first; summing
    /// the signed quantities would call that ten and let it through. The number
    /// returned is the largest |qty| the book can reach given worst-case fill
    /// order of the working book and this submit.
    ///
    /// Under netting the positions collapse to one qty, so that largest is the
    /// worse of the two extreme nets: every buy fills first, or every sell
    /// fills first. A flip through a long ten to a short five never holds more
    /// than ten, and is not refused on that account. Under hedging the sides
    /// coexist, so the same inputs are the larger of the two sides rather than
    /// a net that cancelled them.
    ///
    /// Reduce-only working orders are left out, because they cannot grow a
    /// side. What the exclusion actually guards is the oversized reduce-only
    /// leave: a reduce-only no larger than the side it sits against only ever
    /// moves an extreme toward zero, which never wins the max, so counting it
    /// would change no answer. A reduce-only may rest for more than the
    /// position - it is clamped at the fill, not at rest - and counting that
    /// one would invent a short it can never open, refusing an order over a
    /// size the book cannot reach.
    ///
    /// Held children and mutually exclusive legs are handled by
    /// [`Engine::worst_case_leaves`], which is the one home of what "worst-case
    /// fill order" means. The pending orders retain their linkage here, so an
    /// incoming exclusive leg is counted in its group's max instead of being
    /// added once more on top of that same group.
    #[must_use]
    pub fn projected_qty(&self, symbol: &str, pending: &[SubmitOrder]) -> Decimal {
        let (buys, sells) = (
            self.worst_case_leaves(symbol, Side::Buy, pending),
            self.worst_case_leaves(symbol, Side::Sell, pending),
        );
        match self.oms_type {
            mogwai_protocol::OmsType::Netting => {
                let net: Decimal = self
                    .account
                    .positions
                    .iter()
                    .filter(|((position_symbol, _), _)| position_symbol.as_ref() == symbol)
                    .map(|(_, position)| position.qty)
                    .sum();
                (net + buys).abs().max((net - sells).abs())
            }
            mogwai_protocol::OmsType::Hedging => {
                let (mut long, mut short) = (Decimal::ZERO, Decimal::ZERO);
                for ((position_symbol, _), position) in &self.account.positions {
                    if position_symbol.as_ref() != symbol {
                        continue;
                    }
                    if position.qty > Decimal::ZERO {
                        long += position.qty;
                    } else {
                        short += -position.qty;
                    }
                }
                (long + buys).max(short + sells)
            }
        }
    }

    pub fn positions(&self) -> Vec<Position> {
        let mut positions: Vec<Position> = self
            .account
            .positions
            .iter()
            .map(|((symbol, position_id), state)| Position {
                symbol: std::sync::Arc::clone(symbol),
                position_id: position_id.clone(),
                quantity: state.qty,
                avg_px: state.avg_px,
                mark_px: state.mark_px,
                // Saturating on overflow, NOT zero: the breach check reads a
                // saturated number from `unrealized_pnl`, and a snapshot that
                // reported zero for the same position would contradict the
                // decision the venue just made on it.
                unrealized_pnl: self
                    .instruments
                    .get(symbol)
                    .filter(|def| def.class.is_future())
                    .map_or(Decimal::ZERO, |def| {
                        position_unrealized(def, state.qty, state.avg_px, state.mark_px)
                    }),
            })
            .collect();
        positions.sort_by(|a, b| a.symbol.cmp(&b.symbol));
        positions
    }

    pub fn open_orders(&self) -> &[OpenOrder] {
        &self.open.orders
    }
}

/// Map a resting order onto its truthful `QueryOrders` row. Status derives
/// from fill progress: untouched leaves is `Accepted`, anything partially
/// filled is `PartiallyFilled`.
fn open_order_status(order: &OpenOrder) -> OrderStatusInfo {
    let filled_qty = order.submit.quantity - order.leaves_qty;
    OrderStatusInfo {
        client_order_id: order.submit.client_order_id.clone(),
        venue_order_id: order.venue_order_id.clone(),
        symbol: std::sync::Arc::clone(&order.submit.symbol),
        position_id: order.submit.position_id.clone(),
        side: order.submit.side,
        order_type: order.submit.order_type,
        time_in_force: order.submit.time_in_force,
        status: if filled_qty > Decimal::ZERO {
            WireOrderStatus::PartiallyFilled
        } else if order.ts_triggered.is_some() {
            WireOrderStatus::Triggered
        } else {
            WireOrderStatus::Accepted
        },
        quantity: order.submit.quantity,
        filled_qty,
        price: order.submit.price,
        trigger_price: order.submit.trigger_price,
        ts_triggered: order.ts_triggered,
        reduce_only: order.submit.reduce_only,
        post_only: order.submit.post_only,
        ts_accepted: order.ts_accepted,
        ts_last: order.ts_last,
    }
}

fn venue_order_sequence(id: &str) -> u64 {
    id.strip_prefix("V-")
        .and_then(|value| value.parse().ok())
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mogwai_protocol::{
        Balance, InstrumentClass, OrderFilled, OrderType, POST_ONLY_REFUSAL, Side, TimeInForce,
        WireAssetClass,
    };
    use rust_decimal::prelude::FromPrimitive;

    fn order(id: &str, qty: i64) -> SubmitOrder {
        order_with(id, Side::Buy, "BTCUSDT", qty, Some(Decimal::from(100)))
    }

    fn order_with(
        id: &str,
        side: Side,
        symbol: &str,
        qty: i64,
        price: Option<Decimal>,
    ) -> SubmitOrder {
        order_decimal(id, side, symbol, Decimal::from(qty), price)
    }

    fn order_decimal(
        id: &str,
        side: Side,
        symbol: &str,
        quantity: Decimal,
        price: Option<Decimal>,
    ) -> SubmitOrder {
        SubmitOrder {
            client_order_id: id.into(),
            symbol: symbol.into(),
            position_id: None,
            side,
            order_type: OrderType::Market,
            quantity,
            price,
            trigger_price: None,
            trail_offset: None,
            limit_offset: None,
            reduce_only: false,
            post_only: false,
            time_in_force: TimeInForce::Gtc,
            expire_time: None,
            link: None,
        }
    }

    fn stop_order(
        id: &str,
        side: Side,
        order_type: OrderType,
        trigger: i64,
        price: Option<i64>,
    ) -> SubmitOrder {
        SubmitOrder {
            client_order_id: id.into(),
            symbol: "BTCUSDT".into(),
            position_id: None,
            side,
            order_type,
            quantity: Decimal::ONE,
            price: price.map(Decimal::from),
            trigger_price: Some(Decimal::from(trigger)),
            trail_offset: None,
            limit_offset: None,
            time_in_force: TimeInForce::Gtc,
            expire_time: None,
            reduce_only: false,
            post_only: false,
            link: None,
        }
    }

    #[test]
    fn a_stop_market_rests_untriggered_until_a_print_touches_its_stop() {
        let mut e = banded(7);
        let out = e.process_with_market(
            Command::SubmitOrder(stop_order(
                "stop",
                Side::Sell,
                OrderType::StopMarket,
                90,
                None,
            )),
            10,
            Some(reading(0)),
        );
        assert!(matches!(
            out.as_slice(),
            [
                VenueMessage::OrderAccepted { .. },
                VenueMessage::AccountState(_)
            ]
        ));
        let scan = e.pending_scans().remove(0);
        assert_eq!(scan.kind, ScanKind::StopTrigger);
        let (out, emitted) = e.apply_scans(
            &[ScanResult {
                client_order_id: scan.client_order_id,
                from_ns: scan.from_ns,
                revision: scan.revision,
                hit: Some(Hit {
                    ts_ns: 11,
                    px: Decimal::from(90),
                }),
                scanned_to_ns: 11,
            }],
            11,
        );
        assert_eq!(emitted, 1);
        assert!(matches!(
            out.first(),
            Some(VenueMessage::OrderTriggered { .. })
        ));
        assert!(
            out.iter()
                .any(|event| matches!(event, VenueMessage::OrderFilled(_)))
        );
    }

    /// A record that outlives its walk must always invalidate that walk's result.
    ///
    /// `apply_scans_on_clock` admits a result only while the order's
    /// `revision` and `scanned_ns` still equal the pair the walk was planned
    /// against - that is the whole staleness guard, and it is what makes a
    /// duplicate or late delivery harmless. A triggered stop-market whose
    /// partial remainder rests `Inert` used to keep both values from before the
    /// trigger, so the very result that triggered it still matched. Applied a
    /// second time it found an order that is no longer `Conditional`, fell
    /// through to the resting-limit arm, and panicked on a market-on-trigger
    /// order's absent price.
    #[test]
    fn a_triggered_remainder_does_not_match_the_result_that_triggered_it() {
        let mut e = banded(8);
        let mut stop = stop_order("dup-stop", Side::Sell, OrderType::StopMarket, 90, None);
        stop.quantity = Decimal::from(2);
        e.arm(Divergence::PartialFillNext {
            client_order_id: "dup-stop".into(),
            fraction: Decimal::new(5, 1),
        });
        e.process_with_market(Command::SubmitOrder(stop), 10, Some(reading(0)));
        let scan = e.pending_scans().remove(0);
        let results = [ScanResult {
            client_order_id: scan.client_order_id,
            from_ns: scan.from_ns,
            revision: scan.revision,
            hit: Some(Hit {
                ts_ns: 11,
                px: Decimal::from(90),
            }),
            scanned_to_ns: 11,
        }];
        let (out, _) = e.apply_scans(&results, 11);
        assert!(
            out.iter()
                .any(|event| matches!(event, VenueMessage::OrderFilled(_))),
            "the trigger fills half: {out:?}"
        );
        assert_eq!(e.open[0].leaves_qty, Decimal::ONE, "and rests the rest");

        // The same result again. Nothing may happen: the walk it reports was
        // already spent on the trigger.
        let (again, emitted) = e.apply_scans(&results, 12);
        assert_eq!(emitted, 0, "a spent result emits nothing: {again:?}");
        assert!(
            again.is_empty(),
            "and books nothing against the remainder: {again:?}"
        );
        assert_eq!(e.open[0].leaves_qty, Decimal::ONE);
    }

    #[test]
    fn a_stop_triggers_on_a_print_exactly_at_its_stop_price() {
        let mut e = banded(8);
        let out = e.process_with_market(
            Command::SubmitOrder(stop_order(
                "touch-stop",
                Side::Buy,
                OrderType::StopMarket,
                99,
                None,
            )),
            12,
            Some(reading(0)),
        );
        assert!(matches!(
            out.get(1),
            Some(VenueMessage::OrderTriggered { .. })
        ));
    }

    #[test]
    fn a_gapped_stop_limit_triggers_and_rests_without_filling() {
        let mut e = banded(9);
        e.process(
            Command::SubmitOrder(stop_order(
                "gap",
                Side::Sell,
                OrderType::StopLimit,
                100,
                Some(99),
            )),
            10,
        );
        let scan = e.pending_scans().remove(0);
        let (out, _) = e.apply_scans(
            &[ScanResult {
                client_order_id: scan.client_order_id,
                from_ns: scan.from_ns,
                revision: scan.revision,
                hit: Some(Hit {
                    ts_ns: 11,
                    px: Decimal::from(95),
                }),
                scanned_to_ns: 11,
            }],
            11,
        );
        assert!(matches!(
            out.as_slice(),
            [VenueMessage::OrderTriggered { .. }]
        ));
        assert!(matches!(e.open[0].resting, Resting::Limit { .. }));
    }

    #[test]
    fn query_orders_reports_a_triggered_stop_limit_as_open() {
        let mut e = banded(10);
        e.process(
            Command::SubmitOrder(stop_order(
                "query-stop",
                Side::Sell,
                OrderType::StopLimit,
                100,
                Some(99),
            )),
            10,
        );
        let scan = e.pending_scans().remove(0);
        e.apply_scans(
            &[ScanResult {
                client_order_id: scan.client_order_id,
                from_ns: scan.from_ns,
                revision: scan.revision,
                hit: Some(Hit {
                    ts_ns: 11,
                    px: Decimal::from(95),
                }),
                scanned_to_ns: 11,
            }],
            11,
        );
        let snapshot = e.order_status_snapshot("q".into(), None, true, 12);
        assert_eq!(snapshot.orders[0].status, WireOrderStatus::Triggered);
        assert_eq!(snapshot.orders[0].ts_triggered, Some(11));
        assert_eq!(
            snapshot.orders[0].trigger_price,
            Some(Decimal::from(100)),
            "the row echoes the stop the consumer stated"
        );
    }

    /// Sweep one pending scan of `e` with a print at `px`, applied at `ts`.
    fn sweep(e: &mut Engine, px: i64, ts: u64) -> Vec<VenueMessage> {
        let scan = e.pending_scans().remove(0);
        let (out, _) = e.apply_scans(
            &[ScanResult {
                client_order_id: scan.client_order_id,
                from_ns: scan.from_ns,
                revision: scan.revision,
                hit: Some(Hit {
                    ts_ns: ts,
                    px: Decimal::from(px),
                }),
                scanned_to_ns: ts,
            }],
            ts,
        );
        out
    }

    fn filled(out: &[VenueMessage]) -> &OrderFilled {
        out.iter()
            .find_map(|event| match event {
                VenueMessage::OrderFilled(fill) => Some(fill),
                _ => None,
            })
            .expect("expected a fill")
    }

    #[test]
    fn a_triggered_stop_market_fills_slipped_off_the_triggering_print() {
        // The fill comes from the print that made the order live, slipped
        // adversely. Never the stop price (that is the consumer's own number) and
        // never the acceptance-time last price (that is the look-ahead's mirror
        // image: a reading the trigger did not happen at).
        let mut e = banded(11);
        e.process_with_market(
            Command::SubmitOrder(stop_order(
                "slip",
                Side::Sell,
                OrderType::StopMarket,
                90,
                None,
            )),
            10,
            Some(reading(50)),
        );
        let out = sweep(&mut e, 80, 11);
        let fill = filled(&out);
        assert!(
            fill.last_px <= Decimal::from(80),
            "a sell slips DOWN from the triggering print, got {}",
            fill.last_px
        );
        assert_ne!(fill.last_px, Decimal::from(90), "never the stop price");
        assert_ne!(
            fill.last_px,
            Decimal::from(99),
            "never the acceptance-time last price"
        );
    }

    #[test]
    fn a_stop_already_through_the_market_triggers_on_arrival() {
        // Not rejected, as nautilus' own simulated exchange would: a protective
        // leg submitted a beat late must end up protected-and-filled.
        let mut e = banded(12);
        let out = e.process_with_market(
            Command::SubmitOrder(stop_order(
                "late",
                Side::Buy,
                OrderType::StopMarket,
                99,
                None,
            )),
            12,
            Some(reading(50)),
        );
        assert!(matches!(out[0], VenueMessage::OrderAccepted { .. }));
        assert!(matches!(out[1], VenueMessage::OrderTriggered { .. }));
        let fill = filled(&out);
        assert!(
            fill.last_px >= Decimal::from(99),
            "the arrival fill is the reading's last print slipped UP for a buy"
        );
    }

    #[test]
    fn a_stop_with_ioc_or_fok_is_rejected() {
        // A now-or-never order cannot wait for a trigger, so the state is
        // removed rather than accepted and cancelled in the same breath.
        for tif in [TimeInForce::Ioc, TimeInForce::Fok] {
            let mut e = banded(13);
            let mut order = stop_order("nowait", Side::Sell, OrderType::StopMarket, 90, None);
            order.time_in_force = tif;
            let out = e.process(Command::SubmitOrder(order), 10);
            assert!(
                reject_reason(&out).starts_with("conditional orders cannot be immediate-or-cancel"),
                "{tif:?} stop must be refused"
            );
        }
    }

    #[test]
    fn conditional_field_shapes_are_rejected_by_type() {
        // The whole type table in one place: every field that is required is
        // required, and every field the venue would never consult is refused
        // rather than accepted and ignored.
        type Shape = fn(&mut SubmitOrder);
        let cases: [(&str, Shape, &str); 6] = [
            (
                "stop with no trigger",
                |o| {
                    o.order_type = OrderType::StopMarket;
                    o.price = None;
                    o.trigger_price = None;
                },
                "conditional order must carry trigger_price",
            ),
            (
                "limit with a trigger",
                |o| {
                    o.order_type = OrderType::Limit;
                    o.trigger_price = Some(Decimal::from(90));
                },
                "Limit order must not carry trigger_price",
            ),
            (
                "stop-market with a price",
                |o| {
                    o.order_type = OrderType::StopMarket;
                    o.trigger_price = Some(Decimal::from(90));
                },
                "a market-on-trigger order must not carry a price",
            ),
            (
                "stop-limit with no price",
                |o| {
                    o.order_type = OrderType::StopLimit;
                    o.price = None;
                    o.trigger_price = Some(Decimal::from(90));
                },
                "limit order must carry a price",
            ),
            (
                "off-grid trigger",
                |o| {
                    o.order_type = OrderType::StopMarket;
                    o.price = None;
                    o.trigger_price = Some(Decimal::new(900_001, 4));
                },
                "trigger price violates price increment",
            ),
            (
                "post-only market",
                |o| {
                    o.post_only = true;
                },
                POST_ONLY_REFUSAL,
            ),
        ];
        for (name, shape, reason) in cases {
            let mut e = banded(14);
            let mut order = order("shape", 1);
            shape(&mut order);
            let out = e.process(Command::SubmitOrder(order), 10);
            assert_eq!(reject_reason(&out), reason, "{name}");
        }
    }

    /// One post-only admission rule, stated once and read by both gates.
    ///
    /// The wire gate and the engine gate used to carry separate spellings of
    /// it - the wire `Limit || rests_after_trigger()`, the engine a hand-rolled
    /// `Limit | StopLimit | LimitIfTouched` - and they disagreed on
    /// `TrailingStopLimit`, which the wire admitted and the engine then rejected
    /// after the consumer had already been told its order was on its way. The
    /// order type that came apart is the one added last, which is what a
    /// hand-rolled list does to the next type anyone adds.
    ///
    /// The table is exhaustive over `OrderType` deliberately: a new variant
    /// fails to compile the match below rather than quietly inheriting a
    /// verdict nobody chose.
    #[test]
    fn post_only_is_admitted_by_one_rule_at_the_wire_and_in_the_engine() {
        use mogwai_protocol::validate_submit_order;
        let types = [
            OrderType::Market,
            OrderType::Limit,
            OrderType::StopMarket,
            OrderType::StopLimit,
            OrderType::TrailingStopMarket,
            OrderType::TrailingStopLimit,
            OrderType::MarketIfTouched,
            OrderType::LimitIfTouched,
            OrderType::MarketToLimit,
        ];
        // The message names the set, and this is checked rather than assumed:
        // both gates return the same constant, so an equality against it is
        // vacuous by construction. What is not vacuous is that the text's legal
        // list holds every legal type and no illegal one. The message it
        // replaced - "legal only on orders that rest as a limit" - names none of
        // them and is false for `MarketToLimit`, which rests its remainder as a
        // limit and is refused anyway.
        //
        // The list is parsed, not searched for substrings, and the difference is
        // the whole point of this block. A `contains` check asks only whether a
        // name appears anywhere in the message, so a strictly better message
        // that also spelled out the illegal types - "... and TrailingStopLimit
        // orders; not on Market, StopMarket, ..." - would fail it, reporting
        // illegal types as named legal. A guard that refuses an improvement to
        // the artifact it guards gets deleted rather than understood. Parsing
        // the segment between "legal only on" and the first " orders" reads the
        // list by position, so any suffix the message grows is ignored, and
        // exact name comparison removes the `StopLimit`-inside-`TrailingStopLimit`
        // hazard that the old anchored needles were working around.
        let list = POST_ONLY_REFUSAL
            .split_once("legal only on ")
            .expect("the refusal introduces its legal list with 'legal only on'")
            .1;
        let list = list
            .split_once(" orders")
            .expect("the refusal's legal list ends at ' orders'")
            .0;
        let named: Vec<&str> = list
            .split(&[',', ' '][..])
            .map(str::trim)
            .filter(|word| !word.is_empty() && *word != "and")
            .collect();
        for ty in types {
            let legal = matches!(
                ty,
                OrderType::Limit
                    | OrderType::StopLimit
                    | OrderType::LimitIfTouched
                    | OrderType::TrailingStopLimit
            );
            assert_eq!(
                named.contains(&format!("{ty:?}").as_str()),
                legal,
                "the refusal's legal list must hold the legal types and no others: {ty:?} in {named:?}"
            );
        }
        assert_eq!(
            named.len(),
            4,
            "the legal list must not name anything that is not an order type: {named:?}"
        );

        for ty in types {
            // The legal set, written out rather than derived from the predicate
            // the production code uses: a test that reuses the rule cannot
            // catch the rule being wrong.
            let legal = match ty {
                OrderType::Limit
                | OrderType::StopLimit
                | OrderType::LimitIfTouched
                | OrderType::TrailingStopLimit => true,
                OrderType::Market
                | OrderType::StopMarket
                | OrderType::TrailingStopMarket
                | OrderType::MarketIfTouched
                | OrderType::MarketToLimit => false,
            };
            let mut submit = match ty {
                OrderType::TrailingStopLimit => trailing_stop_limit("PO", Side::Sell, 90, 10, 2),
                OrderType::TrailingStopMarket => trailing_stop("PO", Side::Sell, 90, 10),
                OrderType::StopMarket | OrderType::MarketIfTouched => {
                    stop_order("PO", Side::Buy, ty, 110, None)
                }
                OrderType::StopLimit | OrderType::LimitIfTouched => {
                    stop_order("PO", Side::Buy, ty, 110, Some(100))
                }
                OrderType::Market | OrderType::Limit | OrderType::MarketToLimit => {
                    let mut submit = order("PO", 1);
                    submit.order_type = ty;
                    submit
                }
            };
            submit.post_only = true;
            let wire = validate_submit_order(&submit);
            assert_eq!(
                wire.is_ok(),
                legal,
                "the wire gate must admit post_only on exactly the resting-limit types: {ty:?}"
            );
            let mut engine = banded(14);
            let out = engine.process(Command::SubmitOrder(submit), 10);
            if legal {
                assert!(
                    matches!(out[0], VenueMessage::OrderAccepted { .. }),
                    "an order the wire admitted must not be rejected by the engine: {ty:?}"
                );
            } else {
                assert_eq!(
                    wire.unwrap_err(),
                    POST_ONLY_REFUSAL,
                    "both gates state the legal set rather than a rule that is false for MarketToLimit: {ty:?}"
                );
                assert_eq!(reject_reason(&out), POST_ONLY_REFUSAL, "{ty:?}");
                // The refusal touches nothing: no book entry, no closed row, no
                // reserved client order id.
                assert!(engine.open.is_empty(), "{ty:?} left an open order");
                assert!(engine.closed.is_empty(), "{ty:?} left a closed row");
                assert!(
                    engine.seen_client_order_ids.is_empty(),
                    "{ty:?} burned its client order id"
                );
            }
        }

        // An order that breaks both rules at once. Every case above carries the
        // default `Gtc`, so none of them can see the second half of the defect:
        // unifying the predicate while the two gates reach it in opposite
        // orders leaves one order earning two different refusals depending on
        // which gate spoke, which is the same "a consumer cannot tell which of
        // them spoke" the shared constant exists to remove. A post-only
        // `StopMarket` marked `Ioc` is illegal twice over - post-only on a
        // market-on-trigger type, and now-or-never on a conditional - and both
        // gates must name post-only, because both check it first.
        let mut both = stop_order("PO-BOTH", Side::Buy, OrderType::StopMarket, 110, None);
        both.post_only = true;
        both.time_in_force = TimeInForce::Ioc;
        assert_eq!(
            validate_submit_order(&both).unwrap_err(),
            POST_ONLY_REFUSAL,
            "the wire gate checks post-only before the conditional-IOC rule"
        );
        let mut engine = banded(14);
        let out = engine.process(Command::SubmitOrder(both), 10);
        assert_eq!(
            reject_reason(&out),
            POST_ONLY_REFUSAL,
            "the engine must reach the same rule in the same order as the wire"
        );
    }

    /// The precedence a `MarketToLimit` order's type and its time in force
    /// would otherwise argue over: the time in force governs the remainder.
    ///
    /// The type's own doc says it rests the remainder and an IOC's says it
    /// cancels one, which reads as a contradiction the venue admits. It is not
    /// one: the type says what happens to a remainder, the time in force says
    /// whether there is one to keep, which is why the combination is admitted
    /// rather than refused.
    ///
    /// Both halves of the type are pinned here, because until 2026-08-19 it
    /// implemented neither and this test recorded that. It filled its whole
    /// quantity at its own stated limit with no reference to the tape - so no
    /// remainder arose on the clean path at all, and the only way to reach the
    /// remainder question was to manufacture one with an armed
    /// `PartialFillNext`, which is what this test still does because it is also
    /// the cheapest way to reach it. The fill price assertion is the first half:
    /// the market here is 99 against a limit of 100, so a fill at 100 is the old
    /// behaviour and a fill at 99 is the type taking what the touch offers. The
    /// `Gtc` arm is the second half.
    #[test]
    fn a_market_to_limit_remainder_is_governed_by_its_time_in_force() {
        let armed = |id: &str, tif| {
            let mut submit = order(id, 2);
            submit.order_type = OrderType::MarketToLimit;
            submit.time_in_force = tif;
            let mut engine = banded(14);
            engine.arm(Divergence::PartialFillNext {
                client_order_id: id.into(),
                fraction: Decimal::new(5, 1),
            });
            let out =
                engine.process_with_market(Command::SubmitOrder(submit), 10, Some(reading(0)));
            (engine, out)
        };

        // FOK is now-or-never and the partial makes it never: rejected before
        // acceptance, so nothing is booked at all.
        let (engine, out) = armed("mtl-fok", TimeInForce::Fok);
        assert_eq!(reject_reason(&out), "fill-or-kill could not fully fill");
        assert!(engine.open.is_empty());
        assert!(engine.closed.is_empty());

        // IOC cancels its remainder. The type wanted to rest it; the time in
        // force wins, and the consumer is told so with an explicit cancel.
        let (engine, out) = armed("mtl-ioc", TimeInForce::Ioc);
        assert!(matches!(out[0], VenueMessage::OrderAccepted { .. }));
        assert!(matches!(
            &out[1],
            VenueMessage::OrderFilled(fill)
                if fill.last_qty == Decimal::ONE && fill.leaves_qty == Decimal::ONE
        ));
        assert!(matches!(out[2], VenueMessage::OrderCanceled { .. }));
        assert!(engine.open.is_empty(), "an IOC remainder does not rest");
        assert_eq!(engine.closed["mtl-ioc"].status, WireOrderStatus::Canceled);

        // GTC keeps the remainder, and it rests as a limit at the order's own
        // stated price - which is what makes keeping it mean anything. An inert
        // remainder is on the book with a positive `leaves_qty` and offered to
        // no sweep, so it can neither fill nor expire.
        let (engine, out) = armed("mtl-gtc", TimeInForce::Gtc);
        let fill = out
            .iter()
            .find_map(|event| match event {
                VenueMessage::OrderFilled(fill) => Some(fill),
                _ => None,
            })
            .expect("the market-to-limit takes the touch");
        assert_eq!(
            fill.last_px,
            Decimal::from(99),
            "a market-to-limit takes the market, not its own limit of 100"
        );
        assert_eq!(engine.open.len(), 1);
        assert_eq!(engine.open[0].leaves_qty, Decimal::ONE);
        assert!(matches!(engine.open[0].resting, Resting::Limit { .. }));
        assert_eq!(
            engine.pending_scans().len(),
            1,
            "a kept remainder is offered to the sweep"
        );

        // And the limit binds the first act too. A market at 101 is short of a
        // buy limit of 100, so there is nothing to take: the whole quantity
        // rests rather than filling at a price the consumer refused. This is the
        // arm that separates "takes the market" from "fills at the market
        // whatever it is", and it needs no divergence to reach.
        let mut submit = order("mtl-away", 2);
        submit.order_type = OrderType::MarketToLimit;
        let mut engine = banded(14);
        let out = engine.process_with_market(
            Command::SubmitOrder(submit),
            10,
            Some(MarketReading {
                last_px: Decimal::from(101),
                ts_ns: 0,
                band_ticks: 0,
            }),
        );
        assert!(
            !out.iter()
                .any(|event| matches!(event, VenueMessage::OrderFilled(_))),
            "a market-to-limit does not trade through its own limit: {out:?}"
        );
        assert_eq!(engine.open.len(), 1);
        assert_eq!(engine.open[0].leaves_qty, Decimal::from(2));
        assert!(matches!(engine.open[0].resting, Resting::Limit { .. }));
    }

    #[test]
    fn a_post_only_order_that_would_take_liquidity_is_rejected() {
        // On arrival for a limit, and at trigger time for a stop-limit - after
        // the trigger, which did happen. Rejected rather than canceled: it is
        // the venue refusing the order's own stated terms.
        let mut e = banded(15);
        let mut taker = limit_order("taker", 1);
        taker.post_only = true;
        let out = e.process_with_market(Command::SubmitOrder(taker), 10, Some(reading(0)));
        assert_eq!(reject_reason(&out), "post-only order would take liquidity");

        let mut e = banded(15);
        let mut stop = stop_order("post-stop", Side::Buy, OrderType::StopLimit, 99, Some(100));
        stop.post_only = true;
        let out = e.process_with_market(Command::SubmitOrder(stop), 10, Some(reading(0)));
        assert!(matches!(out[0], VenueMessage::OrderAccepted { .. }));
        assert!(matches!(out[1], VenueMessage::OrderTriggered { .. }));
        assert!(
            matches!(&out[2], VenueMessage::OrderRejected { reason, .. } if reason == "post-only order would take liquidity")
        );
        assert!(e.open.is_empty(), "the rejected order leaves the book");
        assert_eq!(
            e.closed["post-stop"].status,
            WireOrderStatus::Rejected,
            "a rejection after acceptance is a closed row a query can report"
        );
        assert!(matches!(out[3], VenueMessage::AccountState(_)));
    }

    #[test]
    fn a_post_only_reprice_that_would_take_liquidity_is_rejected() {
        let mut e = banded(15);
        let mut resting = limit_order("post-amend", 1);
        resting.price = Some(Decimal::from(90));
        resting.post_only = true;
        let accepted = e.process_with_market(Command::SubmitOrder(resting), 10, Some(reading(0)));
        assert!(matches!(accepted[0], VenueMessage::OrderAccepted { .. }));

        let out = e.process_with_market(
            Command::ModifyOrder {
                client_order_id: "post-amend".into(),
                price: Some(Decimal::from(100)),
                quantity: None,
                trigger_price: None,
            },
            11,
            Some(reading(0)),
        );
        assert!(matches!(
            &out[0],
            VenueMessage::OrderModifyRejected { reason, .. }
                if reason == "post-only order would take liquidity"
        ));
        assert_eq!(e.open_orders()[0].submit.price, Some(Decimal::from(90)));
    }

    #[test]
    fn a_reduce_only_order_rests_while_flat_on_a_funded_account() {
        // The admission exemption: a protective sell-stop placed while flat
        // holds no base, so the funded-sell check must not refuse it and it must
        // place no hold - otherwise the shape this whole surface exists to
        // serve is unreachable on the only account mode that checks anything.
        let mut e = funded(1_000);
        let mut stop = stop_order("protect", Side::Sell, OrderType::StopMarket, 90, None);
        stop.reduce_only = true;
        let out = e.process(Command::SubmitOrder(stop), 10);
        assert!(matches!(out[0], VenueMessage::OrderAccepted { .. }));
        let state = account(&out, out.len() - 1);
        assert_eq!(balance(state, "USDT").locked, Decimal::ZERO);
        assert_eq!(balance(state, "USDT").free, Decimal::from(1_000));
    }

    #[test]
    fn a_reduce_only_order_is_capped_by_the_position_and_cancels_when_flat() {
        let mut e = banded(16);
        let mut stop = stop_order("flat", Side::Sell, OrderType::StopMarket, 90, None);
        stop.reduce_only = true;
        e.process(Command::SubmitOrder(stop), 10);
        let out = sweep(&mut e, 90, 11);
        assert!(matches!(out[0], VenueMessage::OrderTriggered { .. }));
        assert!(
            matches!(out[1], VenueMessage::OrderCanceled { .. }),
            "a cap of zero cancels rather than opening a fresh short"
        );
        assert!(
            !out.iter()
                .any(|event| matches!(event, VenueMessage::OrderFilled(_))),
            "nothing may fill against a position that is already gone"
        );
        assert_eq!(e.closed["flat"].status, WireOrderStatus::Canceled);
        assert!(matches!(out[2], VenueMessage::AccountState(_)));
    }

    #[test]
    fn a_cap_clamped_reduce_only_fill_cancels_its_remainder() {
        // The remainder can never again have a non-zero cap, and an `Inert`
        // remainder reaches no further fill decision, so it would sit open
        // forever. It is closed in the same batch instead.
        let mut e = banded(17);
        e.process(Command::SubmitOrder(order("open-1", 1)), 10);
        let mut stop = stop_order("clamped", Side::Sell, OrderType::StopMarket, 90, None);
        stop.reduce_only = true;
        stop.quantity = Decimal::from(3);
        e.process(Command::SubmitOrder(stop), 11);
        let out = sweep(&mut e, 90, 12);
        assert_eq!(
            filled(&out).last_qty,
            Decimal::ONE,
            "clamped to the position"
        );
        assert!(
            out.iter()
                .any(|event| matches!(event, VenueMessage::OrderCanceled { .. }))
        );
        assert!(e.open.is_empty(), "no Inert remainder is left behind");
        assert_eq!(e.closed["clamped"].status, WireOrderStatus::Canceled);
    }

    #[test]
    fn an_untriggered_buy_stop_holds_against_its_trigger_price() {
        // A stop-market has no price, so the hold is the only number it
        // has. Under-held by exactly the slippage, which the fill-time
        // re-check is what covers.
        let mut e = funded(1_000);
        let mut stop = stop_order("hold", Side::Buy, OrderType::StopMarket, 100, None);
        stop.quantity = Decimal::from(2);
        let out = e.process(Command::SubmitOrder(stop), 10);
        let state = account(&out, out.len() - 1);
        assert_eq!(balance(state, "USDT").locked, Decimal::from(200));
        assert_eq!(balance(state, "USDT").free, Decimal::from(800));
    }

    #[test]
    fn a_fully_funded_buy_stop_does_not_fail_its_own_trigger_on_its_own_hold() {
        // The double-count: at trigger time the order IS resting, so its own
        // hold has already left `free_balance`. Comparing the notional against
        // that would fail a fully funded order at zero slippage.
        let mut e = funded(200);
        let mut stop = stop_order("own-hold", Side::Buy, OrderType::StopMarket, 100, None);
        stop.quantity = Decimal::from(2);
        e.process(Command::SubmitOrder(stop), 10);
        let out = sweep(&mut e, 100, 11);
        assert_eq!(filled(&out).last_qty, Decimal::from(2));
        assert_eq!(e.closed["own-hold"].status, WireOrderStatus::Filled);
    }

    #[test]
    fn a_triggered_stop_limit_rests_banded_and_does_not_fill_for_free() {
        // The trigger pass covered `(from, reached]`, so the live limit resumes
        // from exactly there - never from the pass instant, which on a
        // budget-truncated walk would skip a span nothing looked at.
        let mut e = banded(18);
        e.process(
            Command::SubmitOrder(stop_order(
                "banded",
                Side::Sell,
                OrderType::StopLimit,
                100,
                Some(99),
            )),
            10,
        );
        let scan = e.pending_scans().remove(0);
        let (out, emitted) = e.apply_scans(
            &[ScanResult {
                client_order_id: scan.client_order_id,
                from_ns: scan.from_ns,
                revision: scan.revision,
                hit: Some(Hit {
                    ts_ns: 11,
                    px: Decimal::from(95),
                }),
                // The walk ran out of budget at 11 even though the pass targeted 40.
                scanned_to_ns: 11,
            }],
            40,
        );
        assert_eq!(emitted, 1, "a trigger-only pass still reserves its frame");
        assert!(matches!(
            out.as_slice(),
            [VenueMessage::OrderTriggered { .. }]
        ));
        assert_eq!(
            e.open[0].scanned_ns, 11,
            "the frontier is where the walk REACHED, not the pass instant"
        );
        let next = e.pending_scans().remove(0);
        assert_eq!(next.kind, ScanKind::FillThrough);
        assert_eq!(next.from_ns, 11);
        assert_eq!(next.revision, e.open[0].revision);
    }

    #[test]
    fn a_triggered_stop_limit_marketable_against_its_trigger_print_fills_at_once() {
        // The print that made the order live is offered to it: a limit resting
        // with its frontier past that print would discard a fill it was owed.
        let mut e = banded(19);
        let out = e.process_with_market(
            Command::SubmitOrder(stop_order(
                "atonce",
                Side::Buy,
                OrderType::StopLimit,
                99,
                Some(100),
            )),
            10,
            Some(reading(0)),
        );
        assert!(matches!(out[1], VenueMessage::OrderTriggered { .. }));
        assert_eq!(
            filled(&out).last_px,
            Decimal::from(100),
            "a triggered stop-limit fills at its own stated price"
        );
    }

    #[test]
    fn partial_fill_next_lands_on_the_fill_the_trigger_produces() {
        // The arm targets a fill. A stop-limit that triggers and rests produced
        // none, so the arm must survive the trigger and fire on the sweep fill
        // that follows.
        let mut e = banded(20);
        let mut stop = stop_order("armed", Side::Sell, OrderType::StopLimit, 100, Some(99));
        stop.quantity = Decimal::from(2);
        e.process(Command::SubmitOrder(stop), 10);
        e.arm(Divergence::PartialFillNext {
            client_order_id: "armed".into(),
            fraction: Decimal::new(5, 1),
        });
        let out = sweep(&mut e, 95, 11);
        assert!(
            matches!(out.as_slice(), [VenueMessage::OrderTriggered { .. }]),
            "the gap triggered it and nothing filled"
        );
        let out = sweep(&mut e, 90, 12);
        assert_eq!(
            filled(&out).last_qty,
            Decimal::ONE,
            "the arm was still armed and fired on the fill the trigger produced"
        );
    }

    #[test]
    fn a_silent_cancel_racing_a_trigger_leaves_the_order_canceled() {
        // The composition section 1.11 pins: `CancelOpenOrderSilently` takes the
        // engine lock while a walk that already found the trigger print is in
        // flight. The silent cancel bumps no revision - it removes the order -
        // so the in-flight `ScanResult` fails its `client_order_id` lookup and
        // is dropped. The order is canceled, no trigger is published, and no
        // fill is booked. This is the existing revision-guard contract reaching
        // a conditional, not new machinery.
        let mut e = banded(31);
        e.process(
            Command::SubmitOrder(stop_order(
                "raced",
                Side::Sell,
                OrderType::StopMarket,
                90,
                None,
            )),
            10,
        );
        // Planned off the book before the cancel, exactly as the sweeper plans
        // its walk off the lock and applies the result after re-taking it.
        let scan = e.pending_scans().remove(0);
        e.cancel_open_order_silently("raced", 11)
            .expect("an untriggered conditional is a resting order");

        let (out, emitted) = e.apply_scans(
            &[ScanResult {
                client_order_id: scan.client_order_id,
                from_ns: scan.from_ns,
                revision: scan.revision,
                hit: Some(Hit {
                    ts_ns: 12,
                    px: Decimal::from(90),
                }),
                scanned_to_ns: 12,
            }],
            12,
        );
        assert!(
            out.is_empty(),
            "the venue cancelled silently: the raced trigger publishes nothing, got {out:?}"
        );
        assert_eq!(emitted, 0, "a dropped scan reserves no delivery bytes");
        assert!(
            e.open_orders().is_empty(),
            "the order is gone from the book"
        );
        assert!(
            e.fill_snapshot("f-1".into(), Some("raced"), 13)
                .fills
                .is_empty(),
            "a canceled order must book no fill from the trigger that raced it"
        );

        // Only a `QueryOrders` poll reveals it - the highest-value coverage the
        // arm buys on a protective leg.
        let snapshot = e.order_status_snapshot("q-1".into(), Some("raced"), false, 13);
        let row = snapshot.orders.first().expect("the truth store retains it");
        assert_eq!(row.status, WireOrderStatus::Canceled);
        assert_eq!(row.ts_triggered, None, "it never triggered");
        assert_eq!(row.filled_qty, Decimal::ZERO);
    }

    #[test]
    fn a_trigger_amend_restarts_the_trigger_window_and_is_rejected_after_triggering() {
        let mut e = banded(21);
        e.process(
            Command::SubmitOrder(stop_order(
                "amend-stop",
                Side::Sell,
                OrderType::StopMarket,
                90,
                None,
            )),
            10,
        );
        let out = e.process(
            Command::ModifyOrder {
                client_order_id: "amend-stop".into(),
                price: None,
                quantity: None,
                trigger_price: Some(Decimal::from(80)),
            },
            20,
        );
        assert!(
            matches!(&out[0], VenueMessage::OrderUpdated { trigger_price, .. } if *trigger_price == Some(Decimal::from(80))),
            "the ack has to carry the new trigger or the amend is unverifiable"
        );
        assert_eq!(e.open[0].scanned_ns, 20, "the trigger window restarts");
        let scan = e.pending_scans().remove(0);
        assert_eq!(scan.px, Decimal::from(80));

        // After the trigger there is nothing left to trigger, and silently
        // ignoring the field would make the amend a lie. A stop-limit is the
        // shape that survives its own trigger, so it is what proves the
        // refusal rather than the terminal-order one.
        let mut e = banded(21);
        e.process(
            Command::SubmitOrder(stop_order(
                "fired",
                Side::Sell,
                OrderType::StopLimit,
                100,
                Some(99),
            )),
            10,
        );
        sweep(&mut e, 95, 11);
        let out = e.process(
            Command::ModifyOrder {
                client_order_id: "fired".into(),
                price: None,
                quantity: None,
                trigger_price: Some(Decimal::from(70)),
            },
            12,
        );
        assert!(
            matches!(&out[0], VenueMessage::OrderModifyRejected { reason, .. } if reason == "order has already triggered"),
            "got {out:?}"
        );

        // And on an order that never had a trigger, the reason says so.
        let mut e = banded(21);
        e.process(Command::SubmitOrder(limit_order("plain", 1)), 10);
        let out = e.process(
            Command::ModifyOrder {
                client_order_id: "plain".into(),
                price: None,
                quantity: None,
                trigger_price: Some(Decimal::from(70)),
            },
            11,
        );
        assert!(
            matches!(&out[0], VenueMessage::OrderModifyRejected { reason, .. } if reason == "order carries no trigger to amend"),
            "got {out:?}"
        );
    }

    #[test]
    fn a_price_amend_on_an_untriggered_stop_limit_keeps_it_conditional() {
        // It changes the limit the order will take, not the price the tape has
        // to touch: the trigger window stands and the order stays conditional.
        // Promoting it here would make the venue fill a stop that never fired.
        let mut e = banded(22);
        e.process(
            Command::SubmitOrder(stop_order(
                "repriced",
                Side::Sell,
                OrderType::StopLimit,
                100,
                Some(99),
            )),
            10,
        );
        e.process(
            Command::ModifyOrder {
                client_order_id: "repriced".into(),
                price: Some(Decimal::from(98)),
                quantity: None,
                trigger_price: None,
            },
            20,
        );
        assert!(
            matches!(e.open[0].resting, Resting::Conditional { stop_px, .. } if stop_px == Decimal::from(100))
        );
        assert_eq!(e.open[0].scanned_ns, 10, "the trigger window is untouched");
        assert_eq!(e.open[0].submit.price, Some(Decimal::from(98)));
        let scan = e.pending_scans().remove(0);
        assert_eq!(scan.kind, ScanKind::StopTrigger);
    }

    #[test]
    fn a_price_amend_on_a_stop_market_is_refused() {
        // It carries no price by construction, so an amend must not be able to
        // give it one - `held_balances` and the RNG key both read `price` in
        // preference to the trigger.
        let mut e = banded(23);
        e.process(
            Command::SubmitOrder(stop_order(
                "priceless",
                Side::Sell,
                OrderType::StopMarket,
                90,
                None,
            )),
            10,
        );
        let out = e.process(
            Command::ModifyOrder {
                client_order_id: "priceless".into(),
                price: Some(Decimal::from(80)),
                quantity: None,
                trigger_price: None,
            },
            20,
        );
        assert!(
            matches!(&out[0], VenueMessage::OrderModifyRejected { reason, .. } if reason == "StopMarket order must not carry a price")
        );
        assert_eq!(e.open[0].submit.price, None);
    }

    #[test]
    fn query_orders_distinguishes_untriggered_triggered_and_partially_filled() {
        let mut e = banded(24);
        let mut stop = stop_order("ladder", Side::Sell, OrderType::StopLimit, 100, Some(99));
        stop.quantity = Decimal::from(2);
        e.process(Command::SubmitOrder(stop), 10);
        let row = |e: &mut Engine, ts| {
            e.order_status_snapshot("q".into(), None, true, ts)
                .orders
                .remove(0)
        };
        assert_eq!(row(&mut e, 11).status, WireOrderStatus::Accepted);

        sweep(&mut e, 95, 12);
        assert_eq!(row(&mut e, 13).status, WireOrderStatus::Triggered);

        e.arm(Divergence::PartialFillNext {
            client_order_id: "ladder".into(),
            fraction: Decimal::new(5, 1),
        });
        sweep(&mut e, 90, 14);
        let row = row(&mut e, 15);
        assert_eq!(
            row.status,
            WireOrderStatus::PartiallyFilled,
            "a partial fill is the more specific truth"
        );
        assert_eq!(row.ts_triggered, Some(12), "the trigger instant survives");
    }

    fn account(out: &[VenueMessage], index: usize) -> &AccountState {
        let VenueMessage::AccountState(state) = &out[index] else {
            panic!("expected account state")
        };
        state
    }

    #[test]
    fn account_snapshot_is_empty_before_any_fill() {
        let mut e = Engine::new();
        let state = e.account_snapshot(7);
        assert!(state.balances.is_empty());
        assert!(state.positions.is_empty());
        assert_eq!(state.ts_event, 7);
    }

    #[test]
    fn seeded_balances_fund_the_account_and_fills_ride_the_seed() {
        // The funded account is the initial condition, not a booked event: the
        // seed shows up in the first snapshot untouched, and a fill's delta
        // applies ON TOP of it - a buy's spend debits the funded quote leg
        // instead of driving it negative from zero.
        let mut e = Engine::build(EngineConfig {
            account_id: test_account_id(),
            instruments: default_instruments(),
            balances: HashMap::from([("USDT".to_string(), Decimal::from(1_000))]),
            fill_seed: 0,
        });

        let state = e.account_snapshot(1);
        assert_eq!(state.balances.len(), 1);
        let usdt = balance(&state, "USDT");
        assert_eq!(usdt.total, Decimal::from(1_000));
        assert_eq!(usdt.free, Decimal::from(1_000));
        assert_eq!(usdt.locked, Decimal::ZERO);
        assert!(state.positions.is_empty());

        // Buy 2 @ 100: quote debits to 800, base credits to 2.
        let out = e.process(Command::SubmitOrder(order("F1", 2)), 2);
        let state = account(&out, out.len() - 1);
        assert_eq!(balance(state, "USDT").total, Decimal::from(800));
        assert_eq!(balance(state, "BTC").total, Decimal::from(2));
    }

    #[test]
    fn snapshot_stamps_the_engines_own_account_id() {
        // The snapshot is the wire's only statement of whose ledger it is, and
        // the consumer rejects a mismatch, so a wrong stamp here is a run that
        // errors on its first fill rather than a silent relabel.
        let mut e = Engine::build(EngineConfig {
            account_id: AccountId::parse("WYRD-042:BTCUSDT").expect("deployment-shaped id"),
            instruments: default_instruments(),
            balances: HashMap::new(),
            fill_seed: 0,
        });
        assert_eq!(
            e.account_snapshot(1).account_id.as_str(),
            "WYRD-042:BTCUSDT"
        );
    }

    fn test_account_id() -> AccountId {
        AccountId::parse("TEST-001").expect("static account id")
    }

    fn funded(usdt: i64) -> Engine {
        Engine::build(EngineConfig {
            account_id: test_account_id(),
            instruments: default_instruments(),
            balances: HashMap::from([("USDT".to_string(), Decimal::from(usdt))]),
            fill_seed: 0,
        })
    }

    fn futures_engine(cash: i64, action: MarginBreachAction) -> Engine {
        let def = InstrumentDef {
            symbol: "MNQ".into(),
            class: InstrumentClass::Future {
                underlying: "NQ".into(),
                settlement_currency: "USD".into(),
                multiplier: Decimal::from(2),
                asset_class: WireAssetClass::Index,
            },
            price_precision: 2,
            size_precision: 0,
            price_increment: Decimal::new(25, 2),
            size_increment: Decimal::ONE,
        };
        let mut engine = Engine::build(EngineConfig {
            account_id: test_account_id(),
            instruments: vec![def],
            balances: HashMap::from([("USD".into(), Decimal::from(cash))]),
            fill_seed: 11,
        });
        engine.set_margin_policy(
            "MNQ".into(),
            MarginPolicy {
                initial_per_contract: Decimal::from(2000),
                maintenance_per_contract: Decimal::from(1800),
                breach_action: action,
                basis: MarginBasis::PerContract,
            },
        );
        engine
    }

    fn mnq_order(id: &str, side: Side, quantity: i64, price: i64) -> SubmitOrder {
        order_with(id, side, "MNQ", quantity, Some(Decimal::from(price)))
    }

    fn fill_future(engine: &mut Engine, id: &str, side: Side, quantity: i64, price: i64) {
        let events = engine.process_with_market(
            Command::SubmitOrder(mnq_order(id, side, quantity, price)),
            1,
            Some(MarketReading {
                last_px: Decimal::from(price),
                ts_ns: 1,
                band_ticks: 0,
            }),
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, VenueMessage::OrderFilled(_))),
            "{events:?}"
        );
    }

    fn future_fill(
        engine: &mut Engine,
        id: &str,
        quantity: i64,
        price: i64,
        ts: u64,
    ) -> OrderFilled {
        engine
            .process_with_market(
                Command::SubmitOrder(mnq_order(id, Side::Buy, quantity, price)),
                ts,
                Some(MarketReading {
                    last_px: Decimal::from(price),
                    ts_ns: ts,
                    band_ticks: 0,
                }),
            )
            .into_iter()
            .find_map(|event| match event {
                VenueMessage::OrderFilled(fill) => Some(fill),
                _ => None,
            })
            .expect("future fills")
    }

    #[test]
    fn a_futures_fill_books_no_base_currency_leg() {
        let mut engine = futures_engine(10_000, MarginBreachAction::Refuse);
        fill_future(&mut engine, "F-1", Side::Buy, 1, 21_000);
        let state = engine.account_snapshot(2);
        assert_eq!(state.balances.len(), 1);
        assert_eq!(state.balances[0].currency, "USD");
    }

    #[test]
    fn a_futures_position_values_at_multiplier_times_points() {
        let mut engine = futures_engine(10_000, MarginBreachAction::Refuse);
        fill_future(&mut engine, "F-1", Side::Buy, 2, 21_000);
        engine.mark(&[("MNQ".into(), Decimal::from(21_001))], 2);
        assert_eq!(engine.unrealized_pnl("MNQ"), Decimal::from(4));
    }

    #[test]
    fn a_fresh_futures_position_is_marked_at_its_fill_price() {
        let mut engine = futures_engine(10_000, MarginBreachAction::Refuse);
        fill_future(&mut engine, "F-1", Side::Buy, 1, 21_000);
        let position = &engine.account_snapshot(2).positions[0];
        assert_eq!(position.mark_px, Decimal::from(21_000));
        assert_eq!(position.unrealized_pnl, Decimal::ZERO);
    }

    #[test]
    fn flipping_a_futures_position_preserves_its_last_mark() {
        let mut engine = futures_engine(10_000, MarginBreachAction::Refuse);
        fill_future(&mut engine, "F-1", Side::Buy, 1, 21_000);
        engine.mark(&[("MNQ".into(), Decimal::from(21_100))], 2);
        fill_future(&mut engine, "F-2", Side::Sell, 2, 21_050);
        let position = &engine.account_snapshot(3).positions[0];
        assert_eq!(position.quantity, -Decimal::ONE);
        assert_eq!(position.avg_px, Decimal::from(21_050));
        assert_eq!(position.mark_px, Decimal::from(21_100));
    }

    #[test]
    fn a_resting_futures_order_holds_margin_not_notional() {
        for side in [Side::Buy, Side::Sell] {
            let mut engine = futures_engine(50_000, MarginBreachAction::Refuse);
            let mut order = mnq_order("REST", side, 1, 21_000);
            order.order_type = OrderType::Limit;
            engine.process(Command::SubmitOrder(order), 1);
            let usd = engine
                .account_snapshot(2)
                .balances
                .into_iter()
                .find(|balance| balance.currency == "USD")
                .unwrap();
            assert_eq!(usd.locked, Decimal::from(2000));
        }
    }

    #[test]
    fn margin_requirement_keeps_two_usd_settled_futures_as_two_rows() {
        let make_def = |symbol: &str, underlying: &str| InstrumentDef {
            symbol: symbol.into(),
            class: InstrumentClass::Future {
                underlying: underlying.into(),
                settlement_currency: "USD".into(),
                multiplier: Decimal::from(2),
                asset_class: WireAssetClass::Index,
            },
            price_precision: 2,
            size_precision: 0,
            price_increment: Decimal::new(25, 2),
            size_increment: Decimal::ONE,
        };
        let mut engine = Engine::build(EngineConfig {
            account_id: test_account_id(),
            instruments: vec![make_def("MNQ", "NQ"), make_def("MES", "ES")],
            balances: HashMap::from([("USD".into(), Decimal::from(20_000))]),
            fill_seed: 1,
        });
        for symbol in ["MNQ", "MES"] {
            engine.set_margin_policy(
                symbol.into(),
                MarginPolicy {
                    initial_per_contract: Decimal::from(2_000),
                    maintenance_per_contract: Decimal::from(1_800),
                    breach_action: MarginBreachAction::Refuse,
                    basis: Default::default(),
                },
            );
            let order = order_with(symbol, Side::Buy, symbol, 1, Some(Decimal::from(21_000)));
            let events = engine.process_with_market(
                Command::SubmitOrder(order),
                1,
                Some(MarketReading {
                    last_px: Decimal::from(21_000),
                    ts_ns: 1,
                    band_ticks: 0,
                }),
            );
            assert!(
                events
                    .iter()
                    .any(|event| matches!(event, VenueMessage::OrderFilled(_))),
                "{events:?}"
            );
        }
        let state = engine.account_snapshot(2);
        assert_eq!(state.margins.len(), 2);
        assert_eq!(
            state
                .margins
                .iter()
                .map(|row| row.currency.as_str())
                .collect::<Vec<_>>(),
            vec!["USD", "USD"]
        );
        assert_ne!(state.margins[0].symbol, state.margins[1].symbol);
    }

    #[test]
    fn a_futures_fill_is_funds_checked_against_margin_not_notional() {
        let mut engine = futures_engine(2500, MarginBreachAction::Refuse);
        fill_future(&mut engine, "F-1", Side::Buy, 1, 21_000);
    }

    #[test]
    fn two_reduce_only_legs_place_no_hold_against_one_position() {
        let mut engine = futures_engine(10_000, MarginBreachAction::Refuse);
        fill_future(&mut engine, "F-1", Side::Buy, 1, 21_000);
        for (id, price) in [("STOP", 20_000), ("TARGET", 22_000)] {
            let mut order = mnq_order(id, Side::Sell, 1, price);
            order.order_type = OrderType::Limit;
            order.reduce_only = true;
            engine.process(Command::SubmitOrder(order), 2);
        }
        let usd = engine
            .account_snapshot(3)
            .balances
            .into_iter()
            .find(|balance| balance.currency == "USD")
            .unwrap();
        assert_eq!(usd.locked, Decimal::from(1800));
    }

    #[test]
    fn daily_settlement_moves_unrealized_into_cash_and_resets_avg_px() {
        let mut engine = futures_engine(10_000, MarginBreachAction::Refuse);
        fill_future(&mut engine, "F-1", Side::Buy, 2, 21_000);
        engine.mark(&[("MNQ".into(), Decimal::from(21_001))], 2);
        engine.settle(&[("MNQ".into(), Decimal::from(21_001))], 3);
        let state = engine.account_snapshot(3);
        assert_eq!(state.balances[0].total, Decimal::from(10_004));
        assert_eq!(state.positions[0].avg_px, Decimal::from(21_001));
        assert_eq!(state.positions[0].unrealized_pnl, Decimal::ZERO);
    }

    #[test]
    fn an_equity_above_maintenance_with_maintenance_locked_is_not_a_breach() {
        let mut engine = futures_engine(3000, MarginBreachAction::Refuse);
        fill_future(&mut engine, "F-1", Side::Buy, 1, 21_000);
        engine.mark(&[("MNQ".into(), Decimal::from(21_000))], 2);
        let events = engine.process(
            Command::SubmitOrder(mnq_order("F-2", Side::Sell, 1, 21_000)),
            3,
        );
        assert!(
            !matches!(events.first(), Some(VenueMessage::OrderRejected { reason, .. }) if reason.contains("margin breach")),
            "{events:?}"
        );
    }

    #[test]
    fn a_maintenance_breach_under_refuse_rejects_new_risk_but_not_reduce_only() {
        let mut engine = futures_engine(3000, MarginBreachAction::Refuse);
        fill_future(&mut engine, "F-1", Side::Buy, 1, 21_000);
        engine.mark(&[("MNQ".into(), Decimal::from(20_000))], 2);
        let rejected = engine.process(
            Command::SubmitOrder(mnq_order("F-2", Side::Buy, 1, 20_000)),
            3,
        );
        // The refusal always names its currency. A consumer reads a margin breach as a
        // funds outcome, and every neighbouring funds rejection carries its
        // unit; one that does not leaves the reader guessing which leg is
        // short in a multi-currency account.
        let Some(VenueMessage::OrderRejected { reason, .. }) = rejected.first() else {
            panic!("{rejected:?}");
        };
        assert_eq!(
            reason,
            "margin breach: account equity below maintenance requirement in USD"
        );
        let mut reduce = mnq_order("F-3", Side::Sell, 1, 20_000);
        reduce.reduce_only = true;
        let reduced = engine.process_with_market(
            Command::SubmitOrder(reduce),
            4,
            Some(MarketReading {
                last_px: Decimal::from(20_000),
                ts_ns: 4,
                band_ticks: 0,
            }),
        );
        assert!(
            reduced
                .iter()
                .any(|event| matches!(event, VenueMessage::OrderFilled(_))),
            "{reduced:?}"
        );
    }

    #[test]
    fn a_maintenance_breach_under_liquidate_closes_through_the_fill_band() {
        let mut engine = futures_engine(3000, MarginBreachAction::Liquidate);
        fill_future(&mut engine, "F-1", Side::Buy, 1, 21_000);
        let outcome = engine.mark(&[("MNQ".into(), Decimal::from(20_000))], 2);
        let liquidation = outcome
            .events
            .iter()
            .find_map(|event| match event {
                VenueMessage::OrderFilled(fill) if fill.client_order_id.starts_with("LQ-") => {
                    Some(fill)
                }
                _ => None,
            })
            .expect("liquidation produces an ordinary fill");
        assert_ne!(liquidation.last_px, Decimal::from(20_000));
        assert!(
            !engine
                .account
                .positions
                .keys()
                .any(|(symbol, _)| symbol.as_ref() == "MNQ")
        );
        assert_eq!(outcome.originated_orders, 1);
    }

    #[test]
    fn a_liquidation_bypasses_and_preserves_consumer_armed_divergences() {
        let mut engine = futures_engine(3_000, MarginBreachAction::Liquidate);
        fill_future(&mut engine, "F-1", Side::Buy, 1, 21_000);
        engine.arm(Divergence::RejectNextSubmit {
            reason: "consumer scenario".into(),
        });
        engine.arm(Divergence::DuplicateNextFill);
        engine.arm(Divergence::DropNextAccountUpdate);

        let outcome = engine.mark(&[("MNQ".into(), Decimal::from(20_000))], 2);
        assert_eq!(outcome.originated_orders, 1);
        assert_eq!(
            outcome
                .events
                .iter()
                .filter(|event| matches!(event, VenueMessage::OrderFilled(_)))
                .count(),
            1,
            "the venue fill is neither rejected nor duplicated"
        );
        assert!(matches!(
            outcome.events.last(),
            Some(VenueMessage::AccountState(_))
        ));
        engine.mark(&[("MNQ".into(), Decimal::from(20_000))], 3);

        let mut client_order = mnq_order("CLIENT-1", Side::Buy, 1, 20_000);
        client_order.reduce_only = true;
        let rejected = engine.process(Command::SubmitOrder(client_order), 4);
        assert_eq!(reject_reason(&rejected), "consumer scenario");

        engine
            .account
            .balances
            .insert("USD".into(), Decimal::from(10_000));
        let filled = engine.process_with_market(
            Command::SubmitOrder(mnq_order("CLIENT-2", Side::Buy, 1, 20_000)),
            5,
            Some(MarketReading {
                last_px: Decimal::from(20_000),
                ts_ns: 5,
                band_ticks: 0,
            }),
        );
        assert_eq!(
            filled
                .iter()
                .filter(|event| matches!(event, VenueMessage::OrderFilled(_)))
                .count(),
            2
        );
        assert!(
            !filled
                .iter()
                .any(|event| matches!(event, VenueMessage::AccountState(_)))
        );
    }

    #[test]
    fn a_liquidation_neither_pays_nor_spends_an_armed_fee_surcharge() {
        // `FeeSurcharge` is consumer-armed havoc. A venue-originated liquidation
        // must not be charged it (a large enough multiplier would fail the
        // liquidation's own funds check and leave the breached position open),
        // and must not expire its window either - the arm belongs to the next
        // consumer fill.
        let mut engine = futures_engine(3_000, MarginBreachAction::Liquidate);
        engine.set_fee_schedule(
            "MNQ".into(),
            FeeSchedule {
                maker: FeeRate::PerContract {
                    amount: Decimal::ONE,
                },
                taker: FeeRate::PerContract {
                    amount: Decimal::ONE,
                },
            },
        );
        fill_future(&mut engine, "F-1", Side::Buy, 1, 21_000);
        engine.arm_fee_surcharge(Decimal::from(5_000), 0, 1_000_000_000);

        let outcome = engine.mark(&[("MNQ".into(), Decimal::from(20_000))], 2);
        assert_eq!(outcome.originated_orders, 1);
        let liquidation = outcome
            .events
            .iter()
            .find_map(|event| match event {
                VenueMessage::OrderFilled(fill) => Some(fill),
                _ => None,
            })
            .expect("the liquidation fills rather than failing its funds check");
        assert_eq!(liquidation.commission, Decimal::ONE);

        // The window is still armed and still un-expired: the next consumer fill
        // pays the surcharge the liquidation walked past.
        engine
            .account
            .balances
            .insert("USD".into(), Decimal::from(100_000));
        engine.mark(&[("MNQ".into(), Decimal::from(20_000))], 3);
        let fill = future_fill(&mut engine, "CLIENT-1", 1, 21_000, 4);
        assert_eq!(fill.commission, Decimal::from(5_000));
    }

    #[test]
    fn a_spot_symbol_carrying_a_margin_policy_still_holds_notional() {
        // `hold_for` and `held_balances` derive from one `order_hold`,
        // so a margin policy attached to a spot symbol - which venue config
        // refuses at boot, but the public `set_margin_policy` cannot - changes
        // neither the account's hold nor the add-back the funds check makes
        // against it. Reading the margin map first would hand the fill check a
        // 1-unit add-back against a 50-unit hold and cancel a fully funded
        // order.
        let mut engine = funded(50);
        engine.set_margin_policy(
            "BTCUSDT".into(),
            MarginPolicy {
                initial_per_contract: Decimal::ONE,
                maintenance_per_contract: Decimal::ONE,
                breach_action: MarginBreachAction::Refuse,
                basis: Default::default(),
            },
        );
        let mut resting = order_with("HOLD", Side::Buy, "BTCUSDT", 1, Some(Decimal::from(50)));
        resting.order_type = OrderType::Limit;
        let out = engine.process_with_market(Command::SubmitOrder(resting), 1, Some(reading(0)));
        assert!(matches!(out[0], VenueMessage::OrderAccepted { .. }));
        assert_eq!(
            balance(&engine.account_snapshot(2), "USDT").locked,
            Decimal::from(50)
        );

        let scan = engine.pending_scans().remove(0);
        let (events, emitted) = engine.apply_scans(&[result(&scan, true, 20)], 20);
        assert_eq!(emitted, 1, "the funded order fills rather than canceling");
        assert!(
            events
                .iter()
                .any(|event| matches!(event, VenueMessage::OrderFilled(_)))
        );
    }

    #[test]
    fn an_amend_must_fund_the_commission_its_fill_will_be_charged() {
        // The amended requirement carries commission exactly as the submit
        // requirement does. Without it the amend is admitted and the fill-time
        // check then cancels the order.
        let mut engine = funded(101);
        engine.set_fee_schedule(
            "BTCUSDT".into(),
            FeeSchedule {
                maker: FeeRate::PerContract {
                    amount: Decimal::ONE,
                },
                taker: FeeRate::PerContract {
                    amount: Decimal::ONE,
                },
            },
        );
        let mut resting = order_with("R1", Side::Buy, "BTCUSDT", 1, Some(Decimal::from(50)));
        resting.order_type = OrderType::Limit;
        let out = engine.process_with_market(Command::SubmitOrder(resting), 1, Some(reading(0)));
        assert!(matches!(out[0], VenueMessage::OrderAccepted { .. }));

        // 2 @ 50 is 100 of notional plus 2 of commission. Free is 51 and the
        // order's own hold is 50, so 101 covers the notional but not the fee.
        let out = engine.process(
            Command::ModifyOrder {
                client_order_id: "R1".into(),
                price: None,
                quantity: Some(Decimal::from(2)),
                trigger_price: None,
            },
            2,
        );
        let [VenueMessage::OrderModifyRejected { reason, .. }] = &out[..] else {
            panic!("expected one modify reject, got {out:?}")
        };
        assert_eq!(reason, "insufficient USDT balance");
        assert_eq!(engine.open_orders()[0].leaves_qty, Decimal::ONE);
    }

    #[test]
    fn per_contract_fees_ignore_price_and_scale_with_contracts() {
        let mut engine = futures_engine(20_000, MarginBreachAction::Refuse);
        engine.set_fee_schedule(
            "MNQ".into(),
            FeeSchedule {
                maker: FeeRate::PerContract {
                    amount: Decimal::new(25, 2),
                },
                taker: FeeRate::PerContract {
                    amount: Decimal::new(25, 2),
                },
            },
        );
        let fill = future_fill(&mut engine, "FEE-1", 4, 21_000, 1);
        assert_eq!(fill.commission, Decimal::ONE);
    }

    #[test]
    fn a_spot_buy_must_fund_its_commission_as_well_as_its_notional() {
        let mut engine = funded(100);
        engine.set_fee_schedule(
            "BTCUSDT".into(),
            FeeSchedule {
                maker: FeeRate::PerContract {
                    amount: Decimal::ONE,
                },
                taker: FeeRate::PerContract {
                    amount: Decimal::ONE,
                },
            },
        );
        let out = engine.process(Command::SubmitOrder(order("FEE", 1)), 1);
        assert_eq!(reject_reason(&out), "insufficient USDT balance");
        assert_eq!(
            balance(&engine.account_snapshot(2), "USDT").total,
            Decimal::from(100)
        );

        // The door check, not just the fill check: a limit that rests never
        // reaches `validate_fill_funds`, so only `validate_submit` can refuse
        // it. 50 of notional plus 1 of fee against a 50 balance.
        let mut engine = funded(50);
        engine.set_fee_schedule(
            "BTCUSDT".into(),
            FeeSchedule {
                maker: FeeRate::PerContract {
                    amount: Decimal::ONE,
                },
                taker: FeeRate::PerContract {
                    amount: Decimal::ONE,
                },
            },
        );
        let mut resting = order_with("FEE-REST", Side::Buy, "BTCUSDT", 1, Some(Decimal::from(50)));
        resting.order_type = OrderType::Limit;
        let out = engine.process_with_market(Command::SubmitOrder(resting), 3, Some(reading(0)));
        assert_eq!(reject_reason(&out), "insufficient USDT balance");
        assert!(engine.open_orders().is_empty());
    }

    #[test]
    fn a_spot_sell_cannot_charge_more_commission_than_proceeds_and_cash_cover() {
        let mut engine = Engine::build(EngineConfig {
            account_id: test_account_id(),
            instruments: default_instruments(),
            balances: HashMap::from([
                ("BTC".to_string(), Decimal::ONE),
                ("USDT".to_string(), Decimal::ZERO),
            ]),
            fill_seed: 0,
        });
        engine.set_fee_schedule(
            "BTCUSDT".into(),
            FeeSchedule {
                maker: FeeRate::PerContract {
                    amount: Decimal::from(101),
                },
                taker: FeeRate::PerContract {
                    amount: Decimal::from(101),
                },
            },
        );
        let sell = order_with(
            "FEE-SELL",
            Side::Sell,
            "BTCUSDT",
            1,
            Some(Decimal::from(100)),
        );
        let out = engine.process(Command::SubmitOrder(sell), 1);
        assert_eq!(reject_reason(&out), "insufficient USDT balance");
        assert_eq!(
            balance(&engine.account_snapshot(2), "USDT").total,
            Decimal::ZERO
        );

        // And at the door: a sell limit above the market rests, so the fill
        // check never runs and only `validate_submit` can refuse it.
        let mut resting = order_with(
            "FEE-SELL-REST",
            Side::Sell,
            "BTCUSDT",
            1,
            Some(Decimal::from(100)),
        );
        resting.order_type = OrderType::Limit;
        let out = engine.process_with_market(Command::SubmitOrder(resting), 3, Some(reading(0)));
        assert_eq!(reject_reason(&out), "insufficient USDT balance");
        assert!(engine.open_orders().is_empty());
    }

    #[test]
    fn basis_point_fees_on_a_future_charge_multiplier_aware_notional() {
        let mut engine = futures_engine(20_000, MarginBreachAction::Refuse);
        engine.set_fee_schedule(
            "MNQ".into(),
            FeeSchedule {
                maker: FeeRate::BasisPoints { rate: Decimal::ONE },
                taker: FeeRate::BasisPoints { rate: Decimal::ONE },
            },
        );
        let fill = future_fill(&mut engine, "FEE-1", 1, 21_000, 1);
        assert_eq!(fill.commission, Decimal::new(420, 2));
    }

    #[test]
    fn a_fee_surcharge_bills_above_the_advertised_schedule_and_expires_on_sim_time() {
        let mut engine = futures_engine(20_000, MarginBreachAction::Refuse);
        engine.set_fee_schedule(
            "MNQ".into(),
            FeeSchedule {
                maker: FeeRate::PerContract {
                    amount: Decimal::ONE,
                },
                taker: FeeRate::PerContract {
                    amount: Decimal::ONE,
                },
            },
        );
        engine.arm_fee_surcharge(Decimal::from(3), 1, 10_000_000);
        assert_eq!(
            future_fill(&mut engine, "FEE-1", 1, 21_000, 2).commission,
            Decimal::from(3)
        );
        assert_eq!(
            future_fill(&mut engine, "FEE-2", 1, 21_000, 10_000_001).commission,
            Decimal::ONE
        );
    }

    #[test]
    fn a_re_armed_fee_surcharge_replaces_the_earlier_window() {
        let mut engine = futures_engine(20_000, MarginBreachAction::Refuse);
        engine.set_fee_schedule(
            "MNQ".into(),
            FeeSchedule {
                maker: FeeRate::PerContract {
                    amount: Decimal::ONE,
                },
                taker: FeeRate::PerContract {
                    amount: Decimal::ONE,
                },
            },
        );
        engine.arm_fee_surcharge(Decimal::from(2), 1, 100_000_000);
        engine.arm_fee_surcharge(Decimal::from(4), 2, 100_000_000);
        assert_eq!(
            future_fill(&mut engine, "FEE-1", 1, 21_000, 3).commission,
            Decimal::from(4)
        );
    }

    #[test]
    fn a_surcharge_armed_once_spans_equal_sim_time_for_two_order_clocks() {
        let mut engine = funded(1);
        engine.arm_fee_surcharge(Decimal::from(3), 100, 1_000);
        for sim in [
            SimClock {
                sim_epoch_ns: 10_000,
                wall_anchor_ns: 0,
                speed: 1.0,
            },
            SimClock {
                sim_epoch_ns: 20_000,
                wall_anchor_ns: 50,
                speed: 10.0,
            },
        ] {
            engine.event_sim = sim;
            let opening = sim.sim_ns(100).max(sim.sim_epoch_ns);
            assert_eq!(
                engine.fee_surcharge_multiplier_for(sim, opening + 999),
                Decimal::from(3)
            );
            assert_eq!(
                engine.fee_surcharge_multiplier_for(sim, opening + 1_000),
                Decimal::ONE
            );
        }
    }

    #[test]
    fn netting_collapses_two_opposing_fills_into_one_position() {
        let mut engine = futures_engine(20_000, MarginBreachAction::Refuse);
        let mut buy = mnq_order("NET-1", Side::Buy, 2, 21_000);
        buy.position_id = Some("CLIENT-LONG".into());
        let mut sell = mnq_order("NET-2", Side::Sell, 1, 21_000);
        sell.position_id = Some("CLIENT-SHORT".into());
        for order in [buy, sell] {
            engine.process_with_market(
                Command::SubmitOrder(order),
                1,
                Some(MarketReading {
                    last_px: Decimal::from(21_000),
                    ts_ns: 1,
                    band_ticks: 0,
                }),
            );
        }
        let positions = engine.positions();
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].quantity, Decimal::ONE);
        assert!(positions[0].position_id.is_none());
    }

    #[test]
    fn hedging_keeps_two_opposing_fills_as_two_positions() {
        let mut engine = futures_engine(20_000, MarginBreachAction::Refuse);
        engine.set_oms_type(mogwai_protocol::OmsType::Hedging);
        let mut buy = mnq_order("HEDGE-1", Side::Buy, 1, 21_000);
        buy.position_id = Some("LONG".into());
        let mut sell = mnq_order("HEDGE-2", Side::Sell, 1, 21_000);
        sell.position_id = Some("SHORT".into());
        for order in [buy, sell] {
            engine.process_with_market(
                Command::SubmitOrder(order),
                1,
                Some(MarketReading {
                    last_px: Decimal::from(21_000),
                    ts_ns: 1,
                    band_ticks: 0,
                }),
            );
        }
        assert_eq!(engine.positions().len(), 2);
    }

    #[test]
    fn a_hedging_order_without_a_position_id_opens_a_venue_assigned_one() {
        let mut engine = futures_engine(20_000, MarginBreachAction::Refuse);
        engine.set_oms_type(mogwai_protocol::OmsType::Hedging);
        let fill = future_fill(&mut engine, "HEDGE-1", 1, 21_000, 1);
        assert!(
            fill.position_id
                .as_deref()
                .is_some_and(|id| id.starts_with("MNQ-"))
        );
        assert_eq!(engine.positions()[0].position_id, fill.position_id);
    }

    #[test]
    fn a_hedging_fill_reports_the_position_id_the_venue_booked_it_against() {
        let mut engine = futures_engine(20_000, MarginBreachAction::Refuse);
        engine.set_oms_type(mogwai_protocol::OmsType::Hedging);
        let mut order = mnq_order("HEDGE-1", Side::Buy, 1, 21_000);
        order.position_id = Some("BOOK-7".into());
        let events = engine.process_with_market(
            Command::SubmitOrder(order),
            1,
            Some(MarketReading {
                last_px: Decimal::from(21_000),
                ts_ns: 1,
                band_ticks: 0,
            }),
        );
        assert!(events.iter().any(|event| matches!(event, VenueMessage::OrderFilled(fill) if fill.position_id.as_deref() == Some("BOOK-7"))));
    }

    /// An unfunded engine rooted at one fill seed. The seed picks the trigger
    /// stream and nothing else; a test that wants a specific band supplies it
    /// through the `MarketReading`.
    fn banded(fill_seed: u64) -> Engine {
        Engine::build(EngineConfig {
            account_id: test_account_id(),
            instruments: default_instruments(),
            balances: HashMap::new(),
            fill_seed,
        })
    }

    /// `order` is a market order; a resting limit needs the type set.
    fn limit_order(id: &str, qty: i64) -> SubmitOrder {
        let mut order = order(id, qty);
        order.order_type = OrderType::Limit;
        order
    }

    /// A reading whose band is `band_ticks` wide around a market at 99, which is
    /// strictly through the test orders' buy limit at 100.
    fn reading(band_ticks: u32) -> MarketReading {
        MarketReading {
            last_px: Decimal::from(99),
            ts_ns: 0,
            band_ticks,
        }
    }

    fn result(scan: &PendingScan, triggered: bool, scanned_to_ns: u64) -> ScanResult {
        ScanResult {
            client_order_id: scan.client_order_id.clone(),
            from_ns: scan.from_ns,
            revision: scan.revision,
            hit: triggered.then_some(Hit {
                ts_ns: scanned_to_ns,
                px: scan.px,
            }),
            scanned_to_ns,
        }
    }

    #[test]
    fn a_market_submit_without_a_reading_uses_its_stated_price() {
        let mut e = Engine::new();
        let out = e.process(Command::SubmitOrder(order("legacy", 1)), 7);
        assert!(
            matches!(out.as_slice(), [VenueMessage::OrderAccepted { .. }, VenueMessage::OrderFilled(fill), VenueMessage::AccountState(_)] if fill.last_px == Decimal::from(100))
        );
        assert!(e.pending_scans().is_empty());
    }

    #[test]
    fn a_submit_with_no_reading_rests_rather_than_filling() {
        let mut e = banded(1);
        let out = e.process(Command::SubmitOrder(limit_order("rest", 2)), 7);
        assert!(matches!(
            out.as_slice(),
            [
                VenueMessage::OrderAccepted { .. },
                VenueMessage::AccountState(_)
            ]
        ));
        assert_eq!(e.open.len(), 1);
        assert_eq!(e.open[0].leaves_qty, Decimal::from(2));
    }

    #[test]
    fn a_marketable_on_arrival_limit_fills_only_when_the_reading_is_through_its_trigger() {
        // The buy limit is at 100 and the reading is at 99, so a zero band
        // fills on arrival: the market is already strictly through the stated
        // price. A band wide enough to put the trigger below 99 does not.
        let mut e = banded(1);
        let out = e.process_with_market(
            Command::SubmitOrder(limit_order("cross", 1)),
            7,
            Some(reading(0)),
        );
        assert!(matches!(
            out.as_slice(),
            [
                VenueMessage::OrderAccepted { .. },
                VenueMessage::OrderFilled(_),
                VenueMessage::AccountState(_)
            ]
        ));
        assert!(e.pending_scans().is_empty());

        let wide = reading(10_000);
        let out =
            e.process_with_market(Command::SubmitOrder(limit_order("short", 1)), 8, Some(wide));
        assert!(
            !out.iter()
                .any(|event| matches!(event, VenueMessage::OrderFilled(_))),
            "a limit whose trigger the reading has not reached must rest"
        );
        let scan = e.pending_scans().remove(0);
        assert!(scan.px < wide.last_px);
    }

    #[test]
    fn a_zero_band_reduces_to_a_strict_through_trigger_at_the_stated_price() {
        // The degenerate case of the model, which `fill_band_vol_mult = 0.0`
        // configures: the trigger is the stated price, so a print at it is the
        // market touching rather than trading through and does not fill.
        let mut e = banded(9);
        e.process(Command::SubmitOrder(limit_order("degenerate", 1)), 10);
        assert!(
            matches!(e.open[0].resting, Resting::Limit { fill_trigger_px } if fill_trigger_px == Decimal::from(100))
        );
        let at_touch = e.process_with_market(
            Command::SubmitOrder(limit_order("touch", 1)),
            11,
            Some(MarketReading {
                last_px: Decimal::from(100),
                ts_ns: 0,
                band_ticks: 0,
            }),
        );
        assert!(
            !at_touch
                .iter()
                .any(|event| matches!(event, VenueMessage::OrderFilled(_)))
        );
    }

    #[test]
    fn a_truncated_scan_advances_only_what_it_covered() {
        let mut e = banded(3);
        e.process(Command::SubmitOrder(limit_order("short", 1)), 10);
        let scan = e.pending_scans().remove(0);
        e.apply_scans(&[result(&scan, false, 12)], 99);
        assert_eq!(e.open[0].band_draw, 0);
        assert_eq!(e.open[0].scanned_ns, 12);
    }

    #[test]
    fn a_scan_against_a_stale_revision_is_dropped() {
        let mut e = banded(2);
        e.process(Command::SubmitOrder(limit_order("stale", 1)), 10);
        let scan = e.pending_scans().remove(0);
        e.apply_scans(&[result(&scan, false, 20)], 20);
        let (_, emitted) = e.apply_scans(&[result(&scan, true, 20)], 20);
        assert_eq!(emitted, 0);
        assert_eq!(e.open[0].band_draw, 0);
    }

    #[test]
    fn a_price_amend_redraws_the_trigger_and_a_quantity_amend_does_not() {
        // Asserted on `band_draw` and `scanned_ns`, never on the trigger price
        // being unequal: a redraw may legitimately land on the same offset, so
        // a test asserting the price moved would be flaky by construction.
        let mut e = banded(3);
        e.process(Command::SubmitOrder(limit_order("amend", 2)), 10);
        let scan = e.pending_scans().remove(0);
        e.apply_scans(&[result(&scan, false, 20)], 20);
        e.process(
            Command::ModifyOrder {
                client_order_id: "amend".into(),
                price: None,
                quantity: Some(Decimal::from(3)),
                trigger_price: None,
            },
            30,
        );
        assert_eq!(e.open[0].band_draw, 0);
        e.process(
            Command::ModifyOrder {
                client_order_id: "amend".into(),
                price: Some(Decimal::from(101)),
                quantity: None,
                trigger_price: None,
            },
            40,
        );
        assert_eq!(e.open[0].band_draw, 1);
        assert_eq!(e.open[0].scanned_ns, 40);
    }

    #[test]
    fn a_price_amend_adopts_a_fresh_band_when_the_venue_supplies_one() {
        let mut e = banded(3);
        e.process_with_market(
            Command::SubmitOrder(limit_order("regime", 2)),
            10,
            Some(MarketReading {
                last_px: Decimal::from(101),
                ts_ns: 0,
                band_ticks: 4,
            }),
        );
        assert_eq!(e.open[0].band_ticks, 4);
        let amend = |price: i64| Command::ModifyOrder {
            client_order_id: "regime".into(),
            price: Some(Decimal::from(price)),
            quantity: None,
            trigger_price: None,
        };
        // A reading supplied with the amend replaces the acceptance band, so an
        // order repriced into a new regime is not judged under the old one.
        e.process_with_market(
            amend(102),
            20,
            Some(MarketReading {
                last_px: Decimal::from(101),
                ts_ns: 0,
                band_ticks: 9,
            }),
        );
        assert_eq!(e.open[0].band_ticks, 9);
        // Without one it keeps what it had rather than collapsing to zero.
        e.process(amend(103), 30);
        assert_eq!(e.open[0].band_ticks, 9);
    }

    #[test]
    fn apply_scans_fills_an_order_the_tape_traded_through() {
        let mut e = banded(1);
        e.process(Command::SubmitOrder(limit_order("swept", 1)), 10);
        let scan = e.pending_scans().remove(0);
        let (out, emitted) = e.apply_scans(&[result(&scan, true, 20)], 20);
        assert_eq!(emitted, 1);
        // Accept-free: the fill is unsolicited, and it prints at the order's
        // price, never the triggering trade's - the trigger decides when.
        assert!(matches!(
            out.as_slice(),
            [VenueMessage::OrderFilled(fill), VenueMessage::AccountState(_)]
                if fill.last_px == Decimal::from(100) && fill.leaves_qty == Decimal::ZERO
        ));
        assert!(e.open.is_empty());
        let snapshot = e.order_status_snapshot("r".into(), Some("swept"), false, 21);
        assert_eq!(snapshot.orders[0].status, WireOrderStatus::Filled);
    }

    #[test]
    fn a_partial_fill_remainder_draws_a_fresh_trigger() {
        // A remainder resting on the trigger the tape just went through would
        // be filled for free by the next pass - the band leaking open on
        // exactly the orders it is most meant to hold. Asserted on `band_draw`
        // and `scanned_ns`, not on trigger inequality: a fresh draw may land on
        // the same offset.
        let mut e = banded(1);
        e.arm(Divergence::PartialFillNext {
            client_order_id: "part".into(),
            fraction: Decimal::new(5, 1),
        });
        e.process(Command::SubmitOrder(limit_order("part", 2)), 10);
        let scan = e.pending_scans().remove(0);
        let (out, emitted) = e.apply_scans(&[result(&scan, true, 20)], 20);
        assert_eq!(emitted, 1);
        assert!(matches!(
            out.first(),
            Some(VenueMessage::OrderFilled(fill))
                if fill.last_qty == Decimal::ONE && fill.leaves_qty == Decimal::ONE
        ));
        assert_eq!(e.open[0].band_draw, 1);
        assert_eq!(e.open[0].scanned_ns, 20);
        assert_eq!(e.open[0].leaves_qty, Decimal::ONE);
    }

    #[test]
    fn a_swept_fill_sizes_off_the_remaining_quantity() {
        // The second sweep multiplies its fraction by the leaves, not by the
        // original quantity, so it cannot over-fill a partly filled order.
        let mut e = banded(1);
        e.arm(Divergence::PartialFillNext {
            client_order_id: "leaves".into(),
            fraction: Decimal::new(5, 1),
        });
        e.process(Command::SubmitOrder(limit_order("leaves", 4)), 10);
        let scan = e.pending_scans().remove(0);
        e.apply_scans(&[result(&scan, true, 20)], 20);
        assert_eq!(e.open[0].leaves_qty, Decimal::from(2));
        let scan = e.pending_scans().remove(0);
        let (out, _) = e.apply_scans(&[result(&scan, true, 30)], 30);
        assert!(matches!(
            out.first(),
            Some(VenueMessage::OrderFilled(fill))
                if fill.last_qty == Decimal::from(2) && fill.leaves_qty == Decimal::ZERO
        ));
    }

    #[test]
    fn a_partial_fill_divergence_survives_until_the_resting_order_executes() {
        // Resting calls no `plan_fill`, so the targeted divergence is
        // still armed for the execution it names.
        let mut e = banded(1);
        e.arm(Divergence::PartialFillNext {
            client_order_id: "armed".into(),
            fraction: Decimal::new(5, 1),
        });
        e.process(Command::SubmitOrder(limit_order("armed", 2)), 10);
        assert_eq!(e.armed.len(), 1);
        let scan = e.pending_scans().remove(0);
        let (out, _) = e.apply_scans(&[result(&scan, true, 20)], 20);
        assert!(matches!(
            out.first(),
            Some(VenueMessage::OrderFilled(fill)) if fill.last_qty == Decimal::ONE
        ));
        assert!(e.armed.is_empty());
    }

    #[test]
    fn a_duplicate_fill_divergence_applies_to_a_swept_fill() {
        let mut e = banded(1);
        e.process(Command::SubmitOrder(limit_order("dup", 1)), 10);
        e.arm(Divergence::DuplicateNextFill);
        let scan = e.pending_scans().remove(0);
        let (out, emitted) = e.apply_scans(&[result(&scan, true, 20)], 20);
        // Two wire fills, one booked into the truth store, one account state.
        assert_eq!(emitted, 1);
        assert_eq!(
            out.iter()
                .filter(|event| matches!(event, VenueMessage::OrderFilled(_)))
                .count(),
            2
        );
        assert_eq!(e.fill_snapshot("r".into(), Some("dup"), 21).fills.len(), 1);
    }

    #[test]
    fn a_fok_through_its_trigger_fills_and_a_market_order_never_rests() {
        let mut e = banded(5);
        let mut fok = limit_order("fok-through", 1);
        fok.time_in_force = TimeInForce::Fok;
        let out = e.process_with_market(Command::SubmitOrder(fok), 1, Some(reading(0)));
        assert!(
            out.iter()
                .any(|event| matches!(event, VenueMessage::OrderFilled(_)))
        );

        // A market order arrives price-stamped by the venue; it is marketable
        // by definition and never rests on the honest path.
        let market = order_with("mkt", Side::Buy, "BTCUSDT", 1, None);
        let market = SubmitOrder {
            price: Some(Decimal::from(100)),
            ..market
        };
        let out = e.process(Command::SubmitOrder(market), 2);
        assert!(
            out.iter()
                .any(|event| matches!(event, VenueMessage::OrderFilled(_)))
        );
        assert!(e.pending_scans().is_empty());
    }

    #[test]
    fn a_market_remainder_left_resting_by_havoc_is_never_scanned() {
        // A market order never draws a trigger, but an armed partial can leave one
        // resting with a venue-stamped price. Handing that remainder to the
        // tape walk would hold it until the market traded through a price the
        // venue itself synthesized.
        let mut e = banded(1);
        e.arm(Divergence::PartialFillNext {
            client_order_id: "mkt-part".into(),
            fraction: Decimal::new(5, 1),
        });
        let market = SubmitOrder {
            order_type: OrderType::Market,
            ..order("mkt-part", 2)
        };
        e.process(Command::SubmitOrder(market), 1);
        assert_eq!(e.open[0].leaves_qty, Decimal::ONE);
        assert!(e.pending_scans().is_empty());
    }

    #[test]
    fn a_dropped_account_update_survives_a_resting_accept_and_applies_to_the_swept_fill() {
        let mut e = banded(1);
        e.arm(Divergence::DropNextAccountUpdate);
        let accepted = e.process(Command::SubmitOrder(limit_order("drop", 1)), 10);
        assert!(matches!(
            accepted.last(),
            Some(VenueMessage::AccountState(_))
        ));
        let scan = e.pending_scans().remove(0);
        let (out, _) = e.apply_scans(&[result(&scan, true, 20)], 20);
        assert!(matches!(out.as_slice(), [VenueMessage::OrderFilled(_)]));
    }

    #[test]
    fn a_rejected_fok_still_does_not_reserve_its_client_order_id() {
        let mut e = banded(1);
        let mut fok = limit_order("fok-reuse", 2);
        fok.time_in_force = TimeInForce::Fok;
        e.arm(Divergence::PartialFillNext {
            client_order_id: "fok-reuse".into(),
            fraction: Decimal::new(5, 1),
        });
        // Through its trigger, so the rejection is the all-or-nothing one the
        // partial forces rather than the short-of-trigger one. The two are
        // pinned by their exact text: `validate_submit`'s two FOK refusals are
        // one word apart, and a `contains("trigger")` test admits either the
        // wrong FOK branch or any future refusal that happens to mention a
        // trigger price.
        let rejected =
            e.process_with_market(Command::SubmitOrder(fok.clone()), 1, Some(reading(0)));
        assert_eq!(
            reject_reason(&rejected),
            "fill-or-kill could not fully fill"
        );
        let accepted = e.process_with_market(Command::SubmitOrder(fok), 2, Some(reading(0)));
        assert!(matches!(
            accepted.first(),
            Some(VenueMessage::OrderAccepted { .. })
        ));
    }

    #[test]
    fn an_ioc_short_of_its_trigger_cancels_and_a_fok_short_of_its_trigger_is_rejected() {
        let mut e = banded(1);
        let mut miss = limit_order("ioc-miss", 1);
        miss.time_in_force = TimeInForce::Ioc;
        let out = e.process(Command::SubmitOrder(miss), 1);
        assert!(matches!(
            out.as_slice(),
            [
                VenueMessage::OrderAccepted { .. },
                VenueMessage::OrderCanceled { .. },
                VenueMessage::AccountState(_)
            ]
        ));
        let mut hit = limit_order("ioc-hit", 1);
        hit.time_in_force = TimeInForce::Ioc;
        let out = e.process_with_market(Command::SubmitOrder(hit), 2, Some(reading(0)));
        assert!(
            out.iter()
                .any(|event| matches!(event, VenueMessage::OrderFilled(_)))
        );
        assert!(e.pending_scans().is_empty());

        // A FOK short of its trigger is rejected rather than cancelled, and
        // stops being the free fill it was: it is decided now or never, and now
        // means against the trigger like everything else.
        let mut fok = limit_order("fok-short", 1);
        fok.time_in_force = TimeInForce::Fok;
        let out = e.process(Command::SubmitOrder(fok), 3);
        assert!(matches!(
            out.as_slice(),
            [VenueMessage::OrderRejected { reason, .. }]
                if reason == "fill-or-kill could not fill at its trigger"
        ));
        assert!(e.pending_scans().is_empty());
    }

    #[test]
    fn funded_account_rejects_orders_it_cannot_cover() {
        // A funded account is an honest cash venue: submits the free balance
        // cannot cover are rejected at the door instead of booking a negative
        // leg (which a nautilus cash consumer would refuse to apply).
        let mut e = funded(1_000);

        // Buy past the quote balance: 11 * 100 > 1000.
        let out = e.process(Command::SubmitOrder(order("B1", 11)), 1);
        assert_eq!(reject_reason(&out), "insufficient USDT balance");

        // Sell with no base at all.
        let out = e.process(
            Command::SubmitOrder(order_with(
                "S1",
                Side::Sell,
                "BTCUSDT",
                1,
                Some(Decimal::from(100)),
            )),
            2,
        );
        assert_eq!(reject_reason(&out), "insufficient BTC balance");

        // Spend-then-overspend: a 5 @ 100 buy leaves 500 free, so a second
        // 6 @ 100 buy is refused while a 5 @ 100 one still clears.
        let out = e.process(Command::SubmitOrder(order("B2", 5)), 3);
        assert!(matches!(out[0], VenueMessage::OrderAccepted { .. }));
        let out = e.process(Command::SubmitOrder(order("B3", 6)), 4);
        assert_eq!(reject_reason(&out), "insufficient USDT balance");
        let out = e.process(Command::SubmitOrder(order("B4", 5)), 5);
        assert!(matches!(out[0], VenueMessage::OrderAccepted { .. }));

        // The acquired base is spendable: selling it back clears.
        let out = e.process(
            Command::SubmitOrder(order_with(
                "S2",
                Side::Sell,
                "BTCUSDT",
                10,
                Some(Decimal::from(100)),
            )),
            6,
        );
        assert!(matches!(out[0], VenueMessage::OrderAccepted { .. }));
    }

    #[test]
    fn funded_account_counts_holds_and_gates_amends() {
        // A resting buy's hold reduces free balance for later submits,
        // and an amend that grows the hold past free-plus-own-hold is
        // refused - the venue must never advertise free < 0 in its own
        // snapshot.
        let mut e = funded(1_000);

        // Rest half of a 4 @ 100 buy: 200 spent on the fill, 200 locked for
        // the remainder, so free is 600.
        e.arm(Divergence::PartialFillNext {
            client_order_id: "R1".into(),
            fraction: Decimal::new(5, 1),
        });
        let out = e.process(Command::SubmitOrder(order("R1", 4)), 1);
        let state = account(&out, out.len() - 1);
        assert_eq!(balance(state, "USDT").free, Decimal::from(600));
        assert_eq!(balance(state, "USDT").locked, Decimal::from(200));

        // 7 @ 100 exceeds the 600 free even though the total is 800.
        let out = e.process(Command::SubmitOrder(order("B1", 7)), 2);
        assert_eq!(reject_reason(&out), "insufficient USDT balance");

        // Amending the resting order up to 8 total (6 leaves = 600 hold) fits:
        // 600 free plus its own 200 hold covers it. Afterwards the whole 800
        // of unspent quote backs this one order (free 200, hold 600).
        let out = e.process(
            Command::ModifyOrder {
                client_order_id: "R1".into(),
                price: None,
                quantity: Some(Decimal::from(8)),
                trigger_price: None,
            },
            3,
        );
        updated(&out, 0);

        // 11 total (9 leaves = 900 hold) exceeds the 800 the account has left.
        let out = e.process(
            Command::ModifyOrder {
                client_order_id: "R1".into(),
                price: None,
                quantity: Some(Decimal::from(11)),
                trigger_price: None,
            },
            4,
        );
        let [VenueMessage::OrderModifyRejected { reason, .. }] = &out[..] else {
            panic!("expected one modify reject, got {out:?}")
        };
        assert_eq!(reason, "insufficient USDT balance");

        // Canceling the resting order frees its hold; the refused submit now
        // clears (a rejected id is free to reuse).
        let out = e.process(
            Command::CancelOrder {
                client_order_id: "R1".into(),
            },
            5,
        );
        assert!(matches!(out[0], VenueMessage::OrderCanceled { .. }));
        let out = e.process(Command::SubmitOrder(order("B1", 7)), 6);
        assert!(matches!(out[0], VenueMessage::OrderAccepted { .. }));
    }

    // The reconciliation is a `cfg!(debug_assertions)` check, so this pins it
    // in the profile that runs it. Without the gate the test fails in a
    // release test sweep, where nothing panics by design.
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "resting-order hold cache drifted from the book")]
    fn hold_cache_reconciliation_catches_drift_before_a_funded_command() {
        let mut e = funded(1_000);
        e.arm(Divergence::PartialFillNext {
            client_order_id: "DRIFT".into(),
            fraction: Decimal::new(5, 1),
        });
        e.process(Command::SubmitOrder(order("DRIFT", 4)), 1);
        e.corrupt_order_holds_for_test("USDT", Decimal::ZERO);

        e.process(
            Command::QueryFills {
                request_id: "Q".into(),
                client_order_id: None,
            },
            2,
        );
    }

    #[test]
    fn a_zero_initial_margin_policy_cannot_drift_the_hold_cache() {
        // A zero hold must be no cache entry, not an entry whose amount is
        // zero: the reconciliation fold and the incremental remove would
        // otherwise disagree about whether the currency key exists while
        // agreeing on every amount, and the debug reconciliation would panic
        // on states that are economically identical.
        let mut engine = futures_engine(10_000, MarginBreachAction::Refuse);
        engine.set_margin_policy(
            "MNQ".into(),
            MarginPolicy {
                initial_per_contract: Decimal::ZERO,
                maintenance_per_contract: Decimal::ZERO,
                breach_action: MarginBreachAction::Refuse,
                basis: Default::default(),
            },
        );
        for id in ["Z1", "Z2"] {
            let mut order =
                order_decimal(id, Side::Buy, "MNQ", Decimal::ONE, Some(Decimal::from(100)));
            order.order_type = OrderType::Limit;
            engine.process(Command::SubmitOrder(order), 1);
        }
        // Read the cache while the zero-hold orders are still resting, which is
        // the only instant the defect is visible, and the reason this test does
        // not need a `#[cfg(debug_assertions)]` gate the way its sibling above
        // does. The reconciliation panic is a debug-only witness, so relying on
        // it alone would make this test assert nothing but `open.is_empty()` in
        // a release sweep - and by the time both cancels have run the
        // incremental remove has deleted the key in the broken build too, so
        // the end state cannot tell the two apart either. With the zero-hold
        // filter in `order_hold_entry` removed, each submit inserts a
        // zero-amount entry here and this fires in both profiles.
        assert!(
            engine.order_holds.is_empty(),
            "a zero hold must be NO cache key, not a key holding zero: {:?}",
            engine.order_holds
        );
        engine.process(
            Command::CancelOrder {
                client_order_id: "Z1".into(),
            },
            2,
        );
        // The next command reconciles the aggregate against the fold over the
        // one remaining zero-hold order; both must say "no entry".
        engine.process(
            Command::CancelOrder {
                client_order_id: "Z2".into(),
            },
            3,
        );
        assert!(engine.open.is_empty());
    }

    #[test]
    fn keyed_book_index_tracks_swapped_orders_and_snapshots_stay_deterministic() {
        let mut e = funded(10_000);
        for id in ["I1", "I2", "I3"] {
            e.arm(Divergence::PartialFillNext {
                client_order_id: id.into(),
                fraction: Decimal::new(5, 1),
            });
            e.process(Command::SubmitOrder(order(id, 2)), 1);
        }

        e.process(
            Command::CancelOrder {
                client_order_id: "I1".into(),
            },
            2,
        );
        let out = e.process(
            Command::ModifyOrder {
                client_order_id: "I3".into(),
                price: None,
                quantity: Some(Decimal::from(3)),
                trigger_price: None,
            },
            3,
        );
        updated(&out, 0);
        let i3 = e.open.position("I3").expect("I3 remains indexed");
        assert_eq!(e.open[i3].submit.quantity, Decimal::from(3));
        let snapshot = e.order_status_snapshot("Q".into(), None, true, 4);
        assert_eq!(
            snapshot
                .orders
                .iter()
                .map(|order| order.client_order_id.as_str())
                .collect::<Vec<_>>(),
            ["I2", "I3"]
        );
    }

    #[test]
    fn unfunded_account_stays_permissive() {
        // The empty-seed account keeps the delta-off-zero ledger with no funds
        // checks: its documented purpose is exercising the negative-balance
        // path, which enforcement would make unreachable.
        let mut e = Engine::new();
        let out = e.process(Command::SubmitOrder(order("U1", 5)), 1);
        assert!(matches!(out[0], VenueMessage::OrderAccepted { .. }));
        let state = account(&out, out.len() - 1);
        assert_eq!(balance(state, "USDT").total, Decimal::from(-500));
    }

    fn fill(out: &[VenueMessage], index: usize) -> &OrderFilled {
        let VenueMessage::OrderFilled(fill) = &out[index] else {
            panic!("expected fill")
        };
        fill
    }

    fn updated(out: &[VenueMessage], index: usize) -> &VenueMessage {
        let VenueMessage::OrderUpdated { .. } = &out[index] else {
            panic!("expected order updated")
        };
        &out[index]
    }

    fn reject_reason(out: &[VenueMessage]) -> &str {
        let [VenueMessage::OrderRejected { reason, .. }] = out else {
            panic!("expected one order reject")
        };
        reason
    }

    fn cancel_reject_reason(out: &[VenueMessage]) -> &str {
        let [VenueMessage::OrderCancelRejected { reason, .. }] = out else {
            panic!("expected one order cancel reject")
        };
        reason
    }

    fn accepted_venue_id(out: &[VenueMessage]) -> VenueOrderId {
        let VenueMessage::OrderAccepted { venue_order_id, .. } = &out[0] else {
            panic!("expected accept first")
        };
        venue_order_id.clone()
    }

    fn balance<'a>(state: &'a AccountState, currency: &str) -> &'a Balance {
        state
            .balances
            .iter()
            .find(|balance| balance.currency == currency)
            .unwrap()
    }

    fn position<'a>(state: &'a AccountState, symbol: &str) -> &'a Position {
        state
            .positions
            .iter()
            .find(|position| position.symbol.as_ref() == symbol)
            .unwrap()
    }

    #[test]
    fn submit_fully_fills_by_default() {
        let mut e = Engine::new();
        let out = e.process(Command::SubmitOrder(order("O1", 10)), 1);
        assert_eq!(out.len(), 3);
        assert!(matches!(out[0], VenueMessage::OrderAccepted { .. }));
        let VenueMessage::OrderFilled(f) = &out[1] else {
            panic!("expected fill")
        };
        assert_eq!(f.leaves_qty, Decimal::ZERO);
        assert!(matches!(out[2], VenueMessage::AccountState(_)));
        assert!(e.open_orders().is_empty());
    }

    #[test]
    fn submit_rejects_semantically_invalid_inputs() {
        let cases = [
            (
                order_decimal(
                    "neg_qty",
                    Side::Buy,
                    "BTCUSDT",
                    Decimal::from(-1),
                    Some(100.into()),
                ),
                "submit with non-positive quantity",
            ),
            (
                order_decimal(
                    "zero_qty",
                    Side::Buy,
                    "BTCUSDT",
                    Decimal::ZERO,
                    Some(100.into()),
                ),
                "submit with non-positive quantity",
            ),
            (
                order_decimal("neg_px", Side::Buy, "BTCUSDT", 1.into(), Some((-5).into())),
                "submit with non-positive price",
            ),
            (
                order_decimal(
                    "zero_px",
                    Side::Buy,
                    "BTCUSDT",
                    1.into(),
                    Some(Decimal::ZERO),
                ),
                "submit with non-positive price",
            ),
            (
                order_decimal("market_no_px", Side::Buy, "BTCUSDT", 1.into(), None),
                "submit price required",
            ),
            (
                // A Market order with a non-positive price earns the same
                // generic rejection as any other order type - the validator
                // deliberately has no market-specific price message.
                {
                    let mut market = order_decimal(
                        "market_neg_px",
                        Side::Buy,
                        "BTCUSDT",
                        1.into(),
                        Some((-5).into()),
                    );
                    market.order_type = OrderType::Market;
                    market
                },
                "submit with non-positive price",
            ),
            (
                order_decimal(
                    "bad_qty_grid",
                    Side::Buy,
                    "BTCUSDT",
                    "0.123456789".parse().expect("decimal"),
                    Some(100.into()),
                ),
                "quantity violates size increment",
            ),
            (
                order_decimal(
                    "bad_px_grid",
                    Side::Buy,
                    "BTCUSDT",
                    1.into(),
                    Some("60000.123".parse().expect("decimal")),
                ),
                "price violates price increment",
            ),
            (
                order_decimal("", Side::Buy, "BTCUSDT", 1.into(), Some(100.into())),
                "empty client_order_id",
            ),
            (
                order_decimal("unknown", Side::Buy, "FAKE", 1.into(), Some(100.into())),
                "unknown instrument",
            ),
        ];

        // A funded book, deliberately. The refusals are pinned by exact reason
        // and by the ledger being untouched afterwards - but "untouched" read
        // off `Engine::new()`, which starts with no balances at all, is only
        // the claim "no row was created". A refusal that debited or locked
        // funds it had no right to would have left that reading green, because
        // there was nothing there to debit. With a stated 10,000 USDT the
        // assertion is a real before/after on a non-empty ledger, and `locked`
        // is the field the hold path would move.
        const FUNDS: i64 = 10_000;
        for (order, expected) in cases {
            let mut e = funded(FUNDS);
            let out = e.process(Command::SubmitOrder(order), 1);

            assert_eq!(reject_reason(&out), expected);
            let state = e.account_snapshot(2);
            let usdt = balance(&state, "USDT");
            assert_eq!(
                (usdt.total, usdt.free, usdt.locked),
                (Decimal::from(FUNDS), Decimal::from(FUNDS), Decimal::ZERO),
                "a refusal held or spent funds: {expected}"
            );
            // Both halves of the old claim are kept. `funded` seeds exactly one
            // currency, so reading the USDT row alone would no longer see a
            // refusal that minted a row - and the fill path mutates balances
            // through `entry(..).or_default()`, so a refusal that reached it
            // before refusing would introduce a zero BTC row that the tuple
            // above cannot observe. The row count is the "no row was created"
            // claim the empty fixture used to make for free.
            assert_eq!(
                state.balances.len(),
                1,
                "a refusal created a currency row: {expected}, {:?}",
                state.balances
            );
            assert!(e.positions().is_empty());
            assert!(e.open_orders().is_empty());
        }
    }

    #[test]
    fn duplicate_client_order_id_is_rejected_after_acceptance() {
        let mut e = Engine::new();

        let first = e.process(Command::SubmitOrder(order("DUP", 1)), 1);
        assert!(matches!(first[0], VenueMessage::OrderAccepted { .. }));

        let duplicate = e.process(Command::SubmitOrder(order("DUP", 1)), 2);
        assert_eq!(reject_reason(&duplicate), "duplicate client_order_id");
    }

    #[test]
    fn armed_partial_leaves_remainder_resting() {
        let mut e = Engine::new();
        e.arm(Divergence::PartialFillNext {
            client_order_id: "O1".into(),
            fraction: Decimal::from_f64(0.3).unwrap(),
        });
        let out = e.process(Command::SubmitOrder(order("O1", 10)), 1);
        assert_eq!(out.len(), 3);
        let VenueMessage::OrderFilled(f) = &out[1] else {
            panic!("expected fill")
        };
        assert_eq!(f.last_qty, Decimal::from(3));
        assert_eq!(f.leaves_qty, Decimal::from(7));
        assert!(matches!(out[2], VenueMessage::AccountState(_)));
        assert_eq!(e.open_orders().len(), 1);
    }

    #[test]
    fn ioc_partial_fill_cancels_remainder_without_resting() {
        let mut e = Engine::new();
        e.arm(Divergence::PartialFillNext {
            client_order_id: "O1".into(),
            fraction: Decimal::from_f64(0.4).unwrap(),
        });
        let mut order = order("O1", 10);
        order.time_in_force = TimeInForce::Ioc;

        let out = e.process(Command::SubmitOrder(order), 1);

        assert_eq!(out.len(), 4);
        assert!(matches!(out[0], VenueMessage::OrderAccepted { .. }));
        let f = fill(&out, 1);
        assert_eq!(f.last_qty, Decimal::from(4));
        assert_eq!(f.leaves_qty, Decimal::from(6));
        assert!(matches!(out[2], VenueMessage::OrderCanceled { .. }));
        let state = account(&out, 3);
        assert_eq!(balance(state, "BTC").total, Decimal::from(4));
        assert_eq!(balance(state, "USDT").locked, Decimal::ZERO);
        assert!(e.open_orders().is_empty());
    }

    #[test]
    fn fok_partial_liquidity_rejects_without_fill() {
        let mut e = Engine::new();
        e.arm(Divergence::PartialFillNext {
            client_order_id: "O1".into(),
            fraction: Decimal::from_f64(0.4).unwrap(),
        });
        let mut order = order("O1", 10);
        order.time_in_force = TimeInForce::Fok;

        let out = e.process(Command::SubmitOrder(order), 1);

        assert_eq!(reject_reason(&out), "fill-or-kill could not fully fill");
        assert!(e.open_orders().is_empty());
        assert!(e.account_snapshot(2).balances.is_empty());
        assert!(e.positions().is_empty());
    }

    #[test]
    fn armed_reject_blocks_submit() {
        let mut e = Engine::new();
        e.arm(Divergence::RejectNextSubmit {
            reason: "risk".into(),
        });
        let out = e.process(Command::SubmitOrder(order("O1", 10)), 1);
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0], VenueMessage::OrderRejected { .. }));
    }

    #[test]
    fn duplicate_next_fill_doubles_the_wire_event() {
        let mut e = Engine::new();
        e.arm(Divergence::DuplicateNextFill);

        let out = e.process(Command::SubmitOrder(order("O1", 10)), 1);

        assert_eq!(out.len(), 4);
        assert!(matches!(out[0], VenueMessage::OrderAccepted { .. }));
        let first = fill(&out, 1);
        let second = fill(&out, 2);
        assert_eq!(first.trade_id, second.trade_id);
        assert_eq!(first.last_qty, second.last_qty);
        assert_eq!(first.last_px, second.last_px);
        assert_eq!(first.leaves_qty, second.leaves_qty);
        let state = account(&out, 3);
        assert_eq!(balance(state, "BTC").total, Decimal::from(10));
        assert_eq!(balance(state, "USDT").total, Decimal::from(-1000));
    }

    #[test]
    fn drop_next_account_update_swallows_the_snapshot() {
        let mut e = Engine::new();
        e.arm(Divergence::DropNextAccountUpdate);

        let out = e.process(Command::SubmitOrder(order("O1", 10)), 1);

        assert_eq!(out.len(), 2);
        assert!(matches!(out[0], VenueMessage::OrderAccepted { .. }));
        assert!(matches!(out[1], VenueMessage::OrderFilled(_)));
        let state = e.account_snapshot(2);
        assert_eq!(balance(&state, "BTC").total, Decimal::from(10));
    }

    #[test]
    fn duplicate_and_drop_compose_on_one_submit() {
        let mut e = Engine::new();
        e.arm(Divergence::DuplicateNextFill);
        e.arm(Divergence::DropNextAccountUpdate);

        let out = e.process(Command::SubmitOrder(order("O1", 10)), 1);

        assert_eq!(out.len(), 3);
        assert!(matches!(out[0], VenueMessage::OrderAccepted { .. }));
        assert!(matches!(out[1], VenueMessage::OrderFilled(_)));
        assert!(matches!(out[2], VenueMessage::OrderFilled(_)));
    }

    #[test]
    fn drop_skips_rejected_submit_and_fires_on_next_fill() {
        let mut e = Engine::new();
        e.arm(Divergence::RejectNextSubmit {
            reason: "risk".into(),
        });
        e.arm(Divergence::DropNextAccountUpdate);

        let rejected = e.process(Command::SubmitOrder(order("O1", 10)), 1);
        assert_eq!(rejected.len(), 1);
        assert!(matches!(rejected[0], VenueMessage::OrderRejected { .. }));

        let filled = e.process(Command::SubmitOrder(order("O2", 10)), 2);
        assert_eq!(filled.len(), 2);
        assert!(matches!(filled[0], VenueMessage::OrderAccepted { .. }));
        assert!(matches!(filled[1], VenueMessage::OrderFilled(_)));
    }

    #[test]
    fn a_zero_quantity_partial_leaves_drop_next_account_update_armed() {
        // A wire-valid `PartialFillNext` fraction flooring below one size
        // increment on a minimum-lot order fills nothing, so the order merely
        // comes to rest - exactly the carve-out that must not spend the arm,
        // even though this path runs `on_submit`'s marketable tail rather
        // than the not-marketable resting branch.
        let lot = Decimal::new(1, 8);
        let mut e = Engine::new();
        e.arm(Divergence::PartialFillNext {
            client_order_id: "Z0".into(),
            fraction: Decimal::from_f64(0.3).unwrap(),
        });
        e.arm(Divergence::DropNextAccountUpdate);
        let out = e.process(
            Command::SubmitOrder(order_decimal(
                "Z0",
                Side::Buy,
                "BTCUSDT",
                lot,
                Some(Decimal::from(100)),
            )),
            1,
        );
        assert!(
            out.iter()
                .any(|event| matches!(event, VenueMessage::AccountState(_))),
            "nothing filled, so the resting acceptance still owes its snapshot"
        );
        // Still armed; the cancel that frees the hold is what spends it.
        let canceled = e.process(
            Command::CancelOrder {
                client_order_id: "Z0".into(),
            },
            2,
        );
        assert!(
            !canceled
                .iter()
                .any(|event| matches!(event, VenueMessage::AccountState(_)))
        );
        assert!(e.armed.is_empty());
    }

    /// One well-formed value of every `Divergence` variant.
    ///
    /// Hand-built, and the compiler cannot check that it stays complete - which
    /// is exactly why `Engine::arm` and the expectation below are written as
    /// exhaustive matches rather than leaning on this list. A variant added to
    /// the enum and forgotten here is still classified deliberately on both
    /// sides (neither match compiles until it is); what it loses is only the
    /// end-to-end exercise of that classification.
    fn every_divergence_variant() -> Vec<Divergence> {
        vec![
            Divergence::PartialFillNext {
                client_order_id: "O1".into(),
                fraction: Decimal::from_f64(0.5).unwrap(),
            },
            Divergence::RejectNextSubmit {
                reason: "no".into(),
            },
            Divergence::RejectNextCancel {
                reason: "no".into(),
            },
            Divergence::DuplicateNextFill,
            Divergence::DropNextAccountUpdate,
            Divergence::DelayAcks { ms: 100 },
            Divergence::CommandLatency {
                submit_act_ms: 1,
                modify_act_ms: 1,
                cancel_act_ms: 1,
                submit_ack_ms: 1,
                modify_ack_ms: 1,
                cancel_ack_ms: 1,
            },
            Divergence::GoDark { ms: 100 },
            Divergence::StallData { ms: 100 },
            Divergence::FeeSurcharge {
                mult: Decimal::from(2),
                window_ms: 100,
            },
            Divergence::CancelOpenOrderSilently {
                client_order_id: "O1".into(),
            },
            Divergence::FaultTape,
        ]
    }

    /// The classification `Engine::arm` performs, per variant, read off the
    /// queue rather than inferred from an event count.
    ///
    /// The production comment beside that match claims listing the venue-owned
    /// variants explicitly "stops a future enum variant from falling through
    /// into engine behaviour by accident" - a claim about variants that do not
    /// exist yet, which only the compiler can hold, and it does: both arms of
    /// that match are enumerated, so a new variant breaks the build there and
    /// in the expectation below. What this test adds is the half the compiler
    /// cannot see - that today's seven venue-owned variants really do leave the
    /// queue empty (a dead entry nothing consumes, forever) and today's five
    /// engine-side ones really are stored.
    #[test]
    fn arm_classifies_every_divergence_variant() {
        for divergence in every_divergence_variant() {
            // Exhaustive on purpose. Deliberately not `!is_venue_owned`: a
            // single list would let a new variant be classified once and read
            // twice, which is the accident the production match is guarding.
            let queued = match divergence {
                Divergence::PartialFillNext { .. }
                | Divergence::RejectNextSubmit { .. }
                | Divergence::RejectNextCancel { .. }
                | Divergence::DuplicateNextFill
                | Divergence::DropNextAccountUpdate => true,
                Divergence::DelayAcks { .. }
                | Divergence::CommandLatency { .. }
                | Divergence::GoDark { .. }
                | Divergence::StallData { .. }
                | Divergence::FeeSurcharge { .. }
                | Divergence::CancelOpenOrderSilently { .. }
                | Divergence::FaultTape => false,
            };

            let mut e = Engine::new();
            assert!(
                e.arm(divergence.clone()).is_none(),
                "an arm into an empty queue displaces nothing: {divergence:?}"
            );
            if queued {
                assert_eq!(
                    e.armed.iter().collect::<Vec<_>>(),
                    vec![&divergence],
                    "an engine-side divergence is stored for its trigger: {divergence:?}"
                );
            } else {
                assert!(
                    e.armed.is_empty(),
                    "a venue-owned divergence has no engine trigger, so queueing it \
                     leaves a dead entry `take_armed` can never consume: {divergence:?}"
                );
            }
        }
    }

    #[test]
    fn arm_drops_temporal_variants_without_blocking_engine_divergences() {
        let mut e = Engine::new();
        e.arm(Divergence::DelayAcks { ms: 100 });
        e.arm(Divergence::GoDark { ms: 100 });
        e.arm(Divergence::StallData { ms: 100 });
        e.arm(Divergence::DuplicateNextFill);

        // The queue is the observable, not the event count: four arms, and the
        // one engine-side arm is alone in it and at the front, so the three
        // drops neither queued a dead entry nor sat in front of it.
        assert_eq!(
            e.armed.iter().collect::<Vec<_>>(),
            vec![&Divergence::DuplicateNextFill]
        );

        let out = e.process(Command::SubmitOrder(order("O1", 10)), 1);

        // Length first: a regression emitting fewer events would otherwise
        // panic on an index rather than naming the count it produced.
        assert_eq!(out.len(), 4);
        assert!(matches!(out[1], VenueMessage::OrderFilled(_)));
        assert!(matches!(out[2], VenueMessage::OrderFilled(_)));
    }

    #[test]
    fn mistargeted_partial_fill_does_not_block_later_divergences() {
        // B.1: a `PartialFillNext` aimed at an order that is not the one being
        // processed must stay armed (its target may arrive later) without
        // head-of-line-blocking the divergences queued behind it.
        let mut e = Engine::new();
        e.arm(Divergence::PartialFillNext {
            client_order_id: "O2".into(),
            fraction: Decimal::from_f64(0.3).unwrap(),
        });
        e.arm(Divergence::DuplicateNextFill);

        // O1 is submitted first; the O2-targeted partial must NOT apply to it,
        // but the duplicate behind it must still fire (was silently disarmed).
        let out = e.process(Command::SubmitOrder(order("O1", 10)), 1);
        assert_eq!(
            out.len(),
            4,
            "duplicate fill should still fire behind the parked partial"
        );
        assert!(matches!(out[0], VenueMessage::OrderAccepted { .. }));
        let first = fill(&out, 1);
        let second = fill(&out, 2);
        assert_eq!(
            first.last_qty,
            Decimal::from(10),
            "O1 fills fully, untouched by the O2 partial"
        );
        assert_eq!(first.trade_id, second.trade_id);
        assert!(matches!(out[3], VenueMessage::AccountState(_)));

        // The O2-targeted partial is still armed and now applies to O2.
        let out = e.process(Command::SubmitOrder(order("O2", 10)), 2);
        let f = fill(&out, 1);
        assert_eq!(f.last_qty, Decimal::from(3));
        assert_eq!(f.leaves_qty, Decimal::from(7));
        assert_eq!(e.open_orders().len(), 1);
    }

    #[test]
    fn zero_fraction_partial_fill_falls_back_to_full_fill() {
        // B.2: an unvalidated `fraction == 0` would compute `last_qty == 0` and
        // emit a spurious zero-qty fill. The guard treats it as a normal full
        // fill instead.
        let mut e = Engine::new();
        e.arm(Divergence::PartialFillNext {
            client_order_id: "O1".into(),
            fraction: Decimal::ZERO,
        });
        let out = e.process(Command::SubmitOrder(order("O1", 10)), 1);

        assert_eq!(out.len(), 3);
        let f = fill(&out, 1);
        assert_eq!(
            f.last_qty,
            Decimal::from(10),
            "zero fraction must not emit a zero-qty fill"
        );
        assert_eq!(f.leaves_qty, Decimal::ZERO);
        assert!(e.open_orders().is_empty());
    }

    #[test]
    fn negative_fraction_partial_fill_falls_back_to_full_fill() {
        // B.2: a negative fraction would book a negative position/balance leg.
        let mut e = Engine::new();
        e.arm(Divergence::PartialFillNext {
            client_order_id: "O1".into(),
            fraction: Decimal::from(-1),
        });
        let out = e.process(Command::SubmitOrder(order("O1", 10)), 1);

        let f = fill(&out, 1);
        assert_eq!(f.last_qty, Decimal::from(10));
        assert_eq!(f.leaves_qty, Decimal::ZERO);
        let state = account(&out, 2);
        assert_eq!(balance(state, "BTC").total, Decimal::from(10));
    }

    #[test]
    fn over_one_fraction_partial_fill_clamps_to_full_fill() {
        // B.2: `fraction > 1` would over-fill (`last_qty > quantity`, negative
        // leaves). Clamp to a full fill.
        let mut e = Engine::new();
        e.arm(Divergence::PartialFillNext {
            client_order_id: "O1".into(),
            fraction: Decimal::from(3),
        });
        let out = e.process(Command::SubmitOrder(order("O1", 10)), 1);

        let f = fill(&out, 1);
        assert_eq!(
            f.last_qty,
            Decimal::from(10),
            "over-unit fraction clamps to full quantity"
        );
        assert_eq!(f.leaves_qty, Decimal::ZERO);
        assert!(e.open_orders().is_empty());
    }

    #[test]
    fn extreme_unvalidated_fractions_full_fill_instead_of_panicking() {
        // `Engine::arm` is public and performs no range check of its own
        // (`validate_divergence` is a free function a direct caller may
        // skip), so a fraction far outside the wire-validated (0, 1] can
        // reach `fill_quantity`. The clamp must run before the multiply:
        // `quantity * Decimal::MAX` used to panic inside `fill_quantity`
        // ahead of the `.min(order.quantity)` that was supposed to guard it.
        // `Decimal::MAX` clamps to a plain full fill; `Decimal::MIN` clamps
        // to zero and rides the existing non-positive full-fill fallback.
        for fraction in [Decimal::MAX, Decimal::MIN] {
            let mut e = Engine::new();
            e.arm(Divergence::PartialFillNext {
                client_order_id: "O1".into(),
                fraction,
            });
            let out = e.process(Command::SubmitOrder(order("O1", 10)), 1);

            let f = fill(&out, 1);
            assert_eq!(f.last_qty, Decimal::from(10));
            assert_eq!(f.leaves_qty, Decimal::ZERO);
            assert!(e.open_orders().is_empty());
        }
    }

    #[test]
    fn min_lot_partial_fraction_does_not_invert_into_a_full_fill() {
        // E7: an order of exactly one size increment (1e-8) with a wire-valid
        // armed fraction (0.3). `1e-8 * 0.3 = 3e-9` floors below the grid, so
        // the partial cannot be represented. The old code promoted this to a
        // full fill with a misleading "produced non-positive last_qty" warn -
        // silently inverting the divergence and, for a FOK, letting an order
        // the partial was armed to kill fully fill and pass. The fix fills
        // zero: the FOK gate now rejects on the full leaves, and a GTC rests.
        let lot = Decimal::new(1, 8);
        let px = Decimal::from(100);

        // FOK: nothing fills, leaves stays the whole lot, the all-or-nothing
        // gate rejects rather than sneaking a full fill through.
        let mut e = Engine::new();
        e.arm(Divergence::PartialFillNext {
            client_order_id: "FOK".into(),
            fraction: Decimal::from_f64(0.3).unwrap(),
        });
        let mut fok = order_decimal("FOK", Side::Buy, "BTCUSDT", lot, Some(px));
        fok.time_in_force = TimeInForce::Fok;
        let out = e.process(Command::SubmitOrder(fok), 1);
        assert_eq!(reject_reason(&out), "fill-or-kill could not fully fill");
        assert!(e.open_orders().is_empty());
        assert!(e.account_snapshot(2).balances.is_empty());
        assert!(e.positions().is_empty());

        // GTC: accepted, no fill event emitted, and the order rests fully open
        // with the whole lot as leaves. The snapshot shows only the locked
        // quote hold - nothing filled, so no base/position leg.
        let mut e = Engine::new();
        e.arm(Divergence::PartialFillNext {
            client_order_id: "GTC".into(),
            fraction: Decimal::from_f64(0.3).unwrap(),
        });
        let gtc = order_decimal("GTC", Side::Buy, "BTCUSDT", lot, Some(px));
        let out = e.process(Command::SubmitOrder(gtc), 1);
        assert_eq!(out.len(), 2, "accept + account state only, no fill event");
        assert!(matches!(out[0], VenueMessage::OrderAccepted { .. }));
        assert!(matches!(out[1], VenueMessage::AccountState(_)));
        assert_eq!(e.open_orders().len(), 1);
        assert_eq!(e.open_orders()[0].leaves_qty, lot);
        let state = account(&out, 1);
        // 1e-8 lot * 100 price = 1e-6 quote locked; free is the negation.
        let usdt = balance(state, "USDT");
        assert_eq!(usdt.locked, Decimal::new(1, 6));
        assert_eq!(usdt.total, Decimal::ZERO);
        assert!(state.positions.is_empty());
    }

    #[test]
    fn clear_armed_flushes_stale_targeted_partials() {
        // E5: a targeted `PartialFillNext` whose order never arrives has no
        // trigger to self-disarm and would sit armed forever, ready to ambush a
        // later reuse of the id. `clear_armed` is the explicit flush.
        let mut e = Engine::new();
        e.arm(Divergence::PartialFillNext {
            client_order_id: "NEVER".into(),
            fraction: Decimal::from_f64(0.3).unwrap(),
        });
        assert_eq!(e.armed.len(), 1);

        e.clear_armed();
        assert!(e.armed.is_empty());

        // With the stale partial flushed, a later order reusing the id fills
        // fully instead of being ambushed into a partial.
        let out = e.process(Command::SubmitOrder(order("NEVER", 10)), 1);
        let f = fill(&out, 1);
        assert_eq!(f.last_qty, Decimal::from(10));
        assert_eq!(f.leaves_qty, Decimal::ZERO);
    }

    #[test]
    fn armed_divergence_queue_is_bounded() {
        // E5: arming well past the cap with targeted partials whose orders
        // never arrive (no trigger to self-disarm) must not grow the queue
        // without bound - it saturates at `MAX_ARMED_DIVERGENCES`, always shedding the
        // oldest stale entry per arm past the cap.
        let mut e = Engine::new();
        for i in 0..(MAX_ARMED_DIVERGENCES + 50) {
            e.arm(Divergence::PartialFillNext {
                client_order_id: format!("O-{i}"),
                fraction: Decimal::ONE,
            });
        }
        assert_eq!(e.armed.len(), MAX_ARMED_DIVERGENCES);
    }

    #[test]
    fn arm_reports_the_entry_it_shed_at_the_cap() {
        // The eviction must be visible, not just logged: the control-plane ack
        // is built from this return value, so an armer learns that its post
        // discarded an older armed divergence instead of reading a bare `202`
        // and later concluding that armed divergences do not fire.
        let mut e = Engine::new();
        for i in 0..MAX_ARMED_DIVERGENCES {
            assert!(
                e.arm(Divergence::PartialFillNext {
                    client_order_id: format!("O-{i}"),
                    fraction: Decimal::ONE,
                })
                .is_none(),
                "arming below the cap must not shed anything"
            );
        }
        let shed = e.arm(Divergence::PartialFillNext {
            client_order_id: "OVERFLOW".to_string(),
            fraction: Decimal::ONE,
        });
        // The oldest entry is always the one that goes.
        assert!(matches!(
            shed,
            Some(Divergence::PartialFillNext { ref client_order_id, .. }) if client_order_id == "O-0"
        ));
    }

    #[test]
    fn arm_of_a_venue_owned_variant_sheds_nothing() {
        // The venue-owned and immediate variants never enter the queue, so
        // they can neither displace an entry nor report one - the ack for them
        // must stay a bare accept.
        let mut e = Engine::new();
        for i in 0..MAX_ARMED_DIVERGENCES {
            e.arm(Divergence::PartialFillNext {
                client_order_id: format!("O-{i}"),
                fraction: Decimal::ONE,
            });
        }
        assert!(e.arm(Divergence::GoDark { ms: 10 }).is_none());
        assert!(e.arm(Divergence::DelayAcks { ms: 10 }).is_none());
        assert_eq!(e.armed.len(), MAX_ARMED_DIVERGENCES);
    }

    #[test]
    fn buy_fill_moves_base_and_quote_balances() {
        let mut e = Engine::new();
        let out = e.process(Command::SubmitOrder(order("O1", 10)), 1);
        let state = account(&out, 2);

        let btc = balance(state, "BTC");
        assert_eq!(btc.total, Decimal::from(10));
        assert_eq!(btc.locked, Decimal::ZERO);
        assert_eq!(btc.free, Decimal::from(10));

        let usdt = balance(state, "USDT");
        assert_eq!(usdt.total, Decimal::from(-1000));
        assert_eq!(usdt.locked, Decimal::ZERO);
        assert_eq!(usdt.free, Decimal::from(-1000));
    }

    #[test]
    fn sell_fill_moves_balances_opposite() {
        let mut e = Engine::new();
        let order = order_with("O1", Side::Sell, "BTCUSDT", 10, Some(Decimal::from(100)));
        let out = e.process(Command::SubmitOrder(order), 1);
        let state = account(&out, 2);

        let btc = balance(state, "BTC");
        assert_eq!(btc.total, Decimal::from(-10));
        assert_eq!(btc.locked, Decimal::ZERO);
        assert_eq!(btc.free, Decimal::from(-10));

        let usdt = balance(state, "USDT");
        assert_eq!(usdt.total, Decimal::from(1000));
        assert_eq!(usdt.locked, Decimal::ZERO);
        assert_eq!(usdt.free, Decimal::from(1000));
    }

    #[test]
    fn accumulated_notional_saturates_instead_of_panicking() {
        // Two individually-valid orders whose combined notional exceeds
        // `Decimal::MAX` (~7.92e28): qty 7e20 is on the 1e-8 size grid (the
        // ratio, 7e28, still divides without overflowing), price 1e8 is on
        // the 0.01 grid, and the per-order notional 7e28 passes
        // `validate_submit`'s `checked_mul`. Before the clamped accumulation
        // helpers the second fill panicked in `next_position`'s VWAP
        // numerator (7e28 + 7e28) - a caller-reachable panic through the
        // public `process` API on wire-valid input.
        let qty: Decimal = "700000000000000000000".parse().unwrap();
        let px: Decimal = "100000000".parse().unwrap();

        let mut e = Engine::new();
        let first = e.process(
            Command::SubmitOrder(order_decimal("O1", Side::Buy, "BTCUSDT", qty, Some(px))),
            1,
        );
        assert!(matches!(first[0], VenueMessage::OrderAccepted { .. }));

        let out = e.process(
            Command::SubmitOrder(order_decimal("O2", Side::Buy, "BTCUSDT", qty, Some(px))),
            2,
        );

        assert_eq!(out.len(), 3);
        assert!(matches!(out[0], VenueMessage::OrderAccepted { .. }));
        let state = account(&out, 2);
        // The quote spend (-7e28 then -7e28 again) clips at the lower
        // Decimal boundary instead of panicking the `-=`.
        assert_eq!(balance(state, "USDT").total, Decimal::MIN);
        // Quantities never came near the boundary, so they stay exact -
        // only the overflowing notional legs clip.
        assert_eq!(balance(state, "BTC").total, qty + qty);
        let pos = position(state, "BTCUSDT");
        assert_eq!(pos.quantity, qty + qty);
        // The VWAP numerator saturated at `Decimal::MAX`, so the average
        // lands below the true 1e8 - clipped and warned about, but finite.
        assert!(pos.avg_px > Decimal::ZERO);
        assert!(pos.avg_px <= px);
    }

    #[test]
    fn resting_holds_saturate_locked_instead_of_panicking() {
        // Two resting buys whose summed holds (`leaves_qty * price`,
        // each individually within range and `checked_mul`-approved at
        // submit) exceed `Decimal::MAX`. A tiny armed partial (1e-9 of 7e20
        // = 7e11, still on the 1e-8 grid) leaves almost the whole quantity
        // resting, so each order locks just under 7e28 of quote. Before the
        // clamped helpers the second snapshot panicked in `held_balances`'
        // `+=` accumulation; the `free = total - locked` subtraction then
        // also needs clamping (small negative total minus a boundary lock).
        let qty: Decimal = "700000000000000000000".parse().unwrap();
        let px: Decimal = "100000000".parse().unwrap();
        let fraction = Decimal::new(1, 9);

        let mut e = Engine::new();
        for id in ["O1", "O2"] {
            e.arm(Divergence::PartialFillNext {
                client_order_id: id.into(),
                fraction,
            });
        }

        e.process(
            Command::SubmitOrder(order_decimal("O1", Side::Buy, "BTCUSDT", qty, Some(px))),
            1,
        );
        let out = e.process(
            Command::SubmitOrder(order_decimal("O2", Side::Buy, "BTCUSDT", qty, Some(px))),
            2,
        );

        assert_eq!(out.len(), 3);
        let state = account(&out, 2);
        let usdt = balance(state, "USDT");
        assert_eq!(usdt.locked, Decimal::MAX);
        assert_eq!(usdt.free, Decimal::MIN);
        assert_eq!(e.open_orders().len(), 2);
    }

    #[test]
    fn position_vwap_averages_same_direction_adds() {
        let mut e = Engine::new();
        e.process(Command::SubmitOrder(order("O1", 10)), 1);
        let second = order_with("O2", Side::Buy, "BTCUSDT", 10, Some(Decimal::from(200)));
        let out = e.process(Command::SubmitOrder(second), 2);
        let pos = position(account(&out, 2), "BTCUSDT");

        assert_eq!(pos.quantity, Decimal::from(20));
        assert_eq!(pos.avg_px, Decimal::from(150));
    }

    #[test]
    fn position_reduce_keeps_avg_px_and_shrinks_qty() {
        let mut e = Engine::new();
        e.process(Command::SubmitOrder(order("O1", 10)), 1);
        let reduce = order_with("O2", Side::Sell, "BTCUSDT", 4, Some(Decimal::from(150)));
        let out = e.process(Command::SubmitOrder(reduce), 2);
        let pos = position(account(&out, 2), "BTCUSDT");

        assert_eq!(pos.quantity, Decimal::from(6));
        assert_eq!(pos.avg_px, Decimal::from(100));
    }

    #[test]
    fn position_flip_reopens_at_fill_price() {
        let mut e = Engine::new();
        e.process(Command::SubmitOrder(order("O1", 5)), 1);
        let flip = order_with("O2", Side::Sell, "BTCUSDT", 8, Some(Decimal::from(120)));
        let out = e.process(Command::SubmitOrder(flip), 2);
        let pos = position(account(&out, 2), "BTCUSDT");

        assert_eq!(pos.quantity, Decimal::from(-3));
        assert_eq!(pos.avg_px, Decimal::from(120));
    }

    #[test]
    fn partial_fill_books_only_filled_portion_and_locks_remainder() {
        let mut e = Engine::new();
        e.arm(Divergence::PartialFillNext {
            client_order_id: "O1".into(),
            fraction: Decimal::from_f64(0.3).unwrap(),
        });
        let out = e.process(Command::SubmitOrder(order("O1", 10)), 1);
        let state = account(&out, 2);

        let btc = balance(state, "BTC");
        assert_eq!(btc.total, Decimal::from(3));
        assert_eq!(btc.locked, Decimal::ZERO);
        assert_eq!(btc.free, Decimal::from(3));

        let usdt = balance(state, "USDT");
        assert_eq!(usdt.total, Decimal::from(-300));
        assert_eq!(usdt.locked, Decimal::from(700));
        assert_eq!(usdt.free, Decimal::from(-1000));

        let pos = position(state, "BTCUSDT");
        assert_eq!(pos.quantity, Decimal::from(3));
        assert_eq!(pos.avg_px, Decimal::from(100));
    }

    #[test]
    fn cancel_frees_hold_and_emits_account_state() {
        let mut e = Engine::new();
        e.arm(Divergence::PartialFillNext {
            client_order_id: "O1".into(),
            fraction: Decimal::from_f64(0.3).unwrap(),
        });
        e.process(Command::SubmitOrder(order("O1", 10)), 1);

        let out = e.process(
            Command::CancelOrder {
                client_order_id: "O1".into(),
            },
            2,
        );
        assert_eq!(out.len(), 2);
        assert!(matches!(out[0], VenueMessage::OrderCanceled { .. }));
        let state = account(&out, 1);

        let usdt = balance(state, "USDT");
        assert_eq!(usdt.total, Decimal::from(-300));
        assert_eq!(usdt.locked, Decimal::ZERO);
        assert_eq!(usdt.free, Decimal::from(-300));
    }

    fn trailing_stop(id: &str, side: Side, trigger: i64, offset: i64) -> SubmitOrder {
        SubmitOrder {
            client_order_id: id.into(),
            symbol: "BTCUSDT".into(),
            position_id: None,
            side,
            order_type: OrderType::TrailingStopMarket,
            quantity: Decimal::ONE,
            price: None,
            trigger_price: Some(Decimal::from(trigger)),
            trail_offset: Some(Decimal::from(offset)),
            limit_offset: None,
            time_in_force: TimeInForce::Gtc,
            expire_time: None,
            reduce_only: false,
            post_only: false,
            link: None,
        }
    }

    fn resting_trigger(e: &Engine, id: &str) -> Decimal {
        let order = e
            .open
            .iter()
            .find(|order| order.submit.client_order_id == id)
            .expect("the order is still resting");
        match order.resting {
            Resting::Conditional { stop_px, .. } => stop_px,
            other => panic!("expected an untriggered conditional, got {other:?}"),
        }
    }

    /// A trailing stop follows the tape UP and never comes back down. The
    /// one-way movement is the whole mechanism: a trigger that retreated would
    /// be a stop somebody keeps amending.
    #[test]
    fn a_sell_trailing_stop_ratchets_up_and_never_retreats() {
        let mut e = Engine::build(EngineConfig {
            account_id: test_account_id(),
            instruments: default_instruments(),
            // A spot sell delivers the base, so the account has to hold some or
            // the order is refused before it can rest.
            balances: HashMap::from([("BTC".to_string(), Decimal::from(10))]),
            fill_seed: 7,
        });
        let accepted = e.process_with_market(
            Command::SubmitOrder(trailing_stop("T1", Side::Sell, 90, 10)),
            1,
            Some(MarketReading {
                last_px: Decimal::from(100),
                ts_ns: 1,
                band_ticks: 0,
            }),
        );
        assert!(
            e.open
                .iter()
                .any(|order| order.submit.client_order_id == "T1"),
            "the trailing stop must rest: {accepted:?}"
        );
        assert_eq!(resting_trigger(&e, "T1"), Decimal::from(90));

        // The tape rises, so the trail follows it up to mark less the offset.
        e.mark(&[("BTCUSDT".into(), Decimal::from(150))], 2);
        assert_eq!(
            resting_trigger(&e, "T1"),
            Decimal::from(140),
            "the trail follows the tape up"
        );

        // And it stays there when the tape falls back, which is the point.
        e.mark(&[("BTCUSDT".into(), Decimal::from(120))], 3);
        assert_eq!(
            resting_trigger(&e, "T1"),
            Decimal::from(140),
            "a trailing stop never retreats"
        );
    }

    /// A buy trail is the mirror: it follows the tape down.
    #[test]
    fn a_buy_trailing_stop_ratchets_down() {
        let mut e = Engine::build(EngineConfig {
            account_id: test_account_id(),
            instruments: default_instruments(),
            balances: HashMap::from([("USDT".to_string(), Decimal::from(1_000_000))]),
            fill_seed: 7,
        });
        e.process_with_market(
            Command::SubmitOrder(trailing_stop("T2", Side::Buy, 110, 10)),
            1,
            Some(MarketReading {
                last_px: Decimal::from(100),
                ts_ns: 1,
                band_ticks: 0,
            }),
        );
        e.mark(&[("BTCUSDT".into(), Decimal::from(50))], 2);
        assert_eq!(resting_trigger(&e, "T2"), Decimal::from(60));
        e.mark(&[("BTCUSDT".into(), Decimal::from(80))], 3);
        assert_eq!(
            resting_trigger(&e, "T2"),
            Decimal::from(60),
            "a buy trail never rises"
        );
    }

    fn trailing_stop_limit(
        id: &str,
        side: Side,
        trigger: i64,
        trail: i64,
        limit_gap: i64,
    ) -> SubmitOrder {
        let mut order = trailing_stop(id, side, trigger, trail);
        order.client_order_id = id.into();
        order.order_type = OrderType::TrailingStopLimit;
        order.limit_offset = Some(Decimal::from(limit_gap));
        order
    }

    fn resting_limit_px(e: &Engine, id: &str) -> Decimal {
        e.open
            .iter()
            .find(|order| order.submit.client_order_id == id)
            .expect("the order is still resting")
            .submit
            .price
            .expect("a trailing stop limit carries a derived limit")
    }

    /// The limit rides the trigger. A trailing stop limit carries two
    /// distances, and the second one is what this type exists for: the trigger
    /// trails the tape, and the limit trails the trigger. A limit that stayed
    /// where it started would drift further behind on every ratchet until it
    /// was unreachable, which is a working order that silently stops being one.
    #[test]
    fn a_trailing_stop_limit_moves_its_limit_with_its_trigger() {
        let mut e = Engine::build(EngineConfig {
            account_id: test_account_id(),
            instruments: default_instruments(),
            balances: HashMap::from([("BTC".to_string(), Decimal::from(10))]),
            fill_seed: 7,
        });
        let accepted = e.process_with_market(
            Command::SubmitOrder(trailing_stop_limit("TL1", Side::Sell, 90, 10, 2)),
            1,
            Some(MarketReading {
                last_px: Decimal::from(100),
                ts_ns: 1,
                band_ticks: 0,
            }),
        );
        assert!(
            e.open
                .iter()
                .any(|order| order.submit.client_order_id == "TL1"),
            "the trailing stop limit must rest: {accepted:?}"
        );
        // The limit is derived at acceptance, on the fillable side of the
        // trigger: a sell rests below it.
        assert_eq!(resting_trigger(&e, "TL1"), Decimal::from(90));
        assert_eq!(resting_limit_px(&e, "TL1"), Decimal::from(88));

        // The tape rises; both move, and the gap between them is preserved.
        e.mark(&[("BTCUSDT".into(), Decimal::from(150))], 2);
        assert_eq!(resting_trigger(&e, "TL1"), Decimal::from(140));
        assert_eq!(
            resting_limit_px(&e, "TL1"),
            Decimal::from(138),
            "the limit ratchets with the trigger, holding limit_offset"
        );

        // And neither retreats.
        e.mark(&[("BTCUSDT".into(), Decimal::from(120))], 3);
        assert_eq!(resting_trigger(&e, "TL1"), Decimal::from(140));
        assert_eq!(resting_limit_px(&e, "TL1"), Decimal::from(138));
    }

    /// A buy is the mirror on both distances: the trigger follows the tape down
    /// and the limit sits above the trigger, because that is the side a buy can
    /// fill from.
    #[test]
    fn a_buy_trailing_stop_limit_rests_its_limit_above_the_trigger() {
        let mut e = Engine::build(EngineConfig {
            account_id: test_account_id(),
            instruments: default_instruments(),
            balances: HashMap::from([("USDT".to_string(), Decimal::from(1_000_000))]),
            fill_seed: 7,
        });
        e.process_with_market(
            Command::SubmitOrder(trailing_stop_limit("TL2", Side::Buy, 110, 10, 2)),
            1,
            Some(MarketReading {
                last_px: Decimal::from(100),
                ts_ns: 1,
                band_ticks: 0,
            }),
        );
        assert_eq!(resting_limit_px(&e, "TL2"), Decimal::from(112));
        e.mark(&[("BTCUSDT".into(), Decimal::from(50))], 2);
        assert_eq!(resting_trigger(&e, "TL2"), Decimal::from(60));
        assert_eq!(resting_limit_px(&e, "TL2"), Decimal::from(62));
    }

    /// What the type is for, and it is the gap case rather than the ordinary
    /// one. A sell's limit rests below its trigger, so a print that merely
    /// reaches the trigger is normally through the limit too and fills at once.
    /// The limit is a floor, not a delay. It bites when the tape gaps past
    /// both: the trigger fires, the limit is not reachable, and the order rests
    /// instead of dumping into the hole. That is the whole difference from
    /// `TrailingStopMarket`, which would take whatever the gap offered.
    #[test]
    fn a_trailing_stop_limit_rests_rather_than_filling_through_its_limit() {
        let mut e = Engine::build(EngineConfig {
            account_id: test_account_id(),
            instruments: default_instruments(),
            balances: HashMap::from([("BTC".to_string(), Decimal::from(10))]),
            fill_seed: 7,
        });
        // Trigger 90, limit 88: sell at 88 or better, never worse.
        e.process_with_market(
            Command::SubmitOrder(trailing_stop_limit("TL3", Side::Sell, 90, 10, 2)),
            1,
            Some(MarketReading {
                last_px: Decimal::from(100),
                ts_ns: 1,
                band_ticks: 0,
            }),
        );
        assert_eq!(resting_limit_px(&e, "TL3"), Decimal::from(88));

        // The tape gaps from 100 straight to 70, through the trigger and
        // through the limit. The trigger fires on that print and the limit
        // cannot be met at it.
        let scan = e.pending_scans().remove(0);
        let (out, _) = e.apply_scans(
            &[ScanResult {
                client_order_id: scan.client_order_id,
                from_ns: scan.from_ns,
                revision: scan.revision,
                hit: Some(Hit {
                    ts_ns: 2,
                    px: Decimal::from(70),
                }),
                scanned_to_ns: 2,
            }],
            2,
        );
        assert!(
            out.iter()
                .any(|event| matches!(event, VenueMessage::OrderTriggered { .. })),
            "the trigger fires: {out:?}"
        );
        assert!(
            !out.iter()
                .any(|event| matches!(event, VenueMessage::OrderFilled(_))),
            "it must not fill through its own limit into the gap: {out:?}"
        );
        let order = e
            .open
            .iter()
            .find(|order| order.submit.client_order_id == "TL3")
            .expect("it rests as a limit");
        assert!(
            matches!(order.resting, Resting::Limit { .. }),
            "expected a live limit, got {:?}",
            order.resting
        );
    }

    /// A stop protects and a touched order enters, so for the same side and the
    /// same price they wait on opposite predicates. Getting this backwards turns
    /// every protective order into an entry.
    #[test]
    fn a_touched_order_triggers_from_the_opposite_side_of_a_stop() {
        use mogwai_protocol::{ScanKind, touches_toward, touches_trigger};
        let trigger = Decimal::from(100);
        // A sell stop protects a long: it fires when price falls to it.
        assert!(touches_trigger(Side::Sell, trigger, Decimal::from(99)));
        assert!(!touches_trigger(Side::Sell, trigger, Decimal::from(101)));
        // A sell touched order enters short on strength: it fires when price
        // rises to it.
        assert!(touches_toward(Side::Sell, trigger, Decimal::from(101)));
        assert!(!touches_toward(Side::Sell, trigger, Decimal::from(99)));
        // Both are touch rather than through, so the level itself fires them.
        assert!(touches_trigger(Side::Sell, trigger, trigger));
        assert!(touches_toward(Side::Sell, trigger, trigger));
        assert!(ScanKind::TouchedTrigger.hit(Side::Buy, trigger, Decimal::from(99)));
        assert!(!ScanKind::TouchedTrigger.hit(Side::Buy, trigger, Decimal::from(101)));
    }

    /// The new fields belong to exactly one shape each, and stating one
    /// elsewhere is refused rather than ignored - a consumer whose trail offset
    /// was silently dropped would believe its stop moves.
    #[test]
    fn the_new_order_fields_are_refused_where_they_mean_nothing() {
        use mogwai_protocol::validate_submit_order;
        let mut trailing = trailing_stop("T3", Side::Sell, 90, 10);
        trailing.trail_offset = None;
        assert!(validate_submit_order(&trailing).is_err());

        let mut fixed = stop_order("S1", Side::Sell, OrderType::StopMarket, 90, None);
        fixed.trail_offset = Some(Decimal::from(10));
        assert!(validate_submit_order(&fixed).is_err());

        // A trailing stop limit owes both distances, and the limit price is the
        // venue's to derive - a consumer-stated one would be overwritten by the
        // first ratchet, so it is refused rather than silently replaced.
        let sound = trailing_stop_limit("TL9", Side::Sell, 90, 10, 2);
        assert!(validate_submit_order(&sound).is_ok());

        let mut no_gap = sound.clone();
        no_gap.limit_offset = None;
        assert!(
            validate_submit_order(&no_gap).is_err(),
            "TrailingStopLimit owes a limit_offset"
        );

        let mut no_trail = sound.clone();
        no_trail.trail_offset = None;
        assert!(
            validate_submit_order(&no_trail).is_err(),
            "it owes a trail_offset too, like every trailing order"
        );

        let mut stated = sound.clone();
        stated.price = Some(Decimal::from(88));
        assert!(
            validate_submit_order(&stated).is_err(),
            "the limit price is derived, so stating one is refused"
        );

        let mut zero = sound.clone();
        zero.limit_offset = Some(Decimal::ZERO);
        assert!(
            validate_submit_order(&zero).is_err(),
            "a zero gap is a limit AT the trigger, which is a stop-limit"
        );

        let mut elsewhere = stop_order("S2", Side::Sell, OrderType::StopLimit, 90, Some(88));
        elsewhere.limit_offset = Some(Decimal::from(2));
        assert!(
            validate_submit_order(&elsewhere).is_err(),
            "limit_offset means nothing on a fixed stop-limit"
        );

        let mut gtd = order("G1", 1);
        gtd.time_in_force = TimeInForce::Gtd;
        assert!(validate_submit_order(&gtd).is_err(), "Gtd needs an expiry");
        gtd.expire_time = Some(10);
        assert!(validate_submit_order(&gtd).is_ok());

        let mut day = order("D1", 1);
        day.time_in_force = TimeInForce::Day;
        day.expire_time = Some(10);
        assert!(
            validate_submit_order(&day).is_err(),
            "a day order's expiry comes from the calendar, not the consumer"
        );
    }

    /// A conditional can be Day or Gtd - both can wait for a trigger - but not
    /// Ioc or Fok, which cannot wait for anything.
    #[test]
    fn a_conditional_may_expire_but_may_not_be_immediate() {
        use mogwai_protocol::validate_submit_order;
        let mut stop = stop_order("S2", Side::Sell, OrderType::StopMarket, 90, None);
        stop.time_in_force = TimeInForce::Day;
        assert!(validate_submit_order(&stop).is_ok());
        stop.time_in_force = TimeInForce::Ioc;
        assert!(validate_submit_order(&stop).is_err());
        stop.time_in_force = TimeInForce::Fok;
        assert!(validate_submit_order(&stop).is_err());
    }

    // --- Equity conventions --------------------------------------------------

    struct Shares {
        cash: i64,
        margin: Option<MarginPolicy>,
        borrowable: Option<Decimal>,
        lot_size: Decimal,
        settlement_ns: u64,
    }

    impl Default for Shares {
        fn default() -> Self {
            Self {
                cash: 10_000,
                margin: None,
                borrowable: None,
                lot_size: Decimal::ONE,
                settlement_ns: 0,
            }
        }
    }

    /// One US-listed share at 100, on an account holding `cash` dollars.
    fn equity_engine(shape: &Shares) -> Engine {
        let def = InstrumentDef {
            symbol: "AAPL".into(),
            class: InstrumentClass::Equity {
                currency: "USD".into(),
                multiplier: Decimal::ONE,
                lot_size: shape.lot_size,
                borrowable: shape.borrowable,
                settlement_ns: shape.settlement_ns,
            },
            price_precision: 2,
            size_precision: 0,
            price_increment: Decimal::new(1, 2),
            size_increment: Decimal::ONE,
        };
        let mut engine = Engine::build(EngineConfig {
            account_id: test_account_id(),
            instruments: vec![def],
            balances: HashMap::from([("USD".to_string(), Decimal::from(shape.cash))]),
            fill_seed: 7,
        });
        if let Some(policy) = shape.margin {
            engine.set_margin_policy("AAPL".into(), policy);
        }
        engine
    }

    fn reg_t() -> MarginPolicy {
        MarginPolicy {
            // Reg-T: half the notional to open, a quarter to hold.
            initial_per_contract: Decimal::new(5, 1),
            maintenance_per_contract: Decimal::new(25, 2),
            breach_action: MarginBreachAction::Refuse,
            basis: MarginBasis::Notional,
        }
    }

    fn share_order(id: &str, side: Side, qty: i64) -> SubmitOrder {
        let mut order = order_with(id, side, "AAPL", qty, Some(Decimal::from(100)));
        order.order_type = OrderType::Market;
        order
    }

    fn trade(engine: &mut Engine, order: SubmitOrder, ts: u64) -> Vec<VenueMessage> {
        engine.process_with_market(
            Command::SubmitOrder(order),
            ts,
            Some(MarketReading {
                last_px: Decimal::from(100),
                ts_ns: ts,
                band_ticks: 0,
            }),
        )
    }

    /// The defect this suite starts from: a funded equity account could not sell
    /// at all. The sell path fell through to the spot branch, which asks for the
    /// base currency an equity does not have, so closing a long was refused with
    /// a message about the futures margin ledger.
    #[test]
    fn a_cash_equity_account_can_buy_and_then_sell_what_it_holds() {
        let mut e = equity_engine(&Shares::default());
        let bought = trade(&mut e, share_order("BUY", Side::Buy, 50), 1);
        assert!(
            bought
                .iter()
                .any(|event| matches!(event, VenueMessage::OrderFilled(_))),
            "the buy fills: {bought:?}"
        );
        let sold = trade(&mut e, share_order("SELL", Side::Sell, 50), 2);
        assert!(
            sold.iter()
                .any(|event| matches!(event, VenueMessage::OrderFilled(_))),
            "and the shares can be sold again: {sold:?}"
        );
        assert_eq!(
            balance(&e.snapshot(3), "USD").total,
            Decimal::from(10_000),
            "round-tripping at one price leaves the cash where it started"
        );
    }

    /// Shorting is a margin activity, which is the cash-versus-margin
    /// distinction the class had no way to express.
    #[test]
    fn a_cash_equity_account_cannot_sell_short() {
        let mut e = equity_engine(&Shares::default());
        let out = trade(&mut e, share_order("SHORT", Side::Sell, 10), 1);
        assert!(
            reject_reason(&out).contains("cash equity account cannot sell"),
            "refused by name rather than as a funding shortfall: {out:?}"
        );
    }

    /// With a margin policy the same account can still short - up to its locate.
    #[test]
    fn a_margin_equity_account_shorts_within_its_borrow_and_no_further() {
        let mut e = equity_engine(&Shares {
            margin: Some(reg_t()),
            borrowable: Some(Decimal::from(20)),
            ..Shares::default()
        });
        let out = trade(&mut e, share_order("SHORT", Side::Sell, 20), 1);
        assert!(
            out.iter()
                .any(|event| matches!(event, VenueMessage::OrderFilled(_))),
            "a short inside the borrow fills: {out:?}"
        );
        let refused = trade(&mut e, share_order("MORE", Side::Sell, 5), 2);
        assert!(
            reject_reason(&refused).contains("no shares to borrow"),
            "and one beyond it is refused by name: {refused:?}"
        );
    }

    /// A name nobody will lend is expressible, and is not the same thing as an
    /// account that cannot afford the trade.
    #[test]
    fn a_hard_to_borrow_name_refuses_every_short() {
        let mut e = equity_engine(&Shares {
            margin: Some(reg_t()),
            borrowable: Some(Decimal::ZERO),
            ..Shares::default()
        });
        let out = trade(&mut e, share_order("SHORT", Side::Sell, 1), 1);
        assert!(
            reject_reason(&out).contains("no shares to borrow"),
            "{out:?}"
        );
    }

    /// Reg T: a margin account posts a fraction of the notional and borrows the
    /// rest, so it can hold more stock than it has cash. A cash account, given
    /// the same money, cannot.
    #[test]
    fn a_margin_account_buys_on_leverage_where_a_cash_account_cannot() {
        // 150 shares at 100 is 15,000 of stock against 10,000 of cash.
        let mut cash = equity_engine(&Shares::default());
        let refused = trade(&mut cash, share_order("BUY", Side::Buy, 150), 1);
        assert!(
            reject_reason(&refused).contains("insufficient USD"),
            "a cash account pays the whole notional: {refused:?}"
        );

        let mut margin = equity_engine(&Shares {
            margin: Some(reg_t()),
            ..Shares::default()
        });
        let out = trade(&mut margin, share_order("BUY", Side::Buy, 150), 1);
        assert!(
            out.iter()
                .any(|event| matches!(event, VenueMessage::OrderFilled(_))),
            "the same account on margin posts 7,500 and borrows the rest: {out:?}"
        );
        let state = margin.snapshot(2);
        assert_eq!(
            balance(&state, "USD").total,
            Decimal::from(-5_000),
            "the negative cash IS the margin loan"
        );
        assert_eq!(
            margin.valuation_in("USD"),
            Some(Decimal::from(10_000)),
            "and the account is still worth what it was: the shares are the other side of the loan"
        );
    }

    /// The round lot, which is a rule about what may be submitted rather than
    /// about what the size grid can represent.
    #[test]
    fn a_lot_size_refuses_an_order_that_is_not_a_whole_number_of_lots() {
        let mut e = equity_engine(&Shares {
            lot_size: Decimal::from(100),
            cash: 1_000_000,
            ..Shares::default()
        });
        let out = trade(&mut e, share_order("ODD", Side::Buy, 150), 1);
        assert!(
            reject_reason(&out).contains("multiple of the 100-share lot"),
            "{out:?}"
        );
        let round = trade(&mut e, share_order("ROUND", Side::Buy, 200), 2);
        assert!(
            round
                .iter()
                .any(|event| matches!(event, VenueMessage::OrderFilled(_))),
            "a whole number of lots is served: {round:?}"
        );
    }

    /// The settlement period: the money is yours the moment the trade prints and
    /// you cannot spend it until it settles.
    #[test]
    fn sale_proceeds_are_held_unsettled_until_their_instant() {
        const DAY: u64 = 24 * 60 * 60 * 1_000_000_000;
        let mut e = equity_engine(&Shares {
            settlement_ns: 2 * DAY,
            ..Shares::default()
        });
        trade(&mut e, share_order("BUY", Side::Buy, 50), 1);
        trade(&mut e, share_order("SELL", Side::Sell, 50), 2);

        let state = e.snapshot(3);
        let usd = balance(&state, "USD");
        assert_eq!(
            usd.total,
            Decimal::from(10_000),
            "the proceeds are credited at once - they are the account's money"
        );
        assert_eq!(
            usd.locked,
            Decimal::from(5_000),
            "and held unsettled: T+2 has not run"
        );
        assert_eq!(usd.free, Decimal::from(5_000));

        // Spending it before it settles is refused, which is the whole point.
        let early = trade(&mut e, share_order("EARLY", Side::Buy, 80), 4);
        assert!(
            reject_reason(&early).contains("insufficient USD"),
            "unsettled cash cannot be spent: {early:?}"
        );

        // The instant itself, not a step either side of it. The sale printed at
        // ts 2, so the credit settles at `2 * DAY + 2`; probing `2 * DAY` and
        // `2 * DAY + 3` steps over the boundary and cannot see a `>` / `<=`
        // flip in `release_settled_cash`.
        const SETTLES_AT: u64 = 2 * DAY + 2;
        assert!(
            !e.release_settled_cash(SETTLES_AT - 1),
            "one nanosecond before the instant nothing has settled"
        );
        assert!(
            e.release_settled_cash(SETTLES_AT),
            "and the instant itself settles it"
        );
        assert!(
            !e.release_settled_cash(SETTLES_AT + 1),
            "a credit settles once: the second pass moves nothing"
        );
        assert_eq!(
            balance(&e.snapshot(5), "USD").free,
            Decimal::from(10_000),
            "the money is spendable once it has settled"
        );
    }

    // --- Rivers nobody is reading --------------------------------------------

    /// The gap this closes: a resting order on a river with no cursor cannot
    /// fill, cannot expire, and cannot be told apart from one the tape has not
    /// reached. The venue refuses to leave it there.
    #[test]
    fn an_order_on_a_river_nobody_reads_is_cancelled_rather_than_left() {
        let mut e = Engine::build(EngineConfig {
            account_id: test_account_id(),
            instruments: default_instruments(),
            balances: HashMap::from([("USDT".to_string(), Decimal::from(1_000_000))]),
            fill_seed: 7,
        });
        let mut order = limit_order("REST", 1);
        order.price = Some(Decimal::from(1));
        e.process_with_market(
            Command::SubmitOrder(order),
            1,
            Some(MarketReading {
                last_px: Decimal::from(100),
                ts_ns: 1,
                band_ticks: 0,
            }),
        );
        assert_eq!(e.open.len(), 1);

        // A cursor is reading it: nothing happens.
        let readable = vec![Symbol::from("BTCUSDT")];
        assert!(e.cancel_unreadable_orders(&readable, 5).is_empty());
        assert_eq!(e.open.len(), 1);

        // The cursor wound down.
        let out = e.cancel_unreadable_orders(&[], 6);
        assert!(
            out.iter()
                .any(|event| matches!(event, VenueMessage::OrderCanceled { client_order_id, .. } if client_order_id == "REST")),
            "the consumer is told rather than left with an order nothing can decide: {out:?}"
        );
        assert!(e.open.is_empty());
    }

    /// A returning account's orders carry the departed boat's frontier, which
    /// sits in the new boat's future. Without a re-base the order waits for the
    /// new cursor to reach an instant the old one had already passed.
    #[test]
    fn a_resumed_order_scans_from_the_returning_boats_clock() {
        let mut e = Engine::build(EngineConfig {
            account_id: test_account_id(),
            instruments: default_instruments(),
            balances: HashMap::from([("USDT".to_string(), Decimal::from(1_000_000))]),
            fill_seed: 7,
        });
        let mut order = limit_order("REST", 1);
        order.price = Some(Decimal::from(1));
        e.process_with_market(
            Command::SubmitOrder(order),
            9_000,
            Some(MarketReading {
                last_px: Decimal::from(100),
                ts_ns: 9_000,
                band_ticks: 0,
            }),
        );
        assert_eq!(e.pending_scans()[0].from_ns, 9_000);

        // A fresh boat is placed at its river's origin, which is behind where
        // the departed one got to.
        e.rebase_scans(100);
        assert_eq!(
            e.pending_scans()[0].from_ns,
            100,
            "the order resumes from the clock that is actually running"
        );
    }

    /// `rebase_future_scans` moves only what leads the cursor.
    ///
    /// Both halves are the test. Rebasing a leading frontier is what closes the
    /// eviction-reconnect stall; leaving a trailing one alone is what keeps this
    /// from being `rebase_scans` under another name, applied on every bind. A
    /// trailing frontier names water the cursor has already covered and that
    /// this account is genuinely owed a scan over, so moving it forward would
    /// silently skip a span an order should have been judged on.
    #[test]
    fn only_a_frontier_that_leads_the_cursor_is_rebased() {
        let mut e = Engine::build(EngineConfig {
            account_id: test_account_id(),
            instruments: default_instruments(),
            balances: HashMap::from([("USDT".to_string(), Decimal::from(1_000_000))]),
            fill_seed: 7,
        });
        let mut order = limit_order("REST", 1);
        order.price = Some(Decimal::from(1));
        e.process_with_market(
            Command::SubmitOrder(order),
            9_000,
            Some(MarketReading {
                last_px: Decimal::from(100),
                ts_ns: 9_000,
                band_ticks: 0,
            }),
        );
        assert_eq!(e.pending_scans()[0].from_ns, 9_000);

        assert_eq!(
            e.rebase_future_scans(20_000),
            0,
            "a frontier trailing the cursor is ordinary and must be left alone"
        );
        assert_eq!(
            e.pending_scans()[0].from_ns,
            9_000,
            "the span from 9000 to 20000 is water this account is owed a scan over"
        );

        assert_eq!(
            e.rebase_future_scans(100),
            1,
            "a frontier ahead of the cursor is the eviction-reconnect stall and must be moved"
        );
        assert_eq!(
            e.pending_scans()[0].from_ns,
            100,
            "the order scans from the clock that is actually running"
        );
    }

    /// A returning socket may name a different symbol than the account was
    /// trading. What it holds off the joined river is retired, because the new
    /// session can neither see nor close it.
    #[test]
    fn resuming_on_another_river_retires_what_the_account_left_behind() {
        let mut e = Engine::build(EngineConfig {
            account_id: test_account_id(),
            instruments: default_instruments(),
            balances: HashMap::from([("USDT".to_string(), Decimal::from(1_000_000))]),
            fill_seed: 7,
        });
        let mut order = limit_order("REST", 1);
        order.price = Some(Decimal::from(1));
        e.process_with_market(
            Command::SubmitOrder(order),
            1,
            Some(MarketReading {
                last_px: Decimal::from(100),
                ts_ns: 1,
                band_ticks: 0,
            }),
        );
        let out = e.retire_off_river("MNQ", 5);
        assert!(
            out.iter()
                .any(|event| matches!(event, VenueMessage::OrderCanceled { .. })),
            "the stranded order goes: {out:?}"
        );
        assert!(e.open.is_empty());
        assert!(
            e.retire_off_river("BTCUSDT", 6).is_empty(),
            "and an account resuming on its own river keeps everything"
        );
    }

    // --- Order lists: OCO, OTO and OUO ---------------------------------------

    /// A funded engine on the spot default instrument, cash enough that no test
    /// here is measuring the funds gate by accident.
    fn linked_engine() -> Engine {
        Engine::build(EngineConfig {
            account_id: test_account_id(),
            instruments: default_instruments(),
            balances: HashMap::from([
                ("USDT".to_string(), Decimal::from(1_000_000)),
                ("BTC".to_string(), Decimal::from(100)),
            ]),
            fill_seed: 7,
        })
    }

    fn linked(order: SubmitOrder, link: mogwai_protocol::OrderLink) -> SubmitOrder {
        SubmitOrder {
            link: Some(link),
            ..order
        }
    }

    fn link_of(
        contingency: mogwai_protocol::Contingency,
        siblings: &[&str],
        parent: Option<&str>,
    ) -> mogwai_protocol::OrderLink {
        mogwai_protocol::OrderLink {
            order_list_id: "OL-1".into(),
            contingency,
            linked_order_ids: siblings.iter().map(|id| (*id).to_string()).collect(),
            parent_order_id: parent.map(ToOwned::to_owned),
        }
    }

    /// A resting limit at 100 that the tape has not reached: the market sits at
    /// 200, well above a buy limit's trigger, so nothing fills on arrival.
    fn away_reading() -> MarketReading {
        MarketReading {
            last_px: Decimal::from(200),
            ts_ns: 1,
            band_ticks: 0,
        }
    }

    fn canceled_ids(events: &[VenueMessage]) -> Vec<&str> {
        events
            .iter()
            .filter_map(|event| match event {
                VenueMessage::OrderCanceled {
                    client_order_id, ..
                } => Some(client_order_id.as_str()),
                _ => None,
            })
            .collect()
    }

    /// The primitive the venue had no mechanism for: one leg filling reaps the
    /// other, at the instant the fill is committed.
    #[test]
    fn an_oco_fill_cancels_its_sibling_where_the_fill_is_committed() {
        let mut e = linked_engine();
        for (id, sibling) in [("TP", "SL"), ("SL", "TP")] {
            let mut order = limit_order(id, 1);
            order.price = Some(Decimal::from(100));
            let order = linked(
                order,
                link_of(mogwai_protocol::Contingency::Oco, &[sibling], None),
            );
            e.process_with_market(Command::SubmitOrder(order), 1, Some(away_reading()));
        }
        assert_eq!(e.open.len(), 2, "both legs rest");

        // The tape reaches TP's trigger and nothing else.
        let scans = e.pending_scans();
        let tp = scans
            .iter()
            .find(|scan| scan.client_order_id == "TP")
            .expect("TP is scanned");
        let (out, _) = e.apply_scans(&[result(tp, true, 5)], 5);

        assert!(
            out.iter()
                .any(|event| matches!(event, VenueMessage::OrderFilled(fill) if fill.client_order_id == "TP")),
            "the take-profit filled: {out:?}"
        );
        assert_eq!(
            canceled_ids(&out),
            ["SL"],
            "its sibling is reaped in the same batch: {out:?}"
        );
        assert!(e.open.is_empty(), "no leg is left resting");
    }

    /// The failure the timing exists to prevent: two legs of one bracket swept
    /// in ONE batch. A reap that waited for the next pass would let the second
    /// leg fill against the same span of tape that filled the first.
    #[test]
    fn an_oco_sibling_swept_in_the_same_batch_cannot_also_fill() {
        let mut e = linked_engine();
        for (id, sibling) in [("TP", "SL"), ("SL", "TP")] {
            let mut order = limit_order(id, 1);
            order.price = Some(Decimal::from(100));
            let order = linked(
                order,
                link_of(mogwai_protocol::Contingency::Oco, &[sibling], None),
            );
            e.process_with_market(Command::SubmitOrder(order), 1, Some(away_reading()));
        }
        let scans = e.pending_scans();
        // Both legs are handed back as triggered in one batch, which is exactly
        // what a tape crossing both prices in one span produces.
        let results: Vec<ScanResult> = scans.iter().map(|scan| result(scan, true, 5)).collect();
        let (out, _) = e.apply_scans(&results, 5);

        let fills: Vec<&str> = out
            .iter()
            .filter_map(|event| match event {
                VenueMessage::OrderFilled(fill) => Some(fill.client_order_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            fills.len(),
            1,
            "exactly one leg of an OCO pair may ever fill: {out:?}"
        );
        assert_eq!(
            canceled_ids(&out).len(),
            1,
            "the other is canceled: {out:?}"
        );
        assert!(e.open.is_empty());
    }

    /// The hazard the group frame exists to close, driven per leg so it is
    /// visible, and then closed by the group frame in the test below.
    ///
    /// A two-leg `Ouo` bracket sent as two submits: the entry is marketable and
    /// fills on arrival, and the shrink its rule owes runs against a sibling
    /// that has not been submitted yet, so it adjusts nothing. The stop then
    /// arrives at full size beside a position that is already open, and the
    /// pair's aggregate exposure is twice one bracket's.
    ///
    /// This is not a bug in the linkage - the rule cannot shrink an order the
    /// venue has never seen - it is a property of per-leg dispatch, which is
    /// why the fix is a frame and not an arithmetic change. The wire refuses
    /// this route now; the engine still serves it, so the hazard stays
    /// reachable here and stays measured.
    #[test]
    fn per_leg_dispatch_lets_an_entry_fill_before_its_stop_is_admitted() {
        let mut e = linked_engine();
        let mut entry = limit_order("ENTRY", 2);
        entry.price = Some(Decimal::from(300));
        let entry = linked(
            entry,
            link_of(mogwai_protocol::Contingency::Ouo, &["STOP"], None),
        );
        let out = e.process_with_market(Command::SubmitOrder(entry), 1, Some(away_reading()));
        assert!(
            out.iter()
                .any(|event| matches!(event, VenueMessage::OrderFilled(fill) if fill.client_order_id == "ENTRY")),
            "a buy limit at 300 against a market at 200 fills on arrival: {out:?}"
        );

        let mut stop = limit_order("STOP", 2);
        stop.side = Side::Sell;
        stop.price = Some(Decimal::from(400));
        let stop = linked(
            stop,
            link_of(mogwai_protocol::Contingency::Ouo, &["ENTRY"], None),
        );
        e.process_with_market(Command::SubmitOrder(stop), 2, Some(away_reading()));

        let resting = e
            .open
            .iter()
            .find(|order| order.submit.client_order_id == "STOP")
            .expect("the stop rests");
        assert_eq!(
            resting.leaves_qty,
            Decimal::from(2),
            "the stop arrived at FULL size: its sibling's fill shrank nothing, because the stop \
             was not on the book when it happened"
        );
    }

    /// The same bracket as one group, and the guarantee the consumer cites: the
    /// entry fills during the group and the stop is shrunk by that fill before
    /// the call returns, so the pair's aggregate fill is bounded at one bracket
    /// quantity rather than two.
    ///
    /// The shrink here is the closing pass doing its job - at the instant the
    /// entry filled, the stop had not been admitted yet, exactly as in the
    /// per-leg test above. What differs is that the group has not returned, so
    /// nothing has been able to look at the stop in between.
    #[test]
    fn a_group_shrinks_a_sibling_admitted_after_the_fill_that_shrinks_it() {
        let mut e = linked_engine();
        let mut entry = limit_order("ENTRY", 2);
        entry.price = Some(Decimal::from(300));
        let entry = linked(
            entry,
            link_of(mogwai_protocol::Contingency::Ouo, &["STOP"], None),
        );
        let mut stop = limit_order("STOP", 2);
        stop.side = Side::Sell;
        stop.price = Some(Decimal::from(400));
        let stop = linked(
            stop,
            link_of(mogwai_protocol::Contingency::Ouo, &["ENTRY"], None),
        );

        let out = e.process_with_market(
            Command::SubmitOrderGroup {
                orders: vec![entry, stop],
            },
            1,
            Some(away_reading()),
        );
        let filled: Decimal = out
            .iter()
            .filter_map(|event| match event {
                VenueMessage::OrderFilled(fill) => Some(fill.last_qty),
                _ => None,
            })
            .sum();
        assert_eq!(filled, Decimal::from(2), "the entry filled whole: {out:?}");

        // Shrunk by the whole filled quantity, which takes it to zero, which is
        // a cancel. The bracket's aggregate exposure is therefore one bracket
        // quantity - the per-leg route above leaves it at two.
        assert_eq!(
            canceled_ids(&out),
            ["STOP"],
            "the entry's fill shrank the stop to nothing even though the stop was admitted after \
             it: {out:?}"
        );
        assert!(
            e.open.is_empty(),
            "nothing is left resting beside the filled entry"
        );
    }

    /// The mirror of the test above, and the one the closing pass got wrong.
    /// There the sibling was admitted after the fill, so only the closing pass
    /// could adjust it. Here the sibling is admitted before, by sending the
    /// group stop-first - so the submit path's own linkage would adjust it too,
    /// and `Ouo` subtracts rather than sets, so applying both shrank it twice.
    ///
    /// The quantities are deliberately unequal: with a 2-lot stop and a 1-lot
    /// entry, one shrink leaves 1 resting and two shrinks reach zero and cancel
    /// the stop outright. An equal-sized bracket cannot tell the two apart -
    /// both readings end at zero - which is exactly why the equal-sized test
    /// above passed against the double application.
    #[test]
    fn a_group_shrinks_a_sibling_admitted_before_the_fill_exactly_once() {
        let mut e = linked_engine();
        let mut stop = limit_order("STOP", 2);
        stop.side = Side::Sell;
        stop.price = Some(Decimal::from(400));
        let stop = linked(
            stop,
            link_of(mogwai_protocol::Contingency::Ouo, &["ENTRY"], None),
        );
        let mut entry = limit_order("ENTRY", 1);
        entry.price = Some(Decimal::from(300));
        let entry = linked(
            entry,
            link_of(mogwai_protocol::Contingency::Ouo, &["STOP"], None),
        );

        // Stop first, so it is resting by the time the entry fills.
        let out = e.process_with_market(
            Command::SubmitOrderGroup {
                orders: vec![stop, entry],
            },
            1,
            Some(away_reading()),
        );
        let filled: Decimal = out
            .iter()
            .filter_map(|event| match event {
                VenueMessage::OrderFilled(fill) => Some(fill.last_qty),
                _ => None,
            })
            .sum();
        assert_eq!(filled, Decimal::from(1), "the entry filled whole: {out:?}");

        assert!(
            !canceled_ids(&out).contains(&"STOP"),
            "the stop was shrunk, not cancelled: a second application of the same Ouo rule takes \
             its leaves to zero and reaps it, leaving the filled position with no exit: {out:?}"
        );
        let resting = e
            .open
            .iter()
            .find(|order| order.submit.client_order_id == "STOP")
            .expect("the stop is still resting");
        assert_eq!(
            resting.leaves_qty,
            Decimal::from(1),
            "shrunk by the fill ONCE - 2 less 1 - rather than by twice it: {out:?}"
        );
    }

    /// A hedging reduce-only member naming no `position_id` is refused, and the
    /// refusal reaches the dry pass rather than pass two.
    ///
    /// That refusal is the one `on_submit_from` makes before `validate_submit`,
    /// so a dry pass built only from the validator could not see it: the group
    /// passed admission whole and then lost one member on the second pass, with
    /// its siblings already accepted. It is the likeliest shape to hit it - a
    /// bracket's exits are the orders written reduce-only.
    #[test]
    fn a_group_refuses_a_hedging_reduce_only_member_before_admitting_anything() {
        let mut e = linked_engine();
        e.set_oms_type(mogwai_protocol::OmsType::Hedging);
        let mut entry = limit_order("ENTRY", 1);
        entry.price = Some(Decimal::from(100));
        let entry = linked(
            entry,
            link_of(mogwai_protocol::Contingency::Ouo, &["EXIT"], None),
        );
        let mut exit = limit_order("EXIT", 1);
        exit.side = Side::Sell;
        exit.price = Some(Decimal::from(300));
        exit.reduce_only = true;
        let exit = linked(
            exit,
            link_of(mogwai_protocol::Contingency::Ouo, &["ENTRY"], None),
        );

        let out = e.process_with_market(
            Command::SubmitOrderGroup {
                orders: vec![entry, exit],
            },
            1,
            Some(away_reading()),
        );
        assert!(
            !out.iter()
                .any(|event| matches!(event, VenueMessage::OrderAccepted { .. })),
            "the group is refused whole, with no member admitted first: {out:?}"
        );
        assert_eq!(
            out.iter()
                .filter(|event| matches!(event, VenueMessage::OrderRejected { .. }))
                .count(),
            2,
            "one rejection per member: {out:?}"
        );
        assert!(e.open.is_empty(), "and nothing rests");
    }

    /// The disclosed carve-out, and the test that keeps it distinguishable from
    /// a defect. Two members are individually affordable against the balance the
    /// dry pass reads, and jointly are not - so the first fills, spends the
    /// money, and the second is refused on the second pass.
    ///
    /// The group is right to refuse it and right not to call it an atomicity
    /// defect: both passes agree about the state, and it was the state that
    /// moved. `report_group_member_refusal` decides that by re-asking the dry
    /// question rather than by reading the reason text, and the `debug_assert`
    /// on the other branch is what this test is really pinning - a classifier
    /// that called this a defect would panic here.
    ///
    /// Gated on `debug_assertions` because that is where the assertion it pins
    /// is compiled in; in release the classifier's other branch is a log line
    /// and the test would pass without bite.
    #[cfg(debug_assertions)]
    #[test]
    fn a_group_member_defunded_by_its_own_group_is_the_disclosed_carve_out() {
        // Tight enough that either member alone is affordable and the two
        // together are not.
        let mut e = Engine::build(EngineConfig {
            account_id: test_account_id(),
            instruments: default_instruments(),
            balances: HashMap::from([
                ("USDT".to_string(), Decimal::from(400)),
                ("BTC".to_string(), Decimal::from(100)),
            ]),
            fill_seed: 7,
        });
        let mut first = limit_order("FIRST", 1);
        first.price = Some(Decimal::from(300));
        let first = linked(
            first,
            link_of(mogwai_protocol::Contingency::Ouo, &["SECOND"], None),
        );
        let mut second = limit_order("SECOND", 1);
        second.price = Some(Decimal::from(300));
        let second = linked(
            second,
            link_of(mogwai_protocol::Contingency::Ouo, &["FIRST"], None),
        );

        let out = e.process_with_market(
            Command::SubmitOrderGroup {
                orders: vec![first, second],
            },
            1,
            Some(away_reading()),
        );

        assert!(
            out.iter().any(|event| matches!(
                event,
                VenueMessage::OrderFilled(fill) if fill.client_order_id == "FIRST"
            )),
            "the first member filled and spent the balance: {out:?}"
        );
        assert!(
            out.iter().any(|event| matches!(
                event,
                VenueMessage::OrderRejected { client_order_id, .. } if client_order_id == "SECOND"
            )),
            "and the second could no longer be funded: {out:?}"
        );
    }

    /// A child listed before its parent. Nothing requires a group to list
    /// parents first, so the group context has to travel into the second pass
    /// as well as the dry one - a pass that dropped it refused the child for an
    /// unknown parent after the parent's siblings had been accepted.
    #[test]
    fn a_group_admits_a_child_listed_before_the_parent_it_names() {
        let mut e = linked_engine();
        let mut exit = limit_order("EXIT", 1);
        exit.side = Side::Sell;
        exit.price = Some(Decimal::from(300));
        let exit = linked(
            exit,
            link_of(
                mogwai_protocol::Contingency::NoContingency,
                &[],
                Some("ENTRY"),
            ),
        );
        let mut entry = limit_order("ENTRY", 1);
        entry.price = Some(Decimal::from(100));
        let entry = linked(
            entry,
            link_of(mogwai_protocol::Contingency::Oto, &["EXIT"], None),
        );

        // Child first.
        let out = e.process_with_market(
            Command::SubmitOrderGroup {
                orders: vec![exit, entry],
            },
            1,
            Some(away_reading()),
        );
        assert_eq!(
            out.iter()
                .filter(|event| matches!(event, VenueMessage::OrderAccepted { .. }))
                .count(),
            2,
            "both legs were admitted despite the child preceding its parent: {out:?}"
        );
        assert!(
            !out.iter()
                .any(|event| matches!(event, VenueMessage::OrderRejected { .. })),
            "and nothing was rejected for an unknown parent: {out:?}"
        );
    }

    /// Atomic admission, which is the other half. One unacceptable member
    /// rejects the whole group and leaves nothing on the book - not the members
    /// that were fine, and not an `OrderAccepted` for any of them.
    #[test]
    fn one_bad_member_rejects_a_whole_group_and_accepts_nothing() {
        let mut e = linked_engine();
        let mut good = limit_order("GOOD", 1);
        good.price = Some(Decimal::from(100));
        let good = linked(
            good,
            link_of(mogwai_protocol::Contingency::Oco, &["BAD"], None),
        );
        // Priced off the grid, which `validate_submit` refuses - and the engine
        // is the only gate that can, since the price increment is an instrument
        // fact `mogwai_protocol::validate_submit_group` does not hold. The
        // member used to be bad by naming an unknown symbol, which the group
        // validator now refuses one step earlier for disagreeing with its
        // siblings, leaving this test green while testing a different rule.
        let mut bad = limit_order("BAD", 1);
        bad.price = Some(Decimal::new(100_001, 3));
        let bad = linked(
            bad,
            link_of(mogwai_protocol::Contingency::Oco, &["GOOD"], None),
        );

        let out = e.process_with_market(
            Command::SubmitOrderGroup {
                orders: vec![good, bad],
            },
            1,
            Some(away_reading()),
        );
        // The reason is asserted, not just the ids. An id-only assertion cannot
        // tell the off-grid refusal this test is named for from whatever rule
        // refuses the group next - which is exactly how this test came to be
        // testing an unknown symbol instead.
        let rejected: Vec<(&str, &str)> = out
            .iter()
            .filter_map(|event| match event {
                VenueMessage::OrderRejected {
                    client_order_id,
                    reason,
                    ..
                } => Some((client_order_id.as_str(), reason.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(
            rejected,
            [
                (
                    "GOOD",
                    "order group rejected whole: BAD was refused because price violates price increment"
                ),
                ("BAD", "price violates price increment")
            ],
            "every member is refused for the BAD member's off-grid price, including the one that was fine: {out:?}"
        );
        assert!(
            !out.iter()
                .any(|event| matches!(event, VenueMessage::OrderAccepted { .. })),
            "and nothing was accepted first: {out:?}"
        );
        assert!(e.open.is_empty(), "nothing rests");
    }

    /// The group's closing snapshot obeys `DropNextAccountUpdate`, because it
    /// goes through `push_account_snapshot`, whose doc names exactly which
    /// sites do not. It used to be pushed unconditionally, which made the group
    /// one of three paths where a consumer could arm the divergence and still be
    /// told the truth about its balances. `expire_orders` was the second and
    /// was closed alongside it. The third, `on_modify`, was ruled the other way
    /// rather than closed: `modify_does_not_consume_armed_drop` pins the arm
    /// surviving an amend so it lands on the next fill, so that site is a
    /// marked exemption and not an outstanding gap.
    ///
    /// Two arms, because the closing pass runs after pass two: the filling
    /// member spends the first on its own snapshot, and the second is the one
    /// this test is about. The unarmed run is what makes the assertion
    /// discriminating - the same group, the same closing linkage, and the
    /// snapshot present - so a missing frame can only be the arm.
    #[test]
    fn the_groups_closing_snapshot_can_be_dropped() {
        let bracket = || {
            let mut entry = limit_order("ENTRY", 2);
            entry.price = Some(Decimal::from(300));
            let entry = linked(
                entry,
                link_of(mogwai_protocol::Contingency::Ouo, &["STOP"], None),
            );
            let mut stop = limit_order("STOP", 2);
            stop.side = Side::Sell;
            stop.price = Some(Decimal::from(400));
            let stop = linked(
                stop,
                link_of(mogwai_protocol::Contingency::Ouo, &["ENTRY"], None),
            );
            vec![entry, stop]
        };

        let mut plain = linked_engine();
        let out = plain.process_with_market(
            Command::SubmitOrderGroup { orders: bracket() },
            1,
            Some(away_reading()),
        );
        assert!(
            out.iter().any(|event| matches!(
                event,
                VenueMessage::OrderCanceled { client_order_id, .. } if client_order_id == "STOP"
            )),
            "the closing pass reaped the sibling: {out:?}"
        );
        assert!(
            matches!(out.last(), Some(VenueMessage::AccountState(_))),
            "and reported the ledger it moved: {out:?}"
        );

        let mut armed = linked_engine();
        armed.arm(Divergence::DropNextAccountUpdate);
        armed.arm(Divergence::DropNextAccountUpdate);
        let out = armed.process_with_market(
            Command::SubmitOrderGroup { orders: bracket() },
            1,
            Some(away_reading()),
        );
        assert!(
            out.iter().any(|event| matches!(
                event,
                VenueMessage::OrderCanceled { client_order_id, .. } if client_order_id == "STOP"
            )),
            "the same closing pass ran: {out:?}"
        );
        assert!(
            !matches!(out.last(), Some(VenueMessage::AccountState(_))),
            "and its snapshot was dropped: {out:?}"
        );
    }

    /// The group's wire-shape rules are the engine's own, not a courtesy the
    /// caller performs first.
    ///
    /// Both rules here are unreachable from the dry pass by construction, which
    /// is why the engine has to call `validate_submit_group` rather than trust
    /// what reached it: `validate_submit`'s duplicate test reads
    /// `seen_client_order_ids`, and on pass one no member is in it, so a group
    /// carrying the same id twice was admitted whole and then broke open on
    /// pass two with the first copy already accepted and possibly filled - the
    /// exact atomicity break the two-pass split exists to prevent. And an
    /// `Ioc`/`Fok` member is a now-or-never order whose fate admission does not
    /// decide, which no per-member check would ever have caught.
    ///
    /// Each case asserts its own refusal text, because the group rejects whole
    /// under every one of these rules and an id-only assertion could not tell
    /// them apart.
    #[test]
    fn the_engine_refuses_a_group_the_wire_shape_rules_refuse() {
        let cases: [(&str, [&str; 2], TimeInForce, &str); 2] = [
            (
                "duplicate id within the group",
                ["SAME", "SAME"],
                TimeInForce::Gtc,
                "duplicate client_order_id within the order group",
            ),
            (
                "an immediate-or-cancel member",
                ["A", "B"],
                TimeInForce::Ioc,
                "an order-group member cannot be immediate-or-cancel",
            ),
        ];
        for (name, ids, tif, expected) in cases {
            let mut e = linked_engine();
            let orders: Vec<SubmitOrder> = ids
                .iter()
                .map(|id| {
                    let mut leg = limit_order(id, 1);
                    leg.price = Some(Decimal::from(100));
                    leg.time_in_force = tif;
                    linked(
                        leg,
                        link_of(mogwai_protocol::Contingency::NoContingency, &[], None),
                    )
                })
                .collect();
            let out = e.process_with_market(
                Command::SubmitOrderGroup { orders },
                1,
                Some(away_reading()),
            );
            let reasons: Vec<&str> = out
                .iter()
                .filter_map(|event| match event {
                    VenueMessage::OrderRejected { reason, .. } => Some(reason.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(
                reasons.len(),
                2,
                "{name}: one rejection per member: {out:?}"
            );
            assert!(
                reasons.iter().all(|reason| reason.contains(expected)),
                "{name}: refused by its own rule: {reasons:?}"
            );
            assert!(
                !out.iter()
                    .any(|event| matches!(event, VenueMessage::OrderAccepted { .. })),
                "{name}: and nothing was accepted first: {out:?}"
            );
            assert!(e.open.is_empty(), "{name}: nothing rests");
            assert!(
                e.seen_client_order_ids.is_empty(),
                "{name}: no id was burned"
            );
        }
    }

    /// A child may name a parent that travels with it. Sent per leg the child
    /// would have to follow its parent by a round trip; in one group they are
    /// admitted together, so the dry pass has to treat the group's own ids as
    /// known or the whole frame would refuse itself.
    #[test]
    fn a_group_admits_a_child_whose_parent_is_in_the_same_frame() {
        let mut e = linked_engine();
        let mut entry = limit_order("ENTRY", 1);
        entry.price = Some(Decimal::from(100));
        let entry = linked(
            entry,
            link_of(mogwai_protocol::Contingency::Oto, &["EXIT"], None),
        );
        let mut exit = limit_order("EXIT", 1);
        exit.side = Side::Sell;
        exit.price = Some(Decimal::from(300));
        let exit = linked(
            exit,
            link_of(
                mogwai_protocol::Contingency::NoContingency,
                &[],
                Some("ENTRY"),
            ),
        );

        let out = e.process_with_market(
            Command::SubmitOrderGroup {
                orders: vec![entry, exit],
            },
            1,
            Some(away_reading()),
        );
        assert_eq!(
            out.iter()
                .filter(|event| matches!(event, VenueMessage::OrderAccepted { .. }))
                .count(),
            2,
            "both legs were admitted: {out:?}"
        );
        assert!(
            e.open
                .iter()
                .any(|order| order.submit.client_order_id == "EXIT"
                    && matches!(order.resting, Resting::Held)),
            "and the child is held, waiting on the parent it arrived with"
        );
    }

    /// One-triggers-the-other: the child is accepted, holds nothing, is scanned
    /// by nothing, and goes live at its parent's fill.
    #[test]
    fn an_oto_child_waits_for_its_parent_and_then_rests() {
        let mut e = linked_engine();
        let mut entry = limit_order("ENTRY", 1);
        entry.price = Some(Decimal::from(100));
        let entry = linked(
            entry,
            link_of(mogwai_protocol::Contingency::Oto, &["EXIT"], None),
        );
        e.process_with_market(Command::SubmitOrder(entry), 1, Some(away_reading()));

        let mut exit = limit_order("EXIT", 1);
        exit.side = Side::Sell;
        exit.price = Some(Decimal::from(300));
        let exit = linked(
            exit,
            link_of(
                mogwai_protocol::Contingency::NoContingency,
                &[],
                Some("ENTRY"),
            ),
        );
        let out = e.process_with_market(Command::SubmitOrder(exit), 2, Some(away_reading()));
        assert!(
            out.iter()
                .any(|event| matches!(event, VenueMessage::OrderAccepted { .. })),
            "a held child is accepted, not refused: {out:?}"
        );
        assert_eq!(
            e.pending_scans().len(),
            1,
            "only the parent is scanned while the child waits"
        );
        assert!(
            e.open
                .iter()
                .any(|order| order.submit.client_order_id == "EXIT"
                    && matches!(order.resting, Resting::Held)),
            "the child rests held"
        );
        let snapshot_while_held = e.snapshot(2);
        assert_eq!(
            balance(&snapshot_while_held, "BTC").locked,
            Decimal::ZERO,
            "a held sell child places no base-currency hold"
        );

        let scans = e.pending_scans();
        e.apply_scans(&[result(&scans[0], true, 5)], 5);

        assert!(
            e.open
                .iter()
                .any(|order| order.submit.client_order_id == "EXIT"
                    && matches!(order.resting, Resting::Limit { .. })),
            "the parent's fill released the child"
        );
        assert_eq!(
            e.pending_scans().len(),
            1,
            "the released child is now the scanned order"
        );
        assert_eq!(
            balance(&e.snapshot(5), "BTC").locked,
            Decimal::ONE,
            "and it takes its hold at release"
        );
    }

    /// A conditional parent cancelled at its own trigger reaps its held child.
    ///
    /// The reduce-only cap-zero cancel inside `on_trigger` is one of six
    /// terminal paths that used to take an order off the book without reaping:
    /// the child was left resting `Held`, scanned by nothing, holding nothing,
    /// waiting for a release that could never come, and only a consumer cancel
    /// could ever end it. `close_out` now owns the rule for every terminal
    /// path.
    #[test]
    fn a_parent_cancelled_at_its_trigger_takes_its_held_child_with_it() {
        let mut e = linked_engine();
        let mut entry = order("ENTRY", 1);
        entry.order_type = OrderType::StopMarket;
        entry.price = None;
        entry.trigger_price = Some(Decimal::from(300));
        // Reduce-only against no position at all: its cap is zero the moment it
        // fires, so it cancels at the trigger without ever having filled.
        entry.reduce_only = true;
        let entry = linked(
            entry,
            link_of(mogwai_protocol::Contingency::Oto, &["EXIT"], None),
        );
        e.process_with_market(Command::SubmitOrder(entry), 1, Some(away_reading()));

        let mut exit = limit_order("EXIT", 1);
        exit.side = Side::Sell;
        exit.price = Some(Decimal::from(300));
        let exit = linked(
            exit,
            link_of(
                mogwai_protocol::Contingency::NoContingency,
                &[],
                Some("ENTRY"),
            ),
        );
        e.process_with_market(Command::SubmitOrder(exit), 2, Some(away_reading()));
        assert!(
            e.open
                .iter()
                .any(|open| open.submit.client_order_id == "EXIT"
                    && matches!(open.resting, Resting::Held)),
            "premise: the child is held on the untriggered parent"
        );

        let scans = e.pending_scans();
        assert_eq!(scans.len(), 1, "premise: only the parent is scanned");
        let (out, _) = e.apply_scans(&[result(&scans[0], true, 5)], 5);

        let canceled: Vec<&str> = out
            .iter()
            .filter_map(|event| match event {
                VenueMessage::OrderCanceled {
                    client_order_id, ..
                } => Some(client_order_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            canceled,
            ["ENTRY", "EXIT"],
            "the parent's cancel goes out first and the orphaned child follows it: {out:?}"
        );
        assert!(
            e.open.is_empty(),
            "and nothing is left resting: {:?}",
            e.open.iter().collect::<Vec<_>>()
        );
    }

    /// A price amend on a held child leaves it held.
    ///
    /// `Resting::Held` is not `Resting::Conditional`, so the amend's promotion
    /// guard used to let it through: the child became a live scannable limit,
    /// took a hold it had deliberately not taken, and could fill before
    /// its parent ever executed - one-triggers-the-other defeated by an amend.
    #[test]
    fn a_price_amend_does_not_release_a_held_child() {
        let mut e = linked_engine();
        let mut entry = limit_order("ENTRY", 1);
        entry.price = Some(Decimal::from(100));
        let entry = linked(
            entry,
            link_of(mogwai_protocol::Contingency::Oto, &["EXIT"], None),
        );
        e.process_with_market(Command::SubmitOrder(entry), 1, Some(away_reading()));

        let mut exit = limit_order("EXIT", 1);
        exit.side = Side::Sell;
        exit.price = Some(Decimal::from(300));
        let exit = linked(
            exit,
            link_of(
                mogwai_protocol::Contingency::NoContingency,
                &[],
                Some("ENTRY"),
            ),
        );
        e.process_with_market(Command::SubmitOrder(exit), 2, Some(away_reading()));

        let out = e.process_with_market(
            Command::ModifyOrder {
                client_order_id: "EXIT".into(),
                price: Some(Decimal::from(250)),
                quantity: None,
                trigger_price: None,
            },
            3,
            Some(away_reading()),
        );
        assert!(
            out.iter()
                .any(|event| matches!(event, VenueMessage::OrderUpdated { .. })),
            "the amend itself is honoured: {out:?}"
        );

        let child = e
            .open
            .iter()
            .find(|open| open.submit.client_order_id == "EXIT")
            .expect("the child is still open");
        assert!(
            matches!(child.resting, Resting::Held),
            "the amended child is still held, not promoted to a live limit: {:?}",
            child.resting
        );
        assert_eq!(
            child.submit.price,
            Some(Decimal::from(250)),
            "and the amend moved the price it will rest at when released"
        );
        assert_eq!(
            e.pending_scans().len(),
            1,
            "only the parent is scanned; an amended child is offered no tape"
        );
        assert_eq!(
            balance(&e.snapshot(3), "BTC").locked,
            Decimal::ZERO,
            "and it still holds nothing"
        );
    }

    /// A child of a parent that already filled has nothing to wait for. This is
    /// the fast-market bracket: a market entry that filled on arrival.
    #[test]
    fn a_child_of_an_already_filled_parent_is_live_at_once() {
        let mut e = linked_engine();
        let entry = linked(
            order("ENTRY", 1),
            link_of(mogwai_protocol::Contingency::Oto, &["EXIT"], None),
        );
        e.process_with_market(
            Command::SubmitOrder(entry),
            1,
            Some(MarketReading {
                last_px: Decimal::from(100),
                ts_ns: 1,
                band_ticks: 0,
            }),
        );
        let mut exit = limit_order("EXIT", 1);
        exit.side = Side::Sell;
        exit.price = Some(Decimal::from(300));
        let exit = linked(
            exit,
            link_of(
                mogwai_protocol::Contingency::NoContingency,
                &[],
                Some("ENTRY"),
            ),
        );
        e.process_with_market(Command::SubmitOrder(exit), 2, Some(away_reading()));
        assert!(
            e.open
                .iter()
                .any(|order| order.submit.client_order_id == "EXIT"
                    && matches!(order.resting, Resting::Limit { .. })),
            "nothing to wait for, so the child rests live"
        );
    }

    /// A released `MarketToLimit` child rests; it does not take the market.
    ///
    /// The standalone submit path takes the market for this type, and
    /// `docs/oms-types.md` states the carve-out this test pins: `release_child`
    /// rests every non-conditional child at its stated price, because the
    /// linkage pass that releases it holds no `MarketReading` to price against.
    /// The child here is marketable at release - a sell limited at 50 against a
    /// market of 99 - so "rested" and "took the market" are distinguishable
    /// outcomes rather than the same silence. Give the release a reading and
    /// this test fails on the fill it would produce, which is the point: that
    /// would be a change to what an order-list child means, not a bug fix.
    #[test]
    fn a_released_market_to_limit_child_rests_at_its_stated_price() {
        let mut e = linked_engine();
        // The child is sent first so it is held when its parent fills: pass two
        // submits members in order, and a child of an already-filled parent is
        // live at once and never sees `release_child` at all.
        let mut exit = limit_order("EXIT", 1);
        exit.order_type = OrderType::MarketToLimit;
        exit.side = Side::Sell;
        exit.price = Some(Decimal::from(50));
        let exit = linked(
            exit,
            link_of(
                mogwai_protocol::Contingency::NoContingency,
                &[],
                Some("ENTRY"),
            ),
        );
        let entry = linked(
            limit_order("ENTRY", 1),
            link_of(mogwai_protocol::Contingency::Oto, &["EXIT"], None),
        );

        let out = e.process_with_market(
            Command::SubmitOrderGroup {
                orders: vec![exit, entry],
            },
            1,
            Some(reading(0)),
        );

        assert!(
            out.iter().any(|event| matches!(
                event,
                VenueMessage::OrderFilled(fill) if fill.client_order_id == "ENTRY"
            )),
            "the parent filled on arrival, so the release actually ran: {out:?}"
        );
        assert!(
            !out.iter().any(|event| matches!(
                event,
                VenueMessage::OrderFilled(fill) if fill.client_order_id == "EXIT"
            )),
            "and the released child took nothing, marketable though it is: {out:?}"
        );

        let child = e
            .open
            .iter()
            .find(|open| open.submit.client_order_id == "EXIT")
            .expect("the released child is still open");
        assert!(
            matches!(
                child.resting,
                Resting::Limit { fill_trigger_px } if fill_trigger_px == Decimal::from(50)
            ),
            "it rests as an ordinary limit at its stated price: {:?}",
            child.resting
        );
        assert_eq!(
            child.leaves_qty,
            Decimal::from(1),
            "with its whole quantity still to work"
        );
    }

    /// A bracket whose entry is cancelled must not leave its exits waiting for a
    /// release that can never come.
    #[test]
    fn cancelling_a_parent_that_never_filled_reaps_its_children() {
        let mut e = linked_engine();
        let mut entry = limit_order("ENTRY", 1);
        entry.price = Some(Decimal::from(100));
        let entry = linked(
            entry,
            link_of(mogwai_protocol::Contingency::Oto, &["EXIT"], None),
        );
        e.process_with_market(Command::SubmitOrder(entry), 1, Some(away_reading()));
        let mut exit = limit_order("EXIT", 1);
        exit.side = Side::Sell;
        exit.price = Some(Decimal::from(300));
        let exit = linked(
            exit,
            link_of(
                mogwai_protocol::Contingency::NoContingency,
                &[],
                Some("ENTRY"),
            ),
        );
        e.process_with_market(Command::SubmitOrder(exit), 2, Some(away_reading()));

        let out = e.process(
            Command::CancelOrder {
                client_order_id: "ENTRY".into(),
            },
            3,
        );
        let mut canceled = canceled_ids(&out);
        canceled.sort_unstable();
        assert_eq!(canceled, ["ENTRY", "EXIT"], "both go: {out:?}");
        assert!(e.open.is_empty());
    }

    /// One-updates-the-other, which is the bracket that survives a partial fill:
    /// the surviving leg shrinks to what is left of the position rather than
    /// staying sized for the whole of it.
    #[test]
    fn an_ouo_fill_shrinks_its_sibling_by_the_filled_quantity() {
        let mut e = linked_engine();
        for (id, sibling) in [("TP", "SL"), ("SL", "TP")] {
            let mut order = limit_order(id, 4);
            order.price = Some(Decimal::from(100));
            let order = linked(
                order,
                link_of(mogwai_protocol::Contingency::Ouo, &[sibling], None),
            );
            e.process_with_market(Command::SubmitOrder(order), 1, Some(away_reading()));
        }
        e.arm(Divergence::PartialFillNext {
            client_order_id: "TP".into(),
            fraction: Decimal::new(25, 2),
        });
        let scans = e.pending_scans();
        let tp = scans
            .iter()
            .find(|scan| scan.client_order_id == "TP")
            .expect("TP is scanned");
        let (out, _) = e.apply_scans(&[result(tp, true, 5)], 5);

        assert!(
            out.iter().any(
                |event| matches!(event, VenueMessage::OrderUpdated { client_order_id, quantity, .. }
                    if client_order_id == "SL" && *quantity == Decimal::from(3))
            ),
            "the sibling shrinks by the one unit that filled: {out:?}"
        );
        let sl = e
            .open
            .iter()
            .find(|order| order.submit.client_order_id == "SL")
            .expect("SL still rests");
        assert_eq!(sl.leaves_qty, Decimal::from(3));
    }

    /// The shapes the venue refuses, each for a reason it can state.
    #[test]
    fn linkage_shapes_the_venue_cannot_honour_are_refused() {
        use mogwai_protocol::validate_submit_order;

        let base = limit_order("X", 1);
        let self_linked = linked(
            base.clone(),
            link_of(mogwai_protocol::Contingency::Oco, &["X"], None),
        );
        assert!(validate_submit_order(&self_linked).is_err());

        let empty_oco = linked(
            base.clone(),
            link_of(mogwai_protocol::Contingency::Oco, &[], None),
        );
        assert!(validate_submit_order(&empty_oco).is_err());

        let market_child = linked(
            order("M", 1),
            link_of(mogwai_protocol::Contingency::NoContingency, &[], Some("P")),
        );
        // Named, not merely `is_err`. Both of these orders break exactly one
        // rule, so a bare `is_err` would go on passing if the child arm were
        // deleted and some unrelated arm caught them instead - which is how a
        // 2026-08-19 fix pass came to believe neither rule existed and started
        // installing a second copy in `Engine::validate_submit`.
        assert_eq!(
            validate_submit_order(&market_child).unwrap_err(),
            "a Market order cannot be an order-list child: a released child rests, and a market \
             order has nothing to rest on",
            "a market child has nothing to rest on once released"
        );

        let mut ioc_child = base.clone();
        ioc_child.time_in_force = TimeInForce::Ioc;
        let ioc_child = linked(
            ioc_child,
            link_of(mogwai_protocol::Contingency::NoContingency, &[], Some("P")),
        );
        assert_eq!(
            validate_submit_order(&ioc_child).unwrap_err(),
            "an order-list child cannot be immediate-or-cancel: it must outlive the submit that \
             placed it to be released at all",
            "a now-or-never child would expire before its parent ever fills"
        );
        // And it is reached on the group route too, which is the only route a
        // linked order may legally travel: `boundary_error` refuses a linked
        // standalone `SubmitOrder` outright.
        assert!(
            mogwai_protocol::validate_submit_group(std::slice::from_ref(&ioc_child)).is_err(),
            "the group route runs the same per-member validation"
        );

        // Venue-state refusals: an unknown parent, and a child of a child.
        let mut e = linked_engine();
        let orphan = linked(
            base,
            link_of(
                mogwai_protocol::Contingency::NoContingency,
                &[],
                Some("NOBODY"),
            ),
        );
        let out = e.process_with_market(Command::SubmitOrder(orphan), 1, Some(away_reading()));
        assert!(
            reject_reason(&out).contains("unknown parent order"),
            "{out:?}"
        );
    }

    /// The depth rule, which is what makes a cancel's byte budget
    /// computable: one generation, never a chain.
    #[test]
    fn a_child_may_not_itself_be_a_parent() {
        let mut e = linked_engine();
        let mut entry = limit_order("ENTRY", 1);
        entry.price = Some(Decimal::from(100));
        let entry = linked(
            entry,
            link_of(mogwai_protocol::Contingency::Oto, &["CHILD"], None),
        );
        e.process_with_market(Command::SubmitOrder(entry), 1, Some(away_reading()));

        let mut child = limit_order("CHILD", 1);
        child.price = Some(Decimal::from(100));
        let child = linked(
            child,
            link_of(
                mogwai_protocol::Contingency::NoContingency,
                &[],
                Some("ENTRY"),
            ),
        );
        e.process_with_market(Command::SubmitOrder(child), 2, Some(away_reading()));

        let mut grandchild = limit_order("GRANDCHILD", 1);
        grandchild.price = Some(Decimal::from(100));
        let grandchild = linked(
            grandchild,
            link_of(
                mogwai_protocol::Contingency::NoContingency,
                &[],
                Some("CHILD"),
            ),
        );
        let out = e.process_with_market(Command::SubmitOrder(grandchild), 3, Some(away_reading()));
        assert!(
            reject_reason(&out).contains("may not itself be a parent"),
            "{out:?}"
        );
    }

    /// A `Gtd` order stops resting at its instant whether or not the tape ever
    /// came near it. That independence from the trigger walk is why expiry is
    /// its own pass.
    #[test]
    fn a_gtd_order_expires_at_its_instant_and_not_before() {
        let mut e = Engine::build(EngineConfig {
            account_id: test_account_id(),
            instruments: default_instruments(),
            balances: HashMap::from([("USDT".to_string(), Decimal::from(1_000_000))]),
            fill_seed: 7,
        });
        let mut order = limit_order("G1", 1);
        order.price = Some(Decimal::from(1));
        order.time_in_force = TimeInForce::Gtd;
        order.expire_time = Some(500);
        e.process_with_market(
            Command::SubmitOrder(order),
            1,
            Some(MarketReading {
                last_px: Decimal::from(100),
                ts_ns: 1,
                band_ticks: 0,
            }),
        );
        assert_eq!(e.open.len(), 1);

        assert!(
            e.expire_orders(499, None, 499).is_empty(),
            "an order must not expire before its instant"
        );
        assert_eq!(e.open.len(), 1);

        let out = e.expire_orders(500, None, 500);
        assert!(
            out.iter()
                .any(|event| matches!(event, VenueMessage::OrderExpired { .. })),
            "the order expires at its instant: {out:?}"
        );
        // Expired, never Canceled: nobody pulled this order, its stated
        // lifetime ran out, and a host reconciling the two facts acts on them
        // differently.
        assert!(
            !out.iter()
                .any(|event| matches!(event, VenueMessage::OrderCanceled { .. })),
            "expiry must not also report a cancel: {out:?}"
        );
        assert!(e.open.is_empty());
        assert_eq!(
            e.closed.get("G1").map(|info| info.status),
            Some(WireOrderStatus::Expired),
            "the truth-store row an order query answers from carries the expiry too"
        );
    }

    /// A `Day` order expires when its own symbol's session closes, and a
    /// symbol with no calendar never supplies one - so a day order on a 24/7
    /// instrument rests like a Gtc rather than expiring at an invented hour.
    #[test]
    fn a_day_order_expires_only_when_its_own_session_closes() {
        let mut e = Engine::build(EngineConfig {
            account_id: test_account_id(),
            instruments: default_instruments(),
            balances: HashMap::from([("USDT".to_string(), Decimal::from(1_000_000))]),
            fill_seed: 7,
        });
        let mut order = limit_order("D1", 1);
        order.price = Some(Decimal::from(1));
        order.time_in_force = TimeInForce::Day;
        e.process_with_market(
            Command::SubmitOrder(order),
            1,
            Some(MarketReading {
                last_px: Decimal::from(100),
                ts_ns: 1,
                band_ticks: 0,
            }),
        );

        assert!(
            e.expire_orders(1_000, None, 1_000).is_empty(),
            "no session closed, so nothing expires"
        );
        assert!(
            e.expire_orders(1_000, Some("SOMETHING-ELSE"), 1_000)
                .is_empty(),
            "another instrument's close must not expire this order"
        );
        let out = e.expire_orders(1_000, Some("BTCUSDT"), 1_000);
        assert!(
            out.iter()
                .any(|event| matches!(event, VenueMessage::OrderExpired { .. })),
            "its own session closing expires it: {out:?}"
        );
        // A session close ends the order's stated lifetime; it is not the
        // venue cancelling anyone's order, and the two reach a host as
        // different facts.
        assert!(
            !out.iter()
                .any(|event| matches!(event, VenueMessage::OrderCanceled { .. })),
            "a session close must not report a cancel: {out:?}"
        );
    }

    const EIGHT_HOURS_NS: u64 = 8 * 3_600 * 1_000_000_000;

    fn perpetual(rate: Decimal) -> InstrumentDef {
        perpetual_against(rate, None, Decimal::ZERO)
    }

    fn perpetual_against(
        rate: Decimal,
        index_symbol: Option<&str>,
        clamp: Decimal,
    ) -> InstrumentDef {
        InstrumentDef {
            symbol: "BTCUSDT.P".into(),
            class: InstrumentClass::Perpetual {
                underlying: "BTC".into(),
                settlement_currency: "USDT".into(),
                multiplier: Decimal::ONE,
                asset_class: WireAssetClass::Cryptocurrency,
                funding_interval_ns: EIGHT_HOURS_NS,
                funding_rate: rate,
                index_symbol: index_symbol.map(str::to_owned),
                funding_clamp: clamp,
            },
            price_precision: 2,
            size_precision: 0,
            price_increment: Decimal::new(1, 2),
            size_increment: Decimal::ONE,
        }
    }

    /// A perpetual has no expiry to converge at, so funding is the only thing
    /// tying it to spot. A long pays a positive rate, and pays it on notional at
    /// the mark rather than at entry.
    #[test]
    fn a_long_perpetual_pays_funding_on_its_marked_notional() {
        let mut e = Engine::build(EngineConfig {
            account_id: test_account_id(),
            instruments: vec![perpetual(Decimal::new(1, 4))],
            balances: HashMap::from([("USDT".to_string(), Decimal::from(100_000))]),
            fill_seed: 0,
        });
        e.set_margin_policy(
            "BTCUSDT.P".into(),
            MarginPolicy {
                initial_per_contract: Decimal::ZERO,
                maintenance_per_contract: Decimal::ZERO,
                breach_action: MarginBreachAction::Refuse,
                basis: MarginBasis::PerContract,
            },
        );
        e.process(
            Command::SubmitOrder(order_with(
                "P1",
                Side::Buy,
                "BTCUSDT.P",
                10,
                Some(Decimal::from(50_000)),
            )),
            1,
        );
        e.mark(&[("BTCUSDT.P".into(), Decimal::from(60_000))], 2);
        let before = *e.account.balances.get("USDT").expect("funded");

        // One instant crossed: 10 contracts at 60,000 is 600,000 of notional,
        // and one basis point of that is 60.
        e.apply_funding(EIGHT_HOURS_NS - 1, EIGHT_HOURS_NS + 1, 3);
        let after = *e.account.balances.get("USDT").expect("funded");
        assert_eq!(
            before - after,
            Decimal::from(60),
            "a long pays rate times marked notional per interval"
        );
    }

    /// A negative rate reverses the flow, which is what a perpetual trading
    /// below spot produces and is not an error.
    #[test]
    fn a_negative_funding_rate_pays_the_long() {
        let mut e = Engine::build(EngineConfig {
            account_id: test_account_id(),
            instruments: vec![perpetual(-Decimal::new(1, 4))],
            balances: HashMap::from([("USDT".to_string(), Decimal::from(100_000))]),
            fill_seed: 0,
        });
        e.set_margin_policy(
            "BTCUSDT.P".into(),
            MarginPolicy {
                initial_per_contract: Decimal::ZERO,
                maintenance_per_contract: Decimal::ZERO,
                breach_action: MarginBreachAction::Refuse,
                basis: MarginBasis::PerContract,
            },
        );
        e.process(
            Command::SubmitOrder(order_with(
                "P1",
                Side::Buy,
                "BTCUSDT.P",
                10,
                Some(Decimal::from(50_000)),
            )),
            1,
        );
        e.mark(&[("BTCUSDT.P".into(), Decimal::from(50_000))], 2);
        let before = *e.account.balances.get("USDT").expect("funded");
        e.apply_funding(EIGHT_HOURS_NS - 1, EIGHT_HOURS_NS + 1, 3);
        assert!(
            *e.account.balances.get("USDT").expect("funded") > before,
            "a negative rate pays the long rather than charging it"
        );
    }

    /// The short side, which is the half the sign convention is easy to get
    /// wrong on and which no test held: both funding tests above hold a long
    /// and vary the direction through the rate alone, so `apply_funding` taking
    /// `qty.abs()` would make a short pay funding too - the transfer inverted -
    /// with nothing red.
    ///
    /// Funding is a transfer, so the short's flow is the long's mirror at the
    /// same marks: the long above pays exactly 60 at a positive rate, and this
    /// short receives exactly 60.
    #[test]
    fn a_short_perpetual_receives_the_funding_a_long_pays() {
        // (rate, mark, what the short's balance must do). Row one is the exact
        // mirror of the long test above - same rate, same mark, same size - so
        // the 60 the long pays is the 60 this short receives. Row two mirrors
        // nothing: no long test runs a negative rate at mark 50,000 with the
        // amount asserted, so it is a value pin on the negative-rate short
        // (10 contracts at 50,000 at one basis point, paid rather than
        // received), not a cross-check against a long.
        for (rate, mark, expected) in [
            (Decimal::new(1, 4), 60_000, Decimal::from(60)),
            (-Decimal::new(1, 4), 50_000, Decimal::from(-50)),
        ] {
            let mut e = Engine::build(EngineConfig {
                account_id: test_account_id(),
                instruments: vec![perpetual(rate)],
                balances: HashMap::from([("USDT".to_string(), Decimal::from(100_000))]),
                fill_seed: 0,
            });
            e.set_margin_policy(
                "BTCUSDT.P".into(),
                MarginPolicy {
                    initial_per_contract: Decimal::ZERO,
                    maintenance_per_contract: Decimal::ZERO,
                    breach_action: MarginBreachAction::Refuse,
                    basis: MarginBasis::PerContract,
                },
            );
            let out = e.process(
                Command::SubmitOrder(order_with(
                    "S1",
                    Side::Sell,
                    "BTCUSDT.P",
                    10,
                    Some(Decimal::from(50_000)),
                )),
                1,
            );
            assert!(
                out.iter()
                    .any(|event| matches!(event, VenueMessage::OrderFilled(_))),
                "the short must actually be on the book, or this funds nothing: {out:?}"
            );
            e.mark(&[("BTCUSDT.P".into(), Decimal::from(mark))], 2);
            let before = *e.account.balances.get("USDT").expect("funded");
            e.apply_funding(EIGHT_HOURS_NS - 1, EIGHT_HOURS_NS + 1, 3);
            let after = *e.account.balances.get("USDT").expect("funded");
            assert_eq!(
                after - before,
                expected,
                "a short at rate {rate} marked at {mark} takes the other side of the long's flow"
            );
        }
    }

    /// A span crossing no instant funds nothing, and a span crossing two funds
    /// twice. The schedule is a property of the clock, so it cannot depend on
    /// how the sweep passes were cut.
    #[test]
    fn funding_instants_are_counted_once_per_interval_crossed() {
        assert_eq!(funding_instants(0, EIGHT_HOURS_NS - 1, EIGHT_HOURS_NS), 0);
        assert_eq!(funding_instants(0, EIGHT_HOURS_NS, EIGHT_HOURS_NS), 1);
        assert_eq!(
            funding_instants(EIGHT_HOURS_NS, 3 * EIGHT_HOURS_NS, EIGHT_HOURS_NS),
            2
        );
        // Abutting spans never double-count: the left edge is exclusive.
        let first = funding_instants(0, EIGHT_HOURS_NS, EIGHT_HOURS_NS);
        let second = funding_instants(EIGHT_HOURS_NS, 2 * EIGHT_HOURS_NS, EIGHT_HOURS_NS);
        assert_eq!(first + second, 2);
    }

    /// A mark above the index makes the long pay more than the configured
    /// interest. That is the whole point of basis-responsive funding: a
    /// perpetual trading rich of spot transfers cash from long to short.
    #[test]
    fn a_rich_mark_raises_the_funding_a_long_pays() {
        let mut e = Engine::build(EngineConfig {
            account_id: test_account_id(),
            instruments: vec![perpetual_against(
                Decimal::ZERO,
                Some("BTCUSDT"),
                Decimal::ONE,
            )],
            balances: HashMap::from([("USDT".to_string(), Decimal::from(100_000))]),
            fill_seed: 0,
        });
        e.set_margin_policy(
            "BTCUSDT.P".into(),
            MarginPolicy {
                initial_per_contract: Decimal::ZERO,
                maintenance_per_contract: Decimal::ZERO,
                breach_action: MarginBreachAction::Refuse,
                basis: MarginBasis::PerContract,
            },
        );
        e.process(
            Command::SubmitOrder(order_with(
                "P1",
                Side::Buy,
                "BTCUSDT.P",
                10,
                Some(Decimal::from(50_000)),
            )),
            1,
        );
        e.mark(
            &[
                ("BTCUSDT.P".into(), Decimal::from(60_600)),
                ("BTCUSDT".into(), Decimal::from(60_000)),
            ],
            2,
        );
        let before = *e.account.balances.get("USDT").expect("funded");
        e.apply_funding(EIGHT_HOURS_NS - 1, EIGHT_HOURS_NS + 1, 3);
        let after = *e.account.balances.get("USDT").expect("funded");
        // 10 contracts at 60,600 is 606,000 of notional; 1 percent premium is
        // 6,060. Interest is zero, so that is the whole payment.
        assert_eq!(before - after, Decimal::from(6_060));
    }

    /// No index mark means the configured interest, even when the class names
    /// an index. A missing tape is not a basis.
    #[test]
    fn a_named_index_without_a_mark_keeps_the_interest() {
        let mut e = Engine::build(EngineConfig {
            account_id: test_account_id(),
            instruments: vec![perpetual_against(
                Decimal::new(1, 4),
                Some("BTCUSDT"),
                Decimal::ZERO,
            )],
            balances: HashMap::from([("USDT".to_string(), Decimal::from(100_000))]),
            fill_seed: 0,
        });
        e.set_margin_policy(
            "BTCUSDT.P".into(),
            MarginPolicy {
                initial_per_contract: Decimal::ZERO,
                maintenance_per_contract: Decimal::ZERO,
                breach_action: MarginBreachAction::Refuse,
                basis: MarginBasis::PerContract,
            },
        );
        e.process(
            Command::SubmitOrder(order_with(
                "P1",
                Side::Buy,
                "BTCUSDT.P",
                10,
                Some(Decimal::from(50_000)),
            )),
            1,
        );
        e.mark(&[("BTCUSDT.P".into(), Decimal::from(60_000))], 2);
        let before = *e.account.balances.get("USDT").expect("funded");
        e.apply_funding(EIGHT_HOURS_NS - 1, EIGHT_HOURS_NS + 1, 3);
        let after = *e.account.balances.get("USDT").expect("funded");
        assert_eq!(
            before - after,
            Decimal::from(60),
            "no index mark: 10 * 60,000 * 0.0001"
        );
    }

    fn rest_limit(engine: &mut Engine, order: SubmitOrder, last_px: i64, ts: u64) {
        let out = engine.process_with_market(
            Command::SubmitOrder(order),
            ts,
            Some(MarketReading {
                last_px: Decimal::from(last_px),
                ts_ns: ts,
                band_ticks: 0,
            }),
        );
        assert!(
            matches!(out.first(), Some(VenueMessage::OrderAccepted { .. })),
            "the limit must rest, got {out:?}"
        );
        assert!(
            !out.iter()
                .any(|event| matches!(event, VenueMessage::OrderFilled(_))),
            "a marketable rest would not pin the working book: {out:?}"
        );
    }

    /// A working buy of ten plus an incoming buy of one is eleven. A projection
    /// that ignored `self.open.orders` would answer one and the integration
    /// test, which submits against a flat book, would not notice.
    #[test]
    fn projected_qty_counts_working_orders() {
        let mut e = funded(1_000_000);
        let mut rest = order_with("W1", Side::Buy, "BTCUSDT", 10, Some(Decimal::from(90)));
        rest.order_type = OrderType::Limit;
        rest_limit(&mut e, rest, 100, 1);
        let incoming = order_with("I", Side::Buy, "BTCUSDT", 1, Some(Decimal::from(100)));
        assert_eq!(e.projected_qty("BTCUSDT", &[incoming]), Decimal::from(11));
    }

    /// A reduce-only leave cannot grow a side, so it is not a sell for the
    /// projection's purposes.
    ///
    /// The leave is oversized on purpose - thirty against a long ten - because
    /// that is the only shape in which the exclusion changes an answer. A
    /// reduce-only within the position only moves the `net - sells` extreme
    /// toward zero, which never wins the max, so a test using ten would pass
    /// against a projection that counted reduce-only orders and would guard
    /// nothing. Resting for more than the position is legal: reduce-only is
    /// clamped at the fill, not at rest.
    #[test]
    fn projected_qty_ignores_reduce_only_working_orders() {
        let mut e = funded(1_000_000);
        e.process(
            Command::SubmitOrder(order_with(
                "P1",
                Side::Buy,
                "BTCUSDT",
                10,
                Some(Decimal::from(100)),
            )),
            1,
        );
        let mut ro = order_with("RO", Side::Sell, "BTCUSDT", 30, Some(Decimal::from(110)));
        ro.order_type = OrderType::Limit;
        ro.reduce_only = true;
        rest_limit(&mut e, ro, 100, 2);
        assert_eq!(
            e.projected_qty("BTCUSDT", &[]),
            Decimal::from(10),
            "counting the leave would project a short twenty the book cannot reach"
        );
    }

    /// Long ten, working sell ten, incoming buy ten: the buy can fill first
    /// and the book is then twenty long. Netting those three to ten is the
    /// signed-sum hole.
    #[test]
    fn projected_qty_does_not_net_an_opposing_working_order() {
        let mut e = funded(1_000_000);
        e.process(
            Command::SubmitOrder(order_with(
                "P1",
                Side::Buy,
                "BTCUSDT",
                10,
                Some(Decimal::from(100)),
            )),
            1,
        );
        let mut sell = order_with("S1", Side::Sell, "BTCUSDT", 10, Some(Decimal::from(110)));
        sell.order_type = OrderType::Limit;
        rest_limit(&mut e, sell, 100, 2);
        assert_eq!(
            e.projected_qty(
                "BTCUSDT",
                &[order_with(
                    "I",
                    Side::Buy,
                    "BTCUSDT",
                    10,
                    Some(Decimal::from(100))
                )],
            ),
            Decimal::from(20)
        );
    }

    /// A netting flip never holds more than the open side, so a sell of
    /// fifteen against a long ten projects ten, not fifteen.
    #[test]
    fn projected_qty_allows_a_netting_flip_inside_the_open_side() {
        let mut e = funded(1_000_000);
        e.process(
            Command::SubmitOrder(order_with(
                "P1",
                Side::Buy,
                "BTCUSDT",
                10,
                Some(Decimal::from(100)),
            )),
            1,
        );
        assert_eq!(
            e.projected_qty(
                "BTCUSDT",
                &[order_with(
                    "I",
                    Side::Sell,
                    "BTCUSDT",
                    15,
                    Some(Decimal::from(100))
                )],
            ),
            Decimal::from(10)
        );
    }

    /// Under hedging the sides coexist, so a long ten and a short ten plus an
    /// incoming buy ten is twenty, not a net of ten.
    #[test]
    fn projected_qty_on_a_hedged_book_is_the_larger_side() {
        let mut e = futures_engine(200_000, MarginBreachAction::Refuse);
        e.set_oms_type(mogwai_protocol::OmsType::Hedging);
        fill_future(&mut e, "L", Side::Buy, 10, 21_000);
        fill_future(&mut e, "S", Side::Sell, 10, 21_000);
        assert_eq!(
            e.projected_qty("MNQ", &[mnq_order("I", Side::Buy, 10, 21_000)]),
            Decimal::from(20)
        );
    }

    #[test]
    fn projected_qty_counts_an_incoming_oco_leg_inside_its_group() {
        let mut e = funded(1_000_000);
        let mut first = order_with("O1", Side::Buy, "BTCUSDT", 10, Some(Decimal::from(90)));
        first.order_type = OrderType::Limit;
        first = linked(
            first,
            link_of(mogwai_protocol::Contingency::Oco, &["O2"], None),
        );
        rest_limit(&mut e, first, 100, 1);

        let mut second = order_with("O2", Side::Buy, "BTCUSDT", 10, Some(Decimal::from(80)));
        second.order_type = OrderType::Limit;
        second = linked(
            second,
            link_of(mogwai_protocol::Contingency::Oco, &["O1"], None),
        );
        assert_eq!(
            e.projected_qty("BTCUSDT", &[second]),
            Decimal::from(10),
            "the incoming sibling belongs inside the group's maximum"
        );
    }

    /// A notional margin basis is what makes a leveraged account expressible:
    /// the requirement moves with the price, where a per-contract one does not.
    #[test]
    fn a_notional_margin_basis_scales_with_price() {
        let def = InstrumentDef {
            symbol: "EURUSD".into(),
            class: InstrumentClass::Future {
                underlying: "EUR".into(),
                settlement_currency: "USD".into(),
                multiplier: Decimal::from(100_000),
                asset_class: WireAssetClass::Fx,
            },
            price_precision: 5,
            size_precision: 0,
            price_increment: Decimal::new(1, 5),
            size_increment: Decimal::ONE,
        };
        // Thirty-to-one leverage, the retail forex ceiling.
        let leveraged = MarginPolicy {
            initial_per_contract: Decimal::new(333, 4),
            maintenance_per_contract: Decimal::new(333, 4),
            breach_action: MarginBreachAction::Liquidate,
            basis: MarginBasis::Notional,
        };
        let cheap = leveraged.initial(&def, Decimal::ONE, Decimal::ONE);
        let dear = leveraged.initial(&def, Decimal::ONE, Decimal::TWO);
        assert_eq!(
            dear,
            cheap * Decimal::TWO,
            "a notional requirement doubles when the price doubles"
        );

        let fixed = MarginPolicy {
            initial_per_contract: Decimal::from(2_000),
            maintenance_per_contract: Decimal::from(1_800),
            breach_action: MarginBreachAction::Liquidate,
            basis: MarginBasis::PerContract,
        };
        assert_eq!(
            fixed.initial(&def, Decimal::ONE, Decimal::ONE),
            fixed.initial(&def, Decimal::ONE, Decimal::from(9_999)),
            "a per-contract requirement ignores the price, which is what CME publishes"
        );
    }

    /// A notional-basis policy states its maintenance as a fraction, and the
    /// breach walk multiplied that fraction by a contract count - reading a 25
    /// percent requirement on 150 shares as 37 dollars and 50 cents, so a
    /// leveraged account could not breach at any price. The walk now asks the
    /// policy, which is what honours the basis.
    #[test]
    fn a_notional_maintenance_requirement_is_measured_against_the_position_value() {
        let mut e = equity_engine(&Shares {
            margin: Some(MarginPolicy {
                initial_per_contract: Decimal::new(5, 1),
                maintenance_per_contract: Decimal::new(25, 2),
                breach_action: MarginBreachAction::Refuse,
                basis: MarginBasis::Notional,
            }),
            ..Shares::default()
        });
        trade(&mut e, share_order("BUY", Side::Buy, 150), 1);
        // Cash is -5,000 against 150 shares. At 50 the stock is worth 7,500, so
        // the account holds 2,500 against a maintenance requirement of 1,875 and
        // is fine; at 40 it holds 1,000 against 1,500 and is not.
        e.mark(&[(Symbol::from("AAPL"), Decimal::from(50))], 2);
        assert!(
            !e.margin_breached.contains(&Symbol::from("AAPL")),
            "an account above its maintenance requirement is not breached"
        );
        e.mark(&[(Symbol::from("AAPL"), Decimal::from(40))], 3);
        assert!(
            e.margin_breached.contains(&Symbol::from("AAPL")),
            "and one below it is"
        );
    }

    /// `RejectNextCancel` refuses a cancel the venue could have honoured, and
    /// the order stays resting.
    ///
    /// The order staying resting is the whole arm: a consumer that published a
    /// replacement before its cancel was acknowledged now has two live orders
    /// where its script rests one, which is a real live-path defect no venue
    /// could previously provoke. A refusal that also removed the order would
    /// model nothing.
    #[test]
    fn a_rejected_cancel_leaves_its_order_resting() {
        let mut e = Engine::new();
        e.arm(Divergence::PartialFillNext {
            client_order_id: "O1".into(),
            fraction: Decimal::from_f64(0.3).unwrap(),
        });
        e.process(Command::SubmitOrder(order("O1", 10)), 1);
        e.arm(Divergence::RejectNextCancel {
            reason: "venue said no".into(),
        });

        let out = e.process(
            Command::CancelOrder {
                client_order_id: "O1".into(),
            },
            2,
        );
        assert_eq!(cancel_reject_reason(&out), "venue said no");
        assert!(
            !out.iter()
                .any(|event| matches!(event, VenueMessage::OrderCanceled { .. })),
            "a refused cancel must not also cancel the order"
        );

        // Still there, and cancellable once the single-shot arm is spent.
        let out = e.process(
            Command::CancelOrder {
                client_order_id: "O1".into(),
            },
            3,
        );
        assert!(
            out.iter()
                .any(|event| matches!(event, VenueMessage::OrderCanceled { .. })),
            "the order should have survived the refused cancel and be cancellable now"
        );
    }

    /// A venue-originated cancel pays no consumer-armed divergence, exactly as a
    /// venue-originated submit does not.
    ///
    /// `liquidate_all` flattens the book through `on_cancel`, and an armed
    /// `RejectNextCancel` used to be spent there: the first order the
    /// liquidation tried to pull came back rejected and stayed resting, so the
    /// liquidation went on to close the positions while leaving a live order
    /// behind - the exact hazard its own "resting orders go first" rule exists
    /// to prevent. `retire_off_river` and `cancel_unreadable_orders` share the
    /// path and the fix.
    #[test]
    fn a_venue_liquidation_neither_spends_nor_suffers_a_cancel_arm() {
        let mut e = Engine::new();
        e.process(Command::SubmitOrder(limit_order("O1", 10)), 1);
        assert_eq!(e.open.len(), 1, "premise: the order is resting");
        e.arm(Divergence::RejectNextCancel {
            reason: "venue said no".into(),
        });

        let out = e.liquidate_all(2);
        assert!(
            !out.events
                .iter()
                .any(|event| matches!(event, VenueMessage::OrderCancelRejected { .. })),
            "the venue's own cancel is not refused by an arm aimed at the consumer: {:?}",
            out.events
        );
        assert!(
            e.open.is_empty(),
            "and the liquidation really did clear the book"
        );

        // The arm is unspent: it is still there for the consumer cancel the
        // scenario author aimed it at.
        e.process(Command::SubmitOrder(limit_order("O2", 10)), 3);
        let out = e.process(
            Command::CancelOrder {
                client_order_id: "O2".into(),
            },
            4,
        );
        assert_eq!(
            cancel_reject_reason(&out),
            "venue said no",
            "the liquidation must not have consumed the arm"
        );
    }

    /// The arm is not spent on a cancel that was going to be refused anyway.
    /// Spending it there would look, to a scenario author, exactly like the arm
    /// failing to fire.
    #[test]
    fn a_rejected_cancel_arm_survives_an_unknown_order() {
        let mut e = Engine::new();
        e.arm(Divergence::RejectNextCancel {
            reason: "venue said no".into(),
        });
        let out = e.process(
            Command::CancelOrder {
                client_order_id: "ghost".into(),
            },
            1,
        );
        assert_eq!(cancel_reject_reason(&out), "unknown order");

        e.arm(Divergence::PartialFillNext {
            client_order_id: "O1".into(),
            fraction: Decimal::from_f64(0.3).unwrap(),
        });
        e.process(Command::SubmitOrder(order("O1", 10)), 2);
        let out = e.process(
            Command::CancelOrder {
                client_order_id: "O1".into(),
            },
            3,
        );
        assert_eq!(
            cancel_reject_reason(&out),
            "venue said no",
            "the arm should still have been waiting for a cancel it could refuse"
        );
    }

    #[test]
    fn cancel_of_already_filled_order_distinguishes_terminal_from_unknown() {
        // A limit on the no-book engine fills immediately on accept, so it is
        // already gone from `open` by the time a cancel for it can arrive - a
        // different situation from an id the venue never accepted at all. The
        // reason must say so rather than reusing "unknown order" for both.
        let mut e = Engine::new();
        e.process(Command::SubmitOrder(order("O1", 10)), 1);

        let out = e.process(
            Command::CancelOrder {
                client_order_id: "O1".into(),
            },
            2,
        );
        assert_eq!(cancel_reject_reason(&out), "order already terminal");

        let out = e.process(
            Command::CancelOrder {
                client_order_id: "ghost".into(),
            },
            3,
        );
        assert_eq!(cancel_reject_reason(&out), "unknown order");
    }

    #[test]
    fn terminal_cancel_reject_carries_original_venue_id() {
        // The wire contract: `venue_order_id` is absent only when the order id
        // is unknown. A terminal id was accepted, so its cancel reject must
        // carry the venue id it was accepted under, while a genuinely unknown
        // id carries none - no venue id was ever assigned to it.
        let mut e = Engine::new();
        let accepted = e.process(Command::SubmitOrder(order("O1", 10)), 1);
        let venue_id = accepted_venue_id(&accepted);

        let out = e.process(
            Command::CancelOrder {
                client_order_id: "O1".into(),
            },
            2,
        );
        assert!(matches!(
            &out[0],
            VenueMessage::OrderCancelRejected {
                venue_order_id: Some(id),
                reason,
                ..
            } if *id == venue_id && reason == "order already terminal"
        ));

        let out = e.process(
            Command::CancelOrder {
                client_order_id: "ghost".into(),
            },
            3,
        );
        assert!(matches!(
            &out[0],
            VenueMessage::OrderCancelRejected {
                venue_order_id: None,
                reason,
                ..
            } if reason == "unknown order"
        ));
    }

    #[test]
    fn modify_of_already_filled_order_distinguishes_terminal_from_unknown() {
        let mut e = Engine::new();
        e.process(Command::SubmitOrder(order("O1", 10)), 1);

        let out = e.process(
            Command::ModifyOrder {
                client_order_id: "O1".into(),
                price: Some(Decimal::from(200)),
                quantity: None,
                trigger_price: None,
            },
            2,
        );
        assert!(matches!(
            &out[0],
            VenueMessage::OrderModifyRejected { reason, .. }
                if reason == "order already terminal"
        ));
    }

    #[test]
    fn terminal_modify_reject_carries_original_venue_id() {
        // Same presence rule as the cancel path: terminal means the venue id
        // is known, so it must go out on the reject; only a genuinely unknown
        // id is bare (see modify_unknown_order_is_rejected_without_venue_id).
        let mut e = Engine::new();
        let accepted = e.process(Command::SubmitOrder(order("O1", 10)), 1);
        let venue_id = accepted_venue_id(&accepted);

        let out = e.process(
            Command::ModifyOrder {
                client_order_id: "O1".into(),
                price: Some(Decimal::from(200)),
                quantity: None,
                trigger_price: None,
            },
            2,
        );
        assert!(matches!(
            &out[0],
            VenueMessage::OrderModifyRejected {
                venue_order_id: Some(id),
                reason,
                ..
            } if *id == venue_id && reason == "order already terminal"
        ));
    }

    #[test]
    fn modify_unknown_order_is_rejected_without_venue_id() {
        let mut e = Engine::new();

        let out = e.process(
            Command::ModifyOrder {
                client_order_id: "ghost".into(),
                price: Some(Decimal::from(200)),
                quantity: None,
                trigger_price: None,
            },
            1,
        );

        assert_eq!(out.len(), 1);
        assert!(matches!(
            &out[0],
            VenueMessage::OrderModifyRejected {
                client_order_id,
                venue_order_id: None,
                reason,
                ts_event: 1,
            } if client_order_id == "ghost" && reason == "unknown order"
        ));
        assert!(e.open_orders().is_empty());
    }

    #[test]
    fn modify_price_reprices_resting_hold() {
        let mut e = Engine::new();
        e.arm(Divergence::PartialFillNext {
            client_order_id: "O1".into(),
            fraction: Decimal::from_f64(0.3).unwrap(),
        });
        e.process_with_market(
            Command::SubmitOrder(limit_order("O1", 10)),
            1,
            Some(reading(0)),
        );

        let out = e.process(
            Command::ModifyOrder {
                client_order_id: "O1".into(),
                price: Some(Decimal::from(200)),
                quantity: None,
                trigger_price: None,
            },
            2,
        );

        assert_eq!(out.len(), 2);
        assert!(matches!(
            updated(&out, 0),
            VenueMessage::OrderUpdated {
                price: Some(price),
                leaves_qty,
                ..
            } if *price == Decimal::from(200) && *leaves_qty == Decimal::from(7)
        ));
        let usdt = balance(account(&out, 1), "USDT");
        assert_eq!(usdt.locked, Decimal::from(1400));
        assert_eq!(usdt.free, Decimal::from(-1700));
    }

    #[test]
    fn a_price_amend_cannot_activate_an_inert_market_remainder() {
        let mut e = Engine::new();
        e.arm(Divergence::PartialFillNext {
            client_order_id: "MARKET-REST".into(),
            fraction: Decimal::from_f64(0.3).unwrap(),
        });
        e.process(Command::SubmitOrder(order("MARKET-REST", 10)), 1);
        assert!(matches!(e.open[0].resting, Resting::Inert));

        let out = e.process(
            Command::ModifyOrder {
                client_order_id: "MARKET-REST".into(),
                price: Some(Decimal::from(200)),
                quantity: None,
                trigger_price: None,
            },
            2,
        );
        assert!(matches!(
            &out[0],
            VenueMessage::OrderModifyRejected { reason, .. }
                if reason == "Market order must not carry a price amend"
        ));
        assert!(matches!(e.open[0].resting, Resting::Inert));
        assert!(e.pending_scans().is_empty());
    }

    #[test]
    fn modify_quantity_grows_leaves_and_relocks() {
        let mut e = Engine::new();
        e.arm(Divergence::PartialFillNext {
            client_order_id: "O1".into(),
            fraction: Decimal::from_f64(0.3).unwrap(),
        });
        e.process(Command::SubmitOrder(order("O1", 10)), 1);

        let out = e.process(
            Command::ModifyOrder {
                client_order_id: "O1".into(),
                price: None,
                quantity: Some(Decimal::from(20)),
                trigger_price: None,
            },
            2,
        );

        assert_eq!(out.len(), 2);
        assert!(matches!(
            updated(&out, 0),
            VenueMessage::OrderUpdated {
                quantity,
                leaves_qty,
                ..
            } if *quantity == Decimal::from(20) && *leaves_qty == Decimal::from(17)
        ));
        let usdt = balance(account(&out, 1), "USDT");
        assert_eq!(usdt.locked, Decimal::from(1700));
    }

    #[test]
    fn modify_quantity_shrinks_to_remaining_filled_is_rejected() {
        let mut e = Engine::new();
        e.arm(Divergence::PartialFillNext {
            client_order_id: "O1".into(),
            fraction: Decimal::from_f64(0.3).unwrap(),
        });
        e.process(Command::SubmitOrder(order("O1", 10)), 1);

        // Both the equality case (new total == 3 filled, zero would remain)
        // and the strictly-below case must reject, and the reason must say
        // "at or below" - the guard is `<=`, so a message claiming only
        // "below" would misdescribe the equality rejection.
        for new_total in [Decimal::from(3), Decimal::from(2)] {
            let out = e.process(
                Command::ModifyOrder {
                    client_order_id: "O1".into(),
                    price: None,
                    quantity: Some(new_total),
                    trigger_price: None,
                },
                2,
            );

            assert_eq!(out.len(), 1);
            assert!(matches!(
                &out[0],
                VenueMessage::OrderModifyRejected {
                    venue_order_id: Some(_),
                    reason,
                    ..
                } if reason == "modify to at or below already-filled quantity"
            ));
            assert_eq!(e.open_orders()[0].submit.quantity, Decimal::from(10));
            assert_eq!(e.open_orders()[0].leaves_qty, Decimal::from(7));
        }
    }

    #[test]
    fn modify_to_zero_quantity_is_rejected() {
        let mut e = Engine::new();
        e.arm(Divergence::PartialFillNext {
            client_order_id: "O1".into(),
            fraction: Decimal::from_f64(0.3).unwrap(),
        });
        e.process(Command::SubmitOrder(order("O1", 10)), 1);

        let out = e.process(
            Command::ModifyOrder {
                client_order_id: "O1".into(),
                price: None,
                quantity: Some(Decimal::ZERO),
                trigger_price: None,
            },
            2,
        );

        assert_eq!(out.len(), 1);
        assert!(matches!(
            &out[0],
            VenueMessage::OrderModifyRejected {
                venue_order_id: Some(_),
                reason,
                ..
            } if reason == "modify to non-positive quantity"
        ));
        assert_eq!(e.open_orders()[0].submit.quantity, Decimal::from(10));
        assert_eq!(e.open_orders()[0].leaves_qty, Decimal::from(7));
    }

    #[test]
    fn modify_to_non_positive_price_is_rejected() {
        let mut e = Engine::new();
        e.arm(Divergence::PartialFillNext {
            client_order_id: "O1".into(),
            fraction: Decimal::from_f64(0.3).unwrap(),
        });
        e.process_with_market(
            Command::SubmitOrder(limit_order("O1", 10)),
            1,
            Some(reading(0)),
        );

        let out = e.process(
            Command::ModifyOrder {
                client_order_id: "O1".into(),
                price: Some(Decimal::ZERO),
                quantity: None,
                trigger_price: None,
            },
            2,
        );

        assert_eq!(out.len(), 1);
        assert!(matches!(
            &out[0],
            VenueMessage::OrderModifyRejected {
                venue_order_id: Some(_),
                reason,
                ..
            } if reason == "modify to non-positive price"
        ));
        assert_eq!(e.open_orders()[0].submit.price, Some(Decimal::from(100)));
        assert_eq!(e.open_orders()[0].leaves_qty, Decimal::from(7));
    }

    #[test]
    fn modify_with_no_price_or_quantity_is_rejected() {
        let mut e = Engine::new();
        e.arm(Divergence::PartialFillNext {
            client_order_id: "O1".into(),
            fraction: Decimal::from_f64(0.3).unwrap(),
        });
        e.process(Command::SubmitOrder(order("O1", 10)), 1);

        let out = e.process(
            Command::ModifyOrder {
                client_order_id: "O1".into(),
                price: None,
                quantity: None,
                trigger_price: None,
            },
            2,
        );

        assert_eq!(out.len(), 1);
        assert!(matches!(
            &out[0],
            VenueMessage::OrderModifyRejected {
                venue_order_id: Some(_),
                reason,
                ..
            } if reason == "empty modify (no price or quantity)"
        ));
        assert_eq!(e.open_orders()[0].submit.quantity, Decimal::from(10));
        assert_eq!(e.open_orders()[0].leaves_qty, Decimal::from(7));
    }

    #[test]
    fn modify_does_not_consume_armed_drop() {
        let mut e = Engine::new();
        e.arm(Divergence::PartialFillNext {
            client_order_id: "O1".into(),
            fraction: Decimal::from_f64(0.3).unwrap(),
        });
        e.process_with_market(
            Command::SubmitOrder(limit_order("O1", 10)),
            1,
            Some(reading(0)),
        );

        e.arm(Divergence::DropNextAccountUpdate);
        let modified = e.process(
            Command::ModifyOrder {
                client_order_id: "O1".into(),
                price: Some(Decimal::from(200)),
                quantity: None,
                trigger_price: None,
            },
            2,
        );
        assert_eq!(modified.len(), 2);
        assert!(matches!(modified[0], VenueMessage::OrderUpdated { .. }));
        assert!(matches!(modified[1], VenueMessage::AccountState(_)));

        let filled = e.process(Command::SubmitOrder(order("O2", 10)), 3);
        assert_eq!(filled.len(), 2);
        assert!(matches!(filled[0], VenueMessage::OrderAccepted { .. }));
        assert!(matches!(filled[1], VenueMessage::OrderFilled(_)));
    }

    #[test]
    fn missing_instrument_rejects_without_booking_position() {
        let mut e = Engine::new();
        let order = order_with("O1", Side::Buy, "ETHUSDT", 10, Some(Decimal::from(100)));
        let out = e.process(Command::SubmitOrder(order), 1);

        assert_eq!(reject_reason(&out), "unknown instrument");
        assert!(e.account_snapshot(2).balances.is_empty());
        assert!(e.positions().is_empty());
        assert!(e.open_orders().is_empty());
    }

    #[test]
    fn query_orders_reports_truth_for_open_terminal_and_unknown() {
        let mut e = Engine::new();
        // O1 rests partially filled, O2 fills fully, O3 rests then cancels.
        e.arm(Divergence::PartialFillNext {
            client_order_id: "O1".into(),
            fraction: Decimal::from_f64(0.3).unwrap(),
        });
        e.process(Command::SubmitOrder(order("O1", 10)), 1);
        e.process(Command::SubmitOrder(order("O2", 5)), 2);
        e.arm(Divergence::PartialFillNext {
            client_order_id: "O3".into(),
            fraction: Decimal::from_f64(0.5).unwrap(),
        });
        e.process(Command::SubmitOrder(order("O3", 4)), 3);
        e.process(
            Command::CancelOrder {
                client_order_id: "O3".into(),
            },
            4,
        );

        let out = e.process(
            Command::QueryOrders {
                request_id: "Q1".into(),
                client_order_id: None,
                open_only: false,
            },
            9,
        );
        let [VenueMessage::OrderStatusSnapshot(snap)] = &out[..] else {
            panic!("expected one snapshot, got {out:?}")
        };
        assert_eq!(snap.request_id, "Q1");
        assert_eq!(snap.ts_event, 9);
        assert_eq!(snap.orders.len(), 3, "open and terminal orders all report");
        // Sorted by ts_accepted, so O1, O2, O3.
        let o1 = &snap.orders[0];
        assert_eq!(o1.client_order_id, "O1");
        assert_eq!(o1.status, mogwai_protocol::WireOrderStatus::PartiallyFilled);
        assert_eq!(o1.quantity, Decimal::from(10));
        assert_eq!(o1.filled_qty, Decimal::from(3));
        assert_eq!(o1.ts_accepted, 1);
        let o2 = &snap.orders[1];
        assert_eq!(o2.status, mogwai_protocol::WireOrderStatus::Filled);
        assert_eq!(o2.filled_qty, Decimal::from(5));
        let o3 = &snap.orders[2];
        assert_eq!(o3.status, mogwai_protocol::WireOrderStatus::Canceled);
        assert_eq!(o3.filled_qty, Decimal::from(2));
        assert_eq!(o3.ts_last, 4, "terminal record freezes at the cancel");

        // open_only hides the terminal records.
        let snap = e.order_status_snapshot("Q2".into(), None, true, 10);
        assert_eq!(snap.orders.len(), 1);
        assert_eq!(snap.orders[0].client_order_id, "O1");

        // A targeted query ignores open_only: asking about one specific order
        // deserves its terminal state, not an empty reply that would misread
        // as "unknown order".
        let snap = e.order_status_snapshot("Q3".into(), Some("O2"), true, 11);
        assert_eq!(snap.orders.len(), 1);
        assert_eq!(
            snap.orders[0].status,
            mogwai_protocol::WireOrderStatus::Filled
        );

        // An id the venue never accepted is truthfully absent.
        let snap = e.order_status_snapshot("Q4".into(), Some("GHOST"), false, 12);
        assert!(snap.orders.is_empty());
    }

    #[test]
    fn query_orders_reflects_amends_in_quantity_and_ts_last() {
        let mut e = Engine::new();
        e.arm(Divergence::PartialFillNext {
            client_order_id: "O1".into(),
            fraction: Decimal::from_f64(0.3).unwrap(),
        });
        e.process_with_market(
            Command::SubmitOrder(limit_order("O1", 10)),
            1,
            Some(reading(0)),
        );
        e.process(
            Command::ModifyOrder {
                client_order_id: "O1".into(),
                price: Some(Decimal::from(200)),
                quantity: Some(Decimal::from(20)),
                trigger_price: None,
            },
            5,
        );

        let snap = e.order_status_snapshot("Q".into(), Some("O1"), false, 6);
        let row = &snap.orders[0];
        assert_eq!(row.quantity, Decimal::from(20));
        assert_eq!(row.price, Some(Decimal::from(200)));
        assert_eq!(row.filled_qty, Decimal::from(3));
        assert_eq!(row.ts_accepted, 1, "the accept stamp survives the amend");
        assert_eq!(row.ts_last, 5, "the amend advances ts_last");
    }

    #[test]
    fn query_fills_books_each_fill_once_despite_duplicate_wire_events() {
        let mut e = Engine::new();
        e.arm(Divergence::DuplicateNextFill);
        let out = e.process(Command::SubmitOrder(order("O1", 10)), 1);
        // The wire carried the fill twice (the injected lie)...
        assert_eq!(
            out.iter()
                .filter(|m| matches!(m, VenueMessage::OrderFilled(_)))
                .count(),
            2
        );
        e.process(Command::SubmitOrder(order("O2", 5)), 2);

        // ...but the truth store booked it once.
        let out = e.process(
            Command::QueryFills {
                request_id: "Q1".into(),
                client_order_id: None,
            },
            9,
        );
        let [VenueMessage::FillSnapshot(snap)] = &out[..] else {
            panic!("expected one fill snapshot, got {out:?}")
        };
        assert_eq!(snap.request_id, "Q1");
        assert_eq!(snap.fills.len(), 2, "one booked fill per order, no dupes");
        assert_eq!(snap.fills[0].client_order_id, "O1");
        assert_eq!(snap.fills[1].client_order_id, "O2");

        let snap = e.fill_snapshot("Q2".into(), Some("O2"), 10);
        assert_eq!(snap.fills.len(), 1);
        assert_eq!(snap.fills[0].client_order_id, "O2");
    }

    #[test]
    fn silent_cancel_removes_the_order_wordlessly_and_query_tells_the_truth() {
        // The poll-heal fault: the venue cancels a resting order out-of-band
        // and no lifecycle event is emitted - the consumer's belief and the
        // venue's book now disagree, and only a QueryOrders reply tells the
        // truth.
        let mut e = funded(1_000);
        e.arm(Divergence::PartialFillNext {
            client_order_id: "R1".into(),
            fraction: Decimal::new(5, 1),
        });
        e.process(Command::SubmitOrder(order("R1", 4)), 1);
        assert_eq!(e.open_orders().len(), 1);

        e.cancel_open_order_silently("R1", 7)
            .expect("resting order cancels");

        assert!(e.open_orders().is_empty(), "the book no longer holds R1");
        // The hold is freed: only the filled half's spend remains.
        let state = e.account_snapshot(8);
        let usdt = balance(&state, "USDT");
        assert_eq!(usdt.locked, Decimal::ZERO);
        assert_eq!(usdt.total, Decimal::from(800));

        let snap = e.order_status_snapshot("Q".into(), Some("R1"), false, 9);
        let row = &snap.orders[0];
        assert_eq!(row.status, mogwai_protocol::WireOrderStatus::Canceled);
        assert_eq!(row.filled_qty, Decimal::from(2));
        assert_eq!(
            row.ts_last, 7,
            "the terminal record stamps the cancel instant"
        );

        // Misses are refused loudly: already terminal, and never accepted.
        assert_eq!(
            e.cancel_open_order_silently("R1", 10),
            Err("order already terminal".to_string())
        );
        assert_eq!(
            e.cancel_open_order_silently("GHOST", 11),
            Err("unknown order".to_string())
        );
    }

    #[test]
    fn ioc_partial_remainder_records_terminal_cancel_in_the_truth_store() {
        let mut e = Engine::new();
        e.arm(Divergence::PartialFillNext {
            client_order_id: "O1".into(),
            fraction: Decimal::from_f64(0.4).unwrap(),
        });
        let mut order = order("O1", 10);
        order.time_in_force = TimeInForce::Ioc;
        e.process(Command::SubmitOrder(order), 1);

        let snap = e.order_status_snapshot("Q".into(), Some("O1"), false, 2);
        let row = &snap.orders[0];
        assert_eq!(row.status, mogwai_protocol::WireOrderStatus::Canceled);
        assert_eq!(row.filled_qty, Decimal::from(4));
    }

    #[test]
    fn market_order_without_price_rejects_without_zero_price_fill() {
        let mut e = Engine::new();
        e.arm(Divergence::PartialFillNext {
            client_order_id: "O1".into(),
            fraction: Decimal::from_f64(0.5).unwrap(),
        });
        let order = order_with("O1", Side::Buy, "BTCUSDT", 10, None);
        let out = e.process(Command::SubmitOrder(order), 1);

        assert_eq!(reject_reason(&out), "submit price required");
        assert!(e.account_snapshot(2).balances.is_empty());
        assert!(e.positions().is_empty());
        assert!(e.open_orders().is_empty());
    }

    #[test]
    fn worst_case_byte_budget_covers_actual_output() {
        // The sizing model's claim - `worst_case_output_bytes` always dominates what
        // `process` really produces - checked against a matrix of books crossed
        // with every command class, including the divergence-armed worst cases
        // and adversarially escaped identifiers. A finite matrix samples the
        // bound; the per-constant derivations in `mogwai_protocol::sizing`
        // argue it. Ids and symbols are filled with `\u{0001}`, which serde
        // escapes to six bytes each: an ASCII fixture would pass a bound six
        // times too small.
        //
        // This test is deliberately one-sided, and that is a ruling rather than
        // an omission. An `actual <= bound` assertion is also satisfied by a
        // derivation that over-reserves wildly, and over-budgeting is not
        // free - so every bound `worst_case_output_bytes` is built from carries
        // a two-sided bracket (`bound < 2 * actual` against its maximal
        // fixture) in `mogwai_protocol::sizing`'s own tests. A ceiling here
        // would be a different and much weaker claim, because this test feeds
        // one particular command to one particular book while the bound covers
        // the widest output that command class can produce. Measured over this
        // matrix the ratio runs from 2.2x (a query against the deep book, where
        // the row terms really do carry the bound) to 249x (a cancel refused on
        // the deep book: one 152-byte frame against a bound that must also
        // cover a full linkage reap and a widened account snapshot). Any
        // per-case ceiling over that spread is a table of magic numbers that
        // reads as a gate and is not one.
        let esc_id = "\u{0001}".repeat(mogwai_protocol::MAX_ECHOED_ID_LEN);

        // Book 1: an empty venue, and a submit that fills - the first fill in a
        // fresh pair, which introduces two balance rows and one position the
        // pre-command shape never had.
        let max_account_id = AccountId::parse(&"Z".repeat(mogwai_protocol::MAX_ACCOUNT_ID_LEN))
            .expect("max length account id");
        let mut fresh = Engine::build(EngineConfig {
            account_id: max_account_id.clone(),
            instruments: default_instruments(),
            balances: HashMap::new(),
            fill_seed: 0,
        });

        // Book 2: deep - hundreds of open and closed orders and a long fill
        // history, so the per-row snapshot terms actually carry the bound.
        let mut deep = Engine::build(EngineConfig {
            account_id: max_account_id,
            instruments: default_instruments(),
            balances: HashMap::new(),
            fill_seed: 0,
        });
        for i in 0..200 {
            // A far-from-market limit rests; a marketable one fills and closes.
            deep.process(
                Command::SubmitOrder(order_with(
                    &format!("resting-{i}"),
                    Side::Buy,
                    "BTCUSDT",
                    1,
                    Some(Decimal::from(1)),
                )),
                i,
            );
            deep.process(
                Command::SubmitOrder(order_with(
                    &format!("filled-{i}"),
                    Side::Buy,
                    "BTCUSDT",
                    1,
                    Some(Decimal::from(1_000_000)),
                )),
                i,
            );
        }
        let deep_shape = deep.book_shape();
        assert!(
            deep_shape.open_orders + deep_shape.closed_orders >= 200,
            "the deep book must actually be deep: {deep_shape:?}"
        );

        // An armed reject at the full post-truncation reason length: the engine
        // echoes it verbatim into `OrderRejected.reason`, which is exactly the
        // term `ORDER_EVENT_MAX_BYTES` charges `MAX_REASON_LEN` for.
        let mut armed = Engine::new();
        armed.arm(Divergence::RejectNextSubmit {
            reason: mogwai_protocol::truncate_reason(
                "\u{0001}".repeat(mogwai_protocol::MAX_REASON_LEN * 4),
            ),
        });

        // The widest submit the engine can answer: a duplicated fill plus a
        // partial plus an IOC remainder cancel plus the account state.
        let mut widest = Engine::new();
        widest.arm(Divergence::DuplicateNextFill);
        widest.arm(Divergence::PartialFillNext {
            client_order_id: esc_id.clone(),
            fraction: Decimal::new(5, 1),
        });

        let mut ioc = order_with(&esc_id, Side::Buy, "BTCUSDT", 10, Some(Decimal::from(100)));
        ioc.time_in_force = TimeInForce::Ioc;
        let query_orders = Command::QueryOrders {
            request_id: esc_id.clone(),
            client_order_id: None,
            open_only: false,
        };
        let query_fills = Command::QueryFills {
            request_id: esc_id.clone(),
            client_order_id: None,
        };

        // A `MAX_GROUP_ORDERS`-sized group of marketable, mutually-linked legs
        // with adversarially escaped ids: the widest `SubmitOrderGroup` the
        // wire admits. `Ouo` rather than `Oco` so the linkage really runs on
        // each fill rather than reaping the rest of the group at the first one.
        let mut group_book = Engine::new();
        let group_ids: Vec<String> = (0..mogwai_protocol::MAX_GROUP_ORDERS)
            .map(|i| format!("{esc_id}{i}"))
            .collect();
        let group_members: Vec<SubmitOrder> = group_ids
            .iter()
            .map(|id| {
                let mut leg =
                    order_with(id, Side::Buy, "BTCUSDT", 1, Some(Decimal::from(1_000_000)));
                leg.link = Some(mogwai_protocol::OrderLink {
                    order_list_id: esc_id.clone(),
                    contingency: mogwai_protocol::Contingency::Ouo,
                    linked_order_ids: group_ids
                        .iter()
                        .filter(|other| *other != id)
                        .cloned()
                        .collect(),
                    parent_order_id: None,
                });
                leg
            })
            .collect();

        let cases: Vec<(&str, &mut Engine, Vec<Command>)> = vec![
            (
                "fresh book, first fill in a new pair",
                &mut fresh,
                vec![
                    Command::SubmitOrder(order_with(
                        &esc_id,
                        Side::Buy,
                        "BTCUSDT",
                        10,
                        Some(Decimal::from(1_000_000)),
                    )),
                    Command::CancelOrder {
                        client_order_id: esc_id.clone(),
                    },
                    Command::ModifyOrder {
                        client_order_id: esc_id.clone(),
                        price: Some(Decimal::from(101)),
                        quantity: None,
                        trigger_price: None,
                    },
                    query_orders.clone(),
                    query_fills.clone(),
                ],
            ),
            (
                "deep book",
                &mut deep,
                vec![
                    query_orders.clone(),
                    query_fills.clone(),
                    Command::CancelOrder {
                        client_order_id: "resting-7".into(),
                    },
                    Command::SubmitOrder(order_with(
                        &esc_id,
                        Side::Sell,
                        "BTCUSDT",
                        1,
                        Some(Decimal::from(1)),
                    )),
                ],
            ),
            (
                "armed RejectNextSubmit at MAX_REASON_LEN",
                &mut armed,
                vec![Command::SubmitOrder(order_with(
                    &esc_id,
                    Side::Buy,
                    "BTCUSDT",
                    1,
                    Some(Decimal::from(100)),
                ))],
            ),
            (
                "duplicate + partial + IOC remainder",
                &mut widest,
                vec![Command::SubmitOrder(ioc), query_fills],
            ),
            // A group is the one command whose output scales with the command
            // rather than with the book, so its bound is the one most easily
            // written a factor too small - and a single-submit-sized
            // byte budget would be under by the group's size. Marketable legs,
            // so every member really fills and really snapshots.
            (
                "a full-size marketable group on a fresh book",
                &mut group_book,
                vec![Command::SubmitOrderGroup {
                    orders: group_members,
                }],
            ),
        ];

        // A futures book carries margin rows the spot cases cannot produce, and
        // `book_shape().margins` is what reserves them. Two hedged positions in
        // one symbol are the case a per-position margin row under-reserves.
        let mut hedged = futures_engine(200_000, MarginBreachAction::Refuse);
        hedged.set_oms_type(mogwai_protocol::OmsType::Hedging);
        for (index, side) in [(1, Side::Buy), (2, Side::Sell)] {
            let mut leg = mnq_order(&format!("HEDGE-{index}"), side, 1, 21_000);
            leg.position_id = Some(format!("LEG-{index}"));
            hedged.process_with_market(
                Command::SubmitOrder(leg),
                1,
                Some(MarketReading {
                    last_px: Decimal::from(21_000),
                    ts_ns: 1,
                    band_ticks: 0,
                }),
            );
        }
        assert_eq!(
            hedged.account.positions.len(),
            2,
            "the hedged fixture must actually carry two positions in one symbol"
        );

        let futures_cases: Vec<(&str, &mut Engine, Vec<Command>)> = vec![(
            "hedged futures book with margin rows",
            &mut hedged,
            vec![
                Command::SubmitOrder(mnq_order(&esc_id, Side::Buy, 1, 21_000)),
                query_orders.clone(),
                Command::CancelOrder {
                    client_order_id: esc_id.clone(),
                },
            ],
        )];

        for (label, engine, commands) in cases.into_iter().chain(futures_cases) {
            for (ts, cmd) in commands.into_iter().enumerate() {
                // The shape is read exactly where the real caller reads it -
                // immediately before processing, under the same lock - so it
                // cannot drift between the byte budget and the production.
                let shape = engine.book_shape();
                let bound = mogwai_protocol::sizing::worst_case_output_bytes(&cmd, &shape);
                let output = engine.process(cmd, ts as u64);
                let actual: usize = output
                    .iter()
                    .map(|event| {
                        serde_json::to_vec(event)
                            .expect("engine event serializes")
                            .len()
                    })
                    .sum();
                assert!(
                    actual <= bound,
                    "{label}: produced {actual} bytes against a {bound} byte budget"
                );
            }
        }
    }

    /// The sweep-side half of the byte-budget claim, which `process`-shaped
    /// cases cannot reach: a liquidation cascade emits order frames NO consumer
    /// order paid for, so `emitted` alone under-reserves it and `originated`
    /// is what covers the gap.
    #[test]
    fn worst_case_byte_budget_covers_a_liquidation_cascade() {
        let mut engine = futures_engine(3_000, MarginBreachAction::Liquidate);
        engine.set_oms_type(mogwai_protocol::OmsType::Hedging);
        for index in 1..=2 {
            let mut leg = mnq_order(&format!("LONG-{index}"), Side::Buy, 1, 21_000);
            leg.position_id = Some(format!("LEG-{index}"));
            engine.process_with_market(
                Command::SubmitOrder(leg),
                1,
                Some(MarketReading {
                    last_px: Decimal::from(21_000),
                    ts_ns: 1,
                    band_ticks: 0,
                }),
            );
        }
        let shape = engine.book_shape();
        let outcome = engine.mark(&[("MNQ".into(), Decimal::from(19_000))], 2);
        assert!(
            outcome.originated_orders > 0,
            "the fixture must actually liquidate: {outcome:?}",
            outcome = outcome.events
        );
        let bound =
            mogwai_protocol::sizing::swept_fill_max_bytes(&shape, outcome.originated_orders);
        let actual: usize = outcome
            .events
            .iter()
            .map(|event| serde_json::to_vec(event).expect("event serializes").len())
            .sum();
        assert!(
            actual <= bound,
            "a cascade produced {actual} bytes against a {bound} byte budget"
        );
    }

    #[test]
    fn worst_case_byte_budget_covers_an_arrival_triggered_conditional() {
        // The widest conditional arrival, which the matrix above cannot express
        // because it drives `process` with no market reading and a stop needs
        // one to fire on arrival: accepted, triggered, the duplicated fill, the
        // fill, and the cancel that closes the remainder the reduce-only cap
        // clamped. Five order events - one more than the IOC limit shape, and
        // the reason the submit multiplier is five rather than four.
        let esc_id = "\u{0001}".repeat(mogwai_protocol::MAX_ECHOED_ID_LEN);
        let mut e = Engine::build(EngineConfig {
            account_id: AccountId::parse(&"Z".repeat(mogwai_protocol::MAX_ACCOUNT_ID_LEN))
                .expect("max length account id"),
            instruments: default_instruments(),
            balances: HashMap::new(),
            fill_seed: 0,
        });
        // The position the protective leg reduces, one lot against a ten-lot
        // stop, so the cap clamps the fill and cancels the remainder.
        e.process(
            Command::SubmitOrder(order_with(
                "seed",
                Side::Buy,
                "BTCUSDT",
                1,
                Some(Decimal::from(100)),
            )),
            1,
        );
        e.arm(Divergence::DuplicateNextFill);
        let mut stop = order_with(&esc_id, Side::Sell, "BTCUSDT", 10, None);
        stop.order_type = OrderType::StopMarket;
        stop.trigger_price = Some(Decimal::from(1_000_000));
        stop.reduce_only = true;

        let cmd = Command::SubmitOrder(stop);
        let shape = e.book_shape();
        let bound = mogwai_protocol::sizing::worst_case_output_bytes(&cmd, &shape);
        let output = e.process_with_market(cmd, 2, Some(reading(50)));
        assert_eq!(
            output
                .iter()
                .filter(|event| !matches!(event, VenueMessage::AccountState(_)))
                .count(),
            5,
            "the shape this bound is derived from: {output:?}"
        );
        let actual: usize = output
            .iter()
            .map(|event| serde_json::to_vec(event).expect("serializes").len())
            .sum();
        assert!(
            actual <= bound,
            "produced {actual} bytes against a {bound} byte budget"
        );
    }

    #[test]
    fn swept_fill_byte_budget_covers_a_multi_pair_sweep_batch() {
        // Three distinct pairs, none previously held: a single-pair batch cannot
        // distinguish per-batch from per-order account widening and would pass
        // against an under-reserving bound. Every fill duplicated and every id
        // at max length in `\u{0001}`, which serde escapes to six bytes each.
        let esc =
            |n: usize| "\u{0001}".repeat(mogwai_protocol::MAX_ECHOED_ID_LEN - 1) + &n.to_string();
        let pairs = [
            ("AAABBB", "AAA", "BBB"),
            ("CCCDDD", "CCC", "DDD"),
            ("EEEFFF", "EEE", "FFF"),
        ];
        let instruments: Vec<_> = pairs
            .iter()
            .map(|(symbol, base, quote)| InstrumentDef {
                symbol: (*symbol).into(),
                class: mogwai_protocol::InstrumentClass::Spot {
                    base: (*base).into(),
                    quote: (*quote).into(),
                },
                price_precision: 2,
                size_precision: 8,
                price_increment: Decimal::new(1, 2),
                size_increment: Decimal::new(1, 8),
            })
            .collect();
        let mut e = Engine::build(EngineConfig {
            account_id: AccountId::parse(&"Z".repeat(mogwai_protocol::MAX_ACCOUNT_ID_LEN))
                .expect("max length account id"),
            instruments,
            balances: HashMap::new(),
            fill_seed: 1,
        });
        for (index, (symbol, _, _)) in pairs.iter().enumerate() {
            let mut resting = order_with(
                &esc(index),
                Side::Buy,
                symbol,
                10,
                Some(Decimal::from(1_000_000)),
            );
            resting.order_type = OrderType::Limit;
            e.process(Command::SubmitOrder(resting), index as u64);
            e.arm(Divergence::DuplicateNextFill);
        }
        let scans = e.pending_scans();
        assert_eq!(scans.len(), 3);
        let results: Vec<_> = scans.iter().map(|scan| result(scan, true, 100)).collect();
        // Read exactly where the sweeper reads it: before `apply_scans`.
        let shape = e.book_shape();
        let (events, emitted) = e.apply_scans(&results, 100);
        assert_eq!(emitted, 3);
        let bound = mogwai_protocol::sizing::swept_fill_max_bytes(&shape, emitted);
        let actual: usize = events
            .iter()
            .map(|event| serde_json::to_vec(event).expect("event serializes").len())
            .sum();
        assert!(
            actual <= bound,
            "a three-pair sweep produced {actual} bytes against a {bound} byte budget"
        );
    }

    #[test]
    fn swept_byte_budget_covers_a_trigger_that_fills_duplicates_and_cancels() {
        // The widest swept shape for one order: `OrderTriggered`, the
        // duplicated fill, the fill, and the cancel that closes the reduce-only
        // remainder the position cap clamped. Four order events, which is why
        // the per-order multiplier is four rather than three.
        let mut e = banded(31);
        e.process(Command::SubmitOrder(order("seed", 1)), 1);
        let mut stop = stop_order("wide", Side::Sell, OrderType::StopMarket, 90, None);
        stop.quantity = Decimal::from(10);
        stop.reduce_only = true;
        e.process(Command::SubmitOrder(stop), 2);
        e.arm(Divergence::DuplicateNextFill);

        let scan = e.pending_scans().remove(0);
        let shape = e.book_shape();
        let (events, emitted) = e.apply_scans(&[result(&scan, true, 100)], 100);
        assert_eq!(emitted, 1);
        assert_eq!(
            events
                .iter()
                .filter(|event| !matches!(event, VenueMessage::AccountState(_)))
                .count(),
            4,
            "the shape this bound is derived from: {events:?}"
        );
        let bound = mogwai_protocol::sizing::swept_fill_max_bytes(&shape, emitted);
        let actual: usize = events
            .iter()
            .map(|event| serde_json::to_vec(event).expect("event serializes").len())
            .sum();
        assert!(
            actual <= bound,
            "produced {actual} bytes against a {bound} byte budget"
        );
    }

    #[test]
    fn a_trigger_only_sweep_pass_reserves_its_own_frame() {
        // The sharpest hole the reviews found: a pass in which a stop-limit
        // triggers and rests books no fill, so a fill-keyed `emitted` would be
        // zero and the `OrderTriggered` frame would be written against a
        // zero-order byte budget.
        let mut e = banded(32);
        e.process(
            Command::SubmitOrder(stop_order(
                "trigger-only",
                Side::Sell,
                OrderType::StopLimit,
                100,
                Some(99),
            )),
            10,
        );
        let scan = e.pending_scans().remove(0);
        let shape = e.book_shape();
        let (events, emitted) = e.apply_scans(
            &[ScanResult {
                client_order_id: scan.client_order_id,
                from_ns: scan.from_ns,
                revision: scan.revision,
                hit: Some(Hit {
                    ts_ns: 11,
                    px: Decimal::from(95),
                }),
                scanned_to_ns: 11,
            }],
            11,
        );
        assert_eq!(emitted, 1, "an order that emitted anything is counted");
        let bound = mogwai_protocol::sizing::swept_fill_max_bytes(&shape, emitted);
        let actual: usize = events
            .iter()
            .map(|event| serde_json::to_vec(event).expect("event serializes").len())
            .sum();
        assert!(actual <= bound);
    }

    #[test]
    fn a_limit_rests_until_the_tape_reaches_its_drawn_trigger() {
        let mut e = banded(7);
        e.process(Command::SubmitOrder(limit_order("trigger-rest", 1)), 10);
        let scan = e.pending_scans().remove(0);
        let (quiet, emitted) = e.apply_scans(&[result(&scan, false, 20)], 20);
        assert!(quiet.is_empty());
        assert_eq!(emitted, 0);
        let scan = e.pending_scans().remove(0);
        let (filled, emitted) = e.apply_scans(&[result(&scan, true, 30)], 30);
        assert_eq!(emitted, 1);
        assert!(
            filled
                .iter()
                .any(|event| matches!(event, VenueMessage::OrderFilled(_)))
        );
    }

    #[test]
    fn the_trigger_is_a_pure_function_of_seed_and_order_identity() {
        let reading = MarketReading {
            last_px: Decimal::from(100),
            ts_ns: 0,
            band_ticks: 50,
        };
        let mut a = banded(42);
        let mut b = banded(42);
        a.process_with_market(
            Command::SubmitOrder(limit_order("same", 1)),
            1,
            Some(reading),
        );
        b.process_with_market(
            Command::SubmitOrder(limit_order("unrelated", 1)),
            1,
            Some(reading),
        );
        b.process_with_market(
            Command::SubmitOrder(limit_order("same", 1)),
            2,
            Some(reading),
        );
        let trigger = |engine: &Engine| {
            engine
                .open
                .iter()
                .find(|order| order.submit.client_order_id == "same")
                .unwrap()
                .resting
        };
        assert!(
            matches!((trigger(&a), trigger(&b)), (Resting::Limit { fill_trigger_px: a }, Resting::Limit { fill_trigger_px: b }) if a == b)
        );
    }

    #[test]
    fn a_marketable_on_arrival_partial_remainder_also_draws_a_fresh_trigger() {
        // The non-sweep partial path. The invariant is "every partial
        // increments `band_draw`", and the sweep is not the only place a
        // partial happens.
        let mut e = banded(42);
        e.arm(Divergence::PartialFillNext {
            client_order_id: "arrival-part".into(),
            fraction: Decimal::new(5, 1),
        });
        let out = e.process_with_market(
            Command::SubmitOrder(limit_order("arrival-part", 2)),
            10,
            Some(reading(0)),
        );
        assert!(matches!(
            out.iter().find(|event| matches!(event, VenueMessage::OrderFilled(_))),
            Some(VenueMessage::OrderFilled(fill)) if fill.leaves_qty == Decimal::ONE
        ));
        assert_eq!(e.open[0].band_draw, 1);
        assert_eq!(e.open[0].leaves_qty, Decimal::ONE);
    }

    #[test]
    fn a_fok_rejected_at_its_trigger_still_consumes_a_targeted_partial_fill_arm() {
        // The ordering guard: a FOK short of its trigger calls `plan_fill` for
        // its consuming effect before rejecting, so the arm that would have
        // been the reason it could not fill goes with it rather than staying
        // armed to ambush a resubmit of the same id.
        let mut e = banded(11);
        let mut fok = limit_order("fok-arm", 2);
        fok.time_in_force = TimeInForce::Fok;
        e.arm(Divergence::PartialFillNext {
            client_order_id: "fok-arm".into(),
            fraction: Decimal::new(5, 1),
        });
        let rejected = e.process(Command::SubmitOrder(fok.clone()), 1);
        // The short-of-trigger refusal by its exact text, not by the word
        // "trigger": its sibling refusal ("could not fully fill") is the one
        // this test must NOT be satisfied by, and only the full string
        // separates them.
        assert_eq!(
            reject_reason(&rejected),
            "fill-or-kill could not fill at its trigger"
        );
        assert!(e.armed.is_empty(), "the rejected FOK left its arm standing");
        let resubmit = e.process_with_market(Command::SubmitOrder(fok), 2, Some(reading(0)));
        assert!(matches!(
            resubmit.iter().find(|event| matches!(event, VenueMessage::OrderFilled(_))),
            Some(VenueMessage::OrderFilled(fill)) if fill.last_qty == Decimal::from(2)
        ));
    }

    #[test]
    fn orders_of_one_account_never_interact() {
        // Self-trade is impossible rather than prevented: orders are judged
        // only against the tape, never against each other.
        let mut e = banded(5);
        let mut buy = limit_order("cross-buy", 1);
        buy.price = Some(Decimal::from(110));
        let mut sell = limit_order("cross-sell", 1);
        sell.side = Side::Sell;
        sell.price = Some(Decimal::from(90));
        let first = e.process(Command::SubmitOrder(buy), 1);
        let second = e.process(Command::SubmitOrder(sell), 2);
        assert!(
            !first
                .iter()
                .chain(second.iter())
                .any(|event| matches!(event, VenueMessage::OrderFilled(_))),
            "two crossing limits on one account must both rest"
        );
        assert_eq!(e.open.len(), 2);
    }

    #[test]
    fn a_different_fill_seed_produces_a_different_draw_distribution() {
        // Over a fixture of order ids rather than one draw: with a band a few
        // ticks wide two seeds legitimately agree on any single id.
        let ids: Vec<String> = (0..64).map(|i| format!("seed-fixture-{i}")).collect();
        let draws = |seed: u64| -> Vec<u32> {
            ids.iter()
                .map(|id| {
                    crate::orders::draw_offset(seed, &limit_order(id, 1), Decimal::from(100), 32, 0)
                })
                .collect()
        };
        assert_ne!(draws(1), draws(2));
        // A committed vector, so any change to the key bytes or the stream is a
        // test failure rather than a silent reshuffle of every venue's fills.
        assert_eq!(
            crate::orders::draw_offset(
                42,
                &limit_order("seed-fixture-0", 1),
                Decimal::from(100),
                32,
                0
            ),
            16
        );
    }

    #[test]
    fn a_market_buy_slips_up_and_a_market_sell_slips_down() {
        let reading = MarketReading {
            last_px: Decimal::from(100),
            ts_ns: 0,
            band_ticks: 50,
        };
        let mut e = banded(42);
        let buy = e.process_with_market(
            Command::SubmitOrder(order_with(
                "slip-buy",
                Side::Buy,
                "BTCUSDT",
                1,
                Some(Decimal::from(100)),
            )),
            1,
            Some(reading),
        );
        let sell = e.process_with_market(
            Command::SubmitOrder(order_with(
                "slip-sell",
                Side::Sell,
                "BTCUSDT",
                1,
                Some(Decimal::from(100)),
            )),
            2,
            Some(reading),
        );
        let price = |events: &[VenueMessage]| {
            events
                .iter()
                .find_map(|event| match event {
                    VenueMessage::OrderFilled(fill) => Some(fill.last_px),
                    _ => None,
                })
                .unwrap()
        };
        assert!(price(&buy) >= reading.last_px);
        assert!(price(&sell) <= reading.last_px);

        // `>=` / `<=` alone is satisfied by no slippage at all, so the claim is
        // asked of a fixture, the same way the trigger band's test does it: the
        // draw is uniform on `0 ..= band_ticks` and one order may legitimately
        // draw zero, but some order in the fixture must slip, and every order
        // that slips must slip the adverse way. A zero band is the control -
        // it pins that the last print is where a fill lands when nothing
        // displaces it, so a fixture reporting no slip is distinguishable from
        // one whose engine ignores the band entirely.
        //
        // The control's last print is 99 against a stated price of 100, and
        // that split is what makes it a control at all: `draw_market_price`
        // ignores the stated price, which is the property under test, so a
        // control reading 100 against a stated 100 would still pass for an
        // engine that returned the stated price and never looked at the band.
        const CONTROL_LAST_PX: i64 = 99;
        let filled = |last_px: i64, band_ticks: u32, side: Side, i: usize| {
            let mut e = banded(42);
            let out = e.process_with_market(
                Command::SubmitOrder(order_with(
                    &format!("slip-{side:?}-{i}"),
                    side,
                    "BTCUSDT",
                    1,
                    Some(Decimal::from(100)),
                )),
                1,
                Some(MarketReading {
                    last_px: Decimal::from(last_px),
                    ts_ns: 0,
                    band_ticks,
                }),
            );
            price(&out)
        };
        for side in [Side::Buy, Side::Sell] {
            let mut slipped = 0;
            for i in 0..64 {
                assert_eq!(
                    filled(CONTROL_LAST_PX, 0, side, i),
                    Decimal::from(CONTROL_LAST_PX),
                    "a zero band fills a market {side:?} at the last print exactly, \
                     not at the stated price"
                );
                let banded = filled(100, reading.band_ticks, side, i);
                if banded == reading.last_px {
                    continue;
                }
                slipped += 1;
                match side {
                    Side::Buy => assert!(
                        banded > reading.last_px,
                        "a market buy pays UP from the last print: {banded}"
                    ),
                    Side::Sell => assert!(
                        banded < reading.last_px,
                        "a market sell is paid DOWN from the last print: {banded}"
                    ),
                }
            }
            assert!(
                slipped > 0,
                "some {side:?} in the fixture must slip, or the band moves nothing"
            );
        }
    }

    #[test]
    fn a_market_order_with_no_reading_fills_at_its_stated_price_and_warns() {
        let mut e = banded(42);
        let out = e.process(Command::SubmitOrder(order("market-fallback", 1)), 1);
        assert!(out.iter().any(|event| matches!(event, VenueMessage::OrderFilled(fill) if fill.last_px == Decimal::from(100))));
    }

    #[test]
    fn a_funded_account_is_checked_against_the_slipped_price() {
        let reading = MarketReading {
            last_px: Decimal::from(100),
            ts_ns: 0,
            band_ticks: 200,
        };
        let candidate = (0..100)
            .map(|i| order(&format!("funded-slip-{i}"), 1))
            .find(|order| crate::orders::draw_offset(42, order, Decimal::from(100), 200, 0) > 0)
            .expect("the fixture contains a nonzero draw");
        let mut e = Engine::build(EngineConfig {
            account_id: test_account_id(),
            instruments: default_instruments(),
            balances: HashMap::from([("USDT".to_string(), Decimal::from(100))]),
            fill_seed: 42,
        });
        let out = e.process_with_market(Command::SubmitOrder(candidate), 1, Some(reading));
        assert!(
            matches!(out.as_slice(), [VenueMessage::OrderRejected { reason, .. }] if reason.contains("insufficient USDT"))
        );
    }

    /// The band bites, which the fill golden cannot show. That artifact's five
    /// banded cells are byte-identical to its five unbanded ones, so it
    /// certifies the band pipeline runs and not that it moves anything: a
    /// regression that silently zeroed the band would pass it. The cause is
    /// resolution rather than calibration - the golden quantizes latency to a
    /// one-second sweep and the tape crosses a sub-basis-point displacement
    /// inside one pass - so the fix there is a finer sweep or a tighter offset
    /// ladder, both of which cost runtime that harness deliberately does not
    /// spend.
    ///
    /// This asserts the property directly instead. It proves much less than a
    /// distributional golden would, and it proves exactly the part that was
    /// unpinned: with a band, some trigger is displaced from its stated price,
    /// and the displacement is adverse on both sides. A zeroed band fails it.
    #[test]
    fn a_nonzero_band_displaces_a_trigger_adversely_from_its_stated_price() {
        let stated = Decimal::from(100);
        let increment = Decimal::new(1, 2);
        // The draw is uniform over `0..=band_ticks`, so a single order may
        // legitimately draw zero. The claim is about the band moving prices at
        // all, so it is asked over a fixture rather than of one order.
        let displaced = |side: Side| {
            (0..64)
                .map(|i| {
                    let mut order = limit_order(&format!("band-{side:?}-{i}"), 1);
                    order.side = side;
                    order.price = Some(stated);
                    order
                })
                .filter_map(|order| {
                    let banded = crate::orders::draw_trigger(42, &order, stated, increment, 8, 0);
                    let flat = crate::orders::draw_trigger(42, &order, stated, increment, 0, 0);
                    assert_eq!(
                        flat, stated,
                        "a zero band must leave the stated price exactly where it is"
                    );
                    (banded != stated).then_some(banded)
                })
                .next()
        };

        // A buy's trigger moves down: it must wait for a better price than it
        // asked for, never a worse one. Moving it up would fill a buy limit the
        // tape never actually reached.
        let buy = displaced(Side::Buy).expect("some buy in the fixture draws a nonzero offset");
        assert!(
            buy < stated,
            "a banded buy trigger sits below {stated}: {buy}"
        );

        // A sell's mirrors it and moves up.
        let sell = displaced(Side::Sell).expect("some sell in the fixture draws a nonzero offset");
        assert!(
            sell > stated,
            "a banded sell trigger sits above {stated}: {sell}"
        );
    }

    #[test]
    fn decimal_scale_does_not_change_the_fill_draw() {
        let order = limit_order("scale-stable", 1);
        assert_eq!(
            crate::orders::draw_offset(42, &order, Decimal::from(100), 200, 0),
            crate::orders::draw_offset(42, &order, Decimal::new(10_000, 2), 200, 0)
        );
    }

    #[test]
    fn id_namespaces_advance_independently_and_saturate() {
        let mut e = Engine::new();
        e.venue_order_seq = u64::MAX;
        let first = e.process(Command::SubmitOrder(order("ID-1", 1)), 1);
        let accepted = first.iter().find_map(|event| match event {
            VenueMessage::OrderAccepted { venue_order_id, .. } => Some(venue_order_id),
            _ => None,
        });
        let filled = first.iter().find_map(|event| match event {
            VenueMessage::OrderFilled(fill) => Some(fill),
            _ => None,
        });
        assert_eq!(accepted.map(String::as_str), Some("V-18446744073709551615"));
        assert_eq!(filled.map(|fill| fill.trade_id.as_str()), Some("T-1"));

        let mut hedged = Engine::new();
        hedged.set_oms_type(mogwai_protocol::OmsType::Hedging);
        let events = hedged.process(Command::SubmitOrder(order("ID-2", 1)), 1);
        let fill = events
            .iter()
            .find_map(|event| match event {
                VenueMessage::OrderFilled(fill) => Some(fill),
                _ => None,
            })
            .expect("hedging submit fills");
        assert_eq!(fill.venue_order_id, "V-1");
        assert_eq!(fill.trade_id, "T-1");
        assert_eq!(fill.position_id.as_deref(), Some("BTCUSDT-1"));
    }

    /// "Did anything move the ledger" means every event that does.
    ///
    /// `account_changed` gates the account snapshot a batch owes and the
    /// `DropNextAccountUpdate` arm it spends. It listed fills, cancels and
    /// rejections only, so an `Ouo` shrink - which changes the hold on the
    /// shrunk sibling - and an expiry - which releases one - read as "nothing
    /// happened". Neither is reachable today without an accompanying fill or
    /// cancel at any call site, which is why this test is over the predicate
    /// rather than over a venue transition: the gap is latent, and the point is
    /// that it cannot become live.
    #[test]
    fn a_ledger_moving_event_is_one_the_snapshot_gate_can_see() {
        let updated = VenueMessage::OrderUpdated {
            client_order_id: "U".into(),
            venue_order_id: "V-1".into(),
            quantity: Decimal::ONE,
            price: Some(Decimal::from(100)),
            trigger_price: None,
            leaves_qty: Decimal::ONE,
            ts_event: 1,
        };
        let expired = VenueMessage::OrderExpired {
            client_order_id: "E".into(),
            venue_order_id: "V-2".into(),
            ts_event: 1,
        };
        let accepted = VenueMessage::OrderAccepted {
            client_order_id: "A".into(),
            venue_order_id: "V-3".into(),
            ts_event: 1,
        };
        assert!(crate::orders::account_changed(&[updated]), "an Ouo shrink");
        assert!(crate::orders::account_changed(&[expired]), "an expiry");
        assert!(
            !crate::orders::account_changed(&[accepted]),
            "a bare acceptance moves nothing"
        );
    }

    /// Every prefix the venue mints is reserved, not just the first one.
    ///
    /// The restriction is not cosmetic: a consumer that claims one of these ids
    /// burns it in `seen_client_order_ids`, so the venue-minted close that later
    /// mints the same id is refused as a duplicate and the venue cannot force
    /// the account that pre-claimed it flat. The two prefixes are minted by
    /// different mechanisms - `LQ-` by liquidation, `RISK-` by the
    /// `liquidate_all` risk flatten - and both are reserved for the same reason.
    /// `RISK-` was unreserved until 2026-08-19.
    ///
    /// The loop runs over the constant the minting sites read, so a prefix added
    /// later is covered by this test the moment it exists.
    #[test]
    fn a_consumer_cannot_claim_the_venue_reserved_order_namespace() {
        // The two prefixes are named literally as well as looped over, because a
        // test that only reads the constant shrinks with it: deleting a prefix
        // from the list would delete the case that proves it reserved.
        for prefix in ["LQ-", "RISK-"] {
            assert!(
                RESERVED_ID_PREFIXES.contains(&prefix),
                "{prefix} is minted by the venue and must stay reserved"
            );
        }
        for prefix in RESERVED_ID_PREFIXES {
            let out =
                Engine::new().process(Command::SubmitOrder(order(&format!("{prefix}MNQ-1"), 1)), 1);
            assert_eq!(
                reject_reason(&out),
                "client_order_id uses a venue-reserved prefix (LQ-, RISK-)",
                "{prefix}"
            );
        }
        // And only the prefix is reserved. An id that merely starts with the
        // same letters is an ordinary client id - which is what makes the
        // refusals above a statement about the reserved namespace rather than
        // about ids that look vaguely like it.
        let out = Engine::new().process(Command::SubmitOrder(order("RISKY-MNQ-1", 1)), 1);
        assert!(
            !out.iter()
                .any(|event| matches!(event, VenueMessage::OrderRejected { .. })),
            "an id outside the reserved prefixes is ordinary: {out:?}"
        );
    }

    #[test]
    fn cancel_consumes_drop_next_account_update() {
        let mut e = banded(1);
        e.process(Command::SubmitOrder(limit_order("cancel-drop", 1)), 1);
        e.arm(Divergence::DropNextAccountUpdate);
        let canceled = e.process(
            Command::CancelOrder {
                client_order_id: "cancel-drop".into(),
            },
            2,
        );
        assert!(matches!(
            canceled.as_slice(),
            [VenueMessage::OrderCanceled { .. }]
        ));
        assert!(e.armed.is_empty());
    }

    #[test]
    fn hedging_reduce_only_without_position_id_is_rejected() {
        let mut e = Engine::new();
        e.set_oms_type(mogwai_protocol::OmsType::Hedging);
        let mut reduce = order("hedge-reduce", 1);
        reduce.reduce_only = true;
        let out = e.process(Command::SubmitOrder(reduce), 1);
        assert_eq!(
            reject_reason(&out),
            "hedging reduce-only order requires a position_id"
        );
        assert!(e.open_orders().is_empty());
    }

    #[test]
    fn surcharge_window_is_a_pure_function_of_fill_timestamp() {
        let mut engine = futures_engine(20_000, MarginBreachAction::Refuse);
        engine.set_fee_schedule(
            "MNQ".into(),
            FeeSchedule {
                maker: FeeRate::PerContract {
                    amount: Decimal::ONE,
                },
                taker: FeeRate::PerContract {
                    amount: Decimal::ONE,
                },
            },
        );
        engine.arm_fee_surcharge(Decimal::from(3), 1, 10_000_000);
        assert_eq!(
            future_fill(&mut engine, "AFTER", 1, 21_000, 10_000_001).commission,
            Decimal::ONE
        );
        assert_eq!(
            future_fill(&mut engine, "REPLAY", 1, 21_000, 2).commission,
            Decimal::from(3)
        );
    }

    #[test]
    fn zero_quantity_sweep_does_not_redraw_the_fill_band() {
        let mut e = banded(42);
        let lot = Decimal::new(1, 8);
        let resting = order_decimal(
            "zero-redraw",
            Side::Buy,
            "BTCUSDT",
            lot,
            Some(Decimal::from(100)),
        );
        let resting = SubmitOrder {
            order_type: OrderType::Limit,
            ..resting
        };
        e.process(Command::SubmitOrder(resting), 1);
        e.arm(Divergence::PartialFillNext {
            client_order_id: "zero-redraw".into(),
            fraction: Decimal::new(3, 1),
        });
        let scan = e.pending_scans().remove(0);
        let (events, _) = e.apply_scans(&[result(&scan, true, 2)], 2);
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, VenueMessage::OrderFilled(_)))
        );
        assert_eq!(e.open[0].band_draw, 0);
        assert_eq!(e.open[0].leaves_qty, lot);
    }

    /// A drain budget can end a walk short of the pass's `ts`. When the pass
    /// then executes nothing, the frontier must stay where the walk reached:
    /// pulling it forward to `ts` would retire a span no pass ever scanned, and
    /// the prints in it could never fill the order.
    #[test]
    fn zero_quantity_sweep_keeps_the_truncated_scan_frontier() {
        let mut e = banded(42);
        let lot = Decimal::new(1, 8);
        let resting = order_decimal(
            "zero-frontier",
            Side::Buy,
            "BTCUSDT",
            lot,
            Some(Decimal::from(100)),
        );
        let resting = SubmitOrder {
            order_type: OrderType::Limit,
            ..resting
        };
        e.process(Command::SubmitOrder(resting), 1);
        e.arm(Divergence::PartialFillNext {
            client_order_id: "zero-frontier".into(),
            fraction: Decimal::new(3, 1),
        });
        let scan = e.pending_scans().remove(0);
        // The walk was planned toward 9 but the budget stopped it at 4.
        let (events, _) = e.apply_scans(&[result(&scan, true, 4)], 9);
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, VenueMessage::OrderFilled(_)))
        );
        assert_eq!(e.open[0].scanned_ns, 4);
        assert_eq!(e.open[0].leaves_qty, lot);
    }

    /// The counterpart: a real execution opens a new tranche, which covers from
    /// `ts` by construction, so the frontier does jump forward there.
    #[test]
    fn a_partial_execution_resets_the_scan_frontier_to_the_pass_time() {
        let mut e = banded(42);
        let resting = order_decimal(
            "partial-frontier",
            Side::Buy,
            "BTCUSDT",
            Decimal::from(10),
            Some(Decimal::from(100)),
        );
        let resting = SubmitOrder {
            order_type: OrderType::Limit,
            ..resting
        };
        e.process(Command::SubmitOrder(resting), 1);
        e.arm(Divergence::PartialFillNext {
            client_order_id: "partial-frontier".into(),
            fraction: Decimal::new(3, 1),
        });
        let scan = e.pending_scans().remove(0);
        let (events, _) = e.apply_scans(&[result(&scan, true, 4)], 9);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, VenueMessage::OrderFilled(_)))
        );
        assert_eq!(e.open[0].scanned_ns, 9);
        assert_eq!(e.open[0].band_draw, 1);
    }

    /// The `DropNextAccountUpdate` carve-out that survives the widening: an
    /// order coming to rest places a hold, but must not spend an arm the
    /// author pointed at the fill it has not had yet.
    #[test]
    fn a_resting_acceptance_leaves_drop_next_account_update_armed() {
        let mut e = banded(1);
        e.arm(Divergence::DropNextAccountUpdate);
        let accepted = e.process(Command::SubmitOrder(limit_order("rest-keeps", 1)), 1);
        assert!(
            accepted
                .iter()
                .any(|event| matches!(event, VenueMessage::AccountState(_))),
            "a resting acceptance still owes its snapshot"
        );
        // Still armed, and the later cancel is what spends it.
        let canceled = e.process(
            Command::CancelOrder {
                client_order_id: "rest-keeps".into(),
            },
            2,
        );
        assert!(
            !canceled
                .iter()
                .any(|event| matches!(event, VenueMessage::AccountState(_))),
            "the cancel that frees the hold is where the arm lands"
        );
        assert!(e.armed.is_empty());
    }

    /// The last site that pushed its closing snapshot unconditionally, found
    /// one function away from the group's after that one was closed.
    ///
    /// An expiry frees a hold, which is exactly what `DropNextAccountUpdate` is
    /// armed against, and this path handed the consumer the fresh balances anyway
    /// while leaving the arm loaded for a later fill. The unarmed half is what
    /// makes the assertion discriminating: the same order, the same expiry, and
    /// the snapshot present.
    ///
    /// `on_modify` is not here even though it also moves a hold:
    /// `modify_does_not_consume_armed_drop` rules the other way for it on
    /// purpose, and is cited at that site.
    #[test]
    fn an_expiry_obeys_drop_next_account_update() {
        let gtd = || {
            let mut order = limit_order("gtd-expires", 1);
            order.time_in_force = TimeInForce::Gtd;
            order.expire_time = Some(50);
            order
        };

        let mut plain = banded(1);
        plain.process(Command::SubmitOrder(gtd()), 1);
        let expired = plain.expire_orders(60, None, 60);
        assert!(
            expired
                .iter()
                .any(|event| matches!(event, VenueMessage::OrderExpired { .. })),
            "the order expired: {expired:?}"
        );
        assert!(
            expired
                .iter()
                .any(|event| matches!(event, VenueMessage::AccountState(_))),
            "and reported the hold it freed: {expired:?}"
        );

        let mut armed = banded(1);
        armed.process(Command::SubmitOrder(gtd()), 1);
        armed.arm(Divergence::DropNextAccountUpdate);
        let expired = armed.expire_orders(60, None, 60);
        assert!(
            expired
                .iter()
                .any(|event| matches!(event, VenueMessage::OrderExpired { .. })),
            "the same expiry ran: {expired:?}"
        );
        assert!(
            !expired
                .iter()
                .any(|event| matches!(event, VenueMessage::AccountState(_))),
            "but the arm hid the update it freed: {expired:?}"
        );
        assert!(armed.armed.is_empty(), "and the arm was spent, not left");
    }

    /// The default seed's grid, read off the engine rather than off the
    /// function that produced it.
    ///
    /// `mogwai-protocol` cannot depend on `mogwai-engine`, so a test over there
    /// named for the engine's seed can only compare `default_instruments()`
    /// against a copy of its own literals; this is where both sides exist. The
    /// referent is the engine's own validation, which reads `price_increment`
    /// and `size_increment` out of the seeded definition - so widening either
    /// increment in `default_instruments` moves what `Engine::new` accepts, and
    /// that is what is pinned here.
    #[test]
    fn the_default_seed_puts_the_engine_on_a_btcusdt_cent_and_satoshi_grid() {
        let seed = default_instruments();
        assert_eq!(seed.len(), 1, "the default set is BTCUSDT alone");
        assert_eq!(seed[0].symbol.as_ref(), "BTCUSDT");

        let cent = Decimal::new(1, 2);
        let satoshi = Decimal::new(1, 8);
        let submit = |id: &str, quantity: Decimal, price: Decimal| {
            let mut e = Engine::new();
            e.process(
                Command::SubmitOrder(order_decimal(
                    id,
                    Side::Buy,
                    "BTCUSDT",
                    quantity,
                    Some(price),
                )),
                1,
            )
        };

        // On the grid: accepted, so the refusals below are about the increment
        // and not about some other rule the fixture tripped.
        for (id, quantity, price) in [
            ("on-grid", satoshi, Decimal::from(100)),
            ("on-grid-px", Decimal::ONE, Decimal::from(100) + cent),
        ] {
            assert!(
                matches!(
                    submit(id, quantity, price)[0],
                    VenueMessage::OrderAccepted { .. }
                ),
                "{id} sits on the seeded grid"
            );
        }

        // A tenth of each increment: refused, by the exact rule that reads it.
        // The refusal is read from the event rather than through
        // `reject_reason`, whose panic on an accepted order would name the
        // helper's shape instead of the grid this test is about.
        let refusal = |out: &[VenueMessage], what: &str| match out {
            [VenueMessage::OrderRejected { reason, .. }] => reason.clone(),
            other => panic!("the seeded {what} increment must refuse a tenth of itself: {other:?}"),
        };
        assert_eq!(
            refusal(
                &submit(
                    "sub-cent",
                    Decimal::ONE,
                    Decimal::from(100) + Decimal::new(1, 3)
                ),
                "price"
            ),
            "price violates price increment",
            "the seeded price increment is a cent"
        );
        assert_eq!(
            refusal(
                &submit("sub-satoshi", Decimal::new(1, 9), Decimal::from(100)),
                "size"
            ),
            "quantity violates size increment",
            "the seeded size increment is a satoshi"
        );
    }

    #[test]
    fn ensure_instrument_is_idempotent_and_preserves_policy() {
        let def = default_instruments().remove(0);
        let symbol = std::sync::Arc::clone(&def.symbol);
        let mut engine = Engine::build(EngineConfig {
            account_id: AccountId::parse("SIM-001").unwrap(),
            instruments: Vec::new(),
            balances: HashMap::new(),
            fill_seed: 1,
        });
        assert!(engine.ensure_instrument(def.clone()));
        let margin = MarginPolicy {
            initial_per_contract: Decimal::ONE,
            maintenance_per_contract: Decimal::ONE,
            breach_action: MarginBreachAction::Refuse,
            basis: Default::default(),
        };
        let fees = FeeSchedule {
            maker: FeeRate::BasisPoints { rate: Decimal::ONE },
            taker: FeeRate::BasisPoints { rate: Decimal::ONE },
        };
        engine.set_margin_policy(std::sync::Arc::clone(&symbol), margin);
        engine.set_fee_schedule(std::sync::Arc::clone(&symbol), fees);
        assert!(!engine.ensure_instrument(def));
        let kept_margin = engine.margin.get(&symbol).unwrap();
        assert_eq!(
            kept_margin.initial_per_contract,
            margin.initial_per_contract
        );
        assert_eq!(
            kept_margin.maintenance_per_contract,
            margin.maintenance_per_contract
        );
        let kept_fees = engine.fees.get(&symbol).unwrap();
        assert!(matches!(kept_fees.maker, FeeRate::BasisPoints { rate } if rate == Decimal::ONE));
        assert!(matches!(kept_fees.taker, FeeRate::BasisPoints { rate } if rate == Decimal::ONE));
    }

    // --- The P and L, margin and hold cluster --------------------------

    /// One inverse contract worth 100 quote units, settling in the base asset.
    fn inverse_engine() -> Engine {
        Engine::build(EngineConfig {
            account_id: test_account_id(),
            instruments: vec![InstrumentDef {
                symbol: "XBTUSD".into(),
                class: InstrumentClass::Inverse {
                    underlying: "BTC".into(),
                    settlement_currency: "XBT".into(),
                    quote_currency: "USD".into(),
                    multiplier: Decimal::from(100),
                    asset_class: WireAssetClass::Cryptocurrency,
                },
                price_precision: 1,
                size_precision: 0,
                price_increment: Decimal::new(5, 1),
                size_increment: Decimal::ONE,
            }],
            balances: HashMap::from([("XBT".to_string(), Decimal::ZERO)]),
            fill_seed: 3,
        })
    }

    /// Ten inverse contracts bought at 100 and marked at 200.
    ///
    /// The inverse answer is `100 * 10 * (1/100 - 1/200)` = 5 settlement units.
    /// The linear answer the three readers used to hand-roll is
    /// `(200 - 100) * 10 * 100` = 100,000 - twenty thousand times larger, which
    /// is the discrimination this fixture is chosen for. No coincidence of
    /// rounding can put the wrong formula on 5.
    fn inverse_position(engine: &mut Engine) {
        engine.account.positions.insert(
            (Symbol::from("XBTUSD"), None),
            crate::account::PositionState {
                qty: Decimal::from(10),
                avg_px: Decimal::from(100),
                mark_px: Decimal::from(200),
            },
        );
    }

    /// The margin breach test reads `unrealized_pnl`, so an inverse book was
    /// liquidated (or spared) on a number computed by the linear formula, while
    /// `apply_fill` booked the same position's realized P and L through
    /// `InstrumentDef::unrealized`. Realized and unrealized now come from one
    /// expression for every class, which is what `apply_fill` always claimed.
    #[test]
    fn an_inverse_positions_unrealized_pnl_uses_the_inverse_arithmetic() {
        let mut e = inverse_engine();
        inverse_position(&mut e);
        assert_eq!(
            e.unrealized_pnl("XBTUSD"),
            Decimal::from(5),
            "the linear form would report 100000 here"
        );
        let wire = e.positions();
        assert_eq!(wire.len(), 1);
        assert_eq!(
            wire[0].unrealized_pnl,
            Decimal::from(5),
            "and the wire row must not contradict the number the venue decides on"
        );
    }

    /// Settlement realizes the position, so it must credit exactly what the
    /// mark-to-market it replaces reported - otherwise an inverse account's
    /// value jumps at the settlement instant, which is the same discontinuity
    /// `apply_fill` refuses at a fill.
    #[test]
    fn an_inverse_future_settles_through_the_inverse_arithmetic() {
        let mut e = inverse_engine();
        inverse_position(&mut e);
        e.settle(&[(Symbol::from("XBTUSD"), Decimal::from(200))], 5);
        assert_eq!(
            *e.account.balances.get("XBT").expect("settlement balance"),
            Decimal::from(5),
            "the linear form would credit 100000 here"
        );
    }

    /// `margin_requirement`'s doc says its rows sum to exactly what
    /// `held_balances` holds. Under a notional basis it multiplied the raw
    /// fraction by a contract count instead of asking the policy, so a Reg-T
    /// account's reported `margins` said 37.50 while its reported `locked` said
    /// 3,750 - the wire contradicting itself on every leveraged account. The
    /// assertion is the invariant itself, not a restatement of the number.
    ///
    /// What it pins is the equality case, deliberately: one margined marked
    /// symbol, nothing unsettled, no base-currency hold. Those are exactly the
    /// three carve-outs `margin_requirement`'s doc now names, and the general
    /// statement is a `<=` rather than an equality, so a fixture sitting inside
    /// the carve-outs cannot be read as pinning the wider claim.
    #[test]
    fn the_reported_margin_reconciles_with_the_reported_locked() {
        let mut e = equity_engine(&Shares {
            cash: 20_000,
            margin: Some(reg_t()),
            ..Shares::default()
        });
        trade(&mut e, share_order("BUY", Side::Buy, 150), 1);
        let state = e.account_snapshot(2);
        let margin = state
            .margins
            .iter()
            .find(|row| row.symbol.as_ref() == "AAPL")
            .expect("a held position posts margin");
        assert_eq!(
            margin.maintenance,
            Decimal::from(3_750),
            "a quarter of 150 shares at 100, not a quarter of 150 contracts"
        );
        let held = state
            .balances
            .iter()
            .find(|balance| balance.currency == "USD")
            .expect("settlement balance")
            .locked;
        assert_eq!(
            margin.initial.saturating_add(margin.maintenance),
            held,
            "the reported margin must sum to the reported locked"
        );
    }

    /// A leveraged futures engine: ten-to-one, stated as a fraction of notional.
    fn leveraged_futures(cash: i64) -> Engine {
        let mut engine = futures_engine(cash, MarginBreachAction::Refuse);
        engine.set_margin_policy(
            "MNQ".into(),
            MarginPolicy {
                initial_per_contract: Decimal::new(1, 1),
                maintenance_per_contract: Decimal::new(1, 1),
                breach_action: MarginBreachAction::Refuse,
                basis: MarginBasis::Notional,
            },
        );
        engine
    }

    /// `order_hold` took a `price` argument and ignored it, reaching back
    /// into `submit.price` - which a `StopMarket` does not have. Under a
    /// notional basis that made its requirement `initial(qty, 0)`, so a resting
    /// stop held NO collateral at all until it triggered, while the caller had
    /// already resolved the trigger fallback the branch threw away.
    #[test]
    fn a_resting_stop_market_holds_against_its_trigger() {
        let mut e = leveraged_futures(200_000);
        let mut stop = mnq_order("S1", Side::Buy, 1, 21_000);
        stop.order_type = OrderType::StopMarket;
        stop.price = None;
        stop.trigger_price = Some(Decimal::from(21_000));
        let out = e.process_with_market(
            Command::SubmitOrder(stop),
            1,
            Some(MarketReading {
                last_px: Decimal::from(20_000),
                ts_ns: 1,
                band_ticks: 0,
            }),
        );
        assert!(
            !out.iter()
                .any(|event| matches!(event, VenueMessage::OrderRejected { .. })),
            "{out:?}"
        );
        let held = e
            .account_snapshot(2)
            .balances
            .iter()
            .find(|balance| balance.currency == "USD")
            .expect("settlement balance")
            .locked;
        // One contract at 21,000 with a multiplier of two is 42,000 of
        // notional; a tenth of that is 4,200. Reading `submit.price` gave zero.
        assert_eq!(held, Decimal::from(4_200));
    }

    /// `on_modify`'s futures branch priced the amended requirement at the
    /// amend's own optional `price` rather than `effective_price`, so a
    /// quantity-only amend computed `initial(new_leaves, 0)` - zero under a
    /// notional basis - and the funds check could not fail however far the
    /// amend grew the position.
    ///
    /// "insufficient USD balance" is not a discriminator on its own - several
    /// branches of `on_modify` produce it - so the test runs the identical
    /// amend twice, once on an account that cannot afford it and once on one
    /// that can. Every other refusal `on_modify` can reach for these inputs
    /// (the increment checks, the notional overflow guard, the missing margin
    /// ledger) is balance-independent and would refuse both, so a refusal that
    /// flips with the balance can only be the funds comparison.
    #[test]
    fn a_quantity_only_amend_of_a_leveraged_future_is_checked_against_its_price() {
        let amend = |cash: i64| -> Vec<VenueMessage> {
            let mut e = leveraged_futures(cash);
            let mut resting = mnq_order("A1", Side::Buy, 1, 21_000);
            resting.order_type = OrderType::Limit;
            e.process_with_market(
                Command::SubmitOrder(resting),
                1,
                Some(MarketReading {
                    last_px: Decimal::from(22_000),
                    ts_ns: 1,
                    band_ticks: 0,
                }),
            );
            e.process_with_market(
                Command::ModifyOrder {
                    client_order_id: "A1".into(),
                    quantity: Some(Decimal::from(10)),
                    price: None,
                    trigger_price: None,
                },
                2,
                Some(MarketReading {
                    last_px: Decimal::from(22_000),
                    ts_ns: 2,
                    band_ticks: 0,
                }),
            )
        };
        let funded = amend(100_000);
        assert!(
            !funded
                .iter()
                .any(|event| matches!(event, VenueMessage::OrderModifyRejected { .. })),
            "the same amend on an account that can afford it must be admitted, or the \
             refusal below is not the funds check: {funded:?}"
        );

        let mut e = leveraged_futures(5_000);
        // Resting, so the amend has something to amend: a buy limit under the
        // market at 22,000 takes nothing.
        let mut resting = mnq_order("A1", Side::Buy, 1, 21_000);
        resting.order_type = OrderType::Limit;
        let out = e.process_with_market(
            Command::SubmitOrder(resting),
            1,
            Some(MarketReading {
                last_px: Decimal::from(22_000),
                ts_ns: 1,
                band_ticks: 0,
            }),
        );
        assert!(
            !out.iter()
                .any(|event| matches!(event, VenueMessage::OrderRejected { .. })),
            "{out:?}"
        );
        // Ten contracts would need 42,000 against an account holding 5,000.
        let out = e.process_with_market(
            Command::ModifyOrder {
                client_order_id: "A1".into(),
                quantity: Some(Decimal::from(10)),
                price: None,
                trigger_price: None,
            },
            2,
            Some(MarketReading {
                last_px: Decimal::from(22_000),
                ts_ns: 2,
                band_ticks: 0,
            }),
        );
        let reason = out
            .iter()
            .find_map(|event| match event {
                VenueMessage::OrderModifyRejected { reason, .. } => Some(reason.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("the amend must be refused: {out:?}"));
        assert_eq!(reason, "insufficient USD balance");
    }

    /// A cash equity account refuses shorting by name, and could nonetheless end
    /// the run short: the check asked how short this order alone would leave the
    /// account, so a hundred held shares covered every resting sell
    /// independently. Two sells of a hundred against a hundred held both read
    /// `short = 0`, and both passed.
    #[test]
    fn resting_sells_cannot_each_claim_the_same_held_shares() {
        let mut e = equity_engine(&Shares::default());
        trade(&mut e, share_order("BUY", Side::Buy, 100), 1);
        let mut first = order_with("S1", Side::Sell, "AAPL", 100, Some(Decimal::from(200)));
        first.order_type = OrderType::Limit;
        let out = trade(&mut e, first, 2);
        assert!(
            !out.iter()
                .any(|event| matches!(event, VenueMessage::OrderRejected { .. })),
            "selling what the account holds is not a short: {out:?}"
        );
        let mut second = order_with("S2", Side::Sell, "AAPL", 100, Some(Decimal::from(200)));
        second.order_type = OrderType::Limit;
        let out = trade(&mut e, second, 3);
        let reason = out
            .iter()
            .find_map(|event| match event {
                VenueMessage::OrderRejected { reason, .. } => Some(reason.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("the second sell must be refused: {out:?}"));
        assert!(
            reason.starts_with("a cash equity account cannot sell AAPL short"),
            "{reason}"
        );
        // The resource the finding named, not merely the refusal: the point is
        // that the account cannot be made short, and an error message is not
        // evidence of that on its own.
        assert_eq!(e.net_position("AAPL"), Decimal::from(100));
        assert_eq!(
            e.worst_case_leaves("AAPL", Side::Sell, &[]),
            Decimal::from(100),
            "one working sell, so the held shares cover exactly the book"
        );
    }

    /// The bracket the first version of the short check broke. Two exit legs
    /// over one holding are an `Oco` group: whichever fills cancels the other,
    /// so the pair can execute at most its largest leg, and a hundred held
    /// shares cover it. Summing them read as a hundred-share short and a cash
    /// equity account refused the second leg by name - a refusal of the most
    /// ordinary order list there is.
    #[test]
    fn a_bracket_s_two_exit_legs_are_not_two_shorts() {
        let mut e = equity_engine(&Shares::default());
        trade(&mut e, share_order("BUY", Side::Buy, 100), 1);
        let exits = ["TP", "SL"];
        for (index, id) in exits.iter().enumerate() {
            let mut leg = order_with(id, Side::Sell, "AAPL", 100, Some(Decimal::from(200)));
            leg.order_type = OrderType::Limit;
            let siblings: Vec<&str> = exits.iter().copied().filter(|other| other != id).collect();
            let leg = linked(
                leg,
                link_of(mogwai_protocol::Contingency::Oco, &siblings, None),
            );
            let out = trade(&mut e, leg, 2 + index as u64);
            assert!(
                !out.iter()
                    .any(|event| matches!(event, VenueMessage::OrderRejected { .. })),
                "exit leg {id} must be admitted: {out:?}"
            );
        }
        assert_eq!(
            e.worst_case_leaves("AAPL", Side::Sell, &[]),
            Decimal::from(100),
            "the exclusive pair contributes its max, not its sum"
        );
    }

    /// A `Resting::Held` order-list child cannot execute until its parent fills,
    /// which is exactly why `order_hold_entry` holds no funds against
    /// one. The worst-fill-order accounting has to apply the same rule, or a
    /// bracket's held exit legs count as working shorts against an entry that
    /// has not filled.
    #[test]
    fn a_held_child_is_not_a_working_sell() {
        let mut e = equity_engine(&Shares::default());
        let mut parent = order_with("P", Side::Buy, "AAPL", 100, Some(Decimal::from(50)));
        parent.order_type = OrderType::Limit;
        let parent = linked(
            parent,
            link_of(mogwai_protocol::Contingency::Oto, &["C"], None),
        );
        let out = trade(&mut e, parent, 1);
        assert!(
            !out.iter()
                .any(|event| matches!(event, VenueMessage::OrderRejected { .. })),
            "{out:?}"
        );
        let mut child = order_with("C", Side::Sell, "AAPL", 100, Some(Decimal::from(200)));
        child.order_type = OrderType::Limit;
        let child = linked(
            child,
            link_of(mogwai_protocol::Contingency::NoContingency, &[], Some("P")),
        );
        let out = trade(&mut e, child, 2);
        assert!(
            !out.iter()
                .any(|event| matches!(event, VenueMessage::OrderRejected { .. })),
            "a held child sells nothing yet, so it is not a short: {out:?}"
        );
        assert_eq!(
            e.worst_case_leaves("AAPL", Side::Sell, &[]),
            Decimal::ZERO,
            "a held child contributes nothing to what can execute"
        );
    }

    /// The group's two passes must agree, and a check reading the working book
    /// is the way they stop agreeing: the dry pass runs before any member
    /// rests, the real pass after earlier members do. Two independent equity
    /// sells over one holding really are inadmissible, and the group must be
    /// refused whole on pass one - not admitted by the dry pass and then broken
    /// open on pass two, which `report_group_member_refusal` would have filed
    /// as the disclosed funds carve-out because re-asking after the refusal
    /// refuses again.
    #[test]
    fn a_group_of_two_equity_sells_over_one_holding_is_refused_whole() {
        let mut e = equity_engine(&Shares::default());
        trade(&mut e, share_order("BUY", Side::Buy, 100), 1);
        // Linked, but with no contingency between them: two independent sells
        // that happen to travel in one frame. The link is not decoration - a
        // member without one is refused by `validate_submit_group` before the
        // equity check this test is about is ever reached.
        let mut first = order_with("G1", Side::Sell, "AAPL", 100, Some(Decimal::from(200)));
        first.order_type = OrderType::Limit;
        let first = linked(
            first,
            link_of(mogwai_protocol::Contingency::NoContingency, &[], None),
        );
        let mut second = order_with("G2", Side::Sell, "AAPL", 100, Some(Decimal::from(200)));
        second.order_type = OrderType::Limit;
        let second = linked(
            second,
            link_of(mogwai_protocol::Contingency::NoContingency, &[], None),
        );
        let out = e.process_with_market(
            Command::SubmitOrderGroup {
                orders: vec![first, second],
            },
            2,
            Some(MarketReading {
                last_px: Decimal::from(100),
                ts_ns: 2,
                band_ticks: 0,
            }),
        );
        let rejected: Vec<(&str, &str)> = out
            .iter()
            .filter_map(|event| match event {
                VenueMessage::OrderRejected {
                    client_order_id,
                    reason,
                    ..
                } => Some((client_order_id.as_str(), reason.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(
            rejected.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec!["G1", "G2"],
            "pass one refuses the group whole: {out:?}"
        );
        // And for the right rule. Every wire-shape refusal the group validator
        // makes also refuses the group whole with these same two ids, so an
        // id-only assertion cannot tell this test's subject - the equity short
        // check reading the working book - from a link or symbol rule tripping
        // first.
        assert!(
            rejected
                .iter()
                .all(|(_, reason)| reason.contains("cannot sell AAPL short")),
            "refused for the equity short rule, not a wire-shape rule: {rejected:?}"
        );
        assert!(
            e.open.is_empty(),
            "and no member is left resting: {:?}",
            e.open.len()
        );
        // Whereas the same two legs as an exclusive pair are admissible, which
        // is what makes the assertion above about the sells and not about
        // groups refusing sells in general.
        let legs = ["H1", "H2"];
        let orders: Vec<SubmitOrder> = legs
            .iter()
            .map(|id| {
                let mut leg = order_with(id, Side::Sell, "AAPL", 100, Some(Decimal::from(200)));
                leg.order_type = OrderType::Limit;
                let siblings: Vec<&str> =
                    legs.iter().copied().filter(|other| other != id).collect();
                linked(
                    leg,
                    link_of(mogwai_protocol::Contingency::Oco, &siblings, None),
                )
            })
            .collect();
        let out = e.process_with_market(
            Command::SubmitOrderGroup { orders },
            3,
            Some(MarketReading {
                last_px: Decimal::from(100),
                ts_ns: 3,
                band_ticks: 0,
            }),
        );
        assert!(
            !out.iter()
                .any(|event| matches!(event, VenueMessage::OrderRejected { .. })),
            "an Oco exit pair is one short's worth, not two: {out:?}"
        );
    }

    #[test]
    fn margin_equity_sells_allocate_covered_shares_once() {
        let mut e = equity_engine(&Shares {
            cash: 50_000,
            margin: Some(reg_t()),
            borrowable: Some(Decimal::from(1_000)),
            ..Shares::default()
        });
        trade(&mut e, share_order("BUY", Side::Buy, 100), 1);
        for (id, price) in [("S1", 150), ("S2", 200)] {
            let mut sell = order_with(id, Side::Sell, "AAPL", 100, Some(Decimal::from(price)));
            sell.order_type = OrderType::Limit;
            let out = trade(&mut e, sell, 2);
            assert!(
                !out.iter()
                    .any(|event| matches!(event, VenueMessage::OrderRejected { .. })),
                "both sells are legal on the margin account: {out:?}"
            );
        }
        let state = e.account_snapshot(3);
        let usd = balance(&state, "USD");
        assert_eq!(
            usd.locked,
            Decimal::from(12_500),
            "10,000 initial at the conservative 200 sell price plus 2,500 maintenance"
        );
        let margin = state
            .margins
            .iter()
            .find(|row| row.symbol.as_ref() == "AAPL")
            .expect("the equity has a margin row");
        assert_eq!(margin.initial, Decimal::from(10_000));
        e.reconcile_order_holds();
    }

    #[test]
    fn an_equity_fill_reallocates_cover_across_resting_sells() {
        let mut e = equity_engine(&Shares {
            cash: 50_000,
            margin: Some(reg_t()),
            borrowable: Some(Decimal::from(1_000)),
            ..Shares::default()
        });
        let mut sell = order_with("S1", Side::Sell, "AAPL", 100, Some(Decimal::from(200)));
        sell.order_type = OrderType::Limit;
        trade(&mut e, sell, 1);
        assert_eq!(
            balance(&e.account_snapshot(1), "USD").locked,
            Decimal::from(10_000)
        );

        trade(&mut e, share_order("BUY", Side::Buy, 100), 2);
        assert_eq!(
            balance(&e.account_snapshot(2), "USD").locked,
            Decimal::from(2_500),
            "the new long covers the resting sell, leaving only position maintenance"
        );
        e.reconcile_order_holds();
    }

    #[test]
    fn a_cheaper_sell_cannot_underfund_an_existing_expensive_short() {
        let mut e = equity_engine(&Shares {
            cash: 18_000,
            margin: Some(reg_t()),
            borrowable: Some(Decimal::from(1_000)),
            ..Shares::default()
        });
        trade(&mut e, share_order("BUY", Side::Buy, 100), 1);
        let mut expensive = order_with("S1", Side::Sell, "AAPL", 100, Some(Decimal::from(200)));
        expensive.order_type = OrderType::Limit;
        trade(&mut e, expensive, 2);
        let mut cheap = order_with("S2", Side::Sell, "AAPL", 100, Some(Decimal::from(100)));
        cheap.order_type = OrderType::Limit;
        let out = trade(&mut e, cheap, 3);
        assert!(
            out.iter().any(|event| matches!(
                event,
                VenueMessage::OrderRejected { reason, .. } if reason == "insufficient USD balance"
            )),
            "the cheap order would expose the expensive sell as the short: {out:?}"
        );
    }

    /// The reconciliation `margin_requirement` documents - every `initial` row
    /// is a settlement-currency term `held_balances` also folds - is an equality
    /// on an account whose whole locked balance comes from margined marked
    /// symbols with nothing unsettled. This is exactly such an account, and it is
    /// the mixed case: one resting buy and one uncovered resting sell on the same
    /// equity. The aggregate sell allocation must be added to the buy's own
    /// initial margin, never assigned over it.
    #[test]
    fn a_resting_equity_buy_and_an_uncovered_sell_both_reach_the_margin_row() {
        let mut e = equity_engine(&Shares {
            cash: 100_000,
            margin: Some(reg_t()),
            borrowable: Some(Decimal::from(1_000)),
            ..Shares::default()
        });
        let mut buy = order_with("B1", Side::Buy, "AAPL", 100, Some(Decimal::from(90)));
        buy.order_type = OrderType::Limit;
        trade(&mut e, buy, 1);
        let mut sell = order_with("S1", Side::Sell, "AAPL", 100, Some(Decimal::from(200)));
        sell.order_type = OrderType::Limit;
        trade(&mut e, sell, 2);

        let state = e.account_snapshot(3);
        assert_eq!(
            balance(&state, "USD").locked,
            Decimal::from(14_500),
            "4,500 for the resting buy at 90 plus 10,000 for the uncovered sell at 200"
        );
        let row = state
            .margins
            .iter()
            .find(|row| row.symbol.as_ref() == "AAPL")
            .expect("the equity has a margin row");
        assert_eq!(
            row.initial + row.maintenance,
            balance(&state, "USD").locked,
            "posted margin and the locked balance must agree on this account"
        );
        e.reconcile_order_holds();
    }

    /// `settle` always declines a non-positive price on an inverse instrument, and
    /// declining means the position keeps the mark it had.
    ///
    /// Two distinct failures live here and the test asserts against both. The
    /// first is arithmetic: `InstrumentDef::unrealized` answers `None` for an
    /// inverse at zero because `1/0` has no value, which is a different answer
    /// from "the arithmetic overflowed", and collapsing the two into one
    /// `unwrap_or(Decimal::MAX)` credited the whole Decimal range to the
    /// balance.
    ///
    /// The second is durability, and it is the one the guard exists for.
    /// Settlement writes its price into both `avg_px` and `mark_px`, so a
    /// booked zero does not produce one bad number - it leaves the position
    /// unpriceable for the rest of the run, and every later reader answers on a
    /// price that cannot be inverted. A credits-nothing assertion alone passes
    /// against that, because crediting nothing is exactly what a poisoned
    /// position does.
    #[test]
    fn settling_an_inverse_at_a_non_positive_price_is_declined() {
        for refused in [Decimal::ZERO, Decimal::from(-100)] {
            let mut e = inverse_engine();
            inverse_position(&mut e);
            let before = e
                .account
                .positions
                .get(&(Symbol::from("XBTUSD"), None))
                .expect("the fixture rests a position")
                .mark_px;
            e.settle(&[(Symbol::from("XBTUSD"), refused)], 5);
            assert_eq!(
                *e.account.balances.get("XBT").expect("settlement balance"),
                Decimal::ZERO,
                "an undefined mark contributes nothing; saturating credited Decimal::MAX"
            );
            let after = e
                .account
                .positions
                .get(&(Symbol::from("XBTUSD"), None))
                .expect("a declined settlement retires nothing");
            assert_eq!(
                after.mark_px, before,
                "the position was re-marked at {refused}, so it can never be priced again"
            );
            assert_ne!(
                after.avg_px, refused,
                "the position's VWAP was reset to {refused}, which is the durable half of the \
                 damage rather than the credit"
            );
        }
    }

    /// The other side of the same rule, without which the decline above is
    /// indistinguishable from a settlement path that refuses everything: a
    /// positive price still settles, through the inverse arithmetic.
    #[test]
    fn settling_an_inverse_at_a_positive_price_still_books() {
        let mut e = inverse_engine();
        inverse_position(&mut e);
        e.settle(&[(Symbol::from("XBTUSD"), Decimal::from(200))], 5);
        assert_eq!(
            *e.account.balances.get("XBT").expect("settlement balance"),
            Decimal::from(5),
            "100 * 10 * (1/100 - 1/200) is 5 settlement units"
        );
    }

    /// A closed position is stored with every field zero and is never removed
    /// from the map, so for an inverse instrument the raw `unrealized` answers
    /// `None` forever. `valuation_at` propagates a `None` to the whole account,
    /// so one flat coin-margined row made the tick-resolution risk sweep
    /// silently decline to value the account at all.
    #[test]
    fn a_flat_inverse_position_does_not_void_the_accounts_valuation() {
        let mut e = inverse_engine();
        e.account.positions.insert(
            (Symbol::from("XBTUSD"), None),
            crate::account::PositionState::default(),
        );
        assert_eq!(
            e.valuation_in("XBT"),
            Some(Decimal::ZERO),
            "a flat row is worth zero, not unanswerable"
        );
    }

    /// `order_hold_entry` declines a `Resting::Held` child before it
    /// computes anything, so the venue takes NO hold against a bracket's
    /// unreleased exit leg. `on_modify` reached past that home into
    /// `order_hold` for both sides of its funds comparison, so amending
    /// such a child was judged against a hold the venue would never take, and
    /// an amend the account could plainly afford (it costs nothing) was refused
    /// for funds.
    #[test]
    fn amending_a_held_bracket_child_is_not_checked_against_a_hold_the_venue_never_takes() {
        let mut e = equity_engine(&Shares {
            cash: 20_000,
            margin: Some(reg_t()),
            ..Shares::default()
        });
        let mut parent = order_with("P", Side::Buy, "AAPL", 100, Some(Decimal::from(50)));
        parent.order_type = OrderType::Limit;
        let parent = linked(
            parent,
            link_of(mogwai_protocol::Contingency::Oto, &["C"], None),
        );
        trade(&mut e, parent, 1);
        let mut child = order_with("C", Side::Sell, "AAPL", 100, Some(Decimal::from(200)));
        child.order_type = OrderType::Limit;
        let child = linked(
            child,
            link_of(mogwai_protocol::Contingency::NoContingency, &[], Some("P")),
        );
        let out = trade(&mut e, child, 2);
        assert!(
            !out.iter()
                .any(|event| matches!(event, VenueMessage::OrderRejected { .. })),
            "{out:?}"
        );
        // Held, and therefore holding nothing: growing it to a quantity whose
        // hypothetical Reg-T initial (50,000) exceeds everything the account
        // has must still be admitted, because no hold is taken either way.
        let out = e.process_with_market(
            Command::ModifyOrder {
                client_order_id: "C".into(),
                quantity: Some(Decimal::from(500)),
                price: None,
                trigger_price: None,
            },
            3,
            Some(MarketReading {
                last_px: Decimal::from(100),
                ts_ns: 3,
                band_ticks: 0,
            }),
        );
        assert!(
            !out.iter()
                .any(|event| matches!(event, VenueMessage::OrderModifyRejected { .. })),
            "a held child holds nothing, so nothing about it can be underfunded: {out:?}"
        );
    }

    /// `on_modify`'s pre-rework funds block sent every non-future sell down a
    /// branch that demanded a `base_currency`, which an `Equity` does not
    /// have - so amending any equity sell was refused, with a message about the
    /// futures margin ledger. The rework fixed it incidentally; without a test
    /// the next person restoring the original condition restores the bug.
    #[test]
    fn an_equity_sell_can_be_amended() {
        let mut e = equity_engine(&Shares::default());
        trade(&mut e, share_order("BUY", Side::Buy, 100), 1);
        let mut sell = order_with("S1", Side::Sell, "AAPL", 100, Some(Decimal::from(200)));
        sell.order_type = OrderType::Limit;
        trade(&mut e, sell, 2);
        let out = e.process_with_market(
            Command::ModifyOrder {
                client_order_id: "S1".into(),
                quantity: Some(Decimal::from(50)),
                price: None,
                trigger_price: None,
            },
            3,
            Some(MarketReading {
                last_px: Decimal::from(100),
                ts_ns: 3,
                band_ticks: 0,
            }),
        );
        assert!(
            !out.iter()
                .any(|event| matches!(event, VenueMessage::OrderModifyRejected { .. })),
            "amending an equity sell is not a futures margin question: {out:?}"
        );
        assert!(
            out.iter()
                .any(|event| matches!(event, VenueMessage::OrderUpdated { .. })),
            "{out:?}"
        );
    }
}
