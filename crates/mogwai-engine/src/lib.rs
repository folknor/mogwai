// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The venue-agnostic exchange core: open orders, accounts, and the divergence
//! injection layer. Protocol gateways (native JSON-over-WS, or a future Binance
//! facade) drive this engine and serialize whatever it emits.
//!
//! The engine is intentionally synchronous and side-effect free: `process` takes
//! a [`ClientMessage`] and returns the [`ServerMessage`]s to send. The server
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
    AccountId, AccountState, ClientMessage, ClientOrderId, FillSnapshot, Hit, InstrumentDef,
    OrderFilled, OrderStatusInfo, OrderStatusSnapshot, OrderType, Position, ScanKind,
    ServerMessage, Side, SimClock, SubmitOrder, Symbol, TimeInForce, VenueOrderId, WireOrderStatus,
    control::Divergence, default_instruments,
};
use rust_decimal::Decimal;

mod account;
mod divergence;
mod orders;

use account::{Account, Warned};

/// Upper bound on the engine-side armed-divergence queue.
///
/// Single-shot divergences normally self-disarm on their own trigger, but a
/// TARGETED `PartialFillNext` whose order never arrives has no trigger and
/// would sit armed forever (see `take_armed`). Without a cap a stream of
/// control-plane arms - or a scenario that keeps arming targeted partials whose
/// orders never show up - grows the queue without bound (a test-harness DoS).
/// This ceiling is far above any legitimate scenario's arm count, so reaching
/// it means the queue is leaking; `arm` sheds the OLDEST entry at the cap,
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
    /// Untriggered conditional. A print TOUCHING `stop_px` triggers it.
    Conditional { stop_px: Decimal },
    /// Never scanned: a market remainder left by a partial fill, which has no
    /// meaningful price for the tape to reach. Ends only on a client cancel.
    Inert,
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
    /// lower bound for the next pass. Advanced by the ENGINE when it accepts a
    /// result, never by the walker: a walk whose result is discarded must
    /// re-cover the same span rather than lose it.
    pub scanned_ns: u64,
    /// Bumped on every mutation of this order's identity for gating purposes -
    /// reprice, quantity amend, fill, frontier advance. A `ScanResult` carries
    /// the revision its walk was planned against, so a result computed against
    /// state that has since moved is DROPPED rather than applied. Liveness
    /// alone is not enough: two overlapping walks can both name a still-resting
    /// order, and applying both double-counts the span they share.
    pub revision: u64,
}

/// The one construction path for an engine. The core receives observations,
/// never a tape or clock; the fill seed roots its private trigger stream.
pub struct EngineConfig {
    pub account_id: AccountId,
    pub instruments: Vec<InstrumentDef>,
    pub balances: HashMap<String, Decimal>,
    /// Root of the fill-band RNG stream. Never the generator's stream: a draw
    /// that advanced the tape's state would make the tape a function of client
    /// behaviour, which is exactly the market impact this venue excludes.
    pub fill_seed: u64,
}

/// The band a liquidation close is judged against when nobody has told the
/// engine the run's own cap. Matches the server's `fill_band_max_ticks`
/// default, so an engine built standalone behaves like a default venue.
pub const DEFAULT_LIQUIDATION_BAND_TICKS: u32 = 200;

/// Per-instrument collateral policy. The settlement SCHEDULE is not here: it
/// is the session calendar's `settlement_minute_of_day`, read in exchange-local
/// time, and the sweeper strikes each instant it names.
#[derive(Debug, Clone, Copy)]
pub struct MarginPolicy {
    pub initial_per_contract: Decimal,
    pub maintenance_per_contract: Decimal,
    pub breach_action: BreachAction,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BreachAction {
    #[default]
    Refuse,
    Liquidate,
}

pub struct MarkOutcome {
    pub events: Vec<ServerMessage>,
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
    /// Band half width in TICKS, already scaled by trailing realized volatility
    /// and clamped by the server. The engine multiplies it by the instrument's
    /// price increment, because the instrument table lives here.
    pub band_ticks: u32,
}

impl EngineConfig {
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
/// that decides it. The engine hands these out; the server walks the tape and
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
    /// `None` if the span held nothing. The FIRST such print - there is no
    /// accumulation - and a triggered stop-market prices its fill off it.
    pub hit: Option<Hit>,
    /// The instant the walk ACTUALLY reached, which its drain budget may have
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
    /// Aggregate resting-order reservations by currency. Position maintenance
    /// is folded separately because position count is independent of book
    /// depth; the hot funds path must not walk every resting order.
    order_locked: HashMap<String, Decimal>,
    /// A saturated aggregate cannot be decremented soundly. The next removal
    /// rebuilds the cache from the authoritative orders instead.
    order_locked_clipped: HashSet<String>,
    account: Account,
    /// Whether submits and amends are checked against free balance. Set once
    /// at construction: a FUNDED account (non-empty seed) is an honest cash
    /// venue, so an order the account cannot cover is rejected like a real
    /// exchange would - otherwise the ledger goes negative and a nautilus
    /// cash-account consumer refuses every snapshot after it, silently
    /// desyncing. An UNFUNDED account keeps the permissive delta-off-zero
    /// ledger with no funds checks: its documented purpose is exercising
    /// exactly that negative-balance path, which enforcement would make
    /// unreachable. Constructor-time, not derived from the live balance map:
    /// fills create balance entries as they book, so a dynamic check would
    /// silently flip an unfunded account into enforcing after its first fill.
    enforce_funds: bool,
    /// `InstrumentDef` (from `mogwai-protocol`) is used directly as the engine's
    /// instrument representation - it carries exactly the base/quote and
    /// precision/increment fields the fill and reservation path needs, so the
    /// engine keeps no parallel struct that could drift from the wire type.
    instruments: HashMap<Symbol, InstrumentDef>,
    /// Every ACCEPTED client order id, mapped to the venue order id it was
    /// assigned. Never cleared (a deliberate, unbounded retention): key
    /// presence distinguishes "was once a real order, now terminal" from
    /// "never accepted at all", and the retained venue id lets a cancel/modify
    /// reject for a terminal order still name the order it targets - the wire
    /// contract says `venue_order_id` is absent ONLY for genuinely unknown ids.
    seen_client_order_ids: HashMap<ClientOrderId, VenueOrderId>,
    /// Terminal order records, the closed half of the `QueryOrders` truth
    /// store: every order that reached `Filled` or `Canceled`, frozen at its
    /// terminal transition. Retention is unbounded on purpose, matching
    /// `seen_client_order_ids`: reconciliation must be able to ask about an
    /// order regardless of how long ago it closed, and a test-lifetime venue
    /// accumulates orders at test scale, not exchange scale.
    closed: HashMap<ClientOrderId, OrderStatusInfo>,
    /// Every fill as it BOOKED, in booking order - the `QueryFills` truth
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
    /// The engine is venue-wide but every pass through it belongs to exactly
    /// ONE boat, and the fee surcharge is the only state that must be judged on
    /// that boat's axis. Carrying it as one field set at each pass entry
    /// (`process_with_market_on_clock`, `apply_scans_on_clock`) rather than as
    /// a parameter threaded through the fill-booking helpers is a deliberate
    /// narrowing: those helpers are reached from a dozen places and none of the
    /// others has any business knowing a clock.
    ///
    /// THE INVARIANT THAT KEEPS IT HONEST: every entry point that can book a
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
        self.margin.insert(symbol, policy);
        // Public callers may replace policy after orders already rest. Their
        // holds still derive from `order_reservation`; rebuild the aggregate
        // rather than teaching this setter a second margin formula.
        self.rebuild_order_locked_excluding(None);
    }

    pub fn set_fee_schedule(&mut self, symbol: Symbol, schedule: FeeSchedule) {
        self.fees.insert(symbol, schedule);
    }

    /// The band a venue-originated liquidation close is judged against, in
    /// ticks. The server sets it from `fill_band_max_ticks`; an engine built
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

    /// Arm the surcharge for a SIMULATED span, stamped at a WALL instant.
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
    /// milliseconds to a slow boat and a fast one. The LATE-BOARDER RULE opens
    /// it at `max(sim.sim_ns(armed), sim.sim_epoch_ns)`, so a boat whose anchor
    /// is later than the arm gets the full span from its own epoch instead of a
    /// window that already closed in its past.
    pub(crate) fn fee_surcharge_multiplier_for(&self, sim: SimClock, ts: u64) -> Decimal {
        self.fee_surcharge.map_or(Decimal::ONE, |window| {
            let opening = sim.sim_ns(window.wall_armed_ns).max(sim.sim_epoch_ns);
            if ts >= opening && ts < opening.saturating_add(window.sim_span_ns) {
                window.mult
            } else {
                Decimal::ONE
            }
        })
    }

