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

use std::collections::{HashMap, VecDeque};

use mogwai_protocol::{
    AccountId, AccountState, ClientMessage, ClientOrderId, FillSnapshot, InstrumentDef,
    OrderFilled, OrderStatusInfo, OrderStatusSnapshot, Position, ServerMessage, Side, SubmitOrder,
    Symbol, VenueOrderId, WireOrderStatus, control::Divergence, default_instruments,
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
    /// Trades that have printed THROUGH `submit.price` since this order was
    /// accepted, since its last REPRICE, or since its last FILL - a price
    /// amend and an execution both restart the window, a quantity amend does
    /// not. Compared against the engine's `penetration_ticks`; at the default
    /// 0 it is written once and never read.
    pub penetration_count: u32,
    /// Sim unix-ns instant the penetration walk has already covered, the
    /// exclusive lower bound for the next pass. Advanced by the ENGINE when it
    /// accepts a count, never by the counter: a walk whose result is discarded
    /// must re-cover the same span rather than lose it.
    pub penetration_scanned_ns: u64,
    /// Bumped on every mutation of this order's identity for gating purposes -
    /// reprice, quantity amend, fill, frontier advance. A `ScanResult` carries
    /// the revision its walk was planned against, so a result computed against
    /// state that has since moved is DROPPED rather than applied. Liveness
    /// alone is not enough: two overlapping walks can both name a still-resting
    /// order, and applying both double-counts the span they share.
    pub revision: u64,
}

/// The one construction path for an engine. The core receives observations,
/// never a tape or clock; `penetration_ticks` is therefore a pure model knob.
pub struct EngineConfig {
    pub account_id: AccountId,
    pub instruments: Vec<InstrumentDef>,
    pub balances: HashMap<String, Decimal>,
    pub penetration_ticks: u32,
}

impl EngineConfig {
    #[must_use]
    pub fn unbound(instruments: Vec<InstrumentDef>) -> Self {
        Self {
            account_id: AccountId::parse(Engine::UNBOUND_ACCOUNT_ID).expect("static account id"),
            instruments,
            balances: HashMap::new(),
            penetration_ticks: 0,
        }
    }
}

/// One resting order the caller must count penetrations for, and how many are
/// still owed. The engine hands these out; the server walks the tape and hands
/// back `ScanResult`s. This is the whole seam - the engine never sees a tick.
#[derive(Debug, Clone)]
pub struct PendingScan {
    pub client_order_id: ClientOrderId,
    pub symbol: Symbol,
    pub side: Side,
    /// The resting limit's price. Always present: only a validated LIMIT can
    /// rest under the gate, so the scan surface hands the counter a `Decimal`
    /// rather than re-litigating `submit.price`'s `Option`.
    pub price: Decimal,
    /// Exclusive lower bound of the span still to walk.
    pub from_ns: u64,
    /// Penetrations still required. Never zero: an order already at its
    /// threshold is executed, not returned.
    pub remaining: u32,
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
    /// Penetrations observed in `(from_ns, scanned_to_ns]`.
    pub counted: u32,
    /// The instant the walk ACTUALLY reached, which its drain budget may have
    /// cut short of the pass's target. The frontier advances to exactly this,
    /// never past it.
    pub scanned_to_ns: u64,
}

#[derive(Debug)]
pub struct Engine {
    account_id: AccountId,
    open: Vec<OpenOrder>,
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
    seq: u64,
    warned: Warned,
    penetration_ticks: u32,
}

