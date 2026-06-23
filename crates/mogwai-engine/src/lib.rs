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

use std::collections::{HashMap, HashSet, VecDeque};

use mogwai_protocol::{
    AccountState, Balance, ClientMessage, ClientOrderId, InstrumentDef, OrderFilled, Position,
    ServerMessage, Side, SubmitOrder, Symbol, VenueOrderId, control::Divergence,
};
use rust_decimal::Decimal;

/// A resting order tracked by the venue.
#[derive(Debug, Clone)]
pub struct OpenOrder {
    pub venue_order_id: VenueOrderId,
    pub submit: SubmitOrder,
    pub leaves_qty: Decimal,
}

/// Static instrument definition used to split a symbol fill into balance legs.
#[derive(Debug, Clone)]
pub struct Instrument {
    pub symbol: Symbol,
    pub base: String,
    pub quote: String,
    pub price_precision: u8,
    pub size_precision: u8,
    pub price_increment: Decimal,
    pub size_increment: Decimal,
}

impl From<&Instrument> for InstrumentDef {
    fn from(instrument: &Instrument) -> Self {
        Self {
            symbol: instrument.symbol.clone(),
            base: instrument.base.clone(),
            quote: instrument.quote.clone(),
            price_precision: instrument.price_precision,
            size_precision: instrument.size_precision,
            price_increment: instrument.price_increment,
            size_increment: instrument.size_increment,
        }
    }
}

#[derive(Debug, Default)]
struct Account {
    balances: HashMap<String, Decimal>,
    positions: HashMap<Symbol, PositionState>,
}

#[derive(Debug, Default, Clone)]
struct PositionState {
    qty: Decimal,
    avg_px: Decimal,
}

#[derive(Debug, Default)]
struct Warned {
    missing_instrument: HashSet<Symbol>,
    zero_px: HashSet<Symbol>,
    priceless_reservation: HashSet<Symbol>,
}

#[derive(Debug)]
pub struct Engine {
    open: Vec<OpenOrder>,
    account: Account,
    instruments: HashMap<Symbol, Instrument>,
    /// Armed divergences, consumed as their trigger fires.
    armed: VecDeque<Divergence>,
    seq: u64,
    warned: Warned,
}

