// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Order lifecycle: submit (validation, divergence gates, fills, resting),
//! cancel, and modify. Fills are synthetic - the size/price grid, the
//! partial-fill divergence, and the FOK all-or-nothing gate all live here.

use std::collections::HashMap;

use mogwai_protocol::{
    ClientOrderId, OrderFilled, OrderType, ServerMessage, Side, SubmitOrder, TimeInForce,
    VenueOrderId, WireOrderStatus, control::Divergence, trades_through,
};
use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha8Rng;
use rust_decimal::Decimal;

use crate::{Engine, MarketReading, OpenOrder, ScanResult};

impl Engine {
    pub(crate) fn on_submit(
        &mut self,
        order: SubmitOrder,
        ts: u64,
        reading: Option<MarketReading>,
    ) -> Vec<ServerMessage> {
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

        // The fill band. Every order draws a trigger from a stream keyed on its
        // own identity, never on the generator's: a limit fills at its stated
        // price the moment a print is strictly through that trigger, and a
        // market order fills at the last print slipped adversely by the same
        // draw. The band applies to IOC and FOK as well, on narrower grounds
        // than the resting case - the venue does not know what price an
        // aggressor would really get, and filling it at its own stated price is
        // the same lie for an IOC as for a market order.
        let stated_px = order.price.expect("validated submit carries a price");
        let increment = self.instruments[&order.symbol].price_increment;
        let band_ticks = reading.map_or(0, |value| value.band_ticks);
        let trigger_px = draw_trigger(self.fill_seed, &order, stated_px, increment, band_ticks, 0);
        let fill_px = if order.order_type == OrderType::Market {
            reading.map_or_else(
                || {
                    tracing::warn!(client_order_id = %order.client_order_id, "market order has no market reading; using its stated price");
                    stated_px
                },
                |value| {
                    draw_market_price(
                        self.fill_seed,
                        &order,
                        stated_px,
                        value.last_px,
                        increment,
                        band_ticks,
                    )
                },
            )
        } else {
            stated_px
        };
        if let Err(reason) = self.validate_fill_funds(&order, fill_px) {
            return vec![ServerMessage::OrderRejected {
                client_order_id: order.client_order_id,
                reason,
                ts_event: ts,
            }];
        }
        let marketable = order.order_type == OrderType::Market
            || reading.is_some_and(|value| trades_through(order.side, trigger_px, value.last_px));

        if order.order_type == OrderType::Limit && !marketable {
            if order.time_in_force == TimeInForce::Fok {
                // `plan_fill` is called for its CONSUMING effect and its plan
                // thrown away. A targeted `PartialFillNext` which is the very
                // reason a FOK cannot fill must go with the FOK that hit it,
                // rather than stay armed to ambush a resubmit of the same id.
                // Short of its trigger is still now-or-never, so the rejection
                // follows immediately.
                let _ = self.plan_fill(&order, order.quantity);
                return vec![ServerMessage::OrderRejected {
                    client_order_id: order.client_order_id,
                    reason: "fill-or-kill could not fill at its trigger".into(),
                    ts_event: ts,
                }];
            }
            let venue_order_id = self.next_id("V");
            self.seen_client_order_ids
                .insert(order.client_order_id.clone(), venue_order_id.clone());
            let mut out = vec![ServerMessage::OrderAccepted {
                client_order_id: order.client_order_id.clone(),
                venue_order_id: venue_order_id.clone(),
                ts_event: ts,
            }];
            let leaves_qty = order.quantity;
            let record = OpenOrder {
                venue_order_id,
                submit: order,
                leaves_qty,
                ts_accepted: ts,
                ts_last: ts,
                band_ticks,
                trigger_px,
                band_draw: 0,
                scanned_ns: ts,
                revision: 0,
            };
            match record.submit.time_in_force {
                // GTC rests and is swept; an IOC is evaluated exactly once,
                // against the acceptance-time reading, and cancels short of its
                // trigger rather than filling at a price the market never
                // reached.
                TimeInForce::Gtc => self.open.push(record),
                TimeInForce::Ioc => {
                    out.push(ServerMessage::OrderCanceled {
                        client_order_id: record.submit.client_order_id.clone(),
                        venue_order_id: record.venue_order_id.clone(),
                        ts_event: ts,
                    });
                    self.record_closed(&record, WireOrderStatus::Canceled, ts);
                }
                TimeInForce::Fok => unreachable!("FOK rejected above"),
            }
            // Deliberately does NOT consume `DropNextAccountUpdate`, and does
            // not call `plan_fill`, so neither that divergence nor a targeted
            // `PartialFillNext` is spent here: both are armed against the FILL,
            // and a resting order has not had one yet.
            out.push(ServerMessage::AccountState(self.snapshot(ts)));
            return out;
        }

        // Order here is load-bearing. `plan_fill` consumes the targeted
        // `PartialFillNext` for this id, and it must run BEFORE the FOK decision
        // because all-or-nothing is judged against the (possibly diverged) fill
        // size, not the requested quantity. A FOK the partial pushes below full
        // is rejected right here, and the partial it consumed goes with it: the
        // divergence fired on its target (it is what killed the order), so it is
        // correctly spent rather than left armed to ambush a later resubmit of
        // the same id. `plan_fill` needs no venue id precisely so this ordering
        // survives: a rejected FOK must not burn its client order id.
        let last_qty = self.plan_fill(&order, order.quantity);
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
            let fill = self.commit_fill(&order, &venue_order_id, last_qty, leaves_qty, fill_px, ts);
            out.extend(fill);
        }