    /// The river a RESTING order belongs to, so a control targeting that order
    /// by id can resolve the clock its timestamps live on. `None` for an id
    /// that is unknown or already terminal - exactly the ids
    /// `cancel_open_order_silently` refuses, and resolved through the SAME
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
                let value = position
                    .mark_px
                    .checked_sub(position.avg_px)
                    .and_then(|points| points.checked_mul(position.qty))
                    .and_then(|value| value.checked_mul(def.class.multiplier()))
                    .unwrap_or(if position.qty.is_sign_negative() {
                        Decimal::MIN
                    } else {
                        Decimal::MAX
                    });
                sum.saturating_add(value)
            })
    }

    pub fn mark(&mut self, marks: &[(Symbol, Decimal)], ts: u64) -> MarkOutcome {
        let mut moved = false;
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
                    .is_some_and(|def| def.class.is_future())
                    && position.mark_px != *mark
                {
                    position.mark_px = *mark;
                    moved = true;
                }
            }
        }
        let mut events = Vec::new();
        let originated_orders = self.apply_margin_breaches(marks, ts, &mut events);
        if moved || originated_orders > 0 {
            events.retain(|event| !matches!(event, ServerMessage::AccountState(_)));
            events.push(ServerMessage::AccountState(self.snapshot(ts)));
        }
        MarkOutcome {
            events,
            originated_orders,
        }
    }

    /// Posted collateral, ONE row per symbol - never one per position. Two
    /// hedged positions in the same symbol post against one instrument, and
    /// `book_shape().margins` counts symbols, so a per-position row would both
    /// misreport the requirement and under-reserve the admission budget.
    ///
    /// `maintenance` is what the open positions require; `initial` is what the
    /// resting non-reduce-only orders require. Their sum is exactly what
    /// `locked_balances` reserves, so the reported margin reconciles with the
    /// reported `locked` by construction. Reduce-only orders reserve nothing
    /// and appear here as nothing.
    #[must_use]
    pub(crate) fn margin_requirement(&self) -> Vec<mogwai_protocol::PostedMargin> {
        let mut rows: HashMap<&Symbol, (Decimal, Decimal)> = HashMap::new();
        for ((symbol, _), position) in &self.account.positions {
            let Some(policy) = self.margin.get(symbol) else {
                continue;
            };
            let row = rows.entry(symbol).or_default();
            row.1 = row.1.saturating_add(
                policy
                    .maintenance_per_contract
                    .saturating_mul(position.qty.abs()),
            );
        }
        for order in &self.open {
            if order.submit.reduce_only {
                continue;
            }
            let Some(policy) = self.margin.get(&order.submit.symbol) else {
                continue;
            };
            let row = rows.entry(&order.submit.symbol).or_default();
            row.0 = row
                .0
                .saturating_add(policy.initial_per_contract.saturating_mul(order.leaves_qty));
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

    fn apply_margin_breaches(
        &mut self,
        marks: &[(Symbol, Decimal)],
        ts: u64,
        events: &mut Vec<ServerMessage>,
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
            // Deduplicated by SYMBOL: `unrealized_pnl` already sums every
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
                sum.saturating_add(self.unrealized_pnl(other))
            });
            let maintenance = self
                .account
                .positions
                .iter()
                .filter_map(|((other, _), position)| {
                    let other_policy = self.margin.get(other)?;
                    let other_def = self.instruments.get(other)?;
                    (other_def.class.settlement_currency() == currency).then(|| {
                        other_policy
                            .maintenance_per_contract
                            .saturating_mul(position.qty.abs())
                    })
                })
                .fold(Decimal::ZERO, Decimal::saturating_add);
            let breached = total.saturating_add(unrealized) < maintenance;
            match (breached, policy.breach_action) {
                (true, BreachAction::Refuse) => {
                    self.margin_breached.insert(symbol);
                }
                (true, BreachAction::Liquidate) => {
                    self.margin_breached.insert(std::sync::Arc::clone(&symbol));
                    // One order per open POSITION, not one per symbol: under
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
                client_order_id: format!("LQ-{}-{}", symbol, self.liquidation_seq),
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
                time_in_force: TimeInForce::Ioc,
                reduce_only: true,
                post_only: false,
            };
            events.extend(self.on_submit_from(
                order,
                ts,
                Some(MarketReading {
                    last_px: mark,
                    ts_ns: ts,
                    // A venue-originated close has no client reading to inherit,
                    // so it is judged against the run's CONFIGURED band cap
                    // rather than an invented constant. That is deliberately
                    // pessimistic: a forced close is the one moment a venue is
                    // least likely to do better than its worst advertised
                    // slippage.
                    band_ticks: self.liquidation_band_ticks,
                }),
                false,
            ));
            originated += 1;
        }
        originated
    }

    /// Close every open position and cancel every resting order, as the venue
    /// rather than as the client.
    ///
    /// This is what enforcing an ACCOUNT POLICY does on breach: a strategy that
    /// would have been liquidated must actually be liquidated, or the forward
    /// claim is worth nothing. It is the same close the margin ledger performs
    /// under `BreachAction::Liquidate` - reduce-only IOC market orders at the
    /// mark, judged against the configured liquidation band - applied to the
    /// whole book instead of to one breached symbol.
    ///
    /// RESTING ORDERS GO FIRST. A flatten that left them would leave the
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
            events.extend(self.on_cancel(client_order_id, ts));
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
                client_order_id: format!("RISK-{}-{}", symbol, self.liquidation_seq),
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
                time_in_force: TimeInForce::Ioc,
                reduce_only: true,
                post_only: false,
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
            ));
            originated += 1;
        }
        MarkOutcome {
            events,
            originated_orders: originated,
        }
    }

    pub fn settle(&mut self, marks: &[(Symbol, Decimal)], ts: u64) -> MarkOutcome {
        let mut settled = false;
        for (symbol, settle_px) in marks {
            let Some(def) = self.instruments.get(symbol) else {
                continue;
            };
            if !def.class.is_future() {
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
                let pnl = settle_px
                    .saturating_sub(position.avg_px)
                    .saturating_mul(position.qty)
                    .saturating_mul(def.class.multiplier());
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
            events.retain(|event| !matches!(event, ServerMessage::AccountState(_)));
            events.push(ServerMessage::AccountState(self.snapshot(ts)));
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
    // No `Default`: per spec, `new()` is the sole constructor so the instrument
    // table is always seeded. A derived `Default` would yield an empty table
    // whose fill accounting silently diverges (every fill warns, books
    // position-only); a delegating `Default` is dead surface nothing calls.
    #[expect(
        clippy::new_without_default,
        reason = "new() seeds the instrument table; a Default impl would diverge or be dead surface"
    )]
    pub fn new() -> Self {
        Self::build(EngineConfig::unbound(default_instruments()))
    }

    /// Placeholder identity for `EngineConfig::unbound` and `new()`. Production
    /// always builds an `EngineConfig` by hand, which REQUIRES the real id:
    /// an engine that guessed its own identity would stamp a wrong
    /// `AccountState.account_id` on the wire, and a snapshot is only
    /// self-describing if that field is the account the ledger belongs to.
    pub const UNBOUND_ACCOUNT_ID: &'static str = "UNBOUND";

    /// Constructs the engine with the account pre-funded per currency, the
    /// venue's equivalent of a deposit made before the run. The ledger itself
    /// only ever books fill deltas, so without a seed the first buy drives the
    /// quote leg negative - which a nautilus CASH account (the adapter's
    /// default) refuses to apply, silently desyncing the consumer's account
    /// from the venue's. Funding is initial state, not a mutation: there is no
    /// deposit surface at runtime, so a scenario's capital is fixed at boot
    /// and every balance the venue ever reports is explained by fills alone.
    ///
    /// A non-empty seed also arms funds ENFORCEMENT: a funded venue rejects
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
            order_locked: HashMap::new(),
            order_locked_clipped: HashSet::new(),
            enforce_funds,
            account: Account {
                balances: config.balances,
                positions: HashMap::new(),
            },
            instruments,
            seen_client_order_ids: HashMap::new(),
            closed: HashMap::new(),
            fills: Vec::new(),
            armed: VecDeque::new(),
            venue_order_seq: 0,
            trade_seq: 0,
            position_seq: 0,
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

    /// Monotonic id source; the server stamps real timestamps.
    fn next_venue_order_id(&mut self) -> String {
        self.venue_order_seq = self.venue_order_seq.saturating_add(1);
        format!("V-{}", self.venue_order_seq)
    }

    fn next_trade_id(&mut self) -> String {
        self.trade_seq = self.trade_seq.saturating_add(1);
        format!("T-{}", self.trade_seq)
    }

    /// Process one client message, emitting the resulting execution events.
    ///
    /// `ts` is supplied by the caller (the server's clock) so the engine stays
    /// free of wall-clock access and remains deterministic in tests.
    pub fn process(&mut self, msg: ClientMessage, ts: u64) -> Vec<ServerMessage> {
        self.process_with_market(msg, ts, None)
    }

    /// As `process`, with the tape reading the server took at `ts`.
    ///
    /// A submit needs it to size its band and to judge marketability; a PRICE
    /// amend needs it so a re-draw adopts the current regime rather than the one
    /// the order was accepted under. `None` is a legitimate answer - the venue's
    /// estimator can be cold or its walk can be truncated - and every path here
    /// has a defined behaviour without one: a limit rests untriggerable until a
    /// later walk has evidence, an amend keeps the band it had, and a market
    /// order fills unslipped at its stated price.
    ///
    /// Runs on the IDENTITY clock, which makes it a tests-and-benches
    /// convenience rather than a serving path: an armed fee surcharge is then
    /// judged with simulated time equal to wall time. The server calls
    /// `process_with_market_on_clock` with the commanding socket's boat clock.
    pub fn process_with_market(
        &mut self,
        msg: ClientMessage,
        ts: u64,
        reading: Option<MarketReading>,
    ) -> Vec<ServerMessage> {
        self.process_with_market_on_clock(msg, ts, reading, SimClock::identity())
    }

    /// As `process_with_market`, on the clock of the boat this pass belongs to.
    /// Every timestamp the pass produces was sampled on that clock, so the fee
    /// surcharge is judged on it too.
    pub fn process_with_market_on_clock(
        &mut self,
        msg: ClientMessage,
        ts: u64,
        reading: Option<MarketReading>,
        sim: SimClock,
    ) -> Vec<ServerMessage> {
        self.event_sim = sim;
        if cfg!(debug_assertions) {
            self.reconcile_order_locked();
        }
        let events = match msg {
            ClientMessage::SubmitOrder(order) => self.on_submit(order, ts, reading),
            ClientMessage::CancelOrder { client_order_id } => self.on_cancel(client_order_id, ts),
            ClientMessage::ModifyOrder {
                client_order_id,
                price,
                quantity,
                trigger_price,
            } => self.on_modify(client_order_id, price, quantity, trigger_price, ts, reading),
            ClientMessage::QueryOrders {
                request_id,
                client_order_id,
                open_only,
            } => vec![ServerMessage::OrderStatusSnapshot(
                self.order_status_snapshot(request_id, client_order_id.as_deref(), open_only, ts),
            )],
            ClientMessage::QueryFills {
                request_id,
                client_order_id,
            } => vec![ServerMessage::FillSnapshot(self.fill_snapshot(
                request_id,
                client_order_id.as_deref(),
                ts,
            ))],
        };
        if cfg!(debug_assertions) {
            self.reconcile_order_locked();
        }
        events
    }

    /// Every resting order the tape can decide, each carrying the price the
    /// walk applies its predicate to and WHICH predicate that is. There is no
    /// off switch: the band is always on, so a venue always sweeps.
    ///
    /// The dispatch is a match on `Resting`, and each arm is load-bearing:
    ///
    /// - `Limit` yields a `FillThrough` scan against the order's DRAWN band
    ///   trigger. A print strictly through it fills the order at its own stated
    ///   price.
    /// - `Conditional` yields a `TriggerTouch` scan against the client's STATED
    ///   stop price. A print merely reaching it triggers, because a stop holds
    ///   no queue position and so needs none of the strictness the limit case
    ///   does.
    /// - `Inert` yields NOTHING, which is the naming of what an `order_type`
    ///   filter used to express here: an armed `PartialFillNext` can leave a
    ///   MARKET remainder resting with a stamped price, and handing that to the
    ///   tape walk would hold it until the market traded through a price the
    ///   venue itself synthesized. A market remainder, and a triggered
    ///   stop-market's remainder, have no meaningful price for the tape to
    ///   reach; they rest, are never scanned, and end only on a client cancel.
    #[must_use]
    pub fn pending_scans(&self) -> Vec<PendingScan> {
        // The slot order is NOT stable - `OpenBook::remove` swaps - so the
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
                    Resting::Conditional { stop_px } => (ScanKind::TriggerTouch, stop_px),
                    Resting::Inert => return None,
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
    /// witness, so its content is NEVER touched by divergences - havoc may
    /// only delay or drop the reply's delivery (the server's writer windows),
    /// per the honest-content contract on `ClientMessage::QueryOrders`.
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

    /// The control-plane out-of-band cancel (`CancelOpenOrderSilently`):
    /// remove a RESTING order from the book and free its reservation,
    /// emitting no lifecycle event - the fault class where the venue
    /// cancelled and the client never heard. The truth store records the
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
            return Err(match self.seen_client_order_ids.get(client_order_id) {
                Some(_) => "order already terminal (filled or canceled)".into(),
                None => "unknown order".into(),
            });
        };
        let order = self.take_open(pos);
        self.record_closed(&order, WireOrderStatus::Canceled, ts);
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
                        state
                            .mark_px
                            .checked_sub(state.avg_px)
                            .and_then(|points| points.checked_mul(state.qty))
                            .and_then(|value| value.checked_mul(def.class.multiplier()))
                            .unwrap_or(if state.qty.is_sign_negative() {
                                Decimal::MIN
                            } else {
                                Decimal::MAX
                            })
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
        Balance, InstrumentClass, OrderFilled, OrderType, Side, TimeInForce, WireAssetClass,
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
            reduce_only: false,
            post_only: false,
            time_in_force: TimeInForce::Gtc,
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
            time_in_force: TimeInForce::Gtc,
            reduce_only: false,
            post_only: false,
        }
    }

    #[test]
    fn a_stop_market_rests_untriggered_until_a_print_touches_its_stop() {
        let mut e = banded(7);
        let out = e.process_with_market(
            ClientMessage::SubmitOrder(stop_order(
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
                ServerMessage::OrderAccepted { .. },
                ServerMessage::AccountState(_)
            ]
        ));
        let scan = e.pending_scans().remove(0);
        assert_eq!(scan.kind, ScanKind::TriggerTouch);
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
            Some(ServerMessage::OrderTriggered { .. })
        ));
        assert!(
            out.iter()
                .any(|event| matches!(event, ServerMessage::OrderFilled(_)))
        );
    }

    #[test]
    fn a_stop_triggers_on_a_print_exactly_at_its_stop_price() {
        let mut e = banded(8);
        let out = e.process_with_market(
            ClientMessage::SubmitOrder(stop_order(
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
            Some(ServerMessage::OrderTriggered { .. })
        ));
    }

    #[test]
    fn a_gapped_stop_limit_triggers_and_rests_without_filling() {
        let mut e = banded(9);
        e.process(
            ClientMessage::SubmitOrder(stop_order(
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
            [ServerMessage::OrderTriggered { .. }]
        ));
        assert!(matches!(e.open[0].resting, Resting::Limit { .. }));
    }

    #[test]
    fn query_orders_reports_a_triggered_stop_limit_as_open() {
        let mut e = banded(10);
        e.process(
            ClientMessage::SubmitOrder(stop_order(
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
            "the row echoes the stop the client stated"
        );
    }

    /// Sweep one pending scan of `e` with a print at `px`, applied at `ts`.
    fn sweep(e: &mut Engine, px: i64, ts: u64) -> Vec<ServerMessage> {
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

    fn filled(out: &[ServerMessage]) -> &OrderFilled {
        out.iter()
            .find_map(|event| match event {
                ServerMessage::OrderFilled(fill) => Some(fill),
                _ => None,
            })
            .expect("expected a fill")
    }

    #[test]
    fn a_triggered_stop_market_fills_slipped_off_the_triggering_print() {
        // The fill comes from the print that MADE the order live, slipped
        // adversely. Never the stop price (that is the client's own number) and
        // never the acceptance-time last price (that is the look-ahead's mirror
        // image: a reading the trigger did not happen at).
        let mut e = banded(11);
        e.process_with_market(
            ClientMessage::SubmitOrder(stop_order(
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
            ClientMessage::SubmitOrder(stop_order(
                "late",
                Side::Buy,
                OrderType::StopMarket,
                99,
                None,
            )),
            12,
            Some(reading(50)),
        );
        assert!(matches!(out[0], ServerMessage::OrderAccepted { .. }));
        assert!(matches!(out[1], ServerMessage::OrderTriggered { .. }));
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
            let out = e.process(ClientMessage::SubmitOrder(order), 10);
            assert!(
                reject_reason(&out).starts_with("conditional orders are good-till-cancel only"),
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
                "StopMarket order must not carry a price",
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
                "post_only is legal only on Limit and StopLimit",
            ),
        ];
        for (name, shape, reason) in cases {
            let mut e = banded(14);
            let mut order = order("shape", 1);
            shape(&mut order);
            let out = e.process(ClientMessage::SubmitOrder(order), 10);
            assert_eq!(reject_reason(&out), reason, "{name}");
        }
    }

    #[test]
    fn a_post_only_order_that_would_take_liquidity_is_rejected() {
        // On arrival for a limit, and at TRIGGER time for a stop-limit - after
        // the trigger, which did happen. Rejected rather than canceled: it is
        // the venue refusing the order's own stated terms.
        let mut e = banded(15);
        let mut taker = limit_order("taker", 1);
        taker.post_only = true;
        let out = e.process_with_market(ClientMessage::SubmitOrder(taker), 10, Some(reading(0)));
        assert_eq!(reject_reason(&out), "post-only order would take liquidity");

        let mut e = banded(15);
        let mut stop = stop_order("post-stop", Side::Buy, OrderType::StopLimit, 99, Some(100));
        stop.post_only = true;
        let out = e.process_with_market(ClientMessage::SubmitOrder(stop), 10, Some(reading(0)));
        assert!(matches!(out[0], ServerMessage::OrderAccepted { .. }));
        assert!(matches!(out[1], ServerMessage::OrderTriggered { .. }));
        assert!(
            matches!(&out[2], ServerMessage::OrderRejected { reason, .. } if reason == "post-only order would take liquidity")
        );
        assert!(e.open.is_empty(), "the rejected order leaves the book");
        assert_eq!(
            e.closed["post-stop"].status,
            WireOrderStatus::Rejected,
            "a rejection after acceptance is a closed row a query can report"
        );
        assert!(matches!(out[3], ServerMessage::AccountState(_)));
    }

    #[test]
    fn a_post_only_reprice_that_would_take_liquidity_is_rejected() {
        let mut e = banded(15);
        let mut resting = limit_order("post-amend", 1);
        resting.price = Some(Decimal::from(90));
        resting.post_only = true;
        let accepted =
            e.process_with_market(ClientMessage::SubmitOrder(resting), 10, Some(reading(0)));
        assert!(matches!(accepted[0], ServerMessage::OrderAccepted { .. }));

        let out = e.process_with_market(
            ClientMessage::ModifyOrder {
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
            ServerMessage::OrderModifyRejected { reason, .. }
                if reason == "post-only order would take liquidity"
        ));
        assert_eq!(e.open_orders()[0].submit.price, Some(Decimal::from(90)));
    }

    #[test]
    fn a_reduce_only_order_rests_while_flat_on_a_funded_account() {
        // The admission exemption: a protective sell-stop placed while flat
        // holds no base, so the funded-sell check must not refuse it and it must
        // reserve nothing - otherwise the shape this whole surface exists to
        // serve is unreachable on the only account mode that checks anything.
        let mut e = funded(1_000);
        let mut stop = stop_order("protect", Side::Sell, OrderType::StopMarket, 90, None);
        stop.reduce_only = true;
        let out = e.process(ClientMessage::SubmitOrder(stop), 10);
        assert!(matches!(out[0], ServerMessage::OrderAccepted { .. }));
        let state = account(&out, out.len() - 1);
        assert_eq!(balance(state, "USDT").locked, Decimal::ZERO);
        assert_eq!(balance(state, "USDT").free, Decimal::from(1_000));
    }

    #[test]
    fn a_reduce_only_order_is_capped_by_the_position_and_cancels_when_flat() {
        let mut e = banded(16);
        let mut stop = stop_order("flat", Side::Sell, OrderType::StopMarket, 90, None);
        stop.reduce_only = true;
        e.process(ClientMessage::SubmitOrder(stop), 10);
        let out = sweep(&mut e, 90, 11);
        assert!(matches!(out[0], ServerMessage::OrderTriggered { .. }));
        assert!(
            matches!(out[1], ServerMessage::OrderCanceled { .. }),
            "a cap of zero cancels rather than opening a fresh short"
        );
        assert!(
            !out.iter()
                .any(|event| matches!(event, ServerMessage::OrderFilled(_))),
            "nothing may fill against a position that is already gone"
        );
        assert_eq!(e.closed["flat"].status, WireOrderStatus::Canceled);
        assert!(matches!(out[2], ServerMessage::AccountState(_)));
    }

    #[test]
    fn a_cap_clamped_reduce_only_fill_cancels_its_remainder() {
        // The remainder can never again have a non-zero cap, and an `Inert`
        // remainder reaches no further fill decision, so it would sit open
        // forever. It is closed in the same batch instead.
        let mut e = banded(17);
        e.process(ClientMessage::SubmitOrder(order("open-1", 1)), 10);
        let mut stop = stop_order("clamped", Side::Sell, OrderType::StopMarket, 90, None);
        stop.reduce_only = true;
        stop.quantity = Decimal::from(3);
        e.process(ClientMessage::SubmitOrder(stop), 11);
        let out = sweep(&mut e, 90, 12);
        assert_eq!(
            filled(&out).last_qty,
            Decimal::ONE,
            "clamped to the position"
        );
        assert!(
            out.iter()
                .any(|event| matches!(event, ServerMessage::OrderCanceled { .. }))
        );
        assert!(e.open.is_empty(), "no Inert remainder is left behind");
        assert_eq!(e.closed["clamped"].status, WireOrderStatus::Canceled);
    }

    #[test]
    fn an_untriggered_buy_stop_reserves_against_its_trigger_price() {
        // A stop-market has no price, so the reservation is the only number it
        // has. Under-reserved by exactly the slippage, which the fill-time
        // re-check is what covers.
        let mut e = funded(1_000);
        let mut stop = stop_order("hold", Side::Buy, OrderType::StopMarket, 100, None);
        stop.quantity = Decimal::from(2);
        let out = e.process(ClientMessage::SubmitOrder(stop), 10);
        let state = account(&out, out.len() - 1);
        assert_eq!(balance(state, "USDT").locked, Decimal::from(200));
        assert_eq!(balance(state, "USDT").free, Decimal::from(800));
    }

    #[test]
    fn a_fully_funded_buy_stop_does_not_fail_its_own_trigger_on_its_own_reservation() {
        // The double-count: at trigger time the order IS resting, so its own
        // hold has already left `free_balance`. Comparing the notional against
        // that would fail a fully funded order at zero slippage.
        let mut e = funded(200);
        let mut stop = stop_order("own-hold", Side::Buy, OrderType::StopMarket, 100, None);
        stop.quantity = Decimal::from(2);
        e.process(ClientMessage::SubmitOrder(stop), 10);
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
            ClientMessage::SubmitOrder(stop_order(
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
            [ServerMessage::OrderTriggered { .. }]
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
            ClientMessage::SubmitOrder(stop_order(
                "atonce",
                Side::Buy,
                OrderType::StopLimit,
                99,
                Some(100),
            )),
            10,
            Some(reading(0)),
        );
        assert!(matches!(out[1], ServerMessage::OrderTriggered { .. }));
        assert_eq!(
            filled(&out).last_px,
            Decimal::from(100),
            "a triggered stop-limit fills at its own stated price"
        );
    }

    #[test]
    fn partial_fill_next_lands_on_the_fill_the_trigger_produces() {
        // The arm targets a FILL. A stop-limit that triggers and rests produced
        // none, so the arm must survive the trigger and fire on the sweep fill
        // that follows.
        let mut e = banded(20);
        let mut stop = stop_order("armed", Side::Sell, OrderType::StopLimit, 100, Some(99));
        stop.quantity = Decimal::from(2);
        e.process(ClientMessage::SubmitOrder(stop), 10);
        e.arm(Divergence::PartialFillNext {
            client_order_id: "armed".into(),
            fraction: Decimal::new(5, 1),
        });
        let out = sweep(&mut e, 95, 11);
        assert!(
            matches!(out.as_slice(), [ServerMessage::OrderTriggered { .. }]),
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
        // flight. The silent cancel bumps no revision - it REMOVES the order -
        // so the in-flight `ScanResult` fails its `client_order_id` lookup and
        // is dropped. The order is canceled, no trigger is published, and no
        // fill is booked. This is the existing revision-guard contract reaching
        // a conditional, not new machinery.
        let mut e = banded(31);
        e.process(
            ClientMessage::SubmitOrder(stop_order(
                "raced",
                Side::Sell,
                OrderType::StopMarket,
                90,
                None,
            )),
            10,
        );
        // Planned off the book BEFORE the cancel, exactly as the sweeper plans
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
            ClientMessage::SubmitOrder(stop_order(
                "amend-stop",
                Side::Sell,
                OrderType::StopMarket,
                90,
                None,
            )),
            10,
        );
        let out = e.process(
            ClientMessage::ModifyOrder {
                client_order_id: "amend-stop".into(),
                price: None,
                quantity: None,
                trigger_price: Some(Decimal::from(80)),
            },
            20,
        );
        assert!(
            matches!(&out[0], ServerMessage::OrderUpdated { trigger_price, .. } if *trigger_price == Some(Decimal::from(80))),
            "the ack has to carry the new trigger or the amend is unverifiable"
        );
        assert_eq!(e.open[0].scanned_ns, 20, "the trigger window restarts");
        let scan = e.pending_scans().remove(0);
        assert_eq!(scan.px, Decimal::from(80));

        // After the trigger there is nothing left to trigger, and silently
        // ignoring the field would make the amend a lie. A stop-LIMIT is the
        // shape that survives its own trigger, so it is what proves the
        // refusal rather than the terminal-order one.
        let mut e = banded(21);
        e.process(
            ClientMessage::SubmitOrder(stop_order(
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
            ClientMessage::ModifyOrder {
                client_order_id: "fired".into(),
                price: None,
                quantity: None,
                trigger_price: Some(Decimal::from(70)),
            },
            12,
        );
        assert!(
            matches!(&out[0], ServerMessage::OrderModifyRejected { reason, .. } if reason == "order has already triggered"),
            "got {out:?}"
        );

        // And on an order that never had a trigger, the reason says THAT.
        let mut e = banded(21);
        e.process(ClientMessage::SubmitOrder(limit_order("plain", 1)), 10);
        let out = e.process(
            ClientMessage::ModifyOrder {
                client_order_id: "plain".into(),
                price: None,
                quantity: None,
                trigger_price: Some(Decimal::from(70)),
            },
            11,
        );
        assert!(
            matches!(&out[0], ServerMessage::OrderModifyRejected { reason, .. } if reason == "order carries no trigger to amend"),
            "got {out:?}"
        );
    }

    #[test]
    fn a_price_amend_on_an_untriggered_stop_limit_keeps_it_conditional() {
        // It changes the limit the order will TAKE, not the price the tape has
        // to touch: the trigger window stands and the order stays conditional.
        // Promoting it here would make the venue fill a stop that never fired.
        let mut e = banded(22);
        e.process(
            ClientMessage::SubmitOrder(stop_order(
                "repriced",
                Side::Sell,
                OrderType::StopLimit,
                100,
                Some(99),
            )),
            10,
        );
        e.process(
            ClientMessage::ModifyOrder {
                client_order_id: "repriced".into(),
                price: Some(Decimal::from(98)),
                quantity: None,
                trigger_price: None,
            },
            20,
        );
        assert!(
            matches!(e.open[0].resting, Resting::Conditional { stop_px } if stop_px == Decimal::from(100))
        );
        assert_eq!(e.open[0].scanned_ns, 10, "the trigger window is untouched");
        assert_eq!(e.open[0].submit.price, Some(Decimal::from(98)));
        let scan = e.pending_scans().remove(0);
        assert_eq!(scan.kind, ScanKind::TriggerTouch);
    }

    #[test]
    fn a_price_amend_on_a_stop_market_is_refused() {
        // It carries no price by construction, so an amend must not be able to
        // give it one - `locked_balances` and the RNG key both read `price` in
        // preference to the trigger.
        let mut e = banded(23);
        e.process(
            ClientMessage::SubmitOrder(stop_order(
                "priceless",
                Side::Sell,
                OrderType::StopMarket,
                90,
                None,
            )),
            10,
        );
        let out = e.process(
            ClientMessage::ModifyOrder {
                client_order_id: "priceless".into(),
                price: Some(Decimal::from(80)),
                quantity: None,
                trigger_price: None,
            },
            20,
        );
        assert!(
            matches!(&out[0], ServerMessage::OrderModifyRejected { reason, .. } if reason == "StopMarket order must not carry a price")
        );
        assert_eq!(e.open[0].submit.price, None);
    }

    #[test]
    fn query_orders_distinguishes_untriggered_triggered_and_partially_filled() {
        let mut e = banded(24);
        let mut stop = stop_order("ladder", Side::Sell, OrderType::StopLimit, 100, Some(99));
        stop.quantity = Decimal::from(2);
        e.process(ClientMessage::SubmitOrder(stop), 10);
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

    fn account(out: &[ServerMessage], index: usize) -> &AccountState {
        let ServerMessage::AccountState(state) = &out[index] else {
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
        let out = e.process(ClientMessage::SubmitOrder(order("F1", 2)), 2);
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

    fn futures_engine(cash: i64, action: BreachAction) -> Engine {
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
            },
        );
        engine
    }

    fn mnq_order(id: &str, side: Side, quantity: i64, price: i64) -> SubmitOrder {
        order_with(id, side, "MNQ", quantity, Some(Decimal::from(price)))
    }

    fn fill_future(engine: &mut Engine, id: &str, side: Side, quantity: i64, price: i64) {
        let events = engine.process_with_market(
            ClientMessage::SubmitOrder(mnq_order(id, side, quantity, price)),
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
                .any(|event| matches!(event, ServerMessage::OrderFilled(_))),
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
                ClientMessage::SubmitOrder(mnq_order(id, Side::Buy, quantity, price)),
                ts,
                Some(MarketReading {
                    last_px: Decimal::from(price),
                    ts_ns: ts,
                    band_ticks: 0,
                }),
            )
            .into_iter()
            .find_map(|event| match event {
                ServerMessage::OrderFilled(fill) => Some(fill),
                _ => None,
            })
            .expect("future fills")
    }

    #[test]
    fn a_futures_fill_books_no_base_currency_leg() {
        let mut engine = futures_engine(10_000, BreachAction::Refuse);
        fill_future(&mut engine, "F-1", Side::Buy, 1, 21_000);
        let state = engine.account_snapshot(2);
        assert_eq!(state.balances.len(), 1);
        assert_eq!(state.balances[0].currency, "USD");
    }

    #[test]
    fn a_futures_position_values_at_multiplier_times_points() {
        let mut engine = futures_engine(10_000, BreachAction::Refuse);
        fill_future(&mut engine, "F-1", Side::Buy, 2, 21_000);
        engine.mark(&[("MNQ".into(), Decimal::from(21_001))], 2);
        assert_eq!(engine.unrealized_pnl("MNQ"), Decimal::from(4));
    }

    #[test]
    fn a_fresh_futures_position_is_marked_at_its_fill_price() {
        let mut engine = futures_engine(10_000, BreachAction::Refuse);
        fill_future(&mut engine, "F-1", Side::Buy, 1, 21_000);
        let position = &engine.account_snapshot(2).positions[0];
        assert_eq!(position.mark_px, Decimal::from(21_000));
        assert_eq!(position.unrealized_pnl, Decimal::ZERO);
    }

    #[test]
    fn flipping_a_futures_position_preserves_its_last_mark() {
        let mut engine = futures_engine(10_000, BreachAction::Refuse);
        fill_future(&mut engine, "F-1", Side::Buy, 1, 21_000);
        engine.mark(&[("MNQ".into(), Decimal::from(21_100))], 2);
        fill_future(&mut engine, "F-2", Side::Sell, 2, 21_050);
        let position = &engine.account_snapshot(3).positions[0];
        assert_eq!(position.quantity, -Decimal::ONE);
        assert_eq!(position.avg_px, Decimal::from(21_050));
        assert_eq!(position.mark_px, Decimal::from(21_100));
    }

    #[test]
    fn a_resting_futures_order_reserves_margin_not_notional() {
        for side in [Side::Buy, Side::Sell] {
            let mut engine = futures_engine(50_000, BreachAction::Refuse);
            let mut order = mnq_order("REST", side, 1, 21_000);
            order.order_type = OrderType::Limit;
            engine.process(ClientMessage::SubmitOrder(order), 1);
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
                    breach_action: BreachAction::Refuse,
                },
            );
            let order = order_with(symbol, Side::Buy, symbol, 1, Some(Decimal::from(21_000)));
            let events = engine.process_with_market(
                ClientMessage::SubmitOrder(order),
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
                    .any(|event| matches!(event, ServerMessage::OrderFilled(_))),
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
        let mut engine = futures_engine(2500, BreachAction::Refuse);
        fill_future(&mut engine, "F-1", Side::Buy, 1, 21_000);
    }

    #[test]
    fn two_reduce_only_legs_reserve_nothing_against_one_position() {
        let mut engine = futures_engine(10_000, BreachAction::Refuse);
        fill_future(&mut engine, "F-1", Side::Buy, 1, 21_000);
        for (id, price) in [("STOP", 20_000), ("TARGET", 22_000)] {
            let mut order = mnq_order(id, Side::Sell, 1, price);
            order.order_type = OrderType::Limit;
            order.reduce_only = true;
            engine.process(ClientMessage::SubmitOrder(order), 2);
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
        let mut engine = futures_engine(10_000, BreachAction::Refuse);
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
        let mut engine = futures_engine(3000, BreachAction::Refuse);
        fill_future(&mut engine, "F-1", Side::Buy, 1, 21_000);
        engine.mark(&[("MNQ".into(), Decimal::from(21_000))], 2);
        let events = engine.process(
            ClientMessage::SubmitOrder(mnq_order("F-2", Side::Sell, 1, 21_000)),
            3,
        );
        assert!(
            !matches!(events.first(), Some(ServerMessage::OrderRejected { reason, .. }) if reason.contains("margin breach")),
            "{events:?}"
        );
    }

    #[test]
    fn a_maintenance_breach_under_refuse_rejects_new_risk_but_not_reduce_only() {
        let mut engine = futures_engine(3000, BreachAction::Refuse);
        fill_future(&mut engine, "F-1", Side::Buy, 1, 21_000);
        engine.mark(&[("MNQ".into(), Decimal::from(20_000))], 2);
        let rejected = engine.process(
            ClientMessage::SubmitOrder(mnq_order("F-2", Side::Buy, 1, 20_000)),
            3,
        );
        // The refusal NAMES ITS CURRENCY. A consumer reads a margin breach as a
        // funds outcome, and every neighbouring funds rejection carries its
        // unit; one that does not leaves the reader guessing which leg is
        // short in a multi-currency account.
        let Some(ServerMessage::OrderRejected { reason, .. }) = rejected.first() else {
            panic!("{rejected:?}");
        };
        assert_eq!(
            reason,
            "margin breach: account equity below maintenance requirement in USD"
        );
        let mut reduce = mnq_order("F-3", Side::Sell, 1, 20_000);
        reduce.reduce_only = true;
        let reduced = engine.process_with_market(
            ClientMessage::SubmitOrder(reduce),
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
                .any(|event| matches!(event, ServerMessage::OrderFilled(_))),
            "{reduced:?}"
        );
    }

    #[test]
    fn a_maintenance_breach_under_liquidate_closes_through_the_fill_band() {
        let mut engine = futures_engine(3000, BreachAction::Liquidate);
        fill_future(&mut engine, "F-1", Side::Buy, 1, 21_000);
        let outcome = engine.mark(&[("MNQ".into(), Decimal::from(20_000))], 2);
        let liquidation = outcome
            .events
            .iter()
            .find_map(|event| match event {
                ServerMessage::OrderFilled(fill) if fill.client_order_id.starts_with("LQ-") => {
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
    fn a_liquidation_bypasses_and_preserves_client_armed_divergences() {
        let mut engine = futures_engine(3_000, BreachAction::Liquidate);
        fill_future(&mut engine, "F-1", Side::Buy, 1, 21_000);
        engine.arm(Divergence::RejectNextSubmit {
            reason: "client scenario".into(),
        });
        engine.arm(Divergence::DuplicateNextFill);
        engine.arm(Divergence::DropNextAccountUpdate);

        let outcome = engine.mark(&[("MNQ".into(), Decimal::from(20_000))], 2);
        assert_eq!(outcome.originated_orders, 1);
        assert_eq!(
            outcome
                .events
                .iter()
                .filter(|event| matches!(event, ServerMessage::OrderFilled(_)))
                .count(),
            1,
            "the venue fill is neither rejected nor duplicated"
        );
        assert!(matches!(
            outcome.events.last(),
            Some(ServerMessage::AccountState(_))
        ));
        engine.mark(&[("MNQ".into(), Decimal::from(20_000))], 3);

        let mut client_order = mnq_order("CLIENT-1", Side::Buy, 1, 20_000);
        client_order.reduce_only = true;
        let rejected = engine.process(ClientMessage::SubmitOrder(client_order), 4);
        assert_eq!(reject_reason(&rejected), "client scenario");

        engine
            .account
            .balances
            .insert("USD".into(), Decimal::from(10_000));
        let filled = engine.process_with_market(
            ClientMessage::SubmitOrder(mnq_order("CLIENT-2", Side::Buy, 1, 20_000)),
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
                .filter(|event| matches!(event, ServerMessage::OrderFilled(_)))
                .count(),
            2
        );
        assert!(
            !filled
                .iter()
                .any(|event| matches!(event, ServerMessage::AccountState(_)))
        );
    }

    #[test]
    fn a_liquidation_neither_pays_nor_spends_an_armed_fee_surcharge() {
        // `FeeSurcharge` is client-armed havoc. A venue-originated liquidation
        // must not be charged it (a large enough multiplier would fail the
        // liquidation's own funds check and leave the breached position open),
        // and must not expire its window either - the arm belongs to the next
        // client fill.
        let mut engine = futures_engine(3_000, BreachAction::Liquidate);
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
                ServerMessage::OrderFilled(fill) => Some(fill),
                _ => None,
            })
            .expect("the liquidation fills rather than failing its funds check");
        assert_eq!(liquidation.commission, Decimal::ONE);

        // The window is still armed and still un-expired: the next CLIENT fill
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
    fn a_spot_symbol_carrying_a_margin_policy_still_reserves_notional() {
        // `held_for` and `locked_balances` derive from one `order_reservation`,
        // so a margin policy attached to a SPOT symbol - which server config
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
                breach_action: BreachAction::Refuse,
            },
        );
        let mut resting = order_with("HOLD", Side::Buy, "BTCUSDT", 1, Some(Decimal::from(50)));
        resting.order_type = OrderType::Limit;
        let out =
            engine.process_with_market(ClientMessage::SubmitOrder(resting), 1, Some(reading(0)));
        assert!(matches!(out[0], ServerMessage::OrderAccepted { .. }));
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
                .any(|event| matches!(event, ServerMessage::OrderFilled(_)))
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
        let out =
            engine.process_with_market(ClientMessage::SubmitOrder(resting), 1, Some(reading(0)));
        assert!(matches!(out[0], ServerMessage::OrderAccepted { .. }));

        // 2 @ 50 is 100 of notional plus 2 of commission. Free is 51 and the
        // order's own hold is 50, so 101 covers the notional but not the fee.
        let out = engine.process(
            ClientMessage::ModifyOrder {
                client_order_id: "R1".into(),
                price: None,
                quantity: Some(Decimal::from(2)),
                trigger_price: None,
            },
            2,
        );
        let [ServerMessage::OrderModifyRejected { reason, .. }] = &out[..] else {
            panic!("expected one modify reject, got {out:?}")
        };
        assert_eq!(reason, "insufficient USDT balance");
        assert_eq!(engine.open_orders()[0].leaves_qty, Decimal::ONE);
    }

    #[test]
    fn per_contract_fees_ignore_price_and_scale_with_contracts() {
        let mut engine = futures_engine(20_000, BreachAction::Refuse);
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
        let out = engine.process(ClientMessage::SubmitOrder(order("FEE", 1)), 1);
        assert_eq!(reject_reason(&out), "insufficient USDT balance");
        assert_eq!(
            balance(&engine.account_snapshot(2), "USDT").total,
            Decimal::from(100)
        );

        // The door check, not just the fill check: a limit that RESTS never
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
        let out =
            engine.process_with_market(ClientMessage::SubmitOrder(resting), 3, Some(reading(0)));
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
        let out = engine.process(ClientMessage::SubmitOrder(sell), 1);
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
        let out =
            engine.process_with_market(ClientMessage::SubmitOrder(resting), 3, Some(reading(0)));
        assert_eq!(reject_reason(&out), "insufficient USDT balance");
        assert!(engine.open_orders().is_empty());
    }

    #[test]
    fn basis_point_fees_on_a_future_charge_multiplier_aware_notional() {
        let mut engine = futures_engine(20_000, BreachAction::Refuse);
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
        let mut engine = futures_engine(20_000, BreachAction::Refuse);
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
        let mut engine = futures_engine(20_000, BreachAction::Refuse);
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
        let mut engine = futures_engine(20_000, BreachAction::Refuse);
        let mut buy = mnq_order("NET-1", Side::Buy, 2, 21_000);
        buy.position_id = Some("CLIENT-LONG".into());
        let mut sell = mnq_order("NET-2", Side::Sell, 1, 21_000);
        sell.position_id = Some("CLIENT-SHORT".into());
        for order in [buy, sell] {
            engine.process_with_market(
                ClientMessage::SubmitOrder(order),
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
        let mut engine = futures_engine(20_000, BreachAction::Refuse);
        engine.set_oms_type(mogwai_protocol::OmsType::Hedging);
        let mut buy = mnq_order("HEDGE-1", Side::Buy, 1, 21_000);
        buy.position_id = Some("LONG".into());
        let mut sell = mnq_order("HEDGE-2", Side::Sell, 1, 21_000);
        sell.position_id = Some("SHORT".into());
        for order in [buy, sell] {
            engine.process_with_market(
                ClientMessage::SubmitOrder(order),
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
        let mut engine = futures_engine(20_000, BreachAction::Refuse);
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
        let mut engine = futures_engine(20_000, BreachAction::Refuse);
        engine.set_oms_type(mogwai_protocol::OmsType::Hedging);
        let mut order = mnq_order("HEDGE-1", Side::Buy, 1, 21_000);
        order.position_id = Some("BOOK-7".into());
        let events = engine.process_with_market(
            ClientMessage::SubmitOrder(order),
            1,
            Some(MarketReading {
                last_px: Decimal::from(21_000),
                ts_ns: 1,
                band_ticks: 0,
            }),
        );
        assert!(events.iter().any(|event| matches!(event, ServerMessage::OrderFilled(fill) if fill.position_id.as_deref() == Some("BOOK-7"))));
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

    /// `order` is a MARKET order; a resting limit needs the type set.
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
        let out = e.process(ClientMessage::SubmitOrder(order("legacy", 1)), 7);
        assert!(
            matches!(out.as_slice(), [ServerMessage::OrderAccepted { .. }, ServerMessage::OrderFilled(fill), ServerMessage::AccountState(_)] if fill.last_px == Decimal::from(100))
        );
        assert!(e.pending_scans().is_empty());
    }

    #[test]
    fn a_submit_with_no_reading_rests_rather_than_filling() {
        let mut e = banded(1);
        let out = e.process(ClientMessage::SubmitOrder(limit_order("rest", 2)), 7);
        assert!(matches!(
            out.as_slice(),
            [
                ServerMessage::OrderAccepted { .. },
                ServerMessage::AccountState(_)
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
            ClientMessage::SubmitOrder(limit_order("cross", 1)),
            7,
            Some(reading(0)),
        );
        assert!(matches!(
            out.as_slice(),
            [
                ServerMessage::OrderAccepted { .. },
                ServerMessage::OrderFilled(_),
                ServerMessage::AccountState(_)
            ]
        ));
        assert!(e.pending_scans().is_empty());

        let wide = reading(10_000);
        let out = e.process_with_market(
            ClientMessage::SubmitOrder(limit_order("short", 1)),
            8,
            Some(wide),
        );
        assert!(
            !out.iter()
                .any(|event| matches!(event, ServerMessage::OrderFilled(_))),
            "a limit whose trigger the reading has not reached must rest"
        );
        let scan = e.pending_scans().remove(0);
        assert!(scan.px < wide.last_px);
    }

    #[test]
    fn a_zero_band_reduces_to_a_strict_through_trigger_at_the_stated_price() {
        // The degenerate case of the model, which `fill_band_vol_mult = 0.0`
        // configures: the trigger IS the stated price, so a print AT it is the
        // market touching rather than trading through and does not fill.
        let mut e = banded(9);
        e.process(ClientMessage::SubmitOrder(limit_order("degenerate", 1)), 10);
        assert!(
            matches!(e.open[0].resting, Resting::Limit { fill_trigger_px } if fill_trigger_px == Decimal::from(100))
        );
        let at_touch = e.process_with_market(
            ClientMessage::SubmitOrder(limit_order("touch", 1)),
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
                .any(|event| matches!(event, ServerMessage::OrderFilled(_)))
        );
    }

    #[test]
    fn a_truncated_scan_advances_only_what_it_covered() {
        let mut e = banded(3);
        e.process(ClientMessage::SubmitOrder(limit_order("short", 1)), 10);
        let scan = e.pending_scans().remove(0);
        e.apply_scans(&[result(&scan, false, 12)], 99);
        assert_eq!(e.open[0].band_draw, 0);
        assert_eq!(e.open[0].scanned_ns, 12);
    }

    #[test]
    fn a_scan_against_a_stale_revision_is_dropped() {
        let mut e = banded(2);
        e.process(ClientMessage::SubmitOrder(limit_order("stale", 1)), 10);
        let scan = e.pending_scans().remove(0);
        e.apply_scans(&[result(&scan, false, 20)], 20);
        let (_, emitted) = e.apply_scans(&[result(&scan, true, 20)], 20);
        assert_eq!(emitted, 0);
        assert_eq!(e.open[0].band_draw, 0);
    }

    #[test]
    fn a_price_amend_redraws_the_trigger_and_a_quantity_amend_does_not() {
        // Asserted on `band_draw` and `scanned_ns`, never on the trigger PRICE
        // being unequal: a redraw may legitimately land on the same offset, so
        // a test asserting the price moved would be flaky by construction.
        let mut e = banded(3);
        e.process(ClientMessage::SubmitOrder(limit_order("amend", 2)), 10);
        let scan = e.pending_scans().remove(0);
        e.apply_scans(&[result(&scan, false, 20)], 20);
        e.process(
            ClientMessage::ModifyOrder {
                client_order_id: "amend".into(),
                price: None,
                quantity: Some(Decimal::from(3)),
                trigger_price: None,
            },
            30,
        );
        assert_eq!(e.open[0].band_draw, 0);
        e.process(
            ClientMessage::ModifyOrder {
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
    fn a_price_amend_adopts_a_fresh_band_when_the_server_supplies_one() {
        let mut e = banded(3);
        e.process_with_market(
            ClientMessage::SubmitOrder(limit_order("regime", 2)),
            10,
            Some(MarketReading {
                last_px: Decimal::from(101),
                ts_ns: 0,
                band_ticks: 4,
            }),
        );
        assert_eq!(e.open[0].band_ticks, 4);
        let amend = |price: i64| ClientMessage::ModifyOrder {
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
        e.process(ClientMessage::SubmitOrder(limit_order("swept", 1)), 10);
        let scan = e.pending_scans().remove(0);
        let (out, emitted) = e.apply_scans(&[result(&scan, true, 20)], 20);
        assert_eq!(emitted, 1);
        // Accept-free: the fill is unsolicited, and it prints at the ORDER'S
        // price, never the triggering trade's - the trigger decides WHEN.
        assert!(matches!(
            out.as_slice(),
            [ServerMessage::OrderFilled(fill), ServerMessage::AccountState(_)]
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
        e.process(ClientMessage::SubmitOrder(limit_order("part", 2)), 10);
        let scan = e.pending_scans().remove(0);
        let (out, emitted) = e.apply_scans(&[result(&scan, true, 20)], 20);
        assert_eq!(emitted, 1);
        assert!(matches!(
            out.first(),
            Some(ServerMessage::OrderFilled(fill))
                if fill.last_qty == Decimal::ONE && fill.leaves_qty == Decimal::ONE
        ));
        assert_eq!(e.open[0].band_draw, 1);
        assert_eq!(e.open[0].scanned_ns, 20);
        assert_eq!(e.open[0].leaves_qty, Decimal::ONE);
    }

    #[test]
    fn a_swept_fill_sizes_off_the_remaining_quantity() {
        // The second sweep multiplies its fraction by the LEAVES, not by the
        // original quantity, so it cannot over-fill a partly filled order.
        let mut e = banded(1);
        e.arm(Divergence::PartialFillNext {
            client_order_id: "leaves".into(),
            fraction: Decimal::new(5, 1),
        });
        e.process(ClientMessage::SubmitOrder(limit_order("leaves", 4)), 10);
        let scan = e.pending_scans().remove(0);
        e.apply_scans(&[result(&scan, true, 20)], 20);
        assert_eq!(e.open[0].leaves_qty, Decimal::from(2));
        let scan = e.pending_scans().remove(0);
        let (out, _) = e.apply_scans(&[result(&scan, true, 30)], 30);
        assert!(matches!(
            out.first(),
            Some(ServerMessage::OrderFilled(fill))
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
        e.process(ClientMessage::SubmitOrder(limit_order("armed", 2)), 10);
        assert_eq!(e.armed.len(), 1);
        let scan = e.pending_scans().remove(0);
        let (out, _) = e.apply_scans(&[result(&scan, true, 20)], 20);
        assert!(matches!(
            out.first(),
            Some(ServerMessage::OrderFilled(fill)) if fill.last_qty == Decimal::ONE
        ));
        assert!(e.armed.is_empty());
    }

    #[test]
    fn a_duplicate_fill_divergence_applies_to_a_swept_fill() {
        let mut e = banded(1);
        e.process(ClientMessage::SubmitOrder(limit_order("dup", 1)), 10);
        e.arm(Divergence::DuplicateNextFill);
        let scan = e.pending_scans().remove(0);
        let (out, emitted) = e.apply_scans(&[result(&scan, true, 20)], 20);
        // Two wire fills, ONE booked into the truth store, one account state.
        assert_eq!(emitted, 1);
        assert_eq!(
            out.iter()
                .filter(|event| matches!(event, ServerMessage::OrderFilled(_)))
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
        let out = e.process_with_market(ClientMessage::SubmitOrder(fok), 1, Some(reading(0)));
        assert!(
            out.iter()
                .any(|event| matches!(event, ServerMessage::OrderFilled(_)))
        );

        // A MARKET order arrives price-stamped by the server; it is marketable
        // by definition and never rests on the honest path.
        let market = order_with("mkt", Side::Buy, "BTCUSDT", 1, None);
        let market = SubmitOrder {
            price: Some(Decimal::from(100)),
            ..market
        };
        let out = e.process(ClientMessage::SubmitOrder(market), 2);
        assert!(
            out.iter()
                .any(|event| matches!(event, ServerMessage::OrderFilled(_)))
        );
        assert!(e.pending_scans().is_empty());
    }

    #[test]
    fn a_market_remainder_left_resting_by_havoc_is_never_scanned() {
        // A MARKET order never draws a trigger, but an armed partial can leave one
        // RESTING with a server-stamped price. Handing that remainder to the
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
        e.process(ClientMessage::SubmitOrder(market), 1);
        assert_eq!(e.open[0].leaves_qty, Decimal::ONE);
        assert!(e.pending_scans().is_empty());
    }

    #[test]
    fn a_dropped_account_update_survives_a_resting_accept_and_applies_to_the_swept_fill() {
        let mut e = banded(1);
        e.arm(Divergence::DropNextAccountUpdate);
        let accepted = e.process(ClientMessage::SubmitOrder(limit_order("drop", 1)), 10);
        assert!(matches!(
            accepted.last(),
            Some(ServerMessage::AccountState(_))
        ));
        let scan = e.pending_scans().remove(0);
        let (out, _) = e.apply_scans(&[result(&scan, true, 20)], 20);
        assert!(matches!(out.as_slice(), [ServerMessage::OrderFilled(_)]));
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
        // partial forces rather than the short-of-trigger one.
        let rejected =
            e.process_with_market(ClientMessage::SubmitOrder(fok.clone()), 1, Some(reading(0)));
        assert!(matches!(
            rejected.as_slice(),
            [ServerMessage::OrderRejected { reason, .. }] if !reason.contains("trigger")
        ));
        let accepted = e.process_with_market(ClientMessage::SubmitOrder(fok), 2, Some(reading(0)));
        assert!(matches!(
            accepted.first(),
            Some(ServerMessage::OrderAccepted { .. })
        ));
    }

    #[test]
    fn an_ioc_short_of_its_trigger_cancels_and_a_fok_short_of_its_trigger_is_rejected() {
        let mut e = banded(1);
        let mut miss = limit_order("ioc-miss", 1);
        miss.time_in_force = TimeInForce::Ioc;
        let out = e.process(ClientMessage::SubmitOrder(miss), 1);
        assert!(matches!(
            out.as_slice(),
            [
                ServerMessage::OrderAccepted { .. },
                ServerMessage::OrderCanceled { .. },
                ServerMessage::AccountState(_)
            ]
        ));
        let mut hit = limit_order("ioc-hit", 1);
        hit.time_in_force = TimeInForce::Ioc;
        let out = e.process_with_market(ClientMessage::SubmitOrder(hit), 2, Some(reading(0)));
        assert!(
            out.iter()
                .any(|event| matches!(event, ServerMessage::OrderFilled(_)))
        );
        assert!(e.pending_scans().is_empty());

        // A FOK short of its trigger is REJECTED rather than cancelled, and
        // stops being the free fill it was: it is decided now or never, and now
        // means against the trigger like everything else.
        let mut fok = limit_order("fok-short", 1);
        fok.time_in_force = TimeInForce::Fok;
        let out = e.process(ClientMessage::SubmitOrder(fok), 3);
        assert!(matches!(
            out.as_slice(),
            [ServerMessage::OrderRejected { reason, .. }]
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
        let out = e.process(ClientMessage::SubmitOrder(order("B1", 11)), 1);
        assert_eq!(reject_reason(&out), "insufficient USDT balance");

        // Sell with no base at all.
        let out = e.process(
            ClientMessage::SubmitOrder(order_with(
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
        let out = e.process(ClientMessage::SubmitOrder(order("B2", 5)), 3);
        assert!(matches!(out[0], ServerMessage::OrderAccepted { .. }));
        let out = e.process(ClientMessage::SubmitOrder(order("B3", 6)), 4);
        assert_eq!(reject_reason(&out), "insufficient USDT balance");
        let out = e.process(ClientMessage::SubmitOrder(order("B4", 5)), 5);
        assert!(matches!(out[0], ServerMessage::OrderAccepted { .. }));

        // The acquired base is spendable: selling it back clears.
        let out = e.process(
            ClientMessage::SubmitOrder(order_with(
                "S2",
                Side::Sell,
                "BTCUSDT",
                10,
                Some(Decimal::from(100)),
            )),
            6,
        );
        assert!(matches!(out[0], ServerMessage::OrderAccepted { .. }));
    }

    #[test]
    fn funded_account_counts_reservations_and_gates_amends() {
        // A resting buy's reservation reduces free balance for later submits,
        // and an amend that grows the reservation past free-plus-own-hold is
        // refused - the venue must never advertise free < 0 in its own
        // snapshot.
        let mut e = funded(1_000);

        // Rest half of a 4 @ 100 buy: 200 spent on the fill, 200 locked for
        // the remainder, so free is 600.
        e.arm(Divergence::PartialFillNext {
            client_order_id: "R1".into(),
            fraction: Decimal::new(5, 1),
        });
        let out = e.process(ClientMessage::SubmitOrder(order("R1", 4)), 1);
        let state = account(&out, out.len() - 1);
        assert_eq!(balance(state, "USDT").free, Decimal::from(600));
        assert_eq!(balance(state, "USDT").locked, Decimal::from(200));

        // 7 @ 100 exceeds the 600 free even though the total is 800.
        let out = e.process(ClientMessage::SubmitOrder(order("B1", 7)), 2);
        assert_eq!(reject_reason(&out), "insufficient USDT balance");

        // Amending the resting order up to 8 total (6 leaves = 600 hold) fits:
        // 600 free plus its own 200 hold covers it. Afterwards the whole 800
        // of unspent quote backs this one order (free 200, hold 600).
        let out = e.process(
            ClientMessage::ModifyOrder {
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
            ClientMessage::ModifyOrder {
                client_order_id: "R1".into(),
                price: None,
                quantity: Some(Decimal::from(11)),
                trigger_price: None,
            },
            4,
        );
        let [ServerMessage::OrderModifyRejected { reason, .. }] = &out[..] else {
            panic!("expected one modify reject, got {out:?}")
        };
        assert_eq!(reason, "insufficient USDT balance");

        // Canceling the resting order frees its hold; the refused submit now
        // clears (a rejected id is free to reuse).
        let out = e.process(
            ClientMessage::CancelOrder {
                client_order_id: "R1".into(),
            },
            5,
        );
        assert!(matches!(out[0], ServerMessage::OrderCanceled { .. }));
        let out = e.process(ClientMessage::SubmitOrder(order("B1", 7)), 6);
        assert!(matches!(out[0], ServerMessage::OrderAccepted { .. }));
    }

    // The reconciliation is a `cfg!(debug_assertions)` check, so this pins it
    // in the profile that runs it. Without the gate the test FAILS in a
    // release test sweep, where nothing panics by design.
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "resting-order reservation cache drifted from the book")]
    fn reservation_cache_reconciliation_catches_drift_before_a_funded_command() {
        let mut e = funded(1_000);
        e.arm(Divergence::PartialFillNext {
            client_order_id: "DRIFT".into(),
            fraction: Decimal::new(5, 1),
        });
        e.process(ClientMessage::SubmitOrder(order("DRIFT", 4)), 1);
        e.corrupt_order_locked_for_test("USDT", Decimal::ZERO);

        e.process(
            ClientMessage::QueryFills {
                request_id: "Q".into(),
                client_order_id: None,
            },
            2,
        );
    }

    #[test]
    fn a_zero_initial_margin_policy_cannot_drift_the_reservation_cache() {
        // A zero hold must be NO cache entry, not an entry whose amount is
        // zero: the reconciliation fold and the incremental remove would
        // otherwise disagree about whether the currency KEY exists while
        // agreeing on every amount, and the debug reconciliation would panic
        // on states that are economically identical.
        let mut engine = futures_engine(10_000, BreachAction::Refuse);
        engine.set_margin_policy(
            "MNQ".into(),
            MarginPolicy {
                initial_per_contract: Decimal::ZERO,
                maintenance_per_contract: Decimal::ZERO,
                breach_action: BreachAction::Refuse,
            },
        );
        for id in ["Z1", "Z2"] {
            let mut order =
                order_decimal(id, Side::Buy, "MNQ", Decimal::ONE, Some(Decimal::from(100)));
            order.order_type = OrderType::Limit;
            engine.process(ClientMessage::SubmitOrder(order), 1);
        }
        engine.process(
            ClientMessage::CancelOrder {
                client_order_id: "Z1".into(),
            },
            2,
        );
        // The next command reconciles the aggregate against the fold over the
        // one remaining zero-hold order; both must say "no entry".
        engine.process(
            ClientMessage::CancelOrder {
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
            e.process(ClientMessage::SubmitOrder(order(id, 2)), 1);
        }

        e.process(
            ClientMessage::CancelOrder {
                client_order_id: "I1".into(),
            },
            2,
        );
        let out = e.process(
            ClientMessage::ModifyOrder {
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
        let out = e.process(ClientMessage::SubmitOrder(order("U1", 5)), 1);
        assert!(matches!(out[0], ServerMessage::OrderAccepted { .. }));
        let state = account(&out, out.len() - 1);
        assert_eq!(balance(state, "USDT").total, Decimal::from(-500));
    }

    fn fill(out: &[ServerMessage], index: usize) -> &OrderFilled {
        let ServerMessage::OrderFilled(fill) = &out[index] else {
            panic!("expected fill")
        };
        fill
    }

    fn updated(out: &[ServerMessage], index: usize) -> &ServerMessage {
        let ServerMessage::OrderUpdated { .. } = &out[index] else {
            panic!("expected order updated")
        };
        &out[index]
    }

    fn reject_reason(out: &[ServerMessage]) -> &str {
        let [ServerMessage::OrderRejected { reason, .. }] = out else {
            panic!("expected one order reject")
        };
        reason
    }

    fn cancel_reject_reason(out: &[ServerMessage]) -> &str {
        let [ServerMessage::OrderCancelRejected { reason, .. }] = out else {
            panic!("expected one order cancel reject")
        };
        reason
    }

    fn accepted_venue_id(out: &[ServerMessage]) -> VenueOrderId {
        let ServerMessage::OrderAccepted { venue_order_id, .. } = &out[0] else {
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
        let out = e.process(ClientMessage::SubmitOrder(order("O1", 10)), 1);
        assert_eq!(out.len(), 3);
        assert!(matches!(out[0], ServerMessage::OrderAccepted { .. }));
        let ServerMessage::OrderFilled(f) = &out[1] else {
            panic!("expected fill")
        };
        assert_eq!(f.leaves_qty, Decimal::ZERO);
        assert!(matches!(out[2], ServerMessage::AccountState(_)));
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

        for (order, expected) in cases {
            let mut e = Engine::new();
            let out = e.process(ClientMessage::SubmitOrder(order), 1);

            assert_eq!(reject_reason(&out), expected);
            assert!(e.account_snapshot(2).balances.is_empty());
            assert!(e.positions().is_empty());
            assert!(e.open_orders().is_empty());
        }
    }

    #[test]
    fn duplicate_client_order_id_is_rejected_after_acceptance() {
        let mut e = Engine::new();

        let first = e.process(ClientMessage::SubmitOrder(order("DUP", 1)), 1);
        assert!(matches!(first[0], ServerMessage::OrderAccepted { .. }));

        let duplicate = e.process(ClientMessage::SubmitOrder(order("DUP", 1)), 2);
        assert_eq!(reject_reason(&duplicate), "duplicate client_order_id");
    }

    #[test]
    fn armed_partial_leaves_remainder_resting() {
        let mut e = Engine::new();
        e.arm(Divergence::PartialFillNext {
            client_order_id: "O1".into(),
            fraction: Decimal::from_f64(0.3).unwrap(),
        });
        let out = e.process(ClientMessage::SubmitOrder(order("O1", 10)), 1);
        assert_eq!(out.len(), 3);
        let ServerMessage::OrderFilled(f) = &out[1] else {
            panic!("expected fill")
        };
        assert_eq!(f.last_qty, Decimal::from(3));
        assert_eq!(f.leaves_qty, Decimal::from(7));
        assert!(matches!(out[2], ServerMessage::AccountState(_)));
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

        let out = e.process(ClientMessage::SubmitOrder(order), 1);

        assert_eq!(out.len(), 4);
        assert!(matches!(out[0], ServerMessage::OrderAccepted { .. }));
        let f = fill(&out, 1);
        assert_eq!(f.last_qty, Decimal::from(4));
        assert_eq!(f.leaves_qty, Decimal::from(6));
        assert!(matches!(out[2], ServerMessage::OrderCanceled { .. }));
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

        let out = e.process(ClientMessage::SubmitOrder(order), 1);

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
        let out = e.process(ClientMessage::SubmitOrder(order("O1", 10)), 1);
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0], ServerMessage::OrderRejected { .. }));
    }

    #[test]
    fn duplicate_next_fill_doubles_the_wire_event() {
        let mut e = Engine::new();
        e.arm(Divergence::DuplicateNextFill);

        let out = e.process(ClientMessage::SubmitOrder(order("O1", 10)), 1);

        assert_eq!(out.len(), 4);
        assert!(matches!(out[0], ServerMessage::OrderAccepted { .. }));
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

        let out = e.process(ClientMessage::SubmitOrder(order("O1", 10)), 1);

        assert_eq!(out.len(), 2);
        assert!(matches!(out[0], ServerMessage::OrderAccepted { .. }));
        assert!(matches!(out[1], ServerMessage::OrderFilled(_)));
        let state = e.account_snapshot(2);
        assert_eq!(balance(&state, "BTC").total, Decimal::from(10));
    }

    #[test]
    fn duplicate_and_drop_compose_on_one_submit() {
        let mut e = Engine::new();
        e.arm(Divergence::DuplicateNextFill);
        e.arm(Divergence::DropNextAccountUpdate);

        let out = e.process(ClientMessage::SubmitOrder(order("O1", 10)), 1);

        assert_eq!(out.len(), 3);
        assert!(matches!(out[0], ServerMessage::OrderAccepted { .. }));
        assert!(matches!(out[1], ServerMessage::OrderFilled(_)));
        assert!(matches!(out[2], ServerMessage::OrderFilled(_)));
    }

    #[test]
    fn drop_skips_rejected_submit_and_fires_on_next_fill() {
        let mut e = Engine::new();
        e.arm(Divergence::RejectNextSubmit {
            reason: "risk".into(),
        });
        e.arm(Divergence::DropNextAccountUpdate);

        let rejected = e.process(ClientMessage::SubmitOrder(order("O1", 10)), 1);
        assert_eq!(rejected.len(), 1);
        assert!(matches!(rejected[0], ServerMessage::OrderRejected { .. }));

        let filled = e.process(ClientMessage::SubmitOrder(order("O2", 10)), 2);
        assert_eq!(filled.len(), 2);
        assert!(matches!(filled[0], ServerMessage::OrderAccepted { .. }));
        assert!(matches!(filled[1], ServerMessage::OrderFilled(_)));
    }

    #[test]
    fn a_zero_quantity_partial_leaves_drop_next_account_update_armed() {
        // A wire-valid `PartialFillNext` fraction flooring below one size
        // increment on a minimum-lot order fills NOTHING, so the order merely
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
            ClientMessage::SubmitOrder(order_decimal(
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
                .any(|event| matches!(event, ServerMessage::AccountState(_))),
            "nothing filled, so the resting acceptance still owes its snapshot"
        );
        // Still armed; the cancel that frees the hold is what spends it.
        let canceled = e.process(
            ClientMessage::CancelOrder {
                client_order_id: "Z0".into(),
            },
            2,
        );
        assert!(
            !canceled
                .iter()
                .any(|event| matches!(event, ServerMessage::AccountState(_)))
        );
        assert!(e.armed.is_empty());
    }

    #[test]
    fn arm_drops_temporal_variants_without_blocking_engine_divergences() {
        let mut e = Engine::new();
        e.arm(Divergence::DelayAcks { ms: 100 });
        e.arm(Divergence::GoDark { ms: 100 });
        e.arm(Divergence::StallData { ms: 100 });
        e.arm(Divergence::ClearDivergences);
        e.arm(Divergence::DuplicateNextFill);

        let out = e.process(ClientMessage::SubmitOrder(order("O1", 10)), 1);

        assert_eq!(out.len(), 4);
        assert!(matches!(out[1], ServerMessage::OrderFilled(_)));
        assert!(matches!(out[2], ServerMessage::OrderFilled(_)));
    }

    #[test]
    fn clear_divergences_is_dropped_not_queued() {
        let mut e = Engine::new();
        e.arm(Divergence::ClearDivergences);

        let out = e.process(ClientMessage::SubmitOrder(order("O1", 10)), 1);

        assert_eq!(out.len(), 3);
        assert!(matches!(out[0], ServerMessage::OrderAccepted { .. }));
        assert!(matches!(out[1], ServerMessage::OrderFilled(_)));
        assert!(matches!(out[2], ServerMessage::AccountState(_)));
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
        let out = e.process(ClientMessage::SubmitOrder(order("O1", 10)), 1);
        assert_eq!(
            out.len(),
            4,
            "duplicate fill should still fire behind the parked partial"
        );
        assert!(matches!(out[0], ServerMessage::OrderAccepted { .. }));
        let first = fill(&out, 1);
        let second = fill(&out, 2);
        assert_eq!(
            first.last_qty,
            Decimal::from(10),
            "O1 fills fully, untouched by the O2 partial"
        );
        assert_eq!(first.trade_id, second.trade_id);
        assert!(matches!(out[3], ServerMessage::AccountState(_)));

        // The O2-targeted partial is still armed and now applies to O2.
        let out = e.process(ClientMessage::SubmitOrder(order("O2", 10)), 2);
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
        let out = e.process(ClientMessage::SubmitOrder(order("O1", 10)), 1);

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
        let out = e.process(ClientMessage::SubmitOrder(order("O1", 10)), 1);

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
        let out = e.process(ClientMessage::SubmitOrder(order("O1", 10)), 1);

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
            let out = e.process(ClientMessage::SubmitOrder(order("O1", 10)), 1);

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
        // FULL fill with a misleading "produced non-positive last_qty" warn -
        // silently inverting the divergence and, for a FOK, letting an order
        // the partial was armed to kill fully fill and pass. The fix fills
        // ZERO: the FOK gate now rejects on the full leaves, and a GTC rests.
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
        let out = e.process(ClientMessage::SubmitOrder(fok), 1);
        assert_eq!(reject_reason(&out), "fill-or-kill could not fully fill");
        assert!(e.open_orders().is_empty());
        assert!(e.account_snapshot(2).balances.is_empty());
        assert!(e.positions().is_empty());

        // GTC: accepted, NO fill event emitted, and the order rests fully open
        // with the whole lot as leaves. The snapshot shows only the locked
        // quote reservation - nothing filled, so no base/position leg.
        let mut e = Engine::new();
        e.arm(Divergence::PartialFillNext {
            client_order_id: "GTC".into(),
            fraction: Decimal::from_f64(0.3).unwrap(),
        });
        let gtc = order_decimal("GTC", Side::Buy, "BTCUSDT", lot, Some(px));
        let out = e.process(ClientMessage::SubmitOrder(gtc), 1);
        assert_eq!(out.len(), 2, "accept + account state only, no fill event");
        assert!(matches!(out[0], ServerMessage::OrderAccepted { .. }));
        assert!(matches!(out[1], ServerMessage::AccountState(_)));
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
        let out = e.process(ClientMessage::SubmitOrder(order("NEVER", 10)), 1);
        let f = fill(&out, 1);
        assert_eq!(f.last_qty, Decimal::from(10));
        assert_eq!(f.leaves_qty, Decimal::ZERO);
    }

    #[test]
    fn armed_divergence_queue_is_bounded() {
        // E5: arming well past the cap with targeted partials whose orders
        // never arrive (no trigger to self-disarm) must not grow the queue
        // without bound - it saturates at `MAX_ARMED_DIVERGENCES`, shedding the
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
        // The eviction must be VISIBLE, not just logged: the control-plane ack
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
        // The OLDEST entry is the one that goes.
        assert!(matches!(
            shed,
            Some(Divergence::PartialFillNext { ref client_order_id, .. }) if client_order_id == "O-0"
        ));
    }

    #[test]
    fn arm_of_a_server_owned_variant_sheds_nothing() {
        // The server-owned and immediate variants never enter the queue, so
        // they can neither displace an entry nor report one - the ack for them
        // must stay a bare accept.
        let mut e = Engine::new();
        for i in 0..MAX_ARMED_DIVERGENCES {
            e.arm(Divergence::PartialFillNext {
                client_order_id: format!("O-{i}"),
                fraction: Decimal::ONE,
            });
        }
        assert!(e.arm(Divergence::ClearDivergences).is_none());
        assert!(e.arm(Divergence::DelayAcks { ms: 10 }).is_none());
        assert_eq!(e.armed.len(), MAX_ARMED_DIVERGENCES);
    }

    #[test]
    fn buy_fill_moves_base_and_quote_balances() {
        let mut e = Engine::new();
        let out = e.process(ClientMessage::SubmitOrder(order("O1", 10)), 1);
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
        let out = e.process(ClientMessage::SubmitOrder(order), 1);
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
        // Two individually-valid orders whose COMBINED notional exceeds
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
            ClientMessage::SubmitOrder(order_decimal("O1", Side::Buy, "BTCUSDT", qty, Some(px))),
            1,
        );
        assert!(matches!(first[0], ServerMessage::OrderAccepted { .. }));

        let out = e.process(
            ClientMessage::SubmitOrder(order_decimal("O2", Side::Buy, "BTCUSDT", qty, Some(px))),
            2,
        );

        assert_eq!(out.len(), 3);
        assert!(matches!(out[0], ServerMessage::OrderAccepted { .. }));
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
    fn resting_reservations_saturate_locked_instead_of_panicking() {
        // Two resting buys whose SUMMED reservations (`leaves_qty * price`,
        // each individually within range and `checked_mul`-approved at
        // submit) exceed `Decimal::MAX`. A tiny armed partial (1e-9 of 7e20
        // = 7e11, still on the 1e-8 grid) leaves almost the whole quantity
        // resting, so each order locks just under 7e28 of quote. Before the
        // clamped helpers the second snapshot panicked in `locked_balances`'
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
            ClientMessage::SubmitOrder(order_decimal("O1", Side::Buy, "BTCUSDT", qty, Some(px))),
            1,
        );
        let out = e.process(
            ClientMessage::SubmitOrder(order_decimal("O2", Side::Buy, "BTCUSDT", qty, Some(px))),
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
        e.process(ClientMessage::SubmitOrder(order("O1", 10)), 1);
        let second = order_with("O2", Side::Buy, "BTCUSDT", 10, Some(Decimal::from(200)));
        let out = e.process(ClientMessage::SubmitOrder(second), 2);
        let pos = position(account(&out, 2), "BTCUSDT");

        assert_eq!(pos.quantity, Decimal::from(20));
        assert_eq!(pos.avg_px, Decimal::from(150));
    }

    #[test]
    fn position_reduce_keeps_avg_px_and_shrinks_qty() {
        let mut e = Engine::new();
        e.process(ClientMessage::SubmitOrder(order("O1", 10)), 1);
        let reduce = order_with("O2", Side::Sell, "BTCUSDT", 4, Some(Decimal::from(150)));
        let out = e.process(ClientMessage::SubmitOrder(reduce), 2);
        let pos = position(account(&out, 2), "BTCUSDT");

        assert_eq!(pos.quantity, Decimal::from(6));
        assert_eq!(pos.avg_px, Decimal::from(100));
    }

    #[test]
    fn position_flip_reopens_at_fill_price() {
        let mut e = Engine::new();
        e.process(ClientMessage::SubmitOrder(order("O1", 5)), 1);
        let flip = order_with("O2", Side::Sell, "BTCUSDT", 8, Some(Decimal::from(120)));
        let out = e.process(ClientMessage::SubmitOrder(flip), 2);
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
        let out = e.process(ClientMessage::SubmitOrder(order("O1", 10)), 1);
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
    fn cancel_frees_reservation_and_emits_account_state() {
        let mut e = Engine::new();
        e.arm(Divergence::PartialFillNext {
            client_order_id: "O1".into(),
            fraction: Decimal::from_f64(0.3).unwrap(),
        });
        e.process(ClientMessage::SubmitOrder(order("O1", 10)), 1);

        let out = e.process(
            ClientMessage::CancelOrder {
                client_order_id: "O1".into(),
            },
            2,
        );
        assert_eq!(out.len(), 2);
        assert!(matches!(out[0], ServerMessage::OrderCanceled { .. }));
        let state = account(&out, 1);

        let usdt = balance(state, "USDT");
        assert_eq!(usdt.total, Decimal::from(-300));
        assert_eq!(usdt.locked, Decimal::ZERO);
        assert_eq!(usdt.free, Decimal::from(-300));
    }

    #[test]
    fn cancel_of_already_filled_order_distinguishes_terminal_from_unknown() {
        // A limit on the no-book engine fills immediately on accept, so it is
        // already gone from `open` by the time a cancel for it can arrive - a
        // different situation from an id the venue never accepted at all. The
        // reason must say so rather than reusing "unknown order" for both.
        let mut e = Engine::new();
        e.process(ClientMessage::SubmitOrder(order("O1", 10)), 1);

        let out = e.process(
            ClientMessage::CancelOrder {
                client_order_id: "O1".into(),
            },
            2,
        );
        assert_eq!(
            cancel_reject_reason(&out),
            "order already terminal (filled or canceled)"
        );

        let out = e.process(
            ClientMessage::CancelOrder {
                client_order_id: "ghost".into(),
            },
            3,
        );
        assert_eq!(cancel_reject_reason(&out), "unknown order");
    }

    #[test]
    fn terminal_cancel_reject_carries_original_venue_id() {
        // The wire contract: `venue_order_id` is absent ONLY when the order id
        // is unknown. A terminal id WAS accepted, so its cancel reject must
        // carry the venue id it was accepted under, while a genuinely unknown
        // id carries none - no venue id was ever assigned to it.
        let mut e = Engine::new();
        let accepted = e.process(ClientMessage::SubmitOrder(order("O1", 10)), 1);
        let venue_id = accepted_venue_id(&accepted);

        let out = e.process(
            ClientMessage::CancelOrder {
                client_order_id: "O1".into(),
            },
            2,
        );
        assert!(matches!(
            &out[0],
            ServerMessage::OrderCancelRejected {
                venue_order_id: Some(id),
                reason,
                ..
            } if *id == venue_id && reason == "order already terminal (filled or canceled)"
        ));

        let out = e.process(
            ClientMessage::CancelOrder {
                client_order_id: "ghost".into(),
            },
            3,
        );
        assert!(matches!(
            &out[0],
            ServerMessage::OrderCancelRejected {
                venue_order_id: None,
                reason,
                ..
            } if reason == "unknown order"
        ));
    }

    #[test]
    fn modify_of_already_filled_order_distinguishes_terminal_from_unknown() {
        let mut e = Engine::new();
        e.process(ClientMessage::SubmitOrder(order("O1", 10)), 1);

        let out = e.process(
            ClientMessage::ModifyOrder {
                client_order_id: "O1".into(),
                price: Some(Decimal::from(200)),
                quantity: None,
                trigger_price: None,
            },
            2,
        );
        assert!(matches!(
            &out[0],
            ServerMessage::OrderModifyRejected { reason, .. }
                if reason == "order already terminal (filled or canceled)"
        ));
    }

    #[test]
    fn terminal_modify_reject_carries_original_venue_id() {
        // Same presence rule as the cancel path: terminal means the venue id
        // is known, so it must go out on the reject; only a genuinely unknown
        // id is bare (see modify_unknown_order_is_rejected_without_venue_id).
        let mut e = Engine::new();
        let accepted = e.process(ClientMessage::SubmitOrder(order("O1", 10)), 1);
        let venue_id = accepted_venue_id(&accepted);

        let out = e.process(
            ClientMessage::ModifyOrder {
                client_order_id: "O1".into(),
                price: Some(Decimal::from(200)),
                quantity: None,
                trigger_price: None,
            },
            2,
        );
        assert!(matches!(
            &out[0],
            ServerMessage::OrderModifyRejected {
                venue_order_id: Some(id),
                reason,
                ..
            } if *id == venue_id && reason == "order already terminal (filled or canceled)"
        ));
    }

    #[test]
    fn modify_unknown_order_is_rejected_without_venue_id() {
        let mut e = Engine::new();

        let out = e.process(
            ClientMessage::ModifyOrder {
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
            ServerMessage::OrderModifyRejected {
                client_order_id,
                venue_order_id: None,
                reason,
                ts_event: 1,
            } if client_order_id == "ghost" && reason == "unknown order"
        ));
        assert!(e.open_orders().is_empty());
    }

    #[test]
    fn modify_price_reprices_resting_reservation() {
        let mut e = Engine::new();
        e.arm(Divergence::PartialFillNext {
            client_order_id: "O1".into(),
            fraction: Decimal::from_f64(0.3).unwrap(),
        });
        e.process_with_market(
            ClientMessage::SubmitOrder(limit_order("O1", 10)),
            1,
            Some(reading(0)),
        );

        let out = e.process(
            ClientMessage::ModifyOrder {
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
            ServerMessage::OrderUpdated {
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
        e.process(ClientMessage::SubmitOrder(order("MARKET-REST", 10)), 1);
        assert!(matches!(e.open[0].resting, Resting::Inert));

        let out = e.process(
            ClientMessage::ModifyOrder {
                client_order_id: "MARKET-REST".into(),
                price: Some(Decimal::from(200)),
                quantity: None,
                trigger_price: None,
            },
            2,
        );
        assert!(matches!(
            &out[0],
            ServerMessage::OrderModifyRejected { reason, .. }
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
        e.process(ClientMessage::SubmitOrder(order("O1", 10)), 1);

        let out = e.process(
            ClientMessage::ModifyOrder {
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
            ServerMessage::OrderUpdated {
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
        e.process(ClientMessage::SubmitOrder(order("O1", 10)), 1);

        // Both the equality case (new total == 3 filled, zero would remain)
        // and the strictly-below case must reject, and the reason must say
        // "at or below" - the guard is `<=`, so a message claiming only
        // "below" would misdescribe the equality rejection.
        for new_total in [Decimal::from(3), Decimal::from(2)] {
            let out = e.process(
                ClientMessage::ModifyOrder {
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
                ServerMessage::OrderModifyRejected {
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
        e.process(ClientMessage::SubmitOrder(order("O1", 10)), 1);

        let out = e.process(
            ClientMessage::ModifyOrder {
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
            ServerMessage::OrderModifyRejected {
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
            ClientMessage::SubmitOrder(limit_order("O1", 10)),
            1,
            Some(reading(0)),
        );

        let out = e.process(
            ClientMessage::ModifyOrder {
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
            ServerMessage::OrderModifyRejected {
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
        e.process(ClientMessage::SubmitOrder(order("O1", 10)), 1);

        let out = e.process(
            ClientMessage::ModifyOrder {
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
            ServerMessage::OrderModifyRejected {
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
            ClientMessage::SubmitOrder(limit_order("O1", 10)),
            1,
            Some(reading(0)),
        );

        e.arm(Divergence::DropNextAccountUpdate);
        let modified = e.process(
            ClientMessage::ModifyOrder {
                client_order_id: "O1".into(),
                price: Some(Decimal::from(200)),
                quantity: None,
                trigger_price: None,
            },
            2,
        );
        assert_eq!(modified.len(), 2);
        assert!(matches!(modified[0], ServerMessage::OrderUpdated { .. }));
        assert!(matches!(modified[1], ServerMessage::AccountState(_)));

        let filled = e.process(ClientMessage::SubmitOrder(order("O2", 10)), 3);
        assert_eq!(filled.len(), 2);
        assert!(matches!(filled[0], ServerMessage::OrderAccepted { .. }));
        assert!(matches!(filled[1], ServerMessage::OrderFilled(_)));
    }

    #[test]
    fn missing_instrument_rejects_without_booking_position() {
        let mut e = Engine::new();
        let order = order_with("O1", Side::Buy, "ETHUSDT", 10, Some(Decimal::from(100)));
        let out = e.process(ClientMessage::SubmitOrder(order), 1);

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
        e.process(ClientMessage::SubmitOrder(order("O1", 10)), 1);
        e.process(ClientMessage::SubmitOrder(order("O2", 5)), 2);
        e.arm(Divergence::PartialFillNext {
            client_order_id: "O3".into(),
            fraction: Decimal::from_f64(0.5).unwrap(),
        });
        e.process(ClientMessage::SubmitOrder(order("O3", 4)), 3);
        e.process(
            ClientMessage::CancelOrder {
                client_order_id: "O3".into(),
            },
            4,
        );

        let out = e.process(
            ClientMessage::QueryOrders {
                request_id: "Q1".into(),
                client_order_id: None,
                open_only: false,
            },
            9,
        );
        let [ServerMessage::OrderStatusSnapshot(snap)] = &out[..] else {
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
            ClientMessage::SubmitOrder(limit_order("O1", 10)),
            1,
            Some(reading(0)),
        );
        e.process(
            ClientMessage::ModifyOrder {
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
        let out = e.process(ClientMessage::SubmitOrder(order("O1", 10)), 1);
        // The wire carried the fill twice (the injected lie)...
        assert_eq!(
            out.iter()
                .filter(|m| matches!(m, ServerMessage::OrderFilled(_)))
                .count(),
            2
        );
        e.process(ClientMessage::SubmitOrder(order("O2", 5)), 2);

        // ...but the truth store booked it once.
        let out = e.process(
            ClientMessage::QueryFills {
                request_id: "Q1".into(),
                client_order_id: None,
            },
            9,
        );
        let [ServerMessage::FillSnapshot(snap)] = &out[..] else {
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
        // and no lifecycle event is emitted - the client's belief and the
        // venue's book now disagree, and only a QueryOrders reply tells the
        // truth.
        let mut e = funded(1_000);
        e.arm(Divergence::PartialFillNext {
            client_order_id: "R1".into(),
            fraction: Decimal::new(5, 1),
        });
        e.process(ClientMessage::SubmitOrder(order("R1", 4)), 1);
        assert_eq!(e.open_orders().len(), 1);

        e.cancel_open_order_silently("R1", 7)
            .expect("resting order cancels");

        assert!(e.open_orders().is_empty(), "the book no longer holds R1");
        // The reservation is freed: only the filled half's spend remains.
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
            Err("order already terminal (filled or canceled)".to_string())
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
        e.process(ClientMessage::SubmitOrder(order), 1);

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
        let out = e.process(ClientMessage::SubmitOrder(order), 1);

        assert_eq!(reject_reason(&out), "submit price required");
        assert!(e.account_snapshot(2).balances.is_empty());
        assert!(e.positions().is_empty());
        assert!(e.open_orders().is_empty());
    }

    #[test]
    fn worst_case_reservation_covers_actual_output() {
        // The sizing model's claim - `worst_case_output_bytes` DOMINATES what
        // `process` really produces - checked against a matrix of books crossed
        // with every command class, including the divergence-armed worst cases
        // and adversarially escaped identifiers. A finite matrix samples the
        // bound; the per-constant derivations in `mogwai_protocol::sizing`
        // argue it. Ids and symbols are filled with `\u{0001}`, which serde
        // escapes to six bytes each: an ASCII fixture would pass a bound six
        // times too small.
        let esc_id = "\u{0001}".repeat(mogwai_protocol::MAX_CLIENT_ID_LEN);

        // Book 1: an empty venue, and a submit that fills - the first fill in a
        // fresh pair, which introduces TWO balance rows and one position the
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
                ClientMessage::SubmitOrder(order_with(
                    &format!("resting-{i}"),
                    Side::Buy,
                    "BTCUSDT",
                    1,
                    Some(Decimal::from(1)),
                )),
                i,
            );
            deep.process(
                ClientMessage::SubmitOrder(order_with(
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

        // An armed reject at the FULL post-truncation reason length: the engine
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
        let query_orders = ClientMessage::QueryOrders {
            request_id: esc_id.clone(),
            client_order_id: None,
            open_only: false,
        };
        let query_fills = ClientMessage::QueryFills {
            request_id: esc_id.clone(),
            client_order_id: None,
        };

        let cases: Vec<(&str, &mut Engine, Vec<ClientMessage>)> = vec![
            (
                "fresh book, first fill in a new pair",
                &mut fresh,
                vec![
                    ClientMessage::SubmitOrder(order_with(
                        &esc_id,
                        Side::Buy,
                        "BTCUSDT",
                        10,
                        Some(Decimal::from(1_000_000)),
                    )),
                    ClientMessage::CancelOrder {
                        client_order_id: esc_id.clone(),
                    },
                    ClientMessage::ModifyOrder {
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
                    ClientMessage::CancelOrder {
                        client_order_id: "resting-7".into(),
                    },
                    ClientMessage::SubmitOrder(order_with(
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
                vec![ClientMessage::SubmitOrder(order_with(
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
                vec![ClientMessage::SubmitOrder(ioc), query_fills],
            ),
        ];

        // A futures book carries margin rows the spot cases cannot produce, and
        // `book_shape().margins` is what reserves them. Two hedged positions in
        // one symbol are the case a per-position margin row under-reserves.
        let mut hedged = futures_engine(200_000, BreachAction::Refuse);
        hedged.set_oms_type(mogwai_protocol::OmsType::Hedging);
        for (index, side) in [(1, Side::Buy), (2, Side::Sell)] {
            let mut leg = mnq_order(&format!("HEDGE-{index}"), side, 1, 21_000);
            leg.position_id = Some(format!("LEG-{index}"));
            hedged.process_with_market(
                ClientMessage::SubmitOrder(leg),
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

        let futures_cases: Vec<(&str, &mut Engine, Vec<ClientMessage>)> = vec![(
            "hedged futures book with margin rows",
            &mut hedged,
            vec![
                ClientMessage::SubmitOrder(mnq_order(&esc_id, Side::Buy, 1, 21_000)),
                query_orders.clone(),
                ClientMessage::CancelOrder {
                    client_order_id: esc_id.clone(),
                },
            ],
        )];

        for (label, engine, commands) in cases.into_iter().chain(futures_cases) {
            for (ts, cmd) in commands.into_iter().enumerate() {
                // The shape is read exactly where the real caller reads it -
                // immediately before processing, under the same lock - so it
                // cannot drift between the reservation and the production.
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
                    "{label}: produced {actual} bytes against a {bound} byte reservation"
                );
            }
        }
    }

    /// The sweep-side half of the reservation claim, which `process`-shaped
    /// cases cannot reach: a liquidation cascade emits order frames NO client
    /// order paid for, so `emitted` alone under-reserves it and `originated`
    /// is what covers the gap.
    #[test]
    fn worst_case_reservation_covers_a_liquidation_cascade() {
        let mut engine = futures_engine(3_000, BreachAction::Liquidate);
        engine.set_oms_type(mogwai_protocol::OmsType::Hedging);
        for index in 1..=2 {
            let mut leg = mnq_order(&format!("LONG-{index}"), Side::Buy, 1, 21_000);
            leg.position_id = Some(format!("LEG-{index}"));
            engine.process_with_market(
                ClientMessage::SubmitOrder(leg),
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
            "a cascade produced {actual} bytes against a {bound} byte reservation"
        );
    }

    #[test]
    fn worst_case_reservation_covers_an_arrival_triggered_conditional() {
        // The widest CONDITIONAL arrival, which the matrix above cannot express
        // because it drives `process` with no market reading and a stop needs
        // one to fire on arrival: accepted, triggered, the duplicated fill, the
        // fill, and the cancel that closes the remainder the reduce-only cap
        // clamped. FIVE order events - one more than the IOC limit shape, and
        // the reason the submit multiplier is five rather than four.
        let esc_id = "\u{0001}".repeat(mogwai_protocol::MAX_CLIENT_ID_LEN);
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
            ClientMessage::SubmitOrder(order_with(
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

        let cmd = ClientMessage::SubmitOrder(stop);
        let shape = e.book_shape();
        let bound = mogwai_protocol::sizing::worst_case_output_bytes(&cmd, &shape);
        let output = e.process_with_market(cmd, 2, Some(reading(50)));
        assert_eq!(
            output
                .iter()
                .filter(|event| !matches!(event, ServerMessage::AccountState(_)))
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
            "produced {actual} bytes against a {bound} byte reservation"
        );
    }

    #[test]
    fn swept_fill_reservation_covers_a_multi_pair_sweep_batch() {
        // THREE distinct pairs, none previously held: a single-pair batch cannot
        // distinguish per-batch from per-order account widening and would pass
        // against an under-reserving bound. Every fill duplicated and every id
        // at max length in `\u{0001}`, which serde escapes to six bytes each.
        let esc =
            |n: usize| "\u{0001}".repeat(mogwai_protocol::MAX_CLIENT_ID_LEN - 1) + &n.to_string();
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
            e.process(ClientMessage::SubmitOrder(resting), index as u64);
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
            "a three-pair sweep produced {actual} bytes against a {bound} byte reservation"
        );
    }

    #[test]
    fn swept_reservation_covers_a_trigger_that_fills_duplicates_and_cancels() {
        // The widest SWEPT shape for one order: `OrderTriggered`, the
        // duplicated fill, the fill, and the cancel that closes the reduce-only
        // remainder the position cap clamped. Four order events, which is why
        // the per-order multiplier is four rather than three.
        let mut e = banded(31);
        e.process(ClientMessage::SubmitOrder(order("seed", 1)), 1);
        let mut stop = stop_order("wide", Side::Sell, OrderType::StopMarket, 90, None);
        stop.quantity = Decimal::from(10);
        stop.reduce_only = true;
        e.process(ClientMessage::SubmitOrder(stop), 2);
        e.arm(Divergence::DuplicateNextFill);

        let scan = e.pending_scans().remove(0);
        let shape = e.book_shape();
        let (events, emitted) = e.apply_scans(&[result(&scan, true, 100)], 100);
        assert_eq!(emitted, 1);
        assert_eq!(
            events
                .iter()
                .filter(|event| !matches!(event, ServerMessage::AccountState(_)))
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
            "produced {actual} bytes against a {bound} byte reservation"
        );
    }

    #[test]
    fn a_trigger_only_sweep_pass_reserves_its_own_frame() {
        // The sharpest hole the reviews found: a pass in which a stop-limit
        // triggers and RESTS books no fill, so a fill-keyed `emitted` would be
        // zero and the `OrderTriggered` frame would be written against a
        // zero-order reservation.
        let mut e = banded(32);
        e.process(
            ClientMessage::SubmitOrder(stop_order(
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
        e.process(
            ClientMessage::SubmitOrder(limit_order("trigger-rest", 1)),
            10,
        );
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
                .any(|event| matches!(event, ServerMessage::OrderFilled(_)))
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
            ClientMessage::SubmitOrder(limit_order("same", 1)),
            1,
            Some(reading),
        );
        b.process_with_market(
            ClientMessage::SubmitOrder(limit_order("unrelated", 1)),
            1,
            Some(reading),
        );
        b.process_with_market(
            ClientMessage::SubmitOrder(limit_order("same", 1)),
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
            ClientMessage::SubmitOrder(limit_order("arrival-part", 2)),
            10,
            Some(reading(0)),
        );
        assert!(matches!(
            out.iter().find(|event| matches!(event, ServerMessage::OrderFilled(_))),
            Some(ServerMessage::OrderFilled(fill)) if fill.leaves_qty == Decimal::ONE
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
        let rejected = e.process(ClientMessage::SubmitOrder(fok.clone()), 1);
        assert!(matches!(
            rejected.as_slice(),
            [ServerMessage::OrderRejected { reason, .. }] if reason.contains("trigger")
        ));
        assert!(e.armed.is_empty(), "the rejected FOK left its arm standing");
        let resubmit = e.process_with_market(ClientMessage::SubmitOrder(fok), 2, Some(reading(0)));
        assert!(matches!(
            resubmit.iter().find(|event| matches!(event, ServerMessage::OrderFilled(_))),
            Some(ServerMessage::OrderFilled(fill)) if fill.last_qty == Decimal::from(2)
        ));
    }

    #[test]
    fn orders_of_one_account_never_interact() {
        // Self-trade is IMPOSSIBLE rather than prevented: orders are judged
        // only against the tape, never against each other.
        let mut e = banded(5);
        let mut buy = limit_order("cross-buy", 1);
        buy.price = Some(Decimal::from(110));
        let mut sell = limit_order("cross-sell", 1);
        sell.side = Side::Sell;
        sell.price = Some(Decimal::from(90));
        let first = e.process(ClientMessage::SubmitOrder(buy), 1);
        let second = e.process(ClientMessage::SubmitOrder(sell), 2);
        assert!(
            !first
                .iter()
                .chain(second.iter())
                .any(|event| matches!(event, ServerMessage::OrderFilled(_))),
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
            ClientMessage::SubmitOrder(order_with(
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
            ClientMessage::SubmitOrder(order_with(
                "slip-sell",
                Side::Sell,
                "BTCUSDT",
                1,
                Some(Decimal::from(100)),
            )),
            2,
            Some(reading),
        );
        let price = |events: &[ServerMessage]| {
            events
                .iter()
                .find_map(|event| match event {
                    ServerMessage::OrderFilled(fill) => Some(fill.last_px),
                    _ => None,
                })
                .unwrap()
        };
        assert!(price(&buy) >= reading.last_px);
        assert!(price(&sell) <= reading.last_px);
    }

    #[test]
    fn a_market_order_with_no_reading_fills_at_its_stated_price_and_warns() {
        let mut e = banded(42);
        let out = e.process(ClientMessage::SubmitOrder(order("market-fallback", 1)), 1);
        assert!(out.iter().any(|event| matches!(event, ServerMessage::OrderFilled(fill) if fill.last_px == Decimal::from(100))));
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
        let out = e.process_with_market(ClientMessage::SubmitOrder(candidate), 1, Some(reading));
        assert!(
            matches!(out.as_slice(), [ServerMessage::OrderRejected { reason, .. }] if reason.contains("insufficient USDT"))
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
        let first = e.process(ClientMessage::SubmitOrder(order("ID-1", 1)), 1);
        let accepted = first.iter().find_map(|event| match event {
            ServerMessage::OrderAccepted { venue_order_id, .. } => Some(venue_order_id),
            _ => None,
        });
        let filled = first.iter().find_map(|event| match event {
            ServerMessage::OrderFilled(fill) => Some(fill),
            _ => None,
        });
        assert_eq!(accepted.map(String::as_str), Some("V-18446744073709551615"));
        assert_eq!(filled.map(|fill| fill.trade_id.as_str()), Some("T-1"));

        let mut hedged = Engine::new();
        hedged.set_oms_type(mogwai_protocol::OmsType::Hedging);
        let events = hedged.process(ClientMessage::SubmitOrder(order("ID-2", 1)), 1);
        let fill = events
            .iter()
            .find_map(|event| match event {
                ServerMessage::OrderFilled(fill) => Some(fill),
                _ => None,
            })
            .expect("hedging submit fills");
        assert_eq!(fill.venue_order_id, "V-1");
        assert_eq!(fill.trade_id, "T-1");
        assert_eq!(fill.position_id.as_deref(), Some("BTCUSDT-1"));
    }

    #[test]
    fn client_cannot_claim_the_liquidation_order_namespace() {
        let out = Engine::new().process(ClientMessage::SubmitOrder(order("LQ-MNQ-1", 1)), 1);
        assert_eq!(
            reject_reason(&out),
            "client_order_id uses reserved liquidation prefix"
        );
    }

    #[test]
    fn cancel_consumes_drop_next_account_update() {
        let mut e = banded(1);
        e.process(ClientMessage::SubmitOrder(limit_order("cancel-drop", 1)), 1);
        e.arm(Divergence::DropNextAccountUpdate);
        let canceled = e.process(
            ClientMessage::CancelOrder {
                client_order_id: "cancel-drop".into(),
            },
            2,
        );
        assert!(matches!(
            canceled.as_slice(),
            [ServerMessage::OrderCanceled { .. }]
        ));
        assert!(e.armed.is_empty());
    }

    #[test]
    fn hedging_reduce_only_without_position_id_is_rejected() {
        let mut e = Engine::new();
        e.set_oms_type(mogwai_protocol::OmsType::Hedging);
        let mut reduce = order("hedge-reduce", 1);
        reduce.reduce_only = true;
        let out = e.process(ClientMessage::SubmitOrder(reduce), 1);
        assert_eq!(
            reject_reason(&out),
            "hedging reduce-only order requires a position_id"
        );
        assert!(e.open_orders().is_empty());
    }

    #[test]
    fn surcharge_window_is_a_pure_function_of_fill_timestamp() {
        let mut engine = futures_engine(20_000, BreachAction::Refuse);
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
        e.process(ClientMessage::SubmitOrder(resting), 1);
        e.arm(Divergence::PartialFillNext {
            client_order_id: "zero-redraw".into(),
            fraction: Decimal::new(3, 1),
        });
        let scan = e.pending_scans().remove(0);
        let (events, _) = e.apply_scans(&[result(&scan, true, 2)], 2);
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, ServerMessage::OrderFilled(_)))
        );
        assert_eq!(e.open[0].band_draw, 0);
        assert_eq!(e.open[0].leaves_qty, lot);
    }

    /// A drain budget can end a walk short of the pass's `ts`. When the pass
    /// then executes nothing, the frontier must stay where the walk REACHED:
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
        e.process(ClientMessage::SubmitOrder(resting), 1);
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
                .any(|event| matches!(event, ServerMessage::OrderFilled(_)))
        );
        assert_eq!(e.open[0].scanned_ns, 4);
        assert_eq!(e.open[0].leaves_qty, lot);
    }

    /// The counterpart: a REAL execution opens a new tranche, which covers from
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
        e.process(ClientMessage::SubmitOrder(resting), 1);
        e.arm(Divergence::PartialFillNext {
            client_order_id: "partial-frontier".into(),
            fraction: Decimal::new(3, 1),
        });
        let scan = e.pending_scans().remove(0);
        let (events, _) = e.apply_scans(&[result(&scan, true, 4)], 9);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, ServerMessage::OrderFilled(_)))
        );
        assert_eq!(e.open[0].scanned_ns, 9);
        assert_eq!(e.open[0].band_draw, 1);
    }

    /// The `DropNextAccountUpdate` carve-out that survives the widening: an
    /// order COMING TO REST reserves funds, but must not spend an arm the
    /// author pointed at the fill it has not had yet.
    #[test]
    fn a_resting_acceptance_leaves_drop_next_account_update_armed() {
        let mut e = banded(1);
        e.arm(Divergence::DropNextAccountUpdate);
        let accepted = e.process(ClientMessage::SubmitOrder(limit_order("rest-keeps", 1)), 1);
        assert!(
            accepted
                .iter()
                .any(|event| matches!(event, ServerMessage::AccountState(_))),
            "a resting acceptance still owes its snapshot"
        );
        // Still armed, and the later cancel is what spends it.
        let canceled = e.process(
            ClientMessage::CancelOrder {
                client_order_id: "rest-keeps".into(),
            },
            2,
        );
        assert!(
            !canceled
                .iter()
                .any(|event| matches!(event, ServerMessage::AccountState(_))),
            "the cancel that frees the hold is where the arm lands"
        );
        assert!(e.armed.is_empty());
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
            breach_action: BreachAction::Refuse,
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
}
