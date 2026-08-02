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
use rust_decimal::Decimal;

use crate::{Engine, OpenOrder, ScanResult};

impl Engine {
    pub(crate) fn on_submit(
        &mut self,
        order: SubmitOrder,
        ts: u64,
        market_px: Option<Decimal>,
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

        // The penetration gate. MARKET is marketable by definition and FOK is
        // decided now or never, so neither is ever gated and neither pays for
        // the server's market reading. A LIMIT that is already through the last
        // print is seeded with one penetration, so an aggressive limit is not
        // taxed a whole sweep interval for arriving marketable.
        let gated = self.penetration_ticks > 0
            && order.order_type == OrderType::Limit
            && matches!(order.time_in_force, TimeInForce::Gtc | TimeInForce::Ioc);
        let seeded = if gated {
            let limit = order.price.expect("a validated limit carries a price");
            u32::from(market_px.is_some_and(|px| trades_through(order.side, limit, px)))
        } else {
            0
        };
        if gated && seeded < self.penetration_ticks {
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
                penetration_count: seeded,
                penetration_scanned_ns: ts,
                revision: 0,
            };
            match record.submit.time_in_force {
                // GTC rests and is swept; an IOC is evaluated exactly once,
                // against the acceptance-time seed, and cancels short rather
                // than filling at a price the market never reached.
                TimeInForce::Gtc => self.open.push(record),
                TimeInForce::Ioc => {
                    out.push(ServerMessage::OrderCanceled {
                        client_order_id: record.submit.client_order_id.clone(),
                        venue_order_id: record.venue_order_id.clone(),
                        ts_event: ts,
                    });
                    self.record_closed(&record, WireOrderStatus::Canceled, ts);
                }
                TimeInForce::Fok => unreachable!("FOK is never gated"),
            }
            // Deliberately does NOT consume `DropNextAccountUpdate`, and does
            // not call `plan_fill`, so neither that divergence nor a targeted
            // `PartialFillNext` is spent here: both are armed against the FILL,
            // and under the gate the fill has not happened yet.
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
            let fill = self.commit_fill(&order, &venue_order_id, last_qty, leaves_qty, ts);
            out.extend(fill);
        }

        // Freeze the accepted order's state once, then route it: rest it
        // (GTC remainder), or close it into the terminal truth store (full
        // fill, or an IOC's canceled remainder) so a `QueryOrders` reply can
        // attest to it after it leaves the book.
        let record = OpenOrder {
            venue_order_id,
            submit: order,
            leaves_qty,
            ts_accepted: ts,
            ts_last: ts,
            penetration_count: 0,
            penetration_scanned_ns: ts,
            revision: 0,
        };
        if leaves_qty > Decimal::ZERO {
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

    /// Apply a batch of off-lock walk results and execute whatever the gate now
    /// admits.
    ///
    /// Each result is matched back to a still-resting order whose `revision`
    /// AND `penetration_scanned_ns` both still equal what the walk was planned
    /// against; anything cancelled, filled, repriced, amended or already
    /// advanced by an overlapping pass is dropped. That check, not liveness, is
    /// what makes walking the tape off the engine lock safe: two overlapping
    /// walks naming one order would otherwise both credit the span they share.
    ///
    /// Returns the batch's events and the number of orders it actually EMITTED
    /// a fill for, which is what the caller reserves delivery bytes against - a
    /// scan below its threshold produces no bytes, so reserving for it would
    /// grow the request with the open-order count against a fixed budget.
    pub fn apply_scans(&mut self, results: &[ScanResult], ts: u64) -> (Vec<ServerMessage>, usize) {
        let mut out = Vec::new();
        let mut emitted = 0;
        for result in results {
            let Some(pos) = self.open.iter().position(|order| {
                order.submit.client_order_id == result.client_order_id
                    && order.revision == result.revision
                    && order.penetration_scanned_ns == result.from_ns
            }) else {
                continue;
            };
            let (submit, venue_order_id, leaves) = {
                let order = &mut self.open[pos];
                order.penetration_count = order.penetration_count.saturating_add(result.counted);
                // The frontier advances to exactly where the walk REACHED, which
                // a spent drain budget may leave short of the pass's target, so
                // a truncated pass loses no span rather than skipping over it.
                order.penetration_scanned_ns = result.scanned_to_ns;
                order.revision = order.revision.saturating_add(1);
                (
                    order.submit.clone(),
                    order.venue_order_id.clone(),
                    order.leaves_qty,
                )
            };
            if self.open[pos].penetration_count < self.penetration_ticks {
                continue;
            }
            // Sized off the LEAVES, never `submit.quantity`: a swept order may
            // already be partly filled or have been amended, and multiplying a
            // partial-fill fraction by the original quantity would over-fill.
            let last_qty = self.plan_fill(&submit, leaves);
            let new_leaves = leaves - last_qty;
            if last_qty > Decimal::ZERO {
                out.extend(self.commit_fill(&submit, &venue_order_id, last_qty, new_leaves, ts));
                emitted += 1;
            }
            if new_leaves > Decimal::ZERO {
                let order = &mut self.open[pos];
                order.leaves_qty = new_leaves;
                order.ts_last = ts;
                // An execution RESTARTS the window. Without this the remainder
                // rests at exactly its threshold and the next pass fills it
                // with zero further penetrations - the gate would leak open on
                // precisely the orders it is most meant to hold. Each tranche
                // has to be traded through on its own.
                order.penetration_count = 0;
                order.penetration_scanned_ns = ts;
                order.revision = order.revision.saturating_add(1);
            } else {
                let order = self.open.remove(pos);
                self.record_closed(&order, WireOrderStatus::Filled, ts);
            }
        }
        // ONE snapshot for the whole batch, taken after every fill it booked -
        // which is what `sizing::penetrated_fill_max_bytes` bounds.
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

    /// Emit and book a planned fill at the ORDER'S price: `apply_fill`,
    /// `record_fill`, and the `DuplicateNextFill` consumption. Shared by
    /// `on_submit` (ungated, or marketable on arrival) and `apply_scans` (the
    /// tape has now traded through), so the two paths cannot diverge in WHAT
    /// they produce, only in when.
    fn commit_fill(
        &mut self,
        order: &SubmitOrder,
        venue_order_id: &VenueOrderId,
        last_qty: Decimal,
        leaves_qty: Decimal,
        ts: u64,
    ) -> Vec<ServerMessage> {
        let fill = OrderFilled {
            client_order_id: order.client_order_id.clone(),
            venue_order_id: venue_order_id.clone(),
            trade_id: self.next_id("T"),
            symbol: order.symbol.clone(),
            side: order.side,
            last_qty,
            last_px: order.price.expect("validated submit price is present"),
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
            // Either kind of amend bumps the revision, so a penetration walk
            // already in flight against the pre-amend state is discarded.
            order.revision = order.revision.saturating_add(1);
            if price.is_some() {
                order.submit.price = price;
                // A reprice restarts the penetration window: prints through the
                // OLD price are not evidence about the new one. A quantity-only
                // amend keeps the count, because the price the market has to
                // trade through has not moved.
                order.penetration_count = 0;
                order.penetration_scanned_ns = ts;
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