        // Freeze the accepted order's state once, then route it: rest it
        // (GTC remainder), or close it into the terminal truth store (full
        // fill, or an IOC's canceled remainder) so a `QueryOrders` reply can
        // attest to it after it leaves the book.
        let mut record = OpenOrder {
            venue_order_id,
            submit: order,
            leaves_qty,
            ts_accepted: ts,
            ts_last: ts,
            band_ticks,
            trigger_px,
            band_draw: 0,
            scanned_ns: ts,
            revision: 0,
        };
        if leaves_qty > Decimal::ZERO {
            // Every partial increments `band_draw`, and the sweep is not the
            // only place a partial happens: a marketable-on-arrival limit cut
            // short by an armed `PartialFillNext` leaves a remainder that is a
            // NEW tranche, so it draws a fresh queue position around the
            // unchanged price with the band it was accepted under.
            if last_qty > Decimal::ZERO && record.submit.order_type == OrderType::Limit {
                record.band_draw = 1;
                record.trigger_px = draw_trigger(
                    self.fill_seed,
                    &record.submit,
                    stated_px,
                    increment,
                    band_ticks,
                    1,
                );
            }
            match record.submit.time_in_force {
                TimeInForce::Gtc => {
                    self.open.push(record);
                }
                TimeInForce::Ioc => {
                    out.push(ServerMessage::OrderCanceled {
                        client_order_id: record.submit.client_order_id.clone(),
                        venue_order_id: record.venue_order_id.clone(),
                        ts_event: ts,
                    });
                    self.record_closed(&record, WireOrderStatus::Canceled, ts);
                }
                TimeInForce::Fok => unreachable!("FOK partials are rejected before acceptance"),
            }
        } else {
            self.record_closed(&record, WireOrderStatus::Filled, ts);
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

    /// Apply a batch of off-lock walk results and execute every order the tape
    /// printed through the trigger of.
    ///
    /// Each result is matched back to a still-resting order whose `revision`
    /// AND `scanned_ns` both still equal what the walk was planned against;
    /// anything cancelled, filled, repriced, amended or already
    /// advanced by an overlapping pass is dropped. That check, not liveness, is
    /// what makes walking the tape off the engine lock safe: two overlapping
    /// walks naming one order would otherwise both credit the span they share.
    ///
    /// Returns the batch's events and the number of orders it actually EMITTED
    /// a fill for, which is what the caller reserves delivery bytes against - a
    /// scan the tape did not trigger produces no bytes, so reserving for it would
    /// grow the request with the open-order count against a fixed budget.
    pub fn apply_scans(&mut self, results: &[ScanResult], ts: u64) -> (Vec<ServerMessage>, usize) {
        let mut out = Vec::new();
        let mut emitted = 0;
        for result in results {
            let Some(pos) = self.open.iter().position(|order| {
                order.submit.client_order_id == result.client_order_id
                    && order.revision == result.revision
                    && order.scanned_ns == result.from_ns
            }) else {
                continue;
            };
            let (submit, venue_order_id, leaves) = {
                let order = &mut self.open[pos];
                // The frontier advances to exactly where the walk REACHED, which
                // a spent drain budget may leave short of the pass's target, so
                // a truncated pass loses no span rather than skipping over it.
                order.scanned_ns = result.scanned_to_ns;
                order.revision = order.revision.saturating_add(1);
                (
                    order.submit.clone(),
                    order.venue_order_id.clone(),
                    order.leaves_qty,
                )
            };
            if !result.triggered {
                continue;
            }
            // Sized off the LEAVES, never `submit.quantity`: a swept order may
            // already be partly filled or have been amended, and multiplying a
            // partial-fill fraction by the original quantity would over-fill.
            let last_qty = self.plan_fill(&submit, leaves);
            let new_leaves = leaves - last_qty;
            if last_qty > Decimal::ZERO {
                let fill_px = submit
                    .price
                    .expect("validated resting limit carries a price");
                out.extend(self.commit_fill(
                    &submit,
                    &venue_order_id,
                    last_qty,
                    new_leaves,
                    fill_px,
                    ts,
                ));
                emitted += 1;
            }
            if new_leaves > Decimal::ZERO {
                let order = &mut self.open[pos];
                order.leaves_qty = new_leaves;
                order.ts_last = ts;
                // An execution starts a NEW tranche, so the remainder draws a
                // fresh trigger around the unchanged price and re-covers the
                // span from here. Without this the remainder would rest already
                // triggered and the next pass would fill it for free - the band
                // would leak open on precisely the orders it most has to hold.
                // Each tranche has to be traded through on its own, and gets a
                // fresh queue position while it waits.
                order.band_draw = order.band_draw.saturating_add(1);
                order.trigger_px = draw_trigger(
                    self.fill_seed,
                    &order.submit,
                    order.submit.price.expect("validated limit carries a price"),
                    self.instruments[&order.submit.symbol].price_increment,
                    order.band_ticks,
                    order.band_draw,
                );
                order.scanned_ns = ts;
                order.revision = order.revision.saturating_add(1);
            } else {
                let order = self.open.remove(pos);
                self.record_closed(&order, WireOrderStatus::Filled, ts);
            }
        }
        // ONE snapshot for the whole batch, taken after every fill it booked -
        // which is what `sizing::swept_fill_max_bytes` bounds.
        if emitted > 0 {
            let drop_update = self
                .take_armed(|d| matches!(d, Divergence::DropNextAccountUpdate))
                .is_some();
            if !drop_update {
                out.push(ServerMessage::AccountState(self.snapshot(ts)));
            }
        }
        (out, emitted)
    }

    /// The size this fill WOULD be, and nothing else: consumes the targeted
    /// `PartialFillNext`, clamps it and floors it onto the size grid against
    /// `remaining` (the order's leaves, not its original quantity). Mutates no
    /// ledger and needs no venue id, so `on_submit` can judge FOK against its
    /// answer while a rejected FOK still leaves its client order id unreserved.
    fn plan_fill(&mut self, order: &SubmitOrder, remaining: Decimal) -> Decimal {
        let fraction = self.fill_fraction(order);
        // `validate_submit` already confirmed the instrument exists; a missing
        // entry here would mean `Decimal::ZERO`, which `floor_to_increment`
        // treats as "not a grid" and passes the raw fraction through.
        let increment = self
            .instruments
            .get(&order.symbol)
            .map_or(Decimal::ZERO, |instrument| instrument.size_increment);
        let mut remaining_order = order.clone();
        remaining_order.quantity = remaining;
        fill_quantity(&remaining_order, fraction, increment)
    }

    /// Emit and book a planned fill at the price the caller decided: `apply_fill`,
    /// `record_fill`, and the `DuplicateNextFill` consumption. Shared by
    /// `on_submit` (marketable on arrival, or a slipped market order) and
    /// `apply_scans` (the tape has now traded through a trigger), so the two
    /// paths cannot diverge in WHAT they produce, only in when. A limit always
    /// books at its own stated price; only a market order is slipped, and the
    /// slippage is applied by the caller, not here.
    fn commit_fill(
        &mut self,
        order: &SubmitOrder,
        venue_order_id: &VenueOrderId,
        last_qty: Decimal,
        leaves_qty: Decimal,
        fill_px: Decimal,
        ts: u64,
    ) -> Vec<ServerMessage> {
        let fill = OrderFilled {
            client_order_id: order.client_order_id.clone(),
            venue_order_id: venue_order_id.clone(),
            trade_id: self.next_id("T"),
            symbol: order.symbol.clone(),
            side: order.side,
            last_qty,
            last_px: fill_px,
            leaves_qty,
            commission: Decimal::ZERO,
            ts_event: ts,
        };
        self.apply_fill(&fill);
        // Booked exactly once into the QueryFills truth store, BEFORE the
        // duplicate divergence doubles the wire event: the duplication is the
        // lie this venue injects, the store keeps the truth a reconciler checks
        // against.
        self.record_fill(&fill);
        // Untargeted: applies to the fill just produced. Scanned (not peeked at
        // `front()`) so a non-matching targeted `PartialFillNext` parked ahead
        // of it in the queue cannot block it - see `take_armed`.
        let duplicate = self
            .take_armed(|d| matches!(d, Divergence::DuplicateNextFill))
            .is_some();
        let mut out = Vec::new();
        if duplicate {
            out.push(ServerMessage::OrderFilled(fill.clone()));
        }
        out.push(ServerMessage::OrderFilled(fill));
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
        let Some(notional) = order.quantity.checked_mul(price) else {
            return Err("order notional exceeds maximum representable value".into());
        };

        // Funded accounts are honest cash accounts: an order the free balance
        // cannot cover is rejected at the door, exactly like a real exchange.
        // A buy requires the full quote notional (the immediate synthetic fill
        // spends it, or the resting remainder reserves it - same requirement
        // either way); a sell requires the base quantity. Unfunded accounts
        // skip this entirely and keep the permissive delta-off-zero ledger -
        // see `Engine::enforce_funds`.
        if self.enforce_funds {
            let (currency, required) = match order.side {
                Side::Buy => (&instrument.quote, notional),
                Side::Sell => (&instrument.base, order.quantity),
            };
            if self.free_balance(currency) < required {
                return Err(format!("insufficient {currency} balance"));
            }
        }

        Ok(())
    }

    /// The funds check re-run against the price the order will ACTUALLY fill
    /// at, which for a market order is the slipped price rather than the stated
    /// one. `validate_submit` cleared the stated notional; without this a
    /// slipped buy could overdraw an account that validator had passed. A limit
    /// fills at its own price, so for one this is the same question twice and
    /// answers it the same way.
    fn validate_fill_funds(&self, order: &SubmitOrder, fill_px: Decimal) -> Result<(), String> {
        if !self.enforce_funds || order.side == Side::Sell {
            return Ok(());
        }
        let instrument = &self.instruments[&order.symbol];
        let required = order
            .quantity
            .checked_mul(fill_px)
            .ok_or_else(|| "order notional exceeds maximum representable value".to_string())?;
        if self.free_balance(&instrument.quote) < required {
            return Err(format!("insufficient {} balance", instrument.quote));
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

    pub(crate) fn on_cancel(
        &mut self,
        client_order_id: ClientOrderId,
        ts: u64,
    ) -> Vec<ServerMessage> {
        if let Some(pos) = self
            .open
            .iter()
            .position(|o| o.submit.client_order_id == client_order_id)
        {
            let o = self.open.remove(pos);
            self.record_closed(&o, WireOrderStatus::Canceled, ts);
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

    pub(crate) fn on_modify(
        &mut self,
        client_order_id: ClientOrderId,
        price: Option<Decimal>,
        quantity: Option<Decimal>,
        ts: u64,
        reading: Option<MarketReading>,
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

            // Funded accounts check the amended reservation against free
            // balance, mirroring the submit-side funds check: an amend that
            // grows a reservation past what the account holds is refused, or
            // the venue would advertise free < 0 in its own snapshot. The
            // order's CURRENT reservation is excluded from the comparison
            // (it is being replaced, not added to), so free-plus-own-hold
            // must cover the new hold. Both products are bounded: the new
            // one by the checked_mul just above (leaves <= total), the old
            // one by the same check at its own submit/amend time.
            if self.enforce_funds {
                let order = &self.open[pos];
                let new_leaves = new_total - filled;
                let (currency, held, required) = match order.submit.side {
                    Side::Buy => {
                        let old_price = order.submit.price.unwrap_or_default();
                        (
                            &instrument.quote,
                            order.leaves_qty * old_price,
                            new_leaves * effective_price,
                        )
                    }
                    Side::Sell => (&instrument.base, order.leaves_qty, new_leaves),
                };
                if self.free_balance(currency).saturating_add(held) < required {
                    return vec![ServerMessage::OrderModifyRejected {
                        client_order_id,
                        venue_order_id: Some(venue_order_id),
                        reason: format!("insufficient {currency} balance"),
                        ts_event: ts,
                    }];
                }
            }
        }

        let (quantity, price, leaves_qty) = {
            let order = &mut self.open[pos];
            // Either kind of amend bumps the revision, so a trigger walk
            // already in flight against the pre-amend state is discarded.
            order.revision = order.revision.saturating_add(1);
            if let Some(new_price) = price {
                order.submit.price = Some(new_price);
                // A reprice is a new draw: prints through the OLD trigger are
                // not evidence about the new one, and the order rejoins the
                // queue at the back. A quantity-only amend touches none of it,
                // because the price the market has to trade through has not
                // moved.
                //
                // The re-draw takes a FRESH band when the server supplies one.
                // An amend arrives over the same path that reads the tape on
                // every limit submit, so a reading is available and cheap next
                // to the amend itself; an order repriced hours after acceptance
                // would otherwise keep a band fitted to a regime that is gone.
                // The stored value is the fallback, and is updated to whatever
                // the re-draw used so a later tranche inherits the current
                // regime rather than the acceptance one.
                order.band_draw = order.band_draw.saturating_add(1);
                order.band_ticks = reading.map_or(order.band_ticks, |value| value.band_ticks);
                order.trigger_px = draw_trigger(
                    self.fill_seed,
                    &order.submit,
                    new_price,
                    instrument.price_increment,
                    order.band_ticks,
                    order.band_draw,
                );
                order.scanned_ns = ts;
            }
            order.submit.quantity = new_total;
            order.leaves_qty = new_total - filled;
            order.ts_last = ts;
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
}

/// FNV-1a over the run's fill seed and the order's identity. Deliberately a
/// pure function of client-supplied fields plus `fill_seed`, which means a
/// client that dislikes its trigger can cancel and resubmit under a fresh
/// `client_order_id` to re-roll it. For a test venue whose clients are
/// strategies rather than adversaries that is accepted; it is written down here
/// so nobody later reports it as a leak.
fn draw_key(fill_seed: u64, order: &SubmitOrder, price: Decimal, band_draw: u32) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    let mut feed = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    };
    feed(&fill_seed.to_le_bytes());
    feed(order.symbol.as_bytes());
    feed(&[0]);
    feed(order.client_order_id.as_bytes());
    feed(&[0]);
    feed(&[match order.side {
        Side::Buy => 0,
        Side::Sell => 1,
    }]);
    feed(&price.serialize());
    feed(&band_draw.to_le_bytes());
    hash
}

/// The band draw: an integer number of ticks uniform on `0 ..= band_ticks`.
///
/// Uniform is a declaration, not a fit: the SCALE comes from trailing
/// volatility, and the maximum-entropy shape on a bounded support is the one
/// that claims nothing further. A fresh `ChaCha8Rng` per draw rather than a
/// long-lived stream, so the offset is a pure function of its key and no
/// ordering of submits can perturb another order's trigger.
pub(super) fn draw_offset(
    fill_seed: u64,
    order: &SubmitOrder,
    price: Decimal,
    band_ticks: u32,
    band_draw: u32,
) -> u32 {
    ChaCha8Rng::seed_from_u64(draw_key(fill_seed, order, price, band_draw))
        .random_range(0..=band_ticks)
}

/// Total-function guard for every price the band computes. `Decimal` arithmetic
/// is checked throughout, and a `None` or a non-positive result degenerates to
/// the order's stated price: unreachable at any sane band, but a sell trigger
/// and a buy market fill can overflow at a wide one, and a sell market fill can
/// slip to zero.
fn safe_price(stated: Decimal, candidate: Option<Decimal>) -> Decimal {
    candidate
        .filter(|price| *price > Decimal::ZERO)
        .unwrap_or(stated)
}

/// The trigger for a limit: its stated price moved AWAY from the market by the
/// draw.
///
/// One-sided on purpose. A symmetric band would fill a buy limit while the
/// market traded above it - a fill better than any price the market offered,
/// which is free money the venue manufactured and a strictly worse forward test
/// than the instant fill this replaced. `u = 0` is the front-of-queue draw and
/// reduces to one print strictly through the stated price.
fn draw_trigger(
    fill_seed: u64,
    order: &SubmitOrder,
    price: Decimal,
    increment: Decimal,
    band_ticks: u32,
    band_draw: u32,
) -> Decimal {
    let offset = increment.checked_mul(Decimal::from(draw_offset(
        fill_seed, order, price, band_ticks, band_draw,
    )));
    safe_price(
        price,
        offset.and_then(|offset| match order.side {
            Side::Buy => price.checked_sub(offset),
            Side::Sell => price.checked_add(offset),
        }),
    )
}

/// The fill price for a MARKET order: the last print slipped adversely by a
/// draw from the same band and the same key, with `band_draw = 0`.
///
/// The client's stated price is ignored for PRICING - answering "what price did
/// this trade at" with the client's own number is the same defect the limit
/// band removes - but it is still validated and still keys the draw, because
/// the wire contract requires it. The magnitude here is borrowed rather than
/// fitted: it is the limit band's multiplier, so it introduces no unmeasured
/// number, and a separately fitted market multiplier is a successor change to
/// one config field.
fn draw_market_price(
    fill_seed: u64,
    order: &SubmitOrder,
    stated_px: Decimal,
    last_px: Decimal,
    increment: Decimal,
    band_ticks: u32,
) -> Decimal {
    let offset = increment.checked_mul(Decimal::from(draw_offset(
        fill_seed, order, stated_px, band_ticks, 0,
    )));
    safe_price(
        stated_px,
        offset.and_then(|offset| match order.side {
            Side::Buy => last_px.checked_add(offset),
            Side::Sell => last_px.checked_sub(offset),
        }),
    )
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