impl Engine {
    /// Cheap facts used to reserve a bounded wire response before mutation.
    #[must_use]
    pub fn book_shape(&self) -> mogwai_protocol::sizing::BookShape {
        mogwai_protocol::sizing::BookShape {
            balances: self.account.balances.len(),
            positions: self.account.positions.len(),
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
            .map(|instrument| (instrument.symbol.clone(), instrument))
            .collect();

        Self {
            account_id: config.account_id,
            open: Vec::new(),
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
            seq: 0,
            warned: Warned::default(),
            penetration_ticks: config.penetration_ticks,
        }
    }

    pub fn instrument_defs(&self) -> Vec<InstrumentDef> {
        let mut defs: Vec<_> = self.instruments.values().cloned().collect();
        defs.sort_by(|a, b| a.symbol.cmp(&b.symbol));
        defs
    }

    /// Monotonic id source; the server stamps real timestamps.
    fn next_id(&mut self, prefix: &str) -> String {
        self.seq += 1;
        format!("{prefix}-{}", self.seq)
    }

    /// Process one client message, emitting the resulting execution events.
    ///
    /// `ts` is supplied by the caller (the server's clock) so the engine stays
    /// free of wall-clock access and remains deterministic in tests.
    pub fn process(&mut self, msg: ClientMessage, ts: u64) -> Vec<ServerMessage> {
        self.process_with_market(msg, ts, None)
    }

    pub fn process_with_market(
        &mut self,
        msg: ClientMessage,
        ts: u64,
        market_px: Option<Decimal>,
    ) -> Vec<ServerMessage> {
        match msg {
            ClientMessage::SubmitOrder(order) => self.on_submit(order, ts, market_px),
            ClientMessage::CancelOrder { client_order_id } => self.on_cancel(client_order_id, ts),
            ClientMessage::ModifyOrder {
                client_order_id,
                price,
                quantity,
            } => self.on_modify(client_order_id, price, quantity, ts),
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
        }
    }

    /// Every resting GTC LIMIT whose gate is not yet satisfied. Empty when
    /// `penetration_ticks == 0`, which is what lets a default venue never even
    /// spawn a sweeper.
    ///
    /// The `order_type` filter is load-bearing, not belt-and-braces: a MARKET
    /// order is never gated (it is marketable by definition), but an armed
    /// `PartialFillNext` can leave one RESTING with a stamped price, and
    /// without this filter that remainder would be handed to the tape walk and
    /// held until the market traded through the price the venue itself
    /// synthesized for it.
    #[must_use]
    pub fn pending_scans(&self) -> Vec<PendingScan> {
        if self.penetration_ticks == 0 {
            return Vec::new();
        }
        let mut scans: Vec<_> = self
            .open
            .iter()
            .filter_map(|order| {
                let price = order.submit.price?;
                (order.submit.order_type == mogwai_protocol::OrderType::Limit
                    && order.submit.time_in_force == mogwai_protocol::TimeInForce::Gtc)
                    .then(|| PendingScan {
                        client_order_id: order.submit.client_order_id.clone(),
                        symbol: order.submit.symbol.clone(),
                        side: order.submit.side,
                        price,
                        from_ns: order.penetration_scanned_ns,
                        remaining: self
                            .penetration_ticks
                            .saturating_sub(order.penetration_count),
                        revision: order.revision,
                    })
            })
            .filter(|scan| scan.remaining > 0)
            .collect();
        scans.sort_by_key(|scan| scan.from_ns);
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
        let Some(pos) = self
            .open
            .iter()
            .position(|o| o.submit.client_order_id == client_order_id)
        else {
            return Err(match self.seen_client_order_ids.get(client_order_id) {
                Some(_) => "order already terminal (filled or canceled)".into(),
                None => "unknown order".into(),
            });
        };
        let order = self.open.remove(pos);
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
            .map(|(symbol, state)| Position {
                symbol: symbol.clone(),
                quantity: state.qty,
                avg_px: state.avg_px,
            })
            .collect();
        positions.sort_by(|a, b| a.symbol.cmp(&b.symbol));
        positions
    }

    pub fn open_orders(&self) -> &[OpenOrder] {
        &self.open
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
        symbol: order.submit.symbol.clone(),
        side: order.submit.side,
        order_type: order.submit.order_type,
        time_in_force: order.submit.time_in_force,
        status: if filled_qty > Decimal::ZERO {
            WireOrderStatus::PartiallyFilled
        } else {
            WireOrderStatus::Accepted
        },
        quantity: order.submit.quantity,
        filled_qty,
        price: order.submit.price,
        ts_accepted: order.ts_accepted,
        ts_last: order.ts_last,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mogwai_protocol::{Balance, OrderFilled, OrderType, Side, TimeInForce};
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
            side,
            order_type: if price.is_some() {
                OrderType::Limit
            } else {
                OrderType::Market
            },
            quantity,
            price,
            time_in_force: TimeInForce::Gtc,
        }
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
            penetration_ticks: 0,
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
            penetration_ticks: 0,
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
            penetration_ticks: 0,
        })
    }

    fn gated(ticks: u32) -> Engine {
        Engine::build(EngineConfig {
            account_id: test_account_id(),
            instruments: default_instruments(),
            balances: HashMap::new(),
            penetration_ticks: ticks,
        })
    }

    fn result(scan: &PendingScan, counted: u32, scanned_to_ns: u64) -> ScanResult {
        ScanResult {
            client_order_id: scan.client_order_id.clone(),
            from_ns: scan.from_ns,
            revision: scan.revision,
            counted,
            scanned_to_ns,
        }
    }

    #[test]
    fn zero_penetration_ticks_fills_on_submit_exactly_as_before() {
        let mut e = Engine::new();
        let out = e.process(ClientMessage::SubmitOrder(order("legacy", 1)), 7);
        assert!(
            matches!(out.as_slice(), [ServerMessage::OrderAccepted { .. }, ServerMessage::OrderFilled(fill), ServerMessage::AccountState(_)] if fill.last_px == Decimal::from(100))
        );
        assert!(e.pending_scans().is_empty());
    }

    #[test]
    fn a_gated_limit_rests_without_a_fill_event() {
        let mut e = gated(1);
        let out = e.process(ClientMessage::SubmitOrder(order("rest", 2)), 7);
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
    fn a_marketable_limit_fills_on_arrival_under_a_gate_of_one() {
        let mut e = gated(1);
        let out = e.process_with_market(
            ClientMessage::SubmitOrder(order("cross", 1)),
            7,
            Some(Decimal::from(99)),
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
    }

    #[test]
    fn a_marketable_limit_still_rests_under_a_gate_of_two() {
        let mut e = gated(2);
        let out = e.process_with_market(
            ClientMessage::SubmitOrder(order("two", 1)),
            7,
            Some(Decimal::from(99)),
        );
        assert!(matches!(
            out.as_slice(),
            [
                ServerMessage::OrderAccepted { .. },
                ServerMessage::AccountState(_)
            ]
        ));
        assert_eq!(e.open[0].penetration_count, 1);
    }

    #[test]
    fn apply_scans_accumulates_below_the_threshold() {
        let mut e = gated(2);
        e.process(ClientMessage::SubmitOrder(order("count", 1)), 10);
        let first = e.pending_scans().remove(0);
        let (out, emitted) = e.apply_scans(&[result(&first, 1, 20)], 20);
        assert!(out.is_empty());
        assert_eq!(emitted, 0);
        let second = e.pending_scans().remove(0);
        let (out, emitted) = e.apply_scans(&[result(&second, 1, 30)], 30);
        assert_eq!(emitted, 1);
        assert!(
            out.iter()
                .any(|event| matches!(event, ServerMessage::OrderFilled(_)))
        );
    }

    #[test]
    fn a_truncated_scan_advances_only_what_it_covered() {
        let mut e = gated(3);
        e.process(ClientMessage::SubmitOrder(order("short", 1)), 10);
        let scan = e.pending_scans().remove(0);
        e.apply_scans(&[result(&scan, 1, 12)], 99);
        assert_eq!(e.open[0].penetration_count, 1);
        assert_eq!(e.open[0].penetration_scanned_ns, 12);
    }

    #[test]
    fn a_scan_against_a_stale_revision_is_dropped() {
        let mut e = gated(2);
        e.process(ClientMessage::SubmitOrder(order("stale", 1)), 10);
        let scan = e.pending_scans().remove(0);
        e.apply_scans(&[result(&scan, 1, 20)], 20);
        let (_, emitted) = e.apply_scans(&[result(&scan, 1, 20)], 20);
        assert_eq!(emitted, 0);
        assert_eq!(e.open[0].penetration_count, 1);
    }

    #[test]
    fn a_price_amend_restarts_the_penetration_window_and_quantity_amend_preserves_it() {
        let mut e = gated(3);
        e.process(ClientMessage::SubmitOrder(order("amend", 2)), 10);
        let scan = e.pending_scans().remove(0);
        e.apply_scans(&[result(&scan, 1, 20)], 20);
        e.process(
            ClientMessage::ModifyOrder {
                client_order_id: "amend".into(),
                price: None,
                quantity: Some(Decimal::from(3)),
            },
            30,
        );
        assert_eq!(e.open[0].penetration_count, 1);
        e.process(
            ClientMessage::ModifyOrder {
                client_order_id: "amend".into(),
                price: Some(Decimal::from(101)),
                quantity: None,
            },
            40,
        );
        assert_eq!(e.open[0].penetration_count, 0);
        assert_eq!(e.open[0].penetration_scanned_ns, 40);
    }

    #[test]
    fn apply_scans_fills_an_order_the_tape_traded_through() {
        let mut e = gated(1);
        e.process(ClientMessage::SubmitOrder(order("swept", 1)), 10);
        let scan = e.pending_scans().remove(0);
        let (out, emitted) = e.apply_scans(&[result(&scan, 1, 20)], 20);
        assert_eq!(emitted, 1);
        // Accept-free: the fill is unsolicited, and it prints at the ORDER'S
        // price, never the penetrating trade's - the gate decides WHEN.
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
    fn an_executed_order_restarts_its_penetration_window() {
        // A partly filled remainder resting AT its threshold would be filled by
        // the next pass with zero further penetrations - the gate leaking open
        // on exactly the orders it is most meant to hold.
        let mut e = gated(1);
        e.arm(Divergence::PartialFillNext {
            client_order_id: "part".into(),
            fraction: Decimal::new(5, 1),
        });
        e.process(ClientMessage::SubmitOrder(order("part", 2)), 10);
        let scan = e.pending_scans().remove(0);
        let (out, emitted) = e.apply_scans(&[result(&scan, 1, 20)], 20);
        assert_eq!(emitted, 1);
        assert!(matches!(
            out.first(),
            Some(ServerMessage::OrderFilled(fill))
                if fill.last_qty == Decimal::ONE && fill.leaves_qty == Decimal::ONE
        ));
        assert_eq!(e.open[0].penetration_count, 0);
        assert_eq!(e.open[0].penetration_scanned_ns, 20);
        assert_eq!(e.open[0].leaves_qty, Decimal::ONE);
    }

    #[test]
    fn a_swept_fill_sizes_off_the_remaining_quantity() {
        // The second sweep multiplies its fraction by the LEAVES, not by the
        // original quantity, so it cannot over-fill a partly filled order.
        let mut e = gated(1);
        e.arm(Divergence::PartialFillNext {
            client_order_id: "leaves".into(),
            fraction: Decimal::new(5, 1),
        });
        e.process(ClientMessage::SubmitOrder(order("leaves", 4)), 10);
        let scan = e.pending_scans().remove(0);
        e.apply_scans(&[result(&scan, 1, 20)], 20);
        assert_eq!(e.open[0].leaves_qty, Decimal::from(2));
        let scan = e.pending_scans().remove(0);
        let (out, _) = e.apply_scans(&[result(&scan, 1, 30)], 30);
        assert!(matches!(
            out.first(),
            Some(ServerMessage::OrderFilled(fill))
                if fill.last_qty == Decimal::from(2) && fill.leaves_qty == Decimal::ZERO
        ));
    }

    #[test]
    fn a_partial_fill_divergence_survives_until_the_gated_order_executes() {
        // The gated rest calls no `plan_fill`, so the targeted divergence is
        // still armed for the execution it names.
        let mut e = gated(1);
        e.arm(Divergence::PartialFillNext {
            client_order_id: "armed".into(),
            fraction: Decimal::new(5, 1),
        });
        e.process(ClientMessage::SubmitOrder(order("armed", 2)), 10);
        assert_eq!(e.armed.len(), 1);
        let scan = e.pending_scans().remove(0);
        let (out, _) = e.apply_scans(&[result(&scan, 1, 20)], 20);
        assert!(matches!(
            out.first(),
            Some(ServerMessage::OrderFilled(fill)) if fill.last_qty == Decimal::ONE
        ));
        assert!(e.armed.is_empty());
    }

    #[test]
    fn a_duplicate_fill_divergence_applies_to_a_swept_fill() {
        let mut e = gated(1);
        e.process(ClientMessage::SubmitOrder(order("dup", 1)), 10);
        e.arm(Divergence::DuplicateNextFill);
        let scan = e.pending_scans().remove(0);
        let (out, emitted) = e.apply_scans(&[result(&scan, 1, 20)], 20);
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
    fn fok_and_market_orders_are_never_gated() {
        let mut e = gated(5);
        let mut fok = order("fok-ungated", 1);
        fok.time_in_force = TimeInForce::Fok;
        let out = e.process(ClientMessage::SubmitOrder(fok), 1);
        assert!(
            out.iter()
                .any(|event| matches!(event, ServerMessage::OrderFilled(_)))
        );

        // A MARKET order arrives price-stamped by the server; it is marketable
        // by definition and is neither seeded nor swept.
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
    fn a_partly_filled_market_remainder_is_never_swept() {
        // A MARKET order is never gated, but an armed partial can leave one
        // RESTING with a server-stamped price. Handing that remainder to the
        // tape walk would hold it until the market traded through a price the
        // venue itself synthesized.
        let mut e = gated(1);
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
    fn a_dropped_account_update_survives_a_gated_accept_and_applies_to_the_swept_fill() {
        let mut e = gated(1);
        e.arm(Divergence::DropNextAccountUpdate);
        let accepted = e.process(ClientMessage::SubmitOrder(order("drop", 1)), 10);
        assert!(matches!(
            accepted.last(),
            Some(ServerMessage::AccountState(_))
        ));
        let scan = e.pending_scans().remove(0);
        let (out, _) = e.apply_scans(&[result(&scan, 1, 20)], 20);
        assert!(matches!(out.as_slice(), [ServerMessage::OrderFilled(_)]));
    }

    #[test]
    fn a_rejected_fok_still_does_not_reserve_its_client_order_id() {
        let mut e = gated(1);
        let mut fok = order("fok-reuse", 2);
        fok.time_in_force = TimeInForce::Fok;
        e.arm(Divergence::PartialFillNext {
            client_order_id: "fok-reuse".into(),
            fraction: Decimal::new(5, 1),
        });
        let rejected = e.process(ClientMessage::SubmitOrder(fok.clone()), 1);
        assert!(matches!(
            rejected.as_slice(),
            [ServerMessage::OrderRejected { .. }]
        ));
        let accepted = e.process(ClientMessage::SubmitOrder(fok), 2);
        assert!(matches!(
            accepted.first(),
            Some(ServerMessage::OrderAccepted { .. })
        ));
    }

    #[test]
    fn a_gated_ioc_cancels_when_the_seed_misses_and_fills_when_it_meets() {
        let mut e = gated(1);
        let mut miss = order("ioc-miss", 1);
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
        let mut hit = order("ioc-hit", 1);
        hit.time_in_force = TimeInForce::Ioc;
        let out =
            e.process_with_market(ClientMessage::SubmitOrder(hit), 2, Some(Decimal::from(99)));
        assert!(
            out.iter()
                .any(|event| matches!(event, ServerMessage::OrderFilled(_)))
        );
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
            .find(|position| position.symbol == symbol)
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
        e.process(ClientMessage::SubmitOrder(order("O1", 10)), 1);

        let out = e.process(
            ClientMessage::ModifyOrder {
                client_order_id: "O1".into(),
                price: Some(Decimal::from(200)),
                quantity: None,
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
        e.process(ClientMessage::SubmitOrder(order("O1", 10)), 1);

        let out = e.process(
            ClientMessage::ModifyOrder {
                client_order_id: "O1".into(),
                price: Some(Decimal::ZERO),
                quantity: None,
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
        e.process(ClientMessage::SubmitOrder(order("O1", 10)), 1);

        e.arm(Divergence::DropNextAccountUpdate);
        let modified = e.process(
            ClientMessage::ModifyOrder {
                client_order_id: "O1".into(),
                price: Some(Decimal::from(200)),
                quantity: None,
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
        e.process(ClientMessage::SubmitOrder(order("O1", 10)), 1);
        e.process(
            ClientMessage::ModifyOrder {
                client_order_id: "O1".into(),
                price: Some(Decimal::from(200)),
                quantity: Some(Decimal::from(20)),
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
            penetration_ticks: 0,
        });

        // Book 2: deep - hundreds of open and closed orders and a long fill
        // history, so the per-row snapshot terms actually carry the bound.
        let mut deep = Engine::build(EngineConfig {
            account_id: max_account_id,
            instruments: default_instruments(),
            balances: HashMap::new(),
            penetration_ticks: 0,
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

        for (label, engine, commands) in cases {
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

    #[test]
    fn penetrated_fill_reservation_covers_a_multi_pair_sweep_batch() {
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
                base: (*base).into(),
                quote: (*quote).into(),
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
            penetration_ticks: 1,
        });
        for (index, (symbol, _, _)) in pairs.iter().enumerate() {
            e.process(
                ClientMessage::SubmitOrder(order_with(
                    &esc(index),
                    Side::Buy,
                    symbol,
                    10,
                    Some(Decimal::from(1_000_000)),
                )),
                index as u64,
            );
            e.arm(Divergence::DuplicateNextFill);
        }
        let scans = e.pending_scans();
        assert_eq!(scans.len(), 3);
        let results: Vec<_> = scans.iter().map(|scan| result(scan, 1, 100)).collect();
        // Read exactly where the sweeper reads it: before `apply_scans`.
        let shape = e.book_shape();
        let (events, emitted) = e.apply_scans(&results, 100);
        assert_eq!(emitted, 3);
        let bound = mogwai_protocol::sizing::penetrated_fill_max_bytes(&shape, emitted);
        let actual: usize = events
            .iter()
            .map(|event| serde_json::to_vec(event).expect("event serializes").len())
            .sum();
        assert!(
            actual <= bound,
            "a three-pair sweep produced {actual} bytes against a {bound} byte reservation"
        );
    }
}
