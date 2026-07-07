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
    ServerMessage, Side, SubmitOrder, Symbol, TimeInForce, VenueOrderId, control::Divergence,
    default_instruments,
};
use rust_decimal::Decimal;

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
const MAX_ARMED_DIVERGENCES: usize = 1_024;

/// A resting order tracked by the venue.
#[derive(Debug, Clone)]
pub struct OpenOrder {
    pub venue_order_id: VenueOrderId,
    pub submit: SubmitOrder,
    pub leaves_qty: Decimal,
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
    /// Symbols/currencies whose ledger arithmetic has saturated at the Decimal
    /// range boundary (see `add_clamped` and friends). Once a key clips, its
    /// stored value is a boundary, not a true total, so the condition persists
    /// across every later snapshot - warn once per key, not once per event.
    saturated: HashSet<String>,
}

#[derive(Debug)]
pub struct Engine {
    open: Vec<OpenOrder>,
    account: Account,
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
        Self::with_instruments(default_instruments())
    }

    pub fn with_instruments(instruments: Vec<InstrumentDef>) -> Self {
        let instruments = instruments
            .into_iter()
            .map(|instrument| (instrument.symbol.clone(), instrument))
            .collect();

        Self {
            open: Vec::new(),
            account: Account::default(),
            instruments,
            seen_client_order_ids: HashMap::new(),
            armed: VecDeque::new(),
            seq: 0,
            warned: Warned::default(),
        }
    }

    pub fn instrument_defs(&self) -> Vec<InstrumentDef> {
        let mut defs: Vec<_> = self.instruments.values().cloned().collect();
        defs.sort_by(|a, b| a.symbol.cmp(&b.symbol));
        defs
    }

    /// Arm a divergence to fire on the next matching trigger (control plane).
    pub fn arm(&mut self, d: Divergence) {
        match d {
            // Server-owned temporal/control divergences have no engine-side
            // trigger, so `take_armed` would never consume them. Dropping them
            // here keeps them from accumulating as dead entries in the armed
            // queue.
            Divergence::DelayAcks { .. }
            | Divergence::GoDark { .. }
            | Divergence::StallData { .. }
            | Divergence::ClearDivergences => {}
            other => {
                // Bound the queue so control-plane arms cannot accumulate
                // without limit. At the cap, shed the OLDEST entry: a
                // never-triggered targeted `PartialFillNext` sits at the front
                // (its order may never arrive), so dropping the front sheds the
                // accumulated stale leftovers rather than the arm just
                // requested. See `MAX_ARMED_DIVERGENCES` and `clear_armed`.
                if self.armed.len() >= MAX_ARMED_DIVERGENCES {
                    self.armed.pop_front();
                    tracing::warn!(
                        cap = MAX_ARMED_DIVERGENCES,
                        "armed divergence queue at capacity; dropped the oldest entry"
                    );
                }
                self.armed.push_back(other);
            }
        }
    }

    /// Flush every engine-side armed divergence, draining the queue outright.
    ///
    /// The single-shot divergences (`PartialFillNext`, `RejectNextSubmit`,
    /// `DuplicateNextFill`, `DropNextAccountUpdate`) normally self-disarm on
    /// their own trigger, but a TARGETED `PartialFillNext` whose order never
    /// arrives has no trigger and would sit armed forever - a leftover from one
    /// scenario can otherwise ambush a later scenario that reuses the same order
    /// id. This is the explicit escape hatch a harness calls between scenarios
    /// to guarantee a clean slate.
    ///
    /// Deliberately distinct from the wire `control::Divergence::ClearDivergences`,
    /// which clears ONLY the server-owned temporal windows (`DelayAcks`,
    /// `GoDark`, `StallData`) and by documented contract leaves the engine-side
    /// single-shots alone. Widening that wire variant to also flush the engine
    /// queue would change its contract; this crate-local method flushes without
    /// touching the wire, so a future server that wants a full reset on
    /// `ClearDivergences` can call this alongside its atomics rather than
    /// overloading the variant.
    pub fn clear_armed(&mut self) {
        self.armed.clear();
    }

    /// Monotonic id source; the server stamps real timestamps.
    fn next_id(&mut self, prefix: &str) -> String {
        self.seq += 1;
        format!("{prefix}-{}", self.seq)
    }

    /// Consume the first armed divergence that *applies* to the current event,
    /// leaving every non-matching entry in place and still armed.
    ///
    /// Consumption used to peek only `front()`, which head-of-line-blocked the
    /// whole queue: a `PartialFillNext` targeted at an order other than the one
    /// being processed sat at `front()` forever (its target may never arrive),
    /// silently disarming every engine-side divergence queued behind it
    /// (`DuplicateNextFill`, `DropNextAccountUpdate`, another `PartialFillNext`).
    /// Scanning for the first *applicable* entry instead lets a still-waiting
    /// targeted `PartialFillNext` stay armed until its order shows up without
    /// stalling the divergences behind it. Order is otherwise preserved: the
    /// first applicable entry wins, so two divergences that both apply to the
    /// same event still fire in arm order.
    fn take_armed(&mut self, applies: impl Fn(&Divergence) -> bool) -> Option<Divergence> {
        let pos = self.armed.iter().position(applies)?;
        self.armed.remove(pos)
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
        if let Err(reason) = self.validate_submit(&order) {
            return vec![ServerMessage::OrderRejected {
                client_order_id: order.client_order_id,
                reason,
                ts_event: ts,
            }];
        }

        // Divergence: reject the next submit outright. `RejectNextSubmit`
        // carries no target, so it applies to whatever submit arrives next.
        if let Some(Divergence::RejectNextSubmit { reason }) =
            self.take_armed(|d| matches!(d, Divergence::RejectNextSubmit { .. }))
        {
            return vec![ServerMessage::OrderRejected {
                client_order_id: order.client_order_id,
                reason,
                ts_event: ts,
            }];
        }

        // Order here is load-bearing. `fill_fraction` consumes the targeted
        // `PartialFillNext` for this id, and it must run BEFORE the FOK decision
        // because all-or-nothing is judged against the (possibly diverged) fill
        // size, not the requested quantity. A FOK the partial pushes below full
        // is rejected right here, and the partial it consumed goes with it: the
        // divergence fired on its target (it is what killed the order), so it is
        // correctly spent rather than left armed to ambush a later resubmit of
        // the same id.
        let fill_fraction = self.fill_fraction(&order);
        // `validate_submit` already confirmed the instrument exists; a missing
        // entry here would mean `Decimal::ZERO`, which `floor_to_increment`
        // treats as "not a grid" and passes the raw fraction through.
        let size_increment = self
            .instruments
            .get(&order.symbol)
            .map_or(Decimal::ZERO, |instrument| instrument.size_increment);
        let last_qty = fill_quantity(&order, fill_fraction, size_increment);
        let leaves_qty = order.quantity - last_qty;

        if order.time_in_force == TimeInForce::Fok && leaves_qty > Decimal::ZERO {
            return vec![ServerMessage::OrderRejected {
                client_order_id: order.client_order_id,
                reason: "fill-or-kill could not fully fill".into(),
                ts_event: ts,
            }];
        }

        // Reserve the id only now, past every reject gate (validation,
        // RejectNextSubmit, FOK). Only an ACCEPTED order reserves its id, so a
        // rejected submit can be corrected and resubmitted under the same id,
        // while a duplicate of an accepted id is caught in `validate_submit`.
        // The venue id rides along so a cancel/modify that arrives after this
        // order has gone terminal can still name it on the reject.
        let venue_order_id = self.next_id("V");
        self.seen_client_order_ids
            .insert(order.client_order_id.clone(), venue_order_id.clone());

        let mut out = vec![ServerMessage::OrderAccepted {
            client_order_id: order.client_order_id.clone(),
            venue_order_id: venue_order_id.clone(),
            ts_event: ts,
        }];

        // A zero `last_qty` means a wire-valid `PartialFillNext` fraction
        // floored below one size increment on a minimum-lot order: the grid
        // cannot represent the partial, so nothing fills (see `fill_quantity`).
        // Skip the fill entirely rather than emit a zero-qty fill or silently
        // promote it to a full fill - the order simply rests (GTC) or cancels
        // (IOC) on its full `leaves_qty`, and a FOK was already rejected above
        // because that same `leaves_qty` is the whole order. `last_qty == 0`
        // only ever happens under an armed partial (the clean path fills fully),
        // so this never perturbs a normal submit. An armed `DuplicateNextFill`
        // is left in place: it applies to a fill, and no fill was produced.
        if last_qty > Decimal::ZERO {
            let last_px = order.price.expect("validated submit price is present");

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
            // Untargeted: applies to the fill just produced. Scanned (not peeked
            // at `front()`) so a non-matching targeted `PartialFillNext` parked
            // ahead of it in the queue cannot block it - see `take_armed`.
            let duplicate = self
                .take_armed(|d| matches!(d, Divergence::DuplicateNextFill))
                .is_some();
            if duplicate {
                out.push(ServerMessage::OrderFilled(fill.clone()));
            }
            out.push(ServerMessage::OrderFilled(fill));
        }

        if leaves_qty > Decimal::ZERO {
            match order.time_in_force {
                TimeInForce::Gtc => {
                    self.open.push(OpenOrder {
                        venue_order_id,
                        submit: order,
                        leaves_qty,
                    });
                }
                TimeInForce::Ioc => {
                    out.push(ServerMessage::OrderCanceled {
                        client_order_id: order.client_order_id,
                        venue_order_id,
                        ts_event: ts,
                    });
                }
                TimeInForce::Fok => unreachable!("FOK partials are rejected before acceptance"),
            }
        }

        // Untargeted: applies to this submit's account-state snapshot. Scanned
        // for the same head-of-line-blocking reason as the duplicate divergence.
        let drop_update = self
            .take_armed(|d| matches!(d, Divergence::DropNextAccountUpdate))
            .is_some();
        if !drop_update {
            out.push(ServerMessage::AccountState(self.snapshot(ts)));
        }
        out
    }

    fn validate_submit(&self, order: &SubmitOrder) -> Result<(), String> {
        if order.client_order_id.trim().is_empty() {
            return Err("empty client_order_id".into());
        }

        // "Duplicate" means an id already ACCEPTED (the map is populated only on
        // the accept path in `on_submit`), so a previously-rejected id is free to
        // reuse; an id that named a live or completed order is not.
        if self
            .seen_client_order_ids
            .contains_key(order.client_order_id.as_str())
        {
            return Err("duplicate client_order_id".into());
        }

        let Some(instrument) = self.instruments.get(&order.symbol) else {
            return Err("unknown instrument".into());
        };

        if order.quantity <= Decimal::ZERO {
            return Err("submit with non-positive quantity".into());
        }

        if !on_increment(order.quantity, instrument.size_increment) {
            return Err("quantity violates size increment".into());
        }

        let Some(price) = order.price else {
            return Err("submit price required".into());
        };

        // No order-type special case: this venue prices Market orders like any
        // other submit (a price is required just above, since synthetic fills
        // execute at the order's own price), so a non-positive price earns the
        // same rejection regardless of order type.
        if price <= Decimal::ZERO {
            return Err("submit with non-positive price".into());
        }

        if !on_increment(price, instrument.price_increment) {
            return Err("price violates price increment".into());
        }

        // No upstream layer bounds order size, and rust_decimal's `Mul` panics
        // on overflow: `apply_fill`'s `last_qty * last_px` runs unconditionally
        // once this order is accepted, so a notional this validator lets
        // through must be one `apply_fill` can actually compute. Reject here,
        // at the venue's front door, rather than let a single oversized order
        // panic the engine mid-fill.
        if order.quantity.checked_mul(price).is_none() {
            return Err("order notional exceeds maximum representable value".into());
        }

        Ok(())
    }

    fn fill_fraction(&mut self, order: &SubmitOrder) -> Decimal {
        // Divergence: partial-fill the next order. `PartialFillNext` is
        // targeted: it applies only to the order whose id it names, so a
        // divergence for a different order is left armed and does not block the
        // rest of the queue.
        let order_id = order.client_order_id.clone();
        match self.take_armed(|d| {
            matches!(
                d,
                Divergence::PartialFillNext { client_order_id, .. }
                    if *client_order_id == order_id
            )
        }) {
            Some(Divergence::PartialFillNext { fraction, .. }) => fraction,
            _ => Decimal::ONE,
        }
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
            // The order is not resting, so there is nothing to cancel: it is
            // either unknown or already terminal (the no-book engine fills a
            // limit on accept, so it can be gone before a cancel arrives). This
            // is a CANCEL rejection, not an ORDER rejection - emitting
            // OrderRejected here would drive the adapter (and nautilus) to flip
            // an already-filled order to Rejected, an invalid transition.
            // Mirrors on_modify's OrderModifyRejected path: a terminal id keeps
            // the venue id it was accepted under, and only a genuinely unknown
            // id goes out with venue_order_id absent.
            let (reason, venue_order_id) =
                terminal_or_unknown_reject(&self.seen_client_order_ids, &client_order_id);
            vec![ServerMessage::OrderCancelRejected {
                reason,
                client_order_id,
                venue_order_id,
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
            let (reason, venue_order_id) =
                terminal_or_unknown_reject(&self.seen_client_order_ids, &client_order_id);
            return vec![ServerMessage::OrderModifyRejected {
                reason,
                client_order_id,
                venue_order_id,
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

        // `<=`, not `<`: a new total EQUAL to the filled amount would leave
        // zero remaining - shrinking an order to nothing is a cancel, not a
        // modify - so equality is rejected too, and the reason says "at or
        // below" to match what the condition actually fires on.
        if quantity.is_some() && new_total <= filled {
            return vec![ServerMessage::OrderModifyRejected {
                client_order_id,
                venue_order_id: Some(venue_order_id),
                reason: "modify to at or below already-filled quantity".into(),
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

        // Submit enforces the instrument's price/size grid; modify must too,
        // or a resting order can drift to an off-grid price/quantity that a
        // fresh submit would have rejected outright (and that off-grid state
        // then goes out on the wire via `OrderUpdated`).
        let Some(instrument) = self.instruments.get(&order.submit.symbol) else {
            return vec![ServerMessage::OrderModifyRejected {
                client_order_id,
                venue_order_id: Some(venue_order_id),
                reason: "unknown instrument".into(),
                ts_event: ts,
            }];
        };

        if !on_increment(new_total, instrument.size_increment) {
            return vec![ServerMessage::OrderModifyRejected {
                client_order_id,
                venue_order_id: Some(venue_order_id),
                reason: "quantity violates size increment".into(),
                ts_event: ts,
            }];
        }

        // Every resting order carries a price (submit requires it, modify
        // never clears it), but `price.or(existing)` is spelled out rather
        // than unwrapped so this validation degrades instead of panicking if
        // that invariant is ever loosened.
        let effective_price = price.or(order.submit.price);
        if let Some(effective_price) = effective_price {
            if !on_increment(effective_price, instrument.price_increment) {
                return vec![ServerMessage::OrderModifyRejected {
                    client_order_id,
                    venue_order_id: Some(venue_order_id),
                    reason: "price violates price increment".into(),
                    ts_event: ts,
                }];
            }

            // Same overflow guard as `validate_submit`: `locked_balances`
            // multiplies `leaves_qty * price` for every open buy order on
            // every snapshot, including the one this very modify emits below,
            // so an unbounded notional here panics immediately rather than in
            // some later, harder-to-trace fill.
            if new_total.checked_mul(effective_price).is_none() {
                return vec![ServerMessage::OrderModifyRejected {
                    client_order_id,
                    venue_order_id: Some(venue_order_id),
                    reason: "order notional exceeds maximum representable value".into(),
                    ts_event: ts,
                }];
            }
        }

        let (quantity, price, leaves_qty) = {
            let order = &mut self.open[pos];
            if price.is_some() {
                order.submit.price = price;
            }
            order.submit.quantity = new_total;
            order.leaves_qty = new_total - filled;
            (order.submit.quantity, order.submit.price, order.leaves_qty)
        };

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

        // Defense-in-depth, unreachable through `process` today: a fill's
        // `last_px` is `order.price`, which `validate_submit` already proved
        // `> 0`, so a zero price never gets here on the public path. `apply_fill`
        // is private, so the only way in is a future in-crate caller that builds
        // a fill some other way; the warn is kept (rather than an `.expect`
        // panic) so such a caller books its ledger LOUDLY-wrong instead of
        // silently, matching this crate's saturate-and-warn philosophy.
        if fill.last_px == Decimal::ZERO {
            self.warn_zero_px(&fill.symbol);
        }

        // Also defense-in-depth: `validate_submit` rejects an unknown
        // instrument at the door and there is no instrument-removal API, so
        // every accepted fill's symbol resolves here on the public path. Kept
        // for the same reason - a future direct caller with a delisted symbol
        // skips the balance leg with a once-per-symbol warn rather than
        // silently corrupting the ledger or panicking.
        let Some(instrument) = self.instruments.get(&fill.symbol) else {
            self.warn_missing_instrument(&fill.symbol);
            return;
        };

        let base = instrument.base.clone();
        let quote = instrument.quote.clone();
        // `fill.commission` is always `Decimal::ZERO` on this venue: mogwai
        // models execution DIVERGENCES (partials, rejects, delays, drops), not
        // fees, so no commission source exists and none is wired. The
        // direction-aware handling below (a buy's cost ADDS commission to the
        // quote spend, a sell's proceeds SUBTRACT it) is kept anyway because
        // `OrderFilled` carries the field on the wire: if a future config or
        // divergence ever populates a non-zero commission, the ledger math is
        // already correct rather than silently dropping or mis-signing the fee.

        // This fill's own notional is representable (`validate_submit` /
        // `on_modify` checked `quantity * price`, and `last_qty <= quantity`),
        // but the running balance totals accumulate notionals across every
        // fill the account has ever taken, and no validator bounds that sum -
        // two individually-valid orders can push a total past `Decimal::MAX`.
        // Saturate instead of letting the operator forms panic; `clipped`
        // surfaces the (persistent) corruption as a once-per-symbol warning.
        let mut clipped = false;
        let notional = mul_clamped(fill.last_qty, fill.last_px, &mut clipped);
        match fill.side {
            Side::Buy => {
                let base_total = self.account.balances.entry(base).or_default();
                *base_total = add_clamped(*base_total, fill.last_qty, &mut clipped);
                let spend = add_clamped(notional, fill.commission, &mut clipped);
                let quote_total = self.account.balances.entry(quote).or_default();
                *quote_total = sub_clamped(*quote_total, spend, &mut clipped);
            }
            Side::Sell => {
                let base_total = self.account.balances.entry(base).or_default();
                *base_total = sub_clamped(*base_total, fill.last_qty, &mut clipped);
                let proceeds = sub_clamped(notional, fill.commission, &mut clipped);
                let quote_total = self.account.balances.entry(quote).or_default();
                *quote_total = add_clamped(*quote_total, proceeds, &mut clipped);
            }
        }
        if clipped {
            self.warn_saturated(&fill.symbol);
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

        let mut clipped = false;
        let next = next_position(&current, delta, fill.last_px, &mut clipped);
        if clipped {
            self.warn_saturated(&fill.symbol);
        }
        if next.qty == Decimal::ZERO {
            self.account.positions.remove(&fill.symbol);
        } else {
            self.account.positions.insert(fill.symbol.clone(), next);
        }
    }

    // `&mut self` purely for the once-per-currency saturation warning below;
    // the account state itself is only read.
    fn snapshot(&mut self, ts: u64) -> AccountState {
        let (locked, mut clipped_currencies) = self.locked_balances();
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
                // `total` and `locked` are each in range, but they can sit
                // near opposite Decimal boundaries (a huge short spend vs a
                // huge buy reservation), so the difference itself can
                // overflow. Saturate rather than panic; a clipped `free` is
                // already implied by the clipped inputs it derives from.
                let mut clipped = false;
                let free = sub_clamped(total, locked, &mut clipped);
                if clipped {
                    clipped_currencies.push(currency.clone());
                }
                Balance {
                    currency,
                    total,
                    free,
                    locked,
                }
            })
            .collect();

        for currency in clipped_currencies {
            self.warn_saturated(&currency);
        }

        AccountState {
            balances,
            positions: self.positions(),
            ts_event: ts,
        }
    }

    /// Sums the per-order reservations. Returns the locked totals plus the
    /// currencies whose accumulation clipped at the Decimal boundary, so the
    /// caller (`snapshot`, which holds `&mut self`) can warn once per key -
    /// this stays `&self` because it runs inside iteration over `self.open`.
    fn locked_balances(&self) -> (HashMap<String, Decimal>, Vec<String>) {
        let mut locked = HashMap::new();
        let mut clipped_currencies = Vec::new();

        for order in &self.open {
            // Defense-in-depth, unreachable through `process`: an order only
            // rests here after `validate_submit`/`on_modify` confirmed its
            // instrument, and none is ever removed, so the lookup always
            // resolves on the public path. Kept as a skip (not an `.expect`)
            // so a future direct caller with a stale open order simply omits
            // that order's reservation rather than panicking the snapshot.
            let Some(instrument) = self.instruments.get(&order.submit.symbol) else {
                continue;
            };

            match order.submit.side {
                Side::Buy => {
                    // A resting order's price is never `None`: `validate_submit`
                    // requires it for every submit (market included - see
                    // `on_submit`'s doc), and `on_modify` only ever sets it,
                    // never clears it.
                    let price = order
                        .submit
                        .price
                        .expect("resting order price is always Some");
                    // One order's `leaves_qty * price` is representable
                    // (submit and modify both `checked_mul` the full
                    // notional), but the SUM across all resting buys is
                    // unbounded - saturate instead of letting `+=` panic.
                    let mut clipped = false;
                    let reserve = mul_clamped(order.leaves_qty, price, &mut clipped);
                    let total = locked.entry(instrument.quote.clone()).or_default();
                    *total = add_clamped(*total, reserve, &mut clipped);
                    if clipped {
                        clipped_currencies.push(instrument.quote.clone());
                    }
                }
                Side::Sell => {
                    let mut clipped = false;
                    let total = locked.entry(instrument.base.clone()).or_default();
                    *total = add_clamped(*total, order.leaves_qty, &mut clipped);
                    if clipped {
                        clipped_currencies.push(instrument.base.clone());
                    }
                }
            }
        }

        (locked, clipped_currencies)
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

    /// `key` is a symbol on the position path, a currency on the balance
    /// paths. Saturation is sticky - a clipped total stays at the boundary -
    /// so this fires once per key rather than flooding the log on every
    /// subsequent fill and snapshot.
    fn warn_saturated(&mut self, key: &str) {
        if self.warned.saturated.insert(key.into()) {
            tracing::warn!(
                %key,
                "ledger arithmetic saturated at the Decimal range boundary; \
                 the stored value is clipped, not an exact total"
            );
        }
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

fn next_position(
    current: &PositionState,
    delta: Decimal,
    px: Decimal,
    clipped: &mut bool,
) -> PositionState {
    if current.qty == Decimal::ZERO {
        return PositionState {
            qty: delta,
            avg_px: px,
        };
    }

    if same_sign(current.qty, delta) {
        // The VWAP numerator is a sum of notionals: each product is bounded
        // by its own order's `checked_mul` at submit time, but the running
        // position can carry an arbitrary accumulated notional, so both
        // products and the sum can overflow on input every validator rightly
        // accepted. Saturate rather than panic (this was a caller-reachable
        // panic through `process` with two individually-valid orders).
        let current_abs = current.qty.abs();
        let delta_abs = delta.abs();
        let numerator = add_clamped(
            mul_clamped(current_abs, current.avg_px, clipped),
            mul_clamped(delta_abs, px, clipped),
            clipped,
        );
        let denominator = add_clamped(current_abs, delta_abs, clipped);
        // The true VWAP is a weighted average of `avg_px` and `px`, so it is
        // always representable; the division fails only when the numerator
        // itself already saturated, and then the newest fill's price is as
        // honest an estimate as any other clipped value.
        let avg_px = numerator.checked_div(denominator).unwrap_or_else(|| {
            *clipped = true;
            px
        });
        return PositionState {
            qty: add_clamped(current.qty, delta, clipped),
            avg_px,
        };
    }

    let current_abs = current.qty.abs();
    let delta_abs = delta.abs();
    // Opposite signs: the magnitude shrinks (or flips within the larger
    // operand's range), so this addition cannot overflow.
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

// Overflow-tolerant arithmetic for the ledger accumulation paths.
//
// `validate_submit` and `on_modify` bound a SINGLE order's notional with
// `checked_mul`, so any one fill's arithmetic is representable - but nothing
// bounds what many individually-valid orders accumulate to. Balance totals,
// locked reservations, the position VWAP numerator and the snapshot's
// `total - locked` all combine values across fills and orders, and
// rust_decimal's operator forms panic on overflow, so two orders the
// validators rightly accept could crash the engine mid-fill. These helpers
// compute checked first and saturate at the Decimal range boundary on
// overflow, flagging `clipped` so the caller can surface a once-per-key
// warning through `Engine::warn_saturated`: a saturated ledger is
// numerically wrong, and clipping silently would corrupt the accounting
// invisibly, but a loudly-wrong fake venue beats one that panics
// mid-scenario.

fn add_clamped(a: Decimal, b: Decimal, clipped: &mut bool) -> Decimal {
    a.checked_add(b).unwrap_or_else(|| {
        *clipped = true;
        a.saturating_add(b)
    })
}

fn sub_clamped(a: Decimal, b: Decimal, clipped: &mut bool) -> Decimal {
    a.checked_sub(b).unwrap_or_else(|| {
        *clipped = true;
        a.saturating_sub(b)
    })
}

fn mul_clamped(a: Decimal, b: Decimal, clipped: &mut bool) -> Decimal {
    a.checked_mul(b).unwrap_or_else(|| {
        *clipped = true;
        a.saturating_mul(b)
    })
}

fn fill_quantity(order: &SubmitOrder, fill_fraction: Decimal, size_increment: Decimal) -> Decimal {
    // Defensive guard against an unvalidated `PartialFillNext.fraction`.
    // Public control validation rejects out-of-range fractions, but
    // `Engine::arm` performs no range check of its own, so the engine still
    // protects its arithmetic so direct callers cannot emit zero, negative,
    // or over-sized fills - or panic it. The clamp must run BEFORE the
    // multiply: rust_decimal's `Mul` panics on overflow, so an extreme
    // fraction (e.g. `Decimal::MAX`) would take the engine down before
    // `.min(order.quantity)` ever saw the product. With the fraction confined
    // to [0, 1] the product's magnitude is bounded by the already-validated
    // order quantity and cannot overflow. A negative fraction clamps to zero
    // on purpose: that routes it into the existing non-positive fallback
    // below, preserving its full-fill-plus-warn semantics.
    let fill_fraction = fill_fraction.clamp(Decimal::ZERO, Decimal::ONE);
    let candidate = (order.quantity * fill_fraction).min(order.quantity);
    // Re-align to the instrument's size grid: `quantity * fraction` has no
    // reason to land on a size-increment multiple, and a real venue's tick
    // rules could never produce a fill (or the resulting leaves_qty) off that
    // grid. Floor rather than round so the fill never exceeds the fraction
    // the divergence asked for.
    let aligned = floor_to_increment(candidate, size_increment);
    if aligned > Decimal::ZERO {
        return aligned;
    }

    let symbol = order.symbol.as_str();
    if candidate > Decimal::ZERO {
        // The fraction was a valid positive partial, but `quantity * fraction`
        // is smaller than one size increment: on a minimum-lot order the grid
        // simply cannot represent the partial. Fill ZERO rather than round the
        // partial UP to a full fill - a `PartialFillNext` armed to leave a
        // remainder must never invert into its opposite. Critically this keeps
        // a FOK the divergence was armed to kill dead: `last_qty == 0` leaves
        // the whole order unfilled, so the FOK all-or-nothing gate rejects it
        // instead of letting a full fill sneak through. `on_submit` emits no
        // fill event for a zero `last_qty`.
        tracing::warn!(
            %symbol,
            "PartialFillNext fraction floors below one size increment on a \
             minimum-lot order; the partial cannot be represented on the size \
             grid, so nothing fills"
        );
        Decimal::ZERO
    } else {
        // `candidate <= 0` is the genuinely-degenerate case: the fraction
        // clamped to a non-positive value (an unvalidated direct arm - a
        // negative, zero, or `Decimal::MIN` fraction). Here the defensive
        // fallback is a normal full fill, as documented above.
        tracing::warn!(
            %symbol,
            "PartialFillNext produced non-positive last_qty; treating as a normal full fill"
        );
        order.quantity
    }
}

/// Floors `value` down to the nearest multiple of `increment`. Used to keep a
/// diverged partial fill on the instrument's size grid; `increment <= 0` is
/// not a valid grid, so it passes `value` through unchanged rather than
/// dividing by a non-positive number.
fn floor_to_increment(value: Decimal, increment: Decimal) -> Decimal {
    if increment <= Decimal::ZERO {
        return value;
    }
    match value.checked_div(increment) {
        Some(steps) => steps.trunc() * increment,
        None => value,
    }
}

fn on_increment(value: Decimal, increment: Decimal) -> bool {
    if increment <= Decimal::ZERO {
        return false;
    }
    // `checked_div` rather than `/`: rust_decimal's `Div` panics on overflow,
    // and nothing upstream bounds order quantity/price, so a wildly oversized
    // value (still grid-aligned in principle) must fail this check instead of
    // taking down the engine. A value that cannot even be divided by the
    // increment without overflowing is not on the grid as far as this venue
    // is concerned.
    match value.checked_div(increment) {
        Some(ratio) => ratio.fract() == Decimal::ZERO,
        None => false,
    }
}

/// Distinguishes a cancel/modify target that was never a real order from one
/// that was accepted but has since gone terminal (filled - the no-book engine
/// fills a limit immediately on accept, so it can already be gone by the time
/// a cancel/modify for it arrives - or already canceled). `seen_client_order_ids`
/// is populated only on accept and never cleared, so key presence is exactly
/// "this id was once a real order, just not a resting one anymore" - and the
/// venue id retained alongside it goes out on the reject, upholding the wire
/// contract that `venue_order_id` is absent ONLY when the order id is unknown.
/// An id the venue genuinely never saw reads as "unknown order" with no venue
/// id, because none was ever assigned.
fn terminal_or_unknown_reject(
    seen: &HashMap<ClientOrderId, VenueOrderId>,
    client_order_id: &str,
) -> (String, Option<VenueOrderId>) {
    match seen.get(client_order_id) {
        Some(venue_order_id) => (
            "order already terminal (filled or canceled)".into(),
            Some(venue_order_id.clone()),
        ),
        None => ("unknown order".into(), None),
    }
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
}