impl Engine {
    // No `Default`: per spec, `new()` is the sole constructor so the instrument
    // table is always seeded. A derived `Default` would yield an empty table
    // whose fill accounting silently diverges (every fill warns, books
    // position-only); a delegating `Default` is dead surface nothing calls.
    #[expect(
        clippy::new_without_default,
        reason = "new() seeds the instrument table; a Default impl would diverge or be dead surface"
    )]
    pub fn new() -> Self {
        Self::with_instruments(vec![Instrument {
            symbol: "BTCUSDT".into(),
            base: "BTC".into(),
            quote: "USDT".into(),
            price_precision: 2,
            size_precision: 8,
            price_increment: Decimal::new(1, 2),
            size_increment: Decimal::new(1, 8),
        }])
    }

    pub fn with_instruments(instruments: Vec<Instrument>) -> Self {
        let instruments = instruments
            .into_iter()
            .map(|instrument| (instrument.symbol.clone(), instrument))
            .collect();

        Self {
            open: Vec::new(),
            account: Account::default(),
            instruments,
            armed: VecDeque::new(),
            seq: 0,
            warned: Warned::default(),
        }
    }

    pub fn instrument_defs(&self) -> Vec<InstrumentDef> {
        let mut defs: Vec<_> = self.instruments.values().map(InstrumentDef::from).collect();
        defs.sort_by(|a, b| a.symbol.cmp(&b.symbol));
        defs
    }

    /// Arm a divergence to fire on the next matching trigger (control plane).
    pub fn arm(&mut self, d: Divergence) {
        match d {
            // Temporal, connection-scoped divergences are owned by the server's
            // outbound layer, not the synchronous clock-free engine. Dropping
            // them here keeps stale temporal settings from blocking the next
            // engine-side divergence at front().
            Divergence::DelayAcks { .. } | Divergence::GoDark { .. } => {}
            other => self.armed.push_back(other),
        }
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
        match msg {
            ClientMessage::SubmitOrder(order) => self.on_submit(order, ts),
            ClientMessage::CancelOrder { client_order_id } => self.on_cancel(client_order_id, ts),
            ClientMessage::ModifyOrder {
                client_order_id,
                price,
                quantity,
            } => self.on_modify(client_order_id, price, quantity, ts),
            // Subscriptions are intercepted by the server for replay control.
            ClientMessage::Subscribe { .. } | ClientMessage::Unsubscribe { .. } => Vec::new(),
        }
    }

    fn on_submit(&mut self, order: SubmitOrder, ts: u64) -> Vec<ServerMessage> {
        // Divergence: reject the next submit outright.
        if matches!(
            self.armed.front(),
            Some(Divergence::RejectNextSubmit { .. })
        ) {
            let Some(Divergence::RejectNextSubmit { reason }) = self.armed.pop_front() else {
                unreachable!()
            };
            return vec![ServerMessage::OrderRejected {
                client_order_id: order.client_order_id,
                reason,
                ts_event: ts,
            }];
        }

        let venue_order_id = self.next_id("V");
        let mut out = vec![ServerMessage::OrderAccepted {
            client_order_id: order.client_order_id.clone(),
            venue_order_id: venue_order_id.clone(),
            ts_event: ts,
        }];

        // Divergence: partial-fill the next order, leaving the remainder resting.
        let fill_fraction = match self.armed.front() {
            Some(Divergence::PartialFillNext {
                client_order_id,
                fraction,
            }) if *client_order_id == order.client_order_id => {
                let f = *fraction;
                self.armed.pop_front();
                f
            }
            _ => Decimal::ONE,
        };

        let last_qty = order.quantity * fill_fraction;
        let leaves_qty = order.quantity - last_qty;
        let last_px = order.price.unwrap_or(Decimal::ZERO);

        let fill = OrderFilled {
            client_order_id: order.client_order_id.clone(),
            venue_order_id: venue_order_id.clone(),
            trade_id: self.next_id("T"),
            symbol: order.symbol.clone(),
            side: order.side,
            last_qty,
            last_px,
            leaves_qty,
            commission: Decimal::ZERO,
            ts_event: ts,
        };

        self.apply_fill(&fill);
        let duplicate = matches!(self.armed.front(), Some(Divergence::DuplicateNextFill));
        if duplicate {
            self.armed.pop_front();
            out.push(ServerMessage::OrderFilled(fill.clone()));
        }
        out.push(ServerMessage::OrderFilled(fill));

        if leaves_qty > Decimal::ZERO {
            self.warn_priceless_reservation(&order);
            self.open.push(OpenOrder {
                venue_order_id,
                submit: order,
                leaves_qty,
            });
        }

        let drop_update = matches!(self.armed.front(), Some(Divergence::DropNextAccountUpdate));
        if drop_update {
            self.armed.pop_front();
        } else {
            out.push(ServerMessage::AccountState(self.snapshot(ts)));
        }
        out
    }

    fn on_cancel(&mut self, client_order_id: ClientOrderId, ts: u64) -> Vec<ServerMessage> {
        if let Some(pos) = self
            .open
            .iter()
            .position(|o| o.submit.client_order_id == client_order_id)
        {
            let o = self.open.remove(pos);
            vec![
                ServerMessage::OrderCanceled {
                    client_order_id,
                    venue_order_id: o.venue_order_id,
                    ts_event: ts,
                },
                ServerMessage::AccountState(self.snapshot(ts)),
            ]
        } else {
            vec![ServerMessage::OrderRejected {
                client_order_id,
                reason: "unknown order".into(),
                ts_event: ts,
            }]
        }
    }

    fn on_modify(
        &mut self,
        client_order_id: ClientOrderId,
        price: Option<Decimal>,
        quantity: Option<Decimal>,
        ts: u64,
    ) -> Vec<ServerMessage> {
        let Some(pos) = self
            .open
            .iter()
            .position(|o| o.submit.client_order_id == client_order_id)
        else {
            return vec![ServerMessage::OrderModifyRejected {
                client_order_id,
                venue_order_id: None,
                reason: "unknown order".into(),
                ts_event: ts,
            }];
        };

        let venue_order_id = self.open[pos].venue_order_id.clone();
        if price.is_none() && quantity.is_none() {
            return vec![ServerMessage::OrderModifyRejected {
                client_order_id,
                venue_order_id: Some(venue_order_id),
                reason: "empty modify (no price or quantity)".into(),
                ts_event: ts,
            }];
        }

        let order = &self.open[pos];
        let new_total = quantity.unwrap_or(order.submit.quantity);
        let filled = order.submit.quantity - order.leaves_qty;

        if quantity.is_some() && new_total <= Decimal::ZERO {
            return vec![ServerMessage::OrderModifyRejected {
                client_order_id,
                venue_order_id: Some(venue_order_id),
                reason: "modify to non-positive quantity".into(),
                ts_event: ts,
            }];
        }

        if quantity.is_some() && new_total <= filled {
            return vec![ServerMessage::OrderModifyRejected {
                client_order_id,
                venue_order_id: Some(venue_order_id),
                reason: "modify below already-filled quantity".into(),
                ts_event: ts,
            }];
        }

        if let Some(new_price) = price
            && new_price <= Decimal::ZERO
        {
            return vec![ServerMessage::OrderModifyRejected {
                client_order_id,
                venue_order_id: Some(venue_order_id),
                reason: "modify to non-positive price".into(),
                ts_event: ts,
            }];
        }

        let (quantity, price, leaves_qty, submit_for_warning) = {
            let order = &mut self.open[pos];
            if price.is_some() {
                order.submit.price = price;
            }
            order.submit.quantity = new_total;
            order.leaves_qty = new_total - filled;
            (
                order.submit.quantity,
                order.submit.price,
                order.leaves_qty,
                order.submit.clone(),
            )
        };

        self.warn_priceless_reservation(&submit_for_warning);

        vec![
            ServerMessage::OrderUpdated {
                client_order_id,
                venue_order_id,
                quantity,
                price,
                leaves_qty,
                ts_event: ts,
            },
            ServerMessage::AccountState(self.snapshot(ts)),
        ]
    }

    fn apply_fill(&mut self, fill: &OrderFilled) {
        self.apply_position(fill);

        if fill.last_px == Decimal::ZERO {
            self.warn_zero_px(&fill.symbol);
        }

        let Some(instrument) = self.instruments.get(&fill.symbol) else {
            self.warn_missing_instrument(&fill.symbol);
            return;
        };

        let base = instrument.base.clone();
        let quote = instrument.quote.clone();
        let notional = fill.last_qty * fill.last_px;
        match fill.side {
            Side::Buy => {
                *self.account.balances.entry(base).or_default() += fill.last_qty;
                *self.account.balances.entry(quote).or_default() -= notional + fill.commission;
            }
            Side::Sell => {
                *self.account.balances.entry(base).or_default() -= fill.last_qty;
                *self.account.balances.entry(quote).or_default() += notional - fill.commission;
            }
        }
    }

    fn apply_position(&mut self, fill: &OrderFilled) {
        let delta = match fill.side {
            Side::Buy => fill.last_qty,
            Side::Sell => -fill.last_qty,
        };
        let current = self
            .account
            .positions
            .get(&fill.symbol)
            .cloned()
            .unwrap_or_default();

        let next = next_position(&current, delta, fill.last_px);
        if next.qty == Decimal::ZERO {
            self.account.positions.remove(&fill.symbol);
        } else {
            self.account.positions.insert(fill.symbol.clone(), next);
        }
    }

    fn snapshot(&self, ts: u64) -> AccountState {
        let locked = self.locked_balances();
        let mut currencies: Vec<String> = self.account.balances.keys().cloned().collect();
        for currency in locked.keys() {
            if !currencies.contains(currency) {
                currencies.push(currency.clone());
            }
        }
        currencies.sort();

        let balances = currencies
            .into_iter()
            .map(|currency| {
                let total = *self
                    .account
                    .balances
                    .get(&currency)
                    .unwrap_or(&Decimal::ZERO);
                let locked = *locked.get(&currency).unwrap_or(&Decimal::ZERO);
                Balance {
                    currency,
                    total,
                    free: total - locked,
                    locked,
                }
            })
            .collect();

        AccountState {
            balances,
            positions: self.positions(),
            ts_event: ts,
        }
    }

    fn locked_balances(&self) -> HashMap<String, Decimal> {
        let mut locked = HashMap::new();

        for order in &self.open {
            let Some(instrument) = self.instruments.get(&order.submit.symbol) else {
                continue;
            };

            match order.submit.side {
                Side::Buy => {
                    let reservation = order
                        .submit
                        .price
                        .map_or(Decimal::ZERO, |price| order.leaves_qty * price);
                    *locked.entry(instrument.quote.clone()).or_default() += reservation;
                }
                Side::Sell => {
                    *locked.entry(instrument.base.clone()).or_default() += order.leaves_qty;
                }
            }
        }

        locked
    }

    fn warn_missing_instrument(&mut self, symbol: &str) {
        if self.warned.missing_instrument.insert(symbol.into()) {
            tracing::warn!(%symbol, "account balance leg skipped for unknown instrument");
        }
    }

    fn warn_zero_px(&mut self, symbol: &str) {
        if self.warned.zero_px.insert(symbol.into()) {
            tracing::warn!(%symbol, "account fill booked with zero price");
        }
    }

    fn warn_priceless_reservation(&mut self, order: &SubmitOrder) {
        if order.side == Side::Buy
            && order.price.is_none()
            && self
                .warned
                .priceless_reservation
                .insert(order.symbol.clone())
        {
            let symbol = order.symbol.as_str();
            tracing::warn!(%symbol, "resting buy order has no price to reserve");
        }
    }

    pub fn account_snapshot(&self, ts: u64) -> AccountState {
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

fn next_position(current: &PositionState, delta: Decimal, px: Decimal) -> PositionState {
    if current.qty == Decimal::ZERO {
        return PositionState {
            qty: delta,
            avg_px: px,
        };
    }

    if same_sign(current.qty, delta) {
        let current_abs = abs(current.qty);
        let delta_abs = abs(delta);
        return PositionState {
            qty: current.qty + delta,
            avg_px: ((current_abs * current.avg_px) + (delta_abs * px)) / (current_abs + delta_abs),
        };
    }

    let current_abs = abs(current.qty);
    let delta_abs = abs(delta);
    let qty = current.qty + delta;
    if delta_abs < current_abs {
        PositionState {
            qty,
            avg_px: current.avg_px,
        }
    } else if delta_abs == current_abs {
        PositionState {
            qty: Decimal::ZERO,
            avg_px: Decimal::ZERO,
        }
    } else {
        PositionState { qty, avg_px: px }
    }
}

fn same_sign(a: Decimal, b: Decimal) -> bool {
    (a > Decimal::ZERO && b > Decimal::ZERO) || (a < Decimal::ZERO && b < Decimal::ZERO)
}

fn abs(value: Decimal) -> Decimal {
    if value < Decimal::ZERO { -value } else { value }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mogwai_protocol::{OrderType, Side, TimeInForce};
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
        SubmitOrder {
            client_order_id: id.into(),
            symbol: symbol.into(),
            side,
            order_type: if price.is_some() {
                OrderType::Limit
            } else {
                OrderType::Market
            },
            quantity: Decimal::from(qty),
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
        e.arm(Divergence::DuplicateNextFill);

        let out = e.process(ClientMessage::SubmitOrder(order("O1", 10)), 1);

        assert_eq!(out.len(), 4);
        assert!(matches!(out[1], ServerMessage::OrderFilled(_)));
        assert!(matches!(out[2], ServerMessage::OrderFilled(_)));
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

        let out = e.process(
            ClientMessage::ModifyOrder {
                client_order_id: "O1".into(),
                price: None,
                quantity: Some(Decimal::from(3)),
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
            } if reason == "modify below already-filled quantity"
        ));
        assert_eq!(e.open_orders()[0].submit.quantity, Decimal::from(10));
        assert_eq!(e.open_orders()[0].leaves_qty, Decimal::from(7));
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
    fn missing_instrument_books_position_only_no_panic() {
        let mut e = Engine::new();
        let order = order_with("O1", Side::Buy, "ETHUSDT", 10, Some(Decimal::from(100)));
        let out = e.process(ClientMessage::SubmitOrder(order), 1);
        let state = account(&out, 2);

        assert!(state.balances.is_empty());
        let pos = position(state, "ETHUSDT");
        assert_eq!(pos.quantity, Decimal::from(10));
        assert_eq!(pos.avg_px, Decimal::from(100));
    }

    #[test]
    fn priceless_partial_market_buy_reserves_zero_quote() {
        let mut e = Engine::new();
        e.arm(Divergence::PartialFillNext {
            client_order_id: "O1".into(),
            fraction: Decimal::from_f64(0.5).unwrap(),
        });
        let order = order_with("O1", Side::Buy, "BTCUSDT", 10, None);
        let out = e.process(ClientMessage::SubmitOrder(order), 1);
        let state = account(&out, 2);

        let usdt = balance(state, "USDT");
        assert_eq!(usdt.total, Decimal::ZERO);
        assert_eq!(usdt.locked, Decimal::ZERO);
        assert_eq!(usdt.free, Decimal::ZERO);

        let pos = position(state, "BTCUSDT");
        assert_eq!(pos.quantity, Decimal::from(5));
        assert_eq!(pos.avg_px, Decimal::ZERO);
    }
}
