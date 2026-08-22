// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Order lifecycle: submit (validation, divergence gates, fills, resting),
//! cancel, and modify. Fills are synthetic - the size/price grid, the
//! partial-fill divergence, and the FOK all-or-nothing gate all live here.

use std::collections::HashMap;

use mogwai_protocol::{
    ClientOrderId, Contingency, Hit, LiquiditySide, MAX_LINKED_ORDERS, OrderFilled, OrderType,
    Side, SubmitOrder, TimeInForce, VenueMessage, VenueOrderId, WireOrderStatus,
    control::Divergence, touches_trigger, trades_through,
};
use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha8Rng;
use rust_decimal::Decimal;

use crate::{Engine, MarketReading, OpenOrder, Resting, ScanResult};

impl Engine {
    pub(crate) fn on_submit(
        &mut self,
        order: SubmitOrder,
        ts: u64,
        reading: Option<MarketReading>,
    ) -> Vec<VenueMessage> {
        self.on_submit_from(order, ts, reading, true, &[])
    }

    /// The one refusal `on_submit_from` makes BEFORE `validate_submit`, lifted
    /// out so the group's dry pass can make it too.
    ///
    /// It has to be a separate function rather than a rule inside
    /// `validate_submit`, because its sibling branch MUTATES - a hedging order
    /// with no `position_id` that is not reduce-only gets a fresh one assigned -
    /// and `validate_submit` is a pure read that the dry pass depends on staying
    /// pure. Splitting the refusal from the assignment is what lets both passes
    /// ask the same question.
    ///
    /// A dry pass that skipped this admitted a group whose hedging reduce-only
    /// member was then refused in pass two, with its siblings already accepted
    /// and possibly filled - the exact atomicity break the group frame exists to
    /// prevent, in the mode a bracket's exits are most likely to be written for.
    fn hedging_refusal(&self, order: &SubmitOrder) -> Option<String> {
        (self.oms_type == mogwai_protocol::OmsType::Hedging
            && order.position_id.is_none()
            && order.reduce_only)
            .then(|| "hedging reduce-only order requires a position_id".to_string())
    }

    /// THE ONE HOME OF THE CLOSING-SNAPSHOT RULE for the ORDER-ENTRY paths.
    ///
    /// The rule has two halves and both are load-bearing. A batch that MOVED
    /// THE LEDGER owes a snapshot, and that snapshot is exactly what
    /// `DropNextAccountUpdate` exists to hide - so an armed drop takes it and
    /// the consumer is left to notice. A batch that moved NOTHING (a bare
    /// acceptance, a held child taking no hold) still reports the
    /// account, because there is nothing to hide and spending the arm on it
    /// would burn a divergence the operator armed against a real fill.
    ///
    /// `apply_divergences` carries the same meaning it does everywhere else in
    /// this file: FALSE for a VENUE-ORIGINATED batch - a margin or risk breach
    /// closing a position at the mark - which reports the account but must
    /// never spend an arm the consumer aimed at its own next fill. Passing it is
    /// what makes that structural rather than incidental; see `on_cancel`,
    /// which states the same rule for the cancel path.
    ///
    /// WHAT THIS DOES NOT COVER, stated exactly, because a comment claiming a
    /// wider set than it has is this file's signature defect. Three
    /// order-entry sites are exempt and each says so in place - the
    /// resting-limit acceptance, the held-child acceptance and `on_modify`.
    /// `on_submit_from`'s fill snapshot and `on_cancel`'s spell the rule
    /// themselves because each carries an extra carve-out the helper has no
    /// argument for. And the VENUE-MAINTENANCE snapshots in `lib.rs` - `on_mark`,
    /// `settle` and the funding exchange - do not come from an order-entry
    /// command at all: they report unconditionally and consult no arm, which is
    /// the same ruling `apply_divergences` encodes here.
    ///
    /// A caller that must ALSO stay silent on a batch that moved nothing - the
    /// clock-driven sweep, which runs on every tick - tests `account_changed`
    /// itself before calling and gets the arm half from here. An EMPTY batch is
    /// no batch and reports nothing.
    fn push_account_snapshot(
        &mut self,
        out: &mut Vec<VenueMessage>,
        ts: u64,
        apply_divergences: bool,
    ) {
        if out.is_empty() {
            return;
        }
        if apply_divergences
            && account_changed(out)
            && self
                .take_armed(|d| matches!(d, Divergence::DropNextAccountUpdate))
                .is_some()
        {
            return;
        }
        out.push(VenueMessage::AccountState(self.snapshot(ts)));
    }

    /// Submit a LINKED GROUP: every member admitted, or none of them.
    ///
    /// TWO PASSES, and the split is the whole mechanism.
    ///
    /// PASS ONE is a DRY VALIDATION of every member against the current book and
    /// against the rest of the group - the group's own ids count as known, so a
    /// child may name a parent that has not been accepted yet. It mutates
    /// nothing and consumes no divergence arm. If any member would be refused,
    /// the group is refused: one `OrderRejected` per member, the failing one
    /// carrying its own reason and the others naming it, and NO `OrderAccepted`
    /// for anything. That is the atomic-admission guarantee, and it is cheap
    /// precisely because `validate_submit` is a pure read.
    ///
    /// PASS TWO submits each member through the ordinary path, in the order the
    /// consumer sent them, at ONE instant against ONE market reading. So no tape
    /// advances between members and no member meets a market a sibling did not.
    ///
    /// THEN THE CLOSING LINKAGE PASS, which is what makes the guarantee bite
    /// rather than merely sound good, and which is the SOLE application of a
    /// member's linkage - pass two suppresses the one `on_submit_from` would
    /// otherwise do at the fill.
    ///
    /// Applying it at the fill covers only siblings already on the book, and a
    /// filling member's later siblings are not there yet. That is the hazard
    /// the group frame exists to close: an `Ouo` entry filling before its stop
    /// is admitted would leave the stop at FULL size, so the pair's aggregate
    /// fill would be twice the bracket quantity and a crossed slice would
    /// reverse the account. Applying it at the fill AND again here would fix
    /// that one and introduce its mirror, because `Ouo` subtracts the filled
    /// quantity rather than setting a target: a sibling that WAS on the book
    /// would be shrunk twice, and a two-leg bracket sent stop-first would have
    /// its stop driven to zero leaves and cancelled outright. Hence exactly
    /// once, here, with every member admitted.
    ///
    /// It runs before this call returns, under the engine lock, so no sweep can
    /// have looked at any member in between.
    ///
    /// `RejectNextSubmit` is consumed ONCE, for the group, before pass one.
    /// Spending it per member would refuse one leg of an atomic group, which is
    /// the shape this whole path exists to make impossible.
    ///
    /// THE WIRE-SHAPE VALIDATOR RUNS HERE, before anything else, because the
    /// atomicity guarantee must not be contingent on a caller. Two of its rules
    /// are unreachable from the dry pass by construction: a DUPLICATE ID WITHIN
    /// THE GROUP cannot be seen by `validate_submit`, whose duplicate test reads
    /// `seen_client_order_ids` and no member is in it yet, so pass one admitted
    /// the group whole and pass two refused the second copy with its sibling
    /// already accepted and possibly filled; and an `Ioc`/`Fok` MEMBER is a
    /// now-or-never order whose fate admission does not decide. `mogwai-venue`
    /// runs the same validator at the boundary, so nothing arriving over the
    /// wire changes verdict - what changes is that a caller reaching
    /// `process_with_market` directly (a test, a bench, a future gateway) can no
    /// longer break the group open. It is called rather than copied: one arm of
    /// a validator re-spelled here would read as the rule's home while
    /// implementing a fraction of it, which is the defect this file's structural
    /// note names.
    pub(crate) fn on_submit_group(
        &mut self,
        orders: &[SubmitOrder],
        ts: u64,
        reading: Option<MarketReading>,
    ) -> Vec<VenueMessage> {
        let ids: Vec<ClientOrderId> = orders
            .iter()
            .map(|order| order.client_order_id.clone())
            .collect();
        let reject_all = |reason: &str, blame: Option<&str>| {
            ids.iter()
                .map(|id| VenueMessage::OrderRejected {
                    client_order_id: id.clone(),
                    reason: mogwai_protocol::truncate_reason(match blame {
                        Some(blamed) if blamed != id => format!(
                            "order group rejected whole: {blamed} was refused because {reason}"
                        ),
                        _ => reason.to_owned(),
                    }),
                    ts_event: ts,
                })
                .collect::<Vec<_>>()
        };

        // Before the divergence arm: a malformed group is not a submit the
        // author armed against, and spending the arm on one would hide it.
        if let Err(reason) = mogwai_protocol::validate_submit_group(orders) {
            return reject_all(reason, None);
        }

        if let Some(Divergence::RejectNextSubmit { reason }) =
            self.take_armed(|d| matches!(d, Divergence::RejectNextSubmit { .. }))
        {
            return reject_all(&reason, None);
        }

        // PASS ONE. Note it validates each member against the book as it is
        // NOW, not as it will be after earlier members fill - which is why the
        // funds carve-out on `Command::SubmitOrderGroup` exists and is
        // stated there rather than papered over here.
        for order in orders {
            if let Some(reason) = self.dry_refusal(order, ts, orders) {
                return reject_all(&reason, Some(&order.client_order_id));
            }
        }

        // PASS TWO. The WHOLE member list travels into both passes, not just
        // the ids: a check that reads the working book - the equity short check
        // does - has to see the members that have not rested yet, or pass one
        // and pass two compute different numbers over the same intent.
        let mut out = Vec::new();
        let mut filled: Vec<(SubmitOrder, Decimal)> = Vec::new();
        for order in orders {
            let events = self.on_submit_from(order.clone(), ts, reading, true, orders);
            // Pass one said every member was admissible, so a refusal here is
            // either the disclosed funds carve-out or a hole in the dry pass.
            // Which one it is decides whether this is a warning or a defect,
            // and nothing else in the system can tell them apart afterwards.
            if let Some(reason) = events.iter().find_map(|event| match event {
                VenueMessage::OrderRejected {
                    client_order_id,
                    reason,
                    ..
                } if *client_order_id == order.client_order_id => Some(reason.clone()),
                _ => None,
            }) {
                self.report_group_member_refusal(order, &reason, ts, orders);
            }
            let member_filled: Decimal = events
                .iter()
                .filter_map(|event| match event {
                    VenueMessage::OrderFilled(fill)
                        if fill.client_order_id == order.client_order_id =>
                    {
                        Some(fill.last_qty)
                    }
                    _ => None,
                })
                .sum();
            if member_filled > Decimal::ZERO {
                filled.push((order.clone(), member_filled));
            }
            out.extend(events);
        }

        // THE CLOSING PASS, and it is the ONLY place a group member's linkage
        // is applied - `on_submit_from` skips its own application for a member,
        // because `Ouo` subtracts rather than sets and applying it twice would
        // shrink an already-resting sibling by twice the fill. Running it here
        // and only here means each filled member's rule is applied exactly
        // once, against every member, with all of them on the book - which is
        // both halves of the guarantee at one stroke: the sibling admitted
        // after the fill is covered, and the sibling admitted before it is not
        // adjusted twice.
        let mut closing = Vec::new();
        for (order, qty) in filled {
            closing.extend(self.apply_linkage_after_fill(&order, qty, ts));
        }
        if !closing.is_empty() {
            // The rule is asked of THE CLOSING PASS ALONE, not of `out`: every
            // member's own batch already took its own snapshot decision, and
            // re-reading their fills here would make this pass owe a second
            // snapshot for a ledger move somebody else already reported and
            // already paid an arm for.
            let mut closing = closing;
            self.push_account_snapshot(&mut closing, ts, true);
            out.extend(closing);
        }
        out
    }

    /// EVERY REFUSAL A SUBMIT CAN MEET, asked as one pure question.
    ///
    /// This exists so the group's dry pass and the group's own self-check ask
    /// the SAME question, and so the standing invariant has somewhere to live:
    /// NO REFUSAL MAY REACH A SUBMIT FROM OUTSIDE THIS FUNCTION. Three
    /// atomicity bugs came from that invariant being unwritten - the dry pass
    /// was built from `validate_submit` alone while the real path also refused
    /// on the hedging `position_id` rule and on a link validated without the
    /// group's ids, so a group could pass admission whole and lose a member on
    /// the second pass with its siblings already accepted.
    ///
    /// Pure: it clones what it must mutate and touches no engine state, which
    /// is what lets it run twice per member without the second run meaning
    /// anything different from the first.
    ///
    /// `group` is the member ORDERS, not their ids, and that is load-bearing
    /// rather than a convenience. A refusal computed from the working book -
    /// the equity short check - has to count the members that have not rested
    /// yet, or this pass and the real pass judge different books and the
    /// difference is misfiled as the disclosed funds carve-out by
    /// `report_group_member_refusal`. See `Engine::worst_case_leaves`.
    fn dry_refusal(&self, order: &SubmitOrder, ts: u64, group: &[SubmitOrder]) -> Option<String> {
        if let Some(reason) = self.hedging_refusal(order) {
            return Some(reason);
        }
        let mut candidate = order.clone();
        // The derived trailing limit is what `risk_px` and therefore the funds
        // check read, so a dry pass that skipped the derivation would judge a
        // `TrailingStopLimit` against its trigger rather than its limit and
        // could pass a member pass two then refuses.
        if let Err(reason) = self.derive_trailing_limit(&mut candidate) {
            return Some(reason);
        }
        if let Err(reason) = self.validate_submit(&candidate, ts, false, group) {
            return Some(reason);
        }
        None
    }

    /// A group member was REFUSED on pass two, which the dry pass was supposed
    /// to have made impossible. Decide which of the two causes it was and say
    /// so, because they are a disclosed limit and a defect respectively.
    ///
    /// The discriminator is to ask the dry question AGAIN, now, after the
    /// refusal:
    ///
    /// - It still refuses. Then the two passes agree and the STATE moved under
    ///   them - an earlier member's fill spent the balance this one needed.
    ///   That is the disclosed funds carve-out on `SubmitOrderGroup`: the dry
    ///   pass reads the book before the group runs and cannot see money the
    ///   group is about to spend. A warning, not a defect.
    /// - It passes. Then the two passes DISAGREE about the same state, which is
    ///   the defect family this whole split exists to prevent: a refusal
    ///   reachable from the real path and invisible to the dry one. Loud, and a
    ///   `debug_assert` so it cannot survive a single dev run of the path.
    ///
    /// Deliberately not a classification of the REASON STRING. Matching text
    /// would pin the check to today's wording and would say nothing about a
    /// refusal added later, which is exactly the case that needs catching.
    fn report_group_member_refusal(
        &self,
        order: &SubmitOrder,
        reason: &str,
        ts: u64,
        group: &[SubmitOrder],
    ) {
        if self.dry_refusal(order, ts, group).is_some() {
            tracing::warn!(
                client_order_id = %order.client_order_id,
                %reason,
                "order group member refused after admission: an earlier member's fill moved the \
                 balance it needed - the disclosed funds carve-out, not an admission hole",
            );
            return;
        }
        tracing::error!(
            client_order_id = %order.client_order_id,
            %reason,
            "ATOMICITY DEFECT: an order group member was refused by the submit path that the dry \
             pass admits against this same state, so the group was broken open with earlier \
             members already accepted - a refusal exists outside Engine::dry_refusal",
        );
        debug_assert!(
            false,
            "order group member {} refused on pass two ({reason}) but the dry pass admits it: a \
             refusal reachable from on_submit_from is missing from dry_refusal",
            order.client_order_id,
        );
    }

    /// A trailing stop limit's LIMIT is derived, never stated - the wire refuses
    /// a price on this type for exactly that reason. Deriving it before
    /// `risk_px` reads it is what makes the hold the one the limit
    /// implies rather than the one the trigger implies; the ratchet re-derives
    /// it through the same function so the two cannot disagree about which side
    /// of the trigger the limit sits on.
    ///
    /// A no-op for every other order type, so callers run it unconditionally.
    fn derive_trailing_limit(&self, order: &mut SubmitOrder) -> Result<(), String> {
        if order.order_type != OrderType::TrailingStopLimit {
            return Ok(());
        }
        let trigger = order
            .trigger_price
            .ok_or_else(|| "conditional order must carry trigger_price".to_string())?;
        let offset = order
            .limit_offset
            .ok_or_else(|| "TrailingStopLimit must carry limit_offset".to_string())?;
        let Some(limit) = OrderType::trailing_limit_px(order.side, trigger, offset) else {
            return Err("limit_offset overflows the trigger price".into());
        };
        if limit <= Decimal::ZERO {
            return Err("limit_offset puts the derived limit at or below zero".into());
        }
        order.price = Some(limit);
        Ok(())
    }

    pub(crate) fn on_submit_from(
        &mut self,
        mut order: SubmitOrder,
        ts: u64,
        reading: Option<MarketReading>,
        apply_divergences: bool,
        group: &[SubmitOrder],
    ) -> Vec<VenueMessage> {
        if let Some(reason) = self.hedging_refusal(&order) {
            return vec![VenueMessage::OrderRejected {
                client_order_id: order.client_order_id,
                reason,
                ts_event: ts,
            }];
        }
        if self.oms_type == mogwai_protocol::OmsType::Hedging && order.position_id.is_none() {
            self.position_seq = self.position_seq.saturating_add(1);
            order.position_id = Some(format!("{}-{}", order.symbol, self.position_seq));
        }
        // The group's members travel into `validate_link` on THIS pass too, not just
        // the dry one. A child may name a parent that has not been accepted yet,
        // and a group is not required to list parents before children - so a
        // pass that dropped the group context here refused a forward parent
        // reference that pass one had already passed, breaking the group open
        // with its earlier members accepted.
        if let Err(reason) = self.validate_submit(&order, ts, apply_divergences, group) {
            return vec![VenueMessage::OrderRejected {
                client_order_id: order.client_order_id,
                reason,
                ts_event: ts,
            }];
        }
        if let Err(reason) = self.derive_trailing_limit(&mut order) {
            return vec![VenueMessage::OrderRejected {
                client_order_id: order.client_order_id,
                reason,
                ts_event: ts,
            }];
        }

        // Divergence: reject the next submit outright. `RejectNextSubmit`
        // carries no target, so it applies to whatever submit arrives next.
        //
        // NOT for a group member: `on_submit_group` consumed the arm once for
        // the whole group, because spending it per member would refuse one leg
        // of a group whose whole promise is that its legs share a fate.
        if apply_divergences
            && group.is_empty()
            && let Some(Divergence::RejectNextSubmit { reason }) =
                self.take_armed(|d| matches!(d, Divergence::RejectNextSubmit { .. }))
        {
            return vec![VenueMessage::OrderRejected {
                client_order_id: order.client_order_id,
                // `validate_divergence` refuses an over-length reason at every
                // WIRE arming path, but `Engine::arm` is a public in-process
                // API that reaches no validator, so the byte budget sized
                // against `MAX_REASON_LEN` cannot rest on the caller alone.
                // Truncating at the ECHO closes it by construction for every
                // arming path there will ever be, and an already-valid reason
                // passes through untouched.
                reason: mogwai_protocol::truncate_reason(reason),
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
        let stated_px = risk_px(&order);
        let increment = self.instruments[&order.symbol].price_increment;
        let band_ticks = reading.map_or(0, |value| value.band_ticks);

        // An order-list CHILD whose parent has not executed rests HELD: accepted
        // and answerable, scanned by nothing, holding nothing. It is routed here,
        // ahead of every marketability and fill decision, precisely because none
        // of those questions may be asked of it yet - a child released later gets
        // its band draw and its funds check at RELEASE, against the market that
        // exists then rather than the one its parent was submitted into.
        //
        // A child whose parent has ALREADY filled skips this and takes the
        // ordinary path, which is the fast-market case: a market entry that
        // filled on arrival leaves its exits nothing to wait for.
        if let Some(parent) = order
            .link
            .as_ref()
            .and_then(|link| link.parent_order_id.as_ref())
            && !self.has_filled(parent)
        {
            let venue_order_id = self.next_venue_order_id();
            self.seen_client_order_ids
                .insert(order.client_order_id.clone(), venue_order_id.clone());
            // MARKED EXEMPTION FROM `push_account_snapshot`, two of three, and
            // for the same reason as the resting-limit one: a held child takes
            // no hold and books no balance, so there is no update for
            // `DropNextAccountUpdate` to hide and spending the arm here would
            // burn it on an order that cannot fill until its parent does.
            let out = vec![
                VenueMessage::OrderAccepted {
                    client_order_id: order.client_order_id.clone(),
                    venue_order_id: venue_order_id.clone(),
                    ts_event: ts,
                },
                VenueMessage::AccountState(self.snapshot(ts)),
            ];
            let leaves_qty = order.quantity;
            self.rest_open(OpenOrder {
                venue_order_id,
                submit: order,
                leaves_qty,
                ts_accepted: ts,
                ts_last: ts,
                ts_triggered: None,
                band_ticks,
                resting: Resting::Held,
                band_draw: 0,
                scanned_ns: ts,
                revision: 0,
            });
            return out;
        }

        let trigger_px = draw_trigger(self.fill_seed, &order, stated_px, increment, band_ticks, 0);
        // A MARKET-TO-LIMIT TAKES THE MARKET, and its stated price is the limit
        // that bounds what it will pay and the price its remainder rests at -
        // not, as this used to read, the price it fills its whole quantity at.
        // It shares the market order's draw for the part that executes and the
        // limit order's marketability test for whether any of it does, which is
        // both halves of the type's name. It filled at its own limit with no
        // reference to the tape until 2026-08-19: a buy limited at 200 against a
        // last print of 100 paid 200.
        let takes_the_market = matches!(
            order.order_type,
            OrderType::Market | OrderType::MarketToLimit
        );
        let fill_px = if takes_the_market {
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
                        0,
                    )
                },
            )
        } else {
            stated_px
        };
        // THE LIMIT HALF OF THE NAME. The band's adverse slip is drawn against
        // the last print and can carry a market-to-limit past its own stated
        // price; the limit is the one thing the consumer asked the venue to
        // respect, so it bounds the fill rather than the other way round. A
        // plain market order has no such bound and is left alone.
        let fill_px = if order.order_type == OrderType::MarketToLimit {
            match order.side {
                Side::Buy => fill_px.min(stated_px),
                Side::Sell => fill_px.max(stated_px),
            }
        } else {
            fill_px
        };
        if !order.reduce_only
            && let Err(reason) = self.validate_fill_funds(
                &order,
                order.quantity,
                fill_px,
                Decimal::ZERO,
                LiquiditySide::Taker,
                ts,
                apply_divergences,
            )
        {
            return vec![VenueMessage::OrderRejected {
                client_order_id: order.client_order_id,
                reason,
                ts_event: ts,
            }];
        }
        // A market order is marketable by definition; every type that states a
        // limit - `MarketToLimit` included, which is what the type's first act
        // is judged by - has to have been traded through.
        let marketable = order.order_type == OrderType::Market
            || reading.is_some_and(|value| trades_through(order.side, trigger_px, value.last_px));

        if order.order_type.is_conditional() {
            let venue_order_id = self.next_venue_order_id();
            self.seen_client_order_ids
                .insert(order.client_order_id.clone(), venue_order_id.clone());
            let stop_px = order
                .trigger_price
                .expect("validated conditional has trigger");
            // Which predicate this conditional waits on: a STOP fires when
            // price runs away from what it protects, a TOUCHED order when
            // price comes toward the level it is waiting at.
            let toward = order.order_type.triggers_toward();
            let touched = reading.is_some_and(|value| {
                if toward {
                    mogwai_protocol::touches_toward(order.side, stop_px, value.last_px)
                } else {
                    touches_trigger(order.side, stop_px, value.last_px)
                }
            });
            let leaves_qty = order.quantity;
            let record = OpenOrder {
                venue_order_id: venue_order_id.clone(),
                submit: order,
                leaves_qty,
                ts_accepted: ts,
                ts_last: ts,
                ts_triggered: None,
                band_ticks,
                resting: Resting::Conditional { stop_px, toward },
                band_draw: 0,
                scanned_ns: ts,
                revision: 0,
            };
            let mut out = vec![VenueMessage::OrderAccepted {
                client_order_id: record.submit.client_order_id.clone(),
                venue_order_id,
                ts_event: ts,
            }];
            self.rest_open(record);
            if let (true, Some(value)) = (touched, reading) {
                // The synthesized hit of section 1.4: the reading's own last
                // print, which IS a real print off the canonical tape. The
                // frontier is the application instant, because a stop accepted
                // at `ts` must never be offered prints that predate it.
                let pos = self.open.len() - 1;
                let hit = Hit {
                    ts_ns: value.ts_ns,
                    px: value.last_px,
                };
                out.extend(self.on_trigger(pos, hit, ts, ts));
            }
            // A trigger that booked a fill or freed a hold owes its
            // snapshot under the same `DropNextAccountUpdate` rule as any other
            // fill; a bare acceptance moves no balance and leaves the arm alone.
            self.push_account_snapshot(&mut out, ts, apply_divergences);
            return out;
        }

        if order.post_only && marketable {
            return vec![VenueMessage::OrderRejected {
                client_order_id: order.client_order_id,
                reason: "post-only order would take liquidity".into(),
                ts_event: ts,
            }];
        }

        // THE TYPES THAT REST AS A LIMIT once accepted: the limit itself, and
        // the market-to-limit, whose whole remainder rests at its stated price
        // when the touch is short of it and whose partial remainder rests there
        // when the touch takes only part of it. Spelled once and read at all
        // three decisions below, because a hand-rolled `== Limit` at each is how
        // the market-to-limit came to rest `Inert` - on the book with a positive
        // `leaves_qty`, offered to no sweep, unable to fill or expire.
        let rests_as_limit = matches!(
            order.order_type,
            OrderType::Limit | OrderType::MarketToLimit
        );
        if rests_as_limit && !marketable {
            if order.time_in_force == TimeInForce::Fok {
                // `plan_fill` is called for its CONSUMING effect and its plan
                // thrown away. A targeted `PartialFillNext` which is the very
                // reason a FOK cannot fill must go with the FOK that hit it,
                // rather than stay armed to ambush a resubmit of the same id.
                // Short of its trigger is still now-or-never, so the rejection
                // follows immediately.
                let _ = self.plan_fill(&order, order.quantity, apply_divergences);
                return vec![VenueMessage::OrderRejected {
                    client_order_id: order.client_order_id,
                    reason: "fill-or-kill could not fill at its trigger".into(),
                    ts_event: ts,
                }];
            }
            let venue_order_id = self.next_venue_order_id();
            self.seen_client_order_ids
                .insert(order.client_order_id.clone(), venue_order_id.clone());
            let mut out = vec![VenueMessage::OrderAccepted {
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
                ts_triggered: None,
                band_ticks,
                resting: Resting::Limit {
                    fill_trigger_px: trigger_px,
                },
                band_draw: 0,
                scanned_ns: ts,
                revision: 0,
            };
            match record.submit.time_in_force {
                // GTC rests and is swept; an IOC is evaluated exactly once,
                // against the acceptance-time reading, and cancels short of its
                // trigger rather than filling at a price the market never
                // reached.
                TimeInForce::Gtc | TimeInForce::Day | TimeInForce::Gtd => self.rest_open(record),
                TimeInForce::Ioc => {
                    out.push(VenueMessage::OrderCanceled {
                        client_order_id: record.submit.client_order_id.clone(),
                        venue_order_id: record.venue_order_id.clone(),
                        ts_event: ts,
                    });
                    out.extend(self.close_unrested(&record, WireOrderStatus::Canceled, ts));
                }
                TimeInForce::Fok => unreachable!("FOK rejected above"),
            }
            // MARKED EXEMPTION FROM `push_account_snapshot`, one of three.
            // Deliberately does NOT consume `DropNextAccountUpdate`, and does
            // not call `plan_fill`, so neither that divergence nor a targeted
            // `PartialFillNext` is spent here: both are armed against the FILL,
            // and a resting order has not had one yet. The helper would reach
            // the same verdict today, because an acceptance-and-rest batch
            // moves no ledger - but it would do so INCIDENTALLY, and a future
            // event in this batch that did move one must not silently start
            // spending the arm on an order that has never traded.
            out.push(VenueMessage::AccountState(self.snapshot(ts)));
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
        let planned_qty = self.plan_fill(&order, order.quantity, apply_divergences);
        let cap = self.reduce_only_cap(&order);
        let last_qty = cap.map_or(planned_qty, |cap| planned_qty.min(cap));
        let leaves_qty = order.quantity - last_qty;

        if order.reduce_only
            && last_qty > Decimal::ZERO
            && let Err(reason) = self.validate_fill_funds(
                &order,
                last_qty,
                fill_px,
                Decimal::ZERO,
                LiquiditySide::Taker,
                ts,
                apply_divergences,
            )
        {
            return vec![VenueMessage::OrderRejected {
                client_order_id: order.client_order_id,
                reason,
                ts_event: ts,
            }];
        }

        if order.time_in_force == TimeInForce::Fok && leaves_qty > Decimal::ZERO {
            return vec![VenueMessage::OrderRejected {
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
        let venue_order_id = self.next_venue_order_id();
        self.seen_client_order_ids
            .insert(order.client_order_id.clone(), venue_order_id.clone());

        let mut out = vec![VenueMessage::OrderAccepted {
            client_order_id: order.client_order_id.clone(),
            venue_order_id: venue_order_id.clone(),
            ts_event: ts,
        }];

        if cap == Some(Decimal::ZERO) {
            let record = OpenOrder {
                venue_order_id: venue_order_id.clone(),
                submit: order,
                leaves_qty,
                ts_accepted: ts,
                ts_last: ts,
                ts_triggered: None,
                band_ticks,
                resting: Resting::Inert,
                band_draw: 0,
                scanned_ns: ts,
                revision: 0,
            };
            out.push(VenueMessage::OrderCanceled {
                client_order_id: record.submit.client_order_id.clone(),
                venue_order_id,
                ts_event: ts,
            });
            out.extend(self.close_unrested(&record, WireOrderStatus::Canceled, ts));
            // REACHABLE FROM A VENUE-ORIGINATED CLOSE: `close_at_mark` submits a
            // reduce-only market order, and a position that went flat between
            // the breach walk and this submit caps it at zero. Hence
            // `apply_divergences` rather than `true` - the venue's own flatten
            // must not spend the consumer's arm here any more than it does at the
            // fill snapshot below.
            self.push_account_snapshot(&mut out, ts, apply_divergences);
            return out;
        }

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
            let fill = self.commit_fill(
                &order,
                &venue_order_id,
                last_qty,
                leaves_qty,
                fill_px,
                LiquiditySide::Taker,
                ts,
                apply_divergences,
            );
            out.extend(fill);
            // Before this order is routed to rest or close: a sibling reaped
            // here is off the book by the time anything else looks at it, and
            // the order's own record is untouched by the linkage.
            //
            // NOT FOR A GROUP MEMBER, and this is load-bearing rather than an
            // optimization. `Ouo` SUBTRACTS the filled quantity from each
            // sibling still resting, so it is not idempotent: applying it here
            // and again in `on_submit_group`'s closing pass shrinks a sibling
            // that was already on the book by TWICE the fill. For a two-leg
            // bracket sent stop-first that drove the stop's leaves to zero and
            // CANCELLED it, leaving the position naked - the mirror image of
            // the hazard the group frame was built to close, and reachable only
            // through one of the two member orderings. The group applies every
            // filled member's rule exactly once, in the closing pass, once all
            // members are on the book.
            if group.is_empty() {
                out.extend(self.apply_linkage_after_fill(&order, last_qty, ts));
            }
        }

        // Freeze the accepted order's state once, then route it: rest it
        // (GTC remainder), or close it into the terminal truth store (full
        // fill, or an IOC's canceled remainder) so a `QueryOrders` reply can
        // attest to it after it leaves the book.
        let resting = if rests_as_limit {
            Resting::Limit {
                fill_trigger_px: trigger_px,
            }
        } else {
            Resting::Inert
        };
        let mut record = OpenOrder {
            venue_order_id,
            submit: order,
            leaves_qty,
            ts_accepted: ts,
            ts_last: ts,
            ts_triggered: None,
            band_ticks,
            resting,
            band_draw: 0,
            scanned_ns: ts,
            revision: 0,
        };
        if leaves_qty > Decimal::ZERO && cap.is_some_and(|cap| cap < planned_qty) {
            out.push(VenueMessage::OrderCanceled {
                client_order_id: record.submit.client_order_id.clone(),
                venue_order_id: record.venue_order_id.clone(),
                ts_event: ts,
            });
            out.extend(self.close_unrested(&record, WireOrderStatus::Canceled, ts));
        } else if leaves_qty > Decimal::ZERO {
            // Every partial increments `band_draw`, and the sweep is not the
            // only place a partial happens: a marketable-on-arrival limit cut
            // short by an armed `PartialFillNext` leaves a remainder that is a
            // NEW tranche, so it draws a fresh queue position around the
            // unchanged price with the band it was accepted under.
            if last_qty > Decimal::ZERO && rests_as_limit {
                record.band_draw = 1;
                record.resting = Resting::Limit {
                    fill_trigger_px: draw_trigger(
                        self.fill_seed,
                        &record.submit,
                        stated_px,
                        increment,
                        band_ticks,
                        1,
                    ),
                };
            }
            match record.submit.time_in_force {
                TimeInForce::Gtc | TimeInForce::Day | TimeInForce::Gtd => {
                    self.rest_open(record);
                }
                TimeInForce::Ioc => {
                    out.push(VenueMessage::OrderCanceled {
                        client_order_id: record.submit.client_order_id.clone(),
                        venue_order_id: record.venue_order_id.clone(),
                        ts_event: ts,
                    });
                    out.extend(self.close_unrested(&record, WireOrderStatus::Canceled, ts));
                }
                TimeInForce::Fok => unreachable!("FOK partials are rejected before acceptance"),
            }
        } else {
            out.extend(self.close_unrested(&record, WireOrderStatus::Filled, ts));
        }

        // SPELLS THE RULE ITSELF rather than calling `push_account_snapshot`,
        // and its doc names this site: the helper has no argument for the
        // `last_qty > 0` carve-out below, which is a statement about what the
        // arm was aimed at rather than about whether the batch moved anything.
        //
        // Untargeted: applies to this submit's account-state snapshot. Scanned
        // for the same head-of-line-blocking reason as the duplicate divergence.
        // Gated on an actual execution: a zero `last_qty` (a partial floored
        // below one size increment) fills nothing, so the order either merely
        // comes to rest - the one carve-out, an arm spent there could never
        // reach the fill it was aimed at - or, as an IOC, cancels without ever
        // having held anything, which moves no ledger either. Both keep the
        // arm for the transition the author armed it against.
        let drop_update = apply_divergences
            && last_qty > Decimal::ZERO
            && self
                .take_armed(|d| matches!(d, Divergence::DropNextAccountUpdate))
                .is_some();
        if !drop_update {
            out.push(VenueMessage::AccountState(self.snapshot(ts)));
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
    /// Returns the batch's events and the number of orders it EMITTED ANY EVENT
    /// for, which is what the caller reserves delivery bytes against - a scan
    /// the tape did not satisfy produces no bytes, so reserving for it would
    /// grow the request with the open-order count against a fixed budget. Fills
    /// are not the only bytes: a stop-limit that triggers and rests writes an
    /// `OrderTriggered` and nothing else, and counting only fills would leave
    /// that frame unreserved.
    ///
    /// Runs on the IDENTITY clock; the sweeper calls `apply_scans_on_clock`
    /// with the swept boat's.
    pub fn apply_scans(&mut self, results: &[ScanResult], ts: u64) -> (Vec<VenueMessage>, usize) {
        self.apply_scans_on_clock(results, ts, mogwai_protocol::SimClock::identity())
    }

    /// Cancel every resting order whose time-in-force has run out.
    ///
    /// TIME-DRIVEN and nothing to do with triggers, which is why it is its own
    /// pass rather than a branch of the trigger walk: a `Gtd` limit nothing ever
    /// approached must still stop resting at its instant, and a `Day` order must
    /// stop resting at the session boundary whether or not the tape came near
    /// it.
    ///
    /// `closed_symbol` names an instrument whose SESSION JUST CLOSED, which the
    /// caller detects by asking the calendar whether the span it swept crossed
    /// from open to shut. A day order's expiry is that instant rather than
    /// anything the consumer stated, and an instrument with no calendar never
    /// supplies one - so a day order on a 24/7 symbol rests like a `Gtc`, which
    /// is the honest answer: with no session to end there is no instant to
    /// expire at, and inventing one (midnight UTC, say) would expire orders at a
    /// time the market they trade has never heard of.
    pub fn expire_orders(
        &mut self,
        now_ns: u64,
        closed_symbol: Option<&str>,
        ts: u64,
    ) -> Vec<VenueMessage> {
        let expired: Vec<String> = self
            .open
            .iter()
            .filter(|order| match order.submit.time_in_force {
                TimeInForce::Gtd => order
                    .submit
                    .expire_time
                    .is_some_and(|expire_at| now_ns >= expire_at),
                TimeInForce::Day => {
                    closed_symbol.is_some_and(|symbol| order.submit.symbol.as_ref() == symbol)
                }
                _ => false,
            })
            .map(|order| order.submit.client_order_id.clone())
            .collect();
        let mut out = Vec::new();
        for client_order_id in expired {
            let Some(pos) = self.open.position(&client_order_id) else {
                continue;
            };
            let order = self.open[pos].clone();
            let reaped = self.close_out(pos, &order, WireOrderStatus::Expired, ts);
            // EXPIRED, not Canceled. Nobody cancelled this order: the consumer
            // stated its lifetime at submit and the clock reached the end of
            // it. Reporting a cancel says an actor pulled the order, which is
            // a different fact about the venue and the one a host would act on
            // when reconciling. This used to report `Canceled` on the argument
            // that no consumer matched the difference; that argument is the
            // one the order-type completeness ruling overturned, since the
            // venue's surface is not sized against a consumer's current
            // catalog.
            tracing::debug!(
                client_order_id = %order.submit.client_order_id,
                time_in_force = ?order.submit.time_in_force,
                "resting order expired",
            );
            out.push(VenueMessage::OrderExpired {
                client_order_id: order.submit.client_order_id.clone(),
                venue_order_id: order.venue_order_id.clone(),
                ts_event: ts,
            });
            // An expired parent is a parent that will never fill; `close_out`
            // already decided that and reaped accordingly.
            out.extend(reaped);
        }
        if !out.is_empty() {
            // An expiry FREES A HOLD, so this batch moves the ledger and
            // owes the snapshot under the same rule a fill does - including the
            // `DropNextAccountUpdate` half, which this site used to skip. It
            // was the last unconditional push outside the two marked
            // exemptions: an operator could arm the arm, watch a GTD order
            // expire, and be handed the fresh balances anyway. TRUE, though the
            // clock rather than a consumer drives it: the arm is armed against an
            // order LEAVING THE BOOK, which is exactly what an expiry is, and
            // `an_expiry_obeys_drop_next_account_update` pins that.
            self.push_account_snapshot(&mut out, ts, true);
        }
        out
    }

    /// Advance every resting `TrailingStopMarket` whose trigger the tape has
    /// left behind, against the EXTREME its side follows.
    ///
    /// A sell trail rises with the high and NEVER falls back; a buy trail falls
    /// with the low and never rises. That one-way movement is the whole
    /// mechanism - a trigger that retreated would be a stop somebody keeps
    /// amending, not a trailing stop - so the candidate is only taken when it
    /// improves on where the trigger already is.
    ///
    /// The trigger is moved on BOTH the resting state and the submit, because
    /// the order's hold is derived from the submit and a trail that moved
    /// only the scan predicate would hold the wrong amount.
    ///
    /// `extremes` carries, per symbol, the highest and lowest price the tape
    /// reached since the last pass - not the closing mark. That is what gives
    /// the trail tick resolution: a sell trail tracks the running high and a buy
    /// trail the running low, both monotone, so the maximum over a span's ticks
    /// IS the span's high and ratcheting once against it is bit-for-bit what
    /// ratcheting per tick would have produced. A symbol with no span (a river
    /// that printed nothing, or a symbol the tape did not cover) falls back to
    /// its mark, which is the pre-extremes behaviour and the honest answer when
    /// there is no finer evidence.
    pub(crate) fn ratchet_trailing_stops(
        &mut self,
        marks: &[(mogwai_protocol::Symbol, Decimal)],
        extremes: &[(mogwai_protocol::Symbol, Decimal, Decimal)],
        ts: u64,
    ) {
        for (symbol, mark) in marks {
            let span = extremes
                .iter()
                .find(|(candidate, _, _)| candidate == symbol)
                .map(|(_, high, low)| (*high, *low));
            let positions: Vec<usize> = self
                .open
                .iter()
                .enumerate()
                .filter(|(_, order)| {
                    order.submit.order_type.trails()
                        && order.submit.symbol == *symbol
                        && matches!(order.resting, Resting::Conditional { .. })
                })
                .map(|(pos, _)| pos)
                .collect();
            for pos in positions {
                let order = &self.open[pos];
                let Some(offset) = order.submit.trail_offset else {
                    continue;
                };
                let Resting::Conditional { stop_px, toward } = order.resting else {
                    continue;
                };
                // The price this side's trail follows: the span's HIGH for a
                // sell, its LOW for a buy - the extreme the trail would have
                // ratcheted to had it seen every tick. Without a span, the
                // closing mark.
                let followed = match (order.submit.side, span) {
                    (Side::Sell, Some((high, _))) => high,
                    (Side::Buy, Some((_, low))) => low,
                    (_, None) => *mark,
                };
                let candidate = match order.submit.side {
                    Side::Sell => followed.checked_sub(offset),
                    Side::Buy => followed.checked_add(offset),
                };
                let Some(candidate) = candidate else {
                    continue;
                };
                let improves = match order.submit.side {
                    Side::Sell => candidate > stop_px,
                    Side::Buy => candidate < stop_px,
                };
                if !improves {
                    continue;
                }
                // A trailing stop LIMIT carries its limit `limit_offset` from
                // its own trigger, so the limit ratchets with it. Derived
                // through the same function the accept path uses. An offset
                // that overflows or drives the limit non-positive leaves the
                // trigger where it was rather than moving one of the two: a
                // trigger that advanced without its limit would rest at a price
                // the consumer never agreed to.
                let trailing_limit = if order.submit.order_type == OrderType::TrailingStopLimit {
                    match order.submit.limit_offset.and_then(|gap| {
                        OrderType::trailing_limit_px(order.submit.side, candidate, gap)
                    }) {
                        Some(limit) if limit > Decimal::ZERO => Some(limit),
                        _ => continue,
                    }
                } else {
                    None
                };
                let before = order.clone();
                let order = &mut self.open[pos];
                order.submit.trigger_price = Some(candidate);
                if let Some(limit) = trailing_limit {
                    order.submit.price = Some(limit);
                }
                order.resting = Resting::Conditional {
                    stop_px: candidate,
                    toward,
                };
                // The trigger window restarts, exactly as an amend restarts a
                // limit's: a span walked against the OLD trigger says nothing
                // about the new one.
                order.scanned_ns = ts;
                order.revision = order.revision.saturating_add(1);
                order.ts_last = ts;
                self.refresh_open_hold(pos, &before);
            }
        }
    }

    /// As `apply_scans`, on the clock of the boat whose sweep produced these
    /// results. The sweeper walks one boat at a time and every `ts` here was
    /// sampled on that boat, so the fee surcharge is judged on it.
    pub fn apply_scans_on_clock(
        &mut self,
        results: &[ScanResult],
        ts: u64,
        sim: mogwai_protocol::SimClock,
    ) -> (Vec<VenueMessage>, usize) {
        self.event_sim = sim;
        if cfg!(debug_assertions) {
            self.reconcile_order_holds();
        }
        let mut out = Vec::new();
        let mut emitted = 0;
        for result in results {
            let Some(pos) = self.open.position(&result.client_order_id).filter(|pos| {
                let order = &self.open[*pos];
                order.revision == result.revision && order.scanned_ns == result.from_ns
            }) else {
                continue;
            };
            let resting = self.open[pos].resting;
            if let (Resting::Conditional { .. }, Some(hit)) = (resting, result.hit) {
                // The frontier the trigger's product inherits is where the walk
                // REACHED, never the pass instant: a drain budget that cut the
                // span short must not hand a freshly live limit a span nothing
                // looked at.
                out.extend(self.on_trigger(pos, hit, ts, result.scanned_to_ns));
                emitted += 1;
                continue;
            }
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
            if result.hit.is_none() {
                continue;
            }
            // Sized off the LEAVES, never `submit.quantity`: a swept order may
            // already be partly filled or have been amended, and multiplying a
            // partial-fill fraction by the original quantity would over-fill.
            let planned_qty = self.plan_fill(&submit, leaves, true);
            let cap = self.reduce_only_cap(&submit);
            let last_qty = cap.map_or(planned_qty, |cap| planned_qty.min(cap));
            let new_leaves = leaves - last_qty;
            if cap == Some(Decimal::ZERO) {
                let order = self.open[pos].clone();
                let reaped = self.close_out(pos, &order, WireOrderStatus::Canceled, ts);
                out.push(VenueMessage::OrderCanceled {
                    client_order_id: submit.client_order_id,
                    venue_order_id,
                    ts_event: ts,
                });
                out.extend(reaped);
                emitted += 1;
                continue;
            }
            let fill_px = submit
                .price
                .expect("validated resting limit carries a price");
            // The order is RESTING, so `free_balance` has already had its own
            // hold subtracted; adding it back is what keeps a fully funded
            // order from failing its own fill against its own hold.
            let held = self.hold_for(&submit, leaves, fill_px);
            if let Err(reason) = self.validate_fill_funds(
                &submit,
                last_qty,
                fill_px,
                held,
                LiquiditySide::Maker,
                ts,
                true,
            ) {
                tracing::warn!(client_order_id = %submit.client_order_id, %reason, "resting order canceled at funds check");
                let order = self.open[pos].clone();
                let reaped = self.close_out(pos, &order, WireOrderStatus::Canceled, ts);
                out.push(VenueMessage::OrderCanceled {
                    client_order_id: submit.client_order_id,
                    venue_order_id,
                    ts_event: ts,
                });
                out.extend(reaped);
                emitted += 1;
                continue;
            }
            if last_qty > Decimal::ZERO {
                out.extend(self.commit_fill(
                    &submit,
                    &venue_order_id,
                    last_qty,
                    new_leaves,
                    fill_px,
                    LiquiditySide::Maker,
                    ts,
                    true,
                ));
                emitted += 1;
            }
            // BETWEEN results, never after the loop. Two legs of one bracket can
            // both be in this batch, and a sibling reaped after the loop would
            // already have filled against the same span of tape.
            //
            // Position indices do not survive it - `OpenBook::remove` swaps - and
            // they do not have to: every later iteration looks its own order up
            // again by client order id. The `pos` captured above is not used past
            // this point, and must not be.
            //
            // No `emitted` increment: linkage only ever runs off a fill, and the
            // fill already counted this order once - the per-order hold
            // `swept_fill_max_bytes` charges includes the linkage allowance.
            if last_qty > Decimal::ZERO {
                let linkage = self.apply_linkage_after_fill(&submit, last_qty, ts);
                out.extend(linkage);
            }
            let Some(pos) = self.open.position(&submit.client_order_id) else {
                // The order that just filled was itself reaped by its own
                // linkage - impossible today, since an order never links itself
                // and its own record is not touched above, but the lookup is
                // what makes that a checked fact rather than an assumption.
                continue;
            };
            if new_leaves > Decimal::ZERO && cap.is_some_and(|cap| cap < planned_qty) {
                let order = self.open[pos].clone();
                let reaped = self.close_out(pos, &order, WireOrderStatus::Canceled, ts);
                out.push(VenueMessage::OrderCanceled {
                    client_order_id: order.submit.client_order_id,
                    venue_order_id: order.venue_order_id,
                    ts_event: ts,
                });
                out.extend(reaped);
            } else if new_leaves > Decimal::ZERO {
                let before = self.open[pos].clone();
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
                //
                // A zero-quantity result - a `PartialFillNext` floored below one
                // size increment - opened no tranche, so it must touch neither
                // the draw nor the frontier. Resetting `scanned_ns` to `ts`
                // there would discard the span between where a truncated drain
                // budget actually reached and `ts`, skipping prints no pass
                // would ever look at again. Only a real execution earns the
                // reset, because only a real execution starts covering from
                // here.
                if last_qty > Decimal::ZERO {
                    order.band_draw = order.band_draw.saturating_add(1);
                    order.resting = Resting::Limit {
                        fill_trigger_px: draw_trigger(
                            self.fill_seed,
                            &order.submit,
                            order.submit.price.expect("validated limit carries a price"),
                            self.instruments[&order.submit.symbol].price_increment,
                            order.band_ticks,
                            order.band_draw,
                        ),
                    };
                    order.scanned_ns = ts;
                }
                order.revision = order.revision.saturating_add(1);
                self.refresh_open_hold(pos, &before);
            } else {
                let order = self.open[pos].clone();
                out.extend(self.close_out(pos, &order, WireOrderStatus::Filled, ts));
            }
        }
        // ONE snapshot for the whole batch, taken after every transition it
        // booked - which is what `sizing::swept_fill_max_bytes` bounds. A pass
        // whose only transitions were hold-freeing cancels still owes
        // it, so the test is "did anything move the ledger", not "did anything
        // fill".
        // THE OUTER TEST IS NOT REDUNDANT WITH THE HELPER'S. A sweep pass runs
        // on every clock tick and routinely produces events that move nothing -
        // an `OrderTriggered`, an acceptance - and this path must stay silent
        // for those rather than stamp an unchanged account onto every tick,
        // which is what `swept_fill_max_bytes` sizes against. What the helper
        // owns here is the `DropNextAccountUpdate` half.
        if account_changed(&out) {
            // TRUE for the same reason the expiry pass passes it: a swept fill
            // or a funds-check eviction is an order executing or leaving the
            // book, which is precisely what the arm names.
            self.push_account_snapshot(&mut out, ts, true);
        }
        if cfg!(debug_assertions) {
            self.reconcile_order_holds();
        }
        (out, emitted)
    }

    /// Whether the venue has seen this order execute at all - the predicate a
    /// CHILD's release waits on.
    ///
    /// "Filled" is deliberately not the test. A parent that filled partially and
    /// was then cancelled DID execute, and its child was released at that fill;
    /// reading only the terminal status would call such a parent unfillable and
    /// refuse a resubmitted child that the venue had already honoured one of.
    fn has_filled(&self, client_order_id: &str) -> bool {
        if let Some(pos) = self.open.position(client_order_id) {
            let order = &self.open[pos];
            return order.leaves_qty < order.submit.quantity;
        }
        self.closed.get(client_order_id).is_some_and(|info| {
            info.status == WireOrderStatus::Filled || info.filled_qty > Decimal::ZERO
        })
    }

    /// Every still-held child of this order, by client order id.
    pub(crate) fn held_children_of(&self, parent: &str) -> Vec<ClientOrderId> {
        self.open
            .iter()
            .filter(|order| {
                matches!(order.resting, Resting::Held) && order.parent_order_id() == Some(parent)
            })
            .map(|order| order.submit.client_order_id.clone())
            .collect()
    }

    /// What one order's fill does to the orders it is linked to, applied WHERE
    /// THE FILL IS COMMITTED rather than on a later sweep.
    ///
    /// The timing is the whole point and it is why this is not a periodic reap:
    /// two legs of one bracket can both be swept in a single batch, so a stop
    /// reaped after the batch would already have filled against the same span of
    /// tape that filled its take-profit. The caller applies this between
    /// results, so a sibling cancelled here is off the book before the next
    /// result is even looked up.
    ///
    /// Two independent things happen, in this order:
    ///
    /// 1. RELEASE. Every held child of this order becomes live, whatever rule
    ///    the parent carries - a child waits on its parent's execution, not on a
    ///    contingency value. Releasing emits no wire frame: the child was
    ///    accepted when it was submitted and its status does not change.
    /// 2. THE RULE. `Oco` cancels every named sibling still resting; `Ouo`
    ///    shrinks each by the quantity just filled and cancels the ones that
    ///    reach zero. `Oto` and `NoContingency` do neither - an OTO parent's
    ///    whole effect is the release above.
    ///
    /// Deliberately emits no `AccountState`: the caller owns the batch's single
    /// snapshot, so a linkage that freed holds is reflected in a snapshot
    /// taken after it rather than in one of its own.
    pub(crate) fn apply_linkage_after_fill(
        &mut self,
        order: &SubmitOrder,
        last_qty: Decimal,
        ts: u64,
    ) -> Vec<VenueMessage> {
        let mut out = Vec::new();
        for child in self.held_children_of(&order.client_order_id) {
            self.release_child(&child, ts);
        }
        let Some(link) = order.link.as_ref() else {
            return out;
        };
        match link.contingency {
            Contingency::NoContingency | Contingency::Oto => {}
            Contingency::Oco => {
                for sibling in &link.linked_order_ids {
                    let Some(pos) = self.open.position(sibling) else {
                        self.note_absent_sibling(&order.client_order_id, sibling, "Oco");
                        continue;
                    };
                    let canceled = self.open[pos].clone();
                    let reaped = self.close_out(pos, &canceled, WireOrderStatus::Canceled, ts);
                    tracing::debug!(
                        filled = %order.client_order_id,
                        sibling = %canceled.submit.client_order_id,
                        "one-cancels-the-other: sibling reaped at the fill",
                    );
                    out.push(VenueMessage::OrderCanceled {
                        client_order_id: canceled.submit.client_order_id.clone(),
                        venue_order_id: canceled.venue_order_id.clone(),
                        ts_event: ts,
                    });
                    out.extend(reaped);
                }
            }
            Contingency::Ouo => {
                for sibling in &link.linked_order_ids {
                    let Some(pos) = self.open.position(sibling) else {
                        self.note_absent_sibling(&order.client_order_id, sibling, "Ouo");
                        continue;
                    };
                    let before = self.open[pos].clone();
                    let new_total = before.submit.quantity.saturating_sub(last_qty);
                    let new_leaves = before.leaves_qty.saturating_sub(last_qty);
                    if new_leaves <= Decimal::ZERO {
                        let reaped = self.close_out(pos, &before, WireOrderStatus::Canceled, ts);
                        out.push(VenueMessage::OrderCanceled {
                            client_order_id: before.submit.client_order_id.clone(),
                            venue_order_id: before.venue_order_id.clone(),
                            ts_event: ts,
                        });
                        out.extend(reaped);
                        continue;
                    }
                    let updated = {
                        let sibling = &mut self.open[pos];
                        sibling.submit.quantity = new_total;
                        sibling.leaves_qty = new_leaves;
                        sibling.ts_last = ts;
                        // A shrink is an amend the venue performed, so it
                        // invalidates any walk planned against the old size for
                        // the same reason a consumer's amend does.
                        sibling.revision = sibling.revision.saturating_add(1);
                        VenueMessage::OrderUpdated {
                            client_order_id: sibling.submit.client_order_id.clone(),
                            venue_order_id: sibling.venue_order_id.clone(),
                            quantity: new_total,
                            price: sibling.submit.price,
                            trigger_price: sibling.submit.trigger_price,
                            leaves_qty: new_leaves,
                            ts_event: ts,
                        }
                    };
                    self.refresh_open_hold(pos, &before);
                    out.push(updated);
                }
            }
        }
        out
    }

    /// A linkage rule found a sibling it names NOT ON THE BOOK, which used to be
    /// a silent `continue` and is now said out loud.
    ///
    /// There are two causes and they are not equally innocent. The ordinary one
    /// is a sibling already terminal - filled, cancelled, or reaped by an
    /// earlier rule in this same batch - and there is genuinely nothing to do,
    /// which is why this is a `debug` and not a warning. The DANGEROUS one is a
    /// sibling that has not been ADMITTED yet, which is what a per-leg
    /// submission of a bracket produces: the rule adjusts nothing, and the
    /// sibling then arrives at full size with the position already open. That
    /// second case is what `Command::SubmitOrderGroup` exists to make
    /// unreachable, and this line is what makes it visible when a consumer takes
    /// the per-leg route anyway.
    ///
    /// It is a log rather than a refusal because a rule cannot tell the two
    /// causes apart from where it stands - both look like an id that is not in
    /// `open` - and refusing the innocent one would break every ordinary
    /// bracket at its second leg.
    fn note_absent_sibling(&self, filled: &str, sibling: &str, rule: &str) {
        let terminal = self.closed.contains_key(sibling);
        tracing::debug!(
            %filled,
            %sibling,
            %rule,
            terminal,
            "linkage named a sibling that is not on the book; if it was never admitted, the rule \
             adjusted nothing and the sibling will arrive at full size - submit linked orders as \
             one SubmitOrderGroup",
        );
    }

    /// Promote a held child to the state it would have rested in had it been
    /// submitted standalone, and place the hold it did not have
    /// while it waited.
    ///
    /// The draw is FRESH (`band_draw` advances), because a queue position earned
    /// at submit is not one a child sitting out its parent's execution kept, and
    /// the scan frontier starts at the release instant: the span before it was
    /// walked against an order that could not fill.
    fn release_child(&mut self, client_order_id: &str, ts: u64) {
        let Some(pos) = self.open.position(client_order_id) else {
            return;
        };
        let before = self.open[pos].clone();
        let increment = self
            .instruments
            .get(&before.submit.symbol)
            .map_or(Decimal::ZERO, |def| def.price_increment);
        let band_draw = before.band_draw.saturating_add(1);
        let resting = if before.submit.order_type.is_conditional() {
            match before.submit.trigger_price {
                Some(stop_px) => Resting::Conditional {
                    stop_px,
                    toward: before.submit.order_type.triggers_toward(),
                },
                None => Resting::Inert,
            }
        } else {
            match before.submit.price {
                Some(price) => Resting::Limit {
                    fill_trigger_px: draw_trigger(
                        self.fill_seed,
                        &before.submit,
                        price,
                        increment,
                        before.band_ticks,
                        band_draw,
                    ),
                },
                None => Resting::Inert,
            }
        };
        {
            let child = &mut self.open[pos];
            child.resting = resting;
            child.band_draw = band_draw;
            child.scanned_ns = ts;
            child.revision = child.revision.saturating_add(1);
            child.ts_last = ts;
        }
        self.refresh_open_hold(pos, &before);
        tracing::debug!(child = %client_order_id, "order-list child released by its parent's fill");
    }

    /// Freeze a terminal order into the truth store and reap the held children
    /// its own terminality has just orphaned. THE SINGLE HOME OF THAT RULE.
    ///
    /// `record` is the copy that goes into the store - the caller's own where
    /// it has mutated it (a triggered order's leaves), the book's otherwise -
    /// and `pos` is where the order still sits on the book. The invariant used
    /// to be maintained by REMEMBERING at ten sites and was missed at six of
    /// them: the reduce-only cap-zero cancels, the post-only trigger
    /// rejection, the trigger-time and resting funds cancels, and the silent
    /// control-plane cancel all took an order terminal while leaving any
    /// `Resting::Held` child of it waiting for a release nothing could ever
    /// perform.
    ///
    /// THE SET THIS COVERS, stated exactly, because a comment claiming a wider
    /// set than it has is how the rule got lost the first time. Every terminal
    /// path that closes an order the consumer submitted routes through here or
    /// through `close_unrested` - including the several whose reap is provably
    /// a no-op because the order filled, since a site that skips the call on
    /// the strength of an early return three hundred lines up is maintained by
    /// remembering again. The one path that does NOT is `reap_children_of`
    /// itself, which closes the children: it must not re-enter, and it carries
    /// the generation walk in its own worklist instead.
    ///
    /// Returns the child cancellations, which the caller emits AFTER its own
    /// terminal event so the wire reads parent-then-children.
    pub(crate) fn close_out(
        &mut self,
        pos: usize,
        record: &OpenOrder,
        status: WireOrderStatus,
        ts: u64,
    ) -> Vec<VenueMessage> {
        self.take_open(pos);
        self.close_unrested(record, status, ts)
    }

    /// `close_out` for an order that never reached the book - a submit that
    /// closed inside `on_submit_from`. It can still be a parent: a group may
    /// name its parent AFTER a child (`validate_link` admits the forward
    /// reference on purpose), so the child is already resting `Held` when the
    /// parent's own submit cancels it out.
    fn close_unrested(
        &mut self,
        record: &OpenOrder,
        status: WireOrderStatus,
        ts: u64,
    ) -> Vec<VenueMessage> {
        self.record_closed(record, status, ts);
        // Asked AFTER `record_closed`, so the store answers for an order that
        // is no longer on the book. A parent that filled at all can still
        // release its children, so only a wholly unfilled terminal reaps.
        if self.has_filled(&record.submit.client_order_id) {
            return Vec::new();
        }
        self.reap_children_of(vec![record.submit.client_order_id.clone()], ts)
    }

    /// Cancel every held child of an order that has gone terminal WITHOUT ever
    /// filling, so a bracket whose entry was cancelled does not leave its exits
    /// waiting for a release that can never come.
    ///
    /// Iterative over a worklist rather than recursive, and the depth it can
    /// reach is bounded by `validate_submit` refusing a child of a child: a
    /// cancel reaps one generation, and each generation is capped at
    /// `MAX_LINKED_ORDERS`, which is what makes the byte budget for a
    /// cancel computable.
    pub(crate) fn reap_children_of(
        &mut self,
        parents: Vec<ClientOrderId>,
        ts: u64,
    ) -> Vec<VenueMessage> {
        let mut out = Vec::new();
        let mut worklist = parents;
        while let Some(parent) = worklist.pop() {
            for child in self.held_children_of(&parent) {
                let Some(pos) = self.open.position(&child) else {
                    continue;
                };
                let canceled = self.take_open(pos);
                self.record_closed(&canceled, WireOrderStatus::Canceled, ts);
                tracing::debug!(
                    %parent,
                    %child,
                    "order-list child cancelled: its parent went terminal without filling",
                );
                out.push(VenueMessage::OrderCanceled {
                    client_order_id: canceled.submit.client_order_id.clone(),
                    venue_order_id: canceled.venue_order_id.clone(),
                    ts_event: ts,
                });
                worklist.push(child);
            }
        }
        out
    }

    /// The quantity a reduce-only order may fill: the netted position it
    /// opposes, or zero. `None` for an order that is not reduce-only, which is
    /// what every call site distinguishes from a cap that happens to be zero.
    ///
    /// Reads the ledger's own position map rather than `positions()`, which
    /// clones and sorts the whole book - this runs at every fill decision.
    fn reduce_only_cap(&self, order: &SubmitOrder) -> Option<Decimal> {
        if !order.reduce_only {
            return None;
        }
        let qty = self
            .account
            .positions
            .get(&(
                std::sync::Arc::clone(&order.symbol),
                self.position_key_id(order.position_id.as_ref()),
            ))
            .map_or(Decimal::ZERO, |state| state.qty);
        Some(match order.side {
            Side::Sell if qty > Decimal::ZERO => qty,
            Side::Buy if qty < Decimal::ZERO => -qty,
            _ => Decimal::ZERO,
        })
    }

    /// The triggered conditional's transition, under the engine lock: emits
    /// `OrderTriggered` and then either commits the market fill, promotes the
    /// stop-limit to a live banded limit (filling it at once if the triggering
    /// print is already through its drawn trigger), REJECTS a post-only
    /// stop-limit that would take liquidity, or cancels the order when the
    /// slipped price outruns the account or a reduce-only cap has gone to zero.
    /// Every terminal branch removes the order, frees its hold and calls
    /// `record_closed`.
    ///
    /// `hit` is a real print on the sweep path and the reading's own last print
    /// on the arrival path; `ts` is the application instant and is what every
    /// emitted `ts_event` and `ts_triggered` carries. `frontier` is how far the
    /// venue has EVIDENCE - the walk's reached instant on the sweep path, the
    /// application instant on the arrival path - and is what a surviving order
    /// resumes scanning from.
    ///
    /// The account snapshot is deliberately NOT emitted here: the caller owns
    /// it, so one sweep pass takes one snapshot (which is what
    /// `sizing::swept_fill_max_bytes` bounds) and `DropNextAccountUpdate` is
    /// consumed exactly once per batch.
    fn on_trigger(&mut self, pos: usize, hit: Hit, ts: u64, frontier: u64) -> Vec<VenueMessage> {
        let mut order = self.open[pos].clone();
        order.ts_triggered = Some(ts);
        order.ts_last = ts;
        // Held across the match because every arm may move `order` into the
        // book, and the linkage is applied once at the end - after `pos` has
        // stopped meaning anything, which is what makes reaping a sibling here
        // safe.
        let submit = order.submit.clone();
        let mut filled_qty = Decimal::ZERO;
        let mut out = vec![VenueMessage::OrderTriggered {
            client_order_id: order.submit.client_order_id.clone(),
            venue_order_id: order.venue_order_id.clone(),
            ts_event: ts,
        }];
        // The cap is read before any fill is PLANNED: a reduce-only order whose
        // position is already gone never reaches `plan_fill`, so it consumes no
        // armed `PartialFillNext` on the way to its cancel.
        let cap = self.reduce_only_cap(&order.submit);
        if cap == Some(Decimal::ZERO) {
            let reaped = self.close_out(pos, &order, WireOrderStatus::Canceled, ts);
            tracing::warn!(client_order_id = %order.submit.client_order_id, "reduce-only order canceled at trigger: nothing left to reduce");
            out.push(VenueMessage::OrderCanceled {
                client_order_id: order.submit.client_order_id,
                venue_order_id: order.venue_order_id,
                ts_event: ts,
            });
            out.extend(reaped);
            return out;
        }
        let increment = self.instruments[&order.submit.symbol].price_increment;
        match order.submit.order_type {
            // Every conditional that TAKES liquidity once triggered. The three
            // differ only in what made them live - a stop running away from a
            // position, a touched order coming toward its level, a trailing stop
            // whose own trigger followed the tape - and not at all in what
            // happens next, which is why they share this arm rather than three
            // copies of it.
            OrderType::StopMarket | OrderType::TrailingStopMarket | OrderType::MarketIfTouched => {
                // The triggering print, slipped adversely by a fresh draw from
                // the band the order was accepted under. Never `read_market`
                // again: the print is what made the order live, and a second
                // reading would be exactly the look-ahead the venue refuses.
                let fill_px = draw_market_price(
                    self.fill_seed,
                    &order.submit,
                    hit.px,
                    hit.px,
                    increment,
                    order.band_ticks,
                    order.band_draw.saturating_add(1),
                );
                let planned = self.plan_fill(&order.submit, order.leaves_qty, true);
                let fill_qty = cap.map_or(planned, |cap| planned.min(cap));
                let held = self.hold_for(&order.submit, order.leaves_qty, risk_px(&order.submit));
                if let Err(reason) = self.validate_fill_funds(
                    &order.submit,
                    fill_qty,
                    fill_px,
                    held,
                    LiquiditySide::Taker,
                    ts,
                    true,
                ) {
                    out.extend(self.cancel_triggered(pos, &order, &reason, ts));
                    return out;
                }
                let leaves = order.leaves_qty - fill_qty;
                if fill_qty > Decimal::ZERO {
                    out.extend(self.commit_fill(
                        &order.submit,
                        &order.venue_order_id,
                        fill_qty,
                        leaves,
                        fill_px,
                        LiquiditySide::Taker,
                        ts,
                        true,
                    ));
                    filled_qty = fill_qty;
                }
                self.take_open(pos);
                order.leaves_qty = leaves;
                if leaves == Decimal::ZERO {
                    out.extend(self.close_unrested(&order, WireOrderStatus::Filled, ts));
                } else if cap.is_some_and(|cap| cap < planned) {
                    // A cap-clamped remainder can never again have a non-zero
                    // cap, and `Inert` reaches no further fill decision, so it
                    // would sit open forever. Close it in the same batch.
                    let reaped = self.close_unrested(&order, WireOrderStatus::Canceled, ts);
                    out.push(VenueMessage::OrderCanceled {
                        client_order_id: order.submit.client_order_id.clone(),
                        venue_order_id: order.venue_order_id.clone(),
                        ts_event: ts,
                    });
                    out.extend(reaped);
                } else {
                    order.resting = Resting::Inert;
                    // The frontier and the revision move even though nothing
                    // scans an inert remainder, and that is not bookkeeping for
                    // its own sake: `apply_scans_on_clock` admits a result whose
                    // `from_ns` and `revision` still match the order, and this
                    // record's pair still named the walk that TRIGGERED it. A
                    // second delivery of that same result would therefore be
                    // applied to an order that is no longer conditional, fall
                    // through to the resting-limit arm, and panic on a
                    // market-on-trigger order's absent price. The staleness
                    // guard is what makes a duplicate or late result harmless,
                    // so every record that outlives its walk has to invalidate
                    // it - the same bump the limit remainder below already does.
                    order.scanned_ns = frontier;
                    order.revision = order.revision.saturating_add(1);
                    self.rest_open(order);
                }
            }
            // Every conditional that RESTS as a limit once triggered. A
            // trailing stop limit reaches here with a trigger and a limit that
            // ratcheted together, so by this point it is a stop-limit whose
            // prices happen to have moved - nothing about the trigger handling
            // differs, which is why it shares the arm rather than copying it.
            OrderType::StopLimit | OrderType::LimitIfTouched | OrderType::TrailingStopLimit => {
                // A live limit at the stated price, judged EXACTLY as a limit
                // submitted at this instant: the print that made it live is
                // offered to it, and a gap that never reached the limit leaves
                // it resting rather than manufacturing a price.
                let stated = order.submit.price.expect("validated stop-limit has price");
                order.band_draw = order.band_draw.saturating_add(1);
                let fill_trigger_px = draw_trigger(
                    self.fill_seed,
                    &order.submit,
                    stated,
                    increment,
                    order.band_ticks,
                    order.band_draw,
                );
                let marketable = trades_through(order.submit.side, fill_trigger_px, hit.px);
                if !marketable {
                    order.resting = Resting::Limit { fill_trigger_px };
                    order.scanned_ns = frontier;
                    order.revision = order.revision.saturating_add(1);
                    // Writing the slot back directly would bypass the
                    // hold cache. Today it moves no hold - a stop-limit
                    // becoming live changes neither `leaves_qty` nor
                    // `submit.price`, which is what the hold derives from -
                    // but the refresh is what keeps that a fact about this
                    // arm's fields rather than an invariant every future edit
                    // to it has to rediscover.
                    let before = self.open[pos].clone();
                    self.open[pos] = order;
                    self.refresh_open_hold(pos, &before);
                    return out;
                }
                if order.submit.post_only {
                    self.take_open(pos);
                    let reaped = self.close_unrested(&order, WireOrderStatus::Rejected, ts);
                    out.push(VenueMessage::OrderRejected {
                        client_order_id: order.submit.client_order_id,
                        reason: "post-only order would take liquidity".into(),
                        ts_event: ts,
                    });
                    out.extend(reaped);
                    return out;
                }
                let planned = self.plan_fill(&order.submit, order.leaves_qty, true);
                let fill_qty = cap.map_or(planned, |cap| planned.min(cap));
                let held = self.hold_for(&order.submit, order.leaves_qty, stated);
                if let Err(reason) = self.validate_fill_funds(
                    &order.submit,
                    fill_qty,
                    stated,
                    held,
                    LiquiditySide::Taker,
                    ts,
                    true,
                ) {
                    out.extend(self.cancel_triggered(pos, &order, &reason, ts));
                    return out;
                }
                let leaves = order.leaves_qty - fill_qty;
                if fill_qty > Decimal::ZERO {
                    out.extend(self.commit_fill(
                        &order.submit,
                        &order.venue_order_id,
                        fill_qty,
                        leaves,
                        stated,
                        LiquiditySide::Taker,
                        ts,
                        true,
                    ));
                    filled_qty = fill_qty;
                }
                self.take_open(pos);
                order.leaves_qty = leaves;
                if leaves == Decimal::ZERO {
                    out.extend(self.close_unrested(&order, WireOrderStatus::Filled, ts));
                } else if cap.is_some_and(|cap| cap < planned) {
                    let reaped = self.close_unrested(&order, WireOrderStatus::Canceled, ts);
                    out.push(VenueMessage::OrderCanceled {
                        client_order_id: order.submit.client_order_id.clone(),
                        venue_order_id: order.venue_order_id.clone(),
                        ts_event: ts,
                    });
                    out.extend(reaped);
                } else {
                    // A partial makes the remainder a new tranche, exactly as
                    // it does for any other limit.
                    order.band_draw = order.band_draw.saturating_add(1);
                    order.resting = Resting::Limit {
                        fill_trigger_px: draw_trigger(
                            self.fill_seed,
                            &order.submit,
                            stated,
                            increment,
                            order.band_ticks,
                            order.band_draw,
                        ),
                    };
                    order.scanned_ns = frontier;
                    order.revision = order.revision.saturating_add(1);
                    self.rest_open(order);
                }
            }
            OrderType::Market | OrderType::Limit | OrderType::MarketToLimit => {
                unreachable!("only conditionals trigger")
            }
        }
        if filled_qty > Decimal::ZERO {
            out.extend(self.apply_linkage_after_fill(&submit, filled_qty, ts));
        }
        out
    }

    /// The hold this order itself already contributes to
    /// `held_balances` IN THE SETTLEMENT CURRENCY, which
    /// `validate_fill_funds` adds back before it compares. Zero for a
    /// reduce-only order (which places no hold) and zero for a spot sell
    /// (whose hold is base currency, and whose settlement-side check is about
    /// commission rather than the hold). Derived from the same
    /// `order_hold` the snapshot folds, so the add-back can never be a
    /// different shape from the hold it is canceling.
    fn hold_for(&self, order: &SubmitOrder, leaves: Decimal, price: Decimal) -> Decimal {
        let mut clipped = false;
        match self.order_hold(order, leaves, price, &mut clipped) {
            crate::account::Hold::Settlement(amount) => amount,
            crate::account::Hold::None | crate::account::Hold::Base(_) => Decimal::ZERO,
        }
    }

    /// The trigger-time funds failure: an accepted, triggered order whose
    /// slipped price outran the account is CANCELED, not rejected - running out
    /// of money at the moment of execution is an economic outcome on a live
    /// order, not the venue refusing a request.
    fn cancel_triggered(
        &mut self,
        pos: usize,
        order: &OpenOrder,
        reason: &str,
        ts: u64,
    ) -> Vec<VenueMessage> {
        tracing::warn!(client_order_id = %order.submit.client_order_id, %reason, "triggered order canceled at funds check");
        let reaped = self.close_out(pos, order, WireOrderStatus::Canceled, ts);
        let mut out = vec![VenueMessage::OrderCanceled {
            client_order_id: order.submit.client_order_id.clone(),
            venue_order_id: order.venue_order_id.clone(),
            ts_event: ts,
        }];
        // A conditional that never filled cannot release the children waiting
        // on it, so they leave with it - the cancel goes out first.
        out.extend(reaped);
        out
    }

    /// The size this fill WOULD be, and nothing else: consumes the targeted
    /// `PartialFillNext`, clamps it and floors it onto the size grid against
    /// `remaining` (the order's leaves, not its original quantity). Mutates no
    /// ledger and needs no venue id, so `on_submit` can judge FOK against its
    /// answer while a rejected FOK still leaves its client order id unreserved.
    fn plan_fill(
        &mut self,
        order: &SubmitOrder,
        remaining: Decimal,
        apply_divergences: bool,
    ) -> Decimal {
        let fraction = if apply_divergences {
            self.fill_fraction(order)
        } else {
            Decimal::ONE
        };
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
    #[expect(
        clippy::too_many_arguments,
        reason = "fill booking carries the complete execution fact"
    )]
    fn commit_fill(
        &mut self,
        order: &SubmitOrder,
        venue_order_id: &VenueOrderId,
        last_qty: Decimal,
        leaves_qty: Decimal,
        fill_px: Decimal,
        liquidity_side: LiquiditySide,
        ts: u64,
        apply_divergences: bool,
    ) -> Vec<VenueMessage> {
        // `FeeSurcharge` is a consumer-armed divergence, so a venue-originated
        // order (liquidation) does not pay it. The window is read purely from
        // `ts`; booking a later fill must not erase its answer for an earlier
        // replayed timestamp.
        let surcharge = if apply_divergences {
            self.fee_surcharge_multiplier_for(self.event_sim, ts)
        } else {
            Decimal::ONE
        };
        let commission = self.commission_for(order, last_qty, fill_px, liquidity_side, surcharge);
        let def = &self.instruments[&order.symbol];
        let commission_currency = def.class.settlement_currency().to_owned();
        let fill = OrderFilled {
            client_order_id: order.client_order_id.clone(),
            venue_order_id: venue_order_id.clone(),
            trade_id: self.next_trade_id(),
            symbol: std::sync::Arc::clone(&order.symbol),
            position_id: order.position_id.clone(),
            side: order.side,
            last_qty,
            last_px: fill_px,
            leaves_qty,
            commission,
            commission_currency,
            liquidity_side,
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
        let duplicate = apply_divergences
            && self
                .take_armed(|d| matches!(d, Divergence::DuplicateNextFill))
                .is_some();
        let mut out = Vec::new();
        if duplicate {
            out.push(VenueMessage::OrderFilled(fill.clone()));
        }
        out.push(VenueMessage::OrderFilled(fill));
        out
    }

    fn commission_for(
        &self,
        order: &SubmitOrder,
        qty: Decimal,
        px: Decimal,
        liquidity_side: LiquiditySide,
        surcharge: Decimal,
    ) -> Decimal {
        let def = &self.instruments[&order.symbol];
        self.fees
            .get(&order.symbol)
            .map_or(Decimal::ZERO, |schedule| {
                let rate = match liquidity_side {
                    LiquiditySide::Maker => schedule.maker,
                    LiquiditySide::Taker => schedule.taker,
                };
                match rate {
                    crate::FeeRate::BasisPoints { rate } => def
                        .notional(qty, px)
                        .unwrap_or(Decimal::MAX)
                        .saturating_mul(rate)
                        .checked_div(Decimal::from(10_000))
                        .unwrap_or(Decimal::MAX),
                    crate::FeeRate::PerContract { amount } => amount.saturating_mul(qty),
                }
            })
            .saturating_mul(surcharge)
    }

    fn maximum_commission(
        &self,
        order: &SubmitOrder,
        qty: Decimal,
        px: Decimal,
        ts: u64,
        apply_divergences: bool,
    ) -> Decimal {
        let surcharge = if apply_divergences {
            self.fee_surcharge_multiplier_for(self.event_sim, ts)
        } else {
            Decimal::ONE
        };
        self.commission_for(order, qty, px, LiquiditySide::Maker, surcharge)
            .max(self.commission_for(order, qty, px, LiquiditySide::Taker, surcharge))
    }

    /// The half of the linkage contract that only the LIVE BOOK can answer.
    /// `mogwai_protocol::validate_submit_order` has already ruled on the shape;
    /// what is left is whether the orders this one names can mean anything here.
    ///
    /// THE DEPTH RULE IS LOAD-BEARING, not tidiness. A child of a child would
    /// make cancelling one order reap a chain of unbounded length, and the byte
    /// budget a cancel takes has to be computable before the cancel runs.
    /// One generation, capped at `MAX_LINKED_ORDERS`, is what
    /// `sizing::LINKAGE_MAX_BYTES` bounds - and it costs nothing real, because a
    /// bracket is one entry and its exits.
    /// `group` names the members of the `SubmitOrderGroup` this order arrived
    /// in, empty for a standalone submit. A parent that is a group member is
    /// KNOWN even before it has been accepted, which is the point of the group
    /// frame: the members are admitted together, so a child may name a parent
    /// that is one frame's worth of work away rather than one round trip.
    /// `validate_submit_group` has already checked that every named id IS a
    /// member and that no member is both a parent and a child, so the depth and
    /// existence rules below still hold for the group as a whole.
    fn validate_link(
        &self,
        link: &mogwai_protocol::OrderLink,
        group: &[SubmitOrder],
    ) -> Result<(), String> {
        if matches!(link.contingency, Contingency::Oto) && link.linked_order_ids.is_empty() {
            return Err("Oto must name the orders it triggers".into());
        }
        let Some(parent) = link.parent_order_id.as_ref() else {
            return Ok(());
        };
        // A parent the venue has not accepted YET but which travels in this
        // group is known: the frame admits every member, so it will exist by
        // the time this child does. Narrowed to the existence question alone -
        // every rule below still runs once the parent is really on the book,
        // which it is on the second pass.
        if group.iter().any(|member| member.client_order_id == *parent)
            && !self.seen_client_order_ids.contains_key(parent.as_str())
        {
            return Ok(());
        }
        if !self.seen_client_order_ids.contains_key(parent.as_str()) {
            return Err(
                "unknown parent order: an order list's parent is submitted before its children"
                    .into(),
            );
        }
        // Terminal and never executed: nothing will ever release this child, and
        // resting it forever is worse than saying so.
        if self.open.position(parent).is_none() && !self.has_filled(parent) {
            return Err(
                "parent order is terminal and never filled: its child could never be released"
                    .into(),
            );
        }
        if let Some(pos) = self.open.position(parent)
            && self.open[pos].parent_order_id().is_some()
        {
            return Err("an order-list child may not itself be a parent".into());
        }
        if self
            .open
            .iter()
            .filter(|open| open.parent_order_id() == Some(parent.as_str()))
            .count()
            >= MAX_LINKED_ORDERS
        {
            return Err(format!(
                "parent order already carries {MAX_LINKED_ORDERS} children"
            ));
        }
        Ok(())
    }

    fn validate_submit(
        &self,
        order: &SubmitOrder,
        ts: u64,
        apply_divergences: bool,
        group: &[SubmitOrder],
    ) -> Result<(), String> {
        if order.client_order_id.trim().is_empty() {
            return Err("empty client_order_id".into());
        }
        if apply_divergences
            && crate::RESERVED_ID_PREFIXES
                .iter()
                .any(|prefix| order.client_order_id.starts_with(prefix))
        {
            return Err("client_order_id uses reserved liquidation prefix".into());
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

        if let Some(link) = order.link.as_ref() {
            self.validate_link(link, group)?;
        }

        let Some(instrument) = self.instruments.get(&order.symbol) else {
            return Err("unknown instrument".into());
        };

        if !order.reduce_only && self.margin_breached.contains(&order.symbol) {
            let currency = instrument.class.settlement_currency();
            return Err(format!(
                "margin breach: account equity below maintenance requirement in {currency}"
            ));
        }

        if order.quantity <= Decimal::ZERO {
            return Err("submit with non-positive quantity".into());
        }

        if !on_increment(order.quantity, instrument.size_increment) {
            return Err("quantity violates size increment".into());
        }

        // THE ROUND LOT, which is a rule about what may be SUBMITTED and not
        // about what the grid can represent. A partial fill legitimately leaves
        // an odd-lot remainder, so the size increment stays at one share and the
        // lot rule sits here, where an order is judged.
        let lot = instrument.class.lot_size();
        if lot > Decimal::ONE && !on_increment(order.quantity, lot) {
            return Err(format!(
                "quantity must be a multiple of the {lot}-share lot"
            ));
        }

        match order.order_type {
            OrderType::Market if order.price.is_none() => {
                return Err("submit price required".into());
            }
            OrderType::Market if order.trigger_price.is_some() => {
                return Err("Market order must not carry trigger_price".into());
            }
            OrderType::Limit if order.trigger_price.is_some() => {
                return Err("Limit order must not carry trigger_price".into());
            }
            OrderType::StopMarket | OrderType::MarketIfTouched | OrderType::TrailingStopMarket
                if order.price.is_some() =>
            {
                return Err("a market-on-trigger order must not carry a price".into());
            }
            _ if order.order_type.is_conditional() && order.trigger_price.is_none() => {
                return Err("conditional order must carry trigger_price".into());
            }
            OrderType::Limit
            | OrderType::StopLimit
            | OrderType::LimitIfTouched
            | OrderType::MarketToLimit
                if order.price.is_none() =>
            {
                return Err("limit order must carry a price".into());
            }
            _ => {}
        }
        let price = risk_px(order);
        // THE SAME PREDICATE THE WIRE GATE READS, IN THE SAME POSITION. This
        // was a hand-rolled `Limit | StopLimit | LimitIfTouched` and it had
        // drifted: the wire admitted a post-only `TrailingStopLimit` and this
        // then rejected it, so a consumer was told its order was on its way and
        // then that it was not. `on_trigger` has always enforced post-only on
        // that type, so the list was the only thing refusing it.
        //
        // THE ORDER OF THESE TWO CHECKS IS PART OF THE CONTRACT, not an
        // accident of how they were written. An order can break both rules at
        // once - a post-only `StopMarket` marked `Ioc` - and unifying the
        // PREDICATE while leaving the two gates to reach it in opposite orders
        // reinstates the same defect one layer up: two gates, one order, two
        // different reasons, and a consumer that still cannot tell which of them
        // spoke. Post-only is checked FIRST here because `validate_submit_order`
        // checks it first. Pinned by
        // `post_only_is_admitted_by_one_rule_at_the_wire_and_in_the_engine`.
        if order.post_only && !order.order_type.may_be_post_only() {
            return Err(mogwai_protocol::POST_ONLY_REFUSAL.into());
        }
        if order.order_type.is_conditional()
            && matches!(order.time_in_force, TimeInForce::Ioc | TimeInForce::Fok)
        {
            return Err("conditional orders cannot be immediate-or-cancel: a now-or-never order cannot wait for a trigger".into());
        }
        // THE CHILD FORM OF THIS RULE IS NOT REPEATED HERE, deliberately.
        // `mogwai_protocol::validate_order_link` already refuses a `Market`
        // child and an `Ioc`/`Fok` child for exactly these reasons, and
        // `validate_submit_order` runs it on every wire submit - the standalone
        // `SubmitOrder` frame and every member of a `SubmitOrderGroup` alike -
        // so no consumer-reachable route delivers one here. Restating it in this
        // function would be a second home for a rule that has one, which is the
        // defect the reap consolidation above just closed. The engine's own
        // exposure to a caller that skips the protocol validator is the whole
        // group-validation gap filed as finding 10, and it is closed by having
        // `on_submit_group` call the validator, not by copying one arm of it.
        if let Some(trigger) = order.trigger_price
            && !on_increment(trigger, instrument.price_increment)
        {
            return Err("trigger price violates price increment".into());
        }

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
        let Some(notional) = instrument.notional(order.quantity, price) else {
            return Err("order notional exceeds maximum representable value".into());
        };

        // Funded accounts are honest cash accounts: an order the free balance
        // cannot cover is rejected at the door, exactly like a real exchange.
        // A buy requires the full quote notional (the immediate synthetic fill
        // spends it, or the resting remainder holds it - same requirement
        // either way); a sell requires the base quantity. Unfunded accounts
        // skip this entirely and keep the permissive delta-off-zero ledger -
        // see `Engine::enforce_funds`.
        if self.enforce_funds && !order.reduce_only {
            if instrument.class.is_future() {
                let policy = self
                    .margin
                    .get(&order.symbol)
                    .ok_or_else(|| "cash-settled futures require the margin ledger".to_string())?;
                let commission =
                    self.maximum_commission(order, order.quantity, price, ts, apply_divergences);
                let required = policy
                    .initial(instrument, order.quantity, price)
                    .saturating_add(commission);
                let currency = instrument.class.settlement_currency();
                if self.free_balance(currency) < required {
                    return Err(format!("insufficient {currency} balance"));
                }
                return Ok(());
            }
            if instrument.class.is_equity() {
                let currency = instrument.class.settlement_currency();
                let commission =
                    self.maximum_commission(order, order.quantity, price, ts, apply_divergences);
                let policy = self.margin.get(&order.symbol);
                let required = match order.side {
                    // A cash account pays the whole notional; a margin account
                    // posts the Reg-T initial requirement and borrows the rest.
                    Side::Buy => match policy {
                        Some(policy) => policy.initial(instrument, order.quantity, price),
                        None => notional,
                    },
                    Side::Sell => {
                        // How short this order would leave the account IN THE
                        // WORST FILL ORDER. Selling shares you hold is not a
                        // short and needs neither a borrow nor collateral - but
                        // the shares can only cover one sale, and this check
                        // used to hand the same holding to every resting sell
                        // independently. Holding 100 shares, two sells of 100
                        // each computed `short = 0`, so both passed the cash
                        // account's by-name short refusal and the locate, and a
                        // cash equity account ended the run short 100 through an
                        // order path that refuses shorting outright.
                        //
                        // WORST FILL ORDER IS `worst_case_leaves`, NOT A SUM,
                        // and this order is one of its inputs rather than a
                        // term added to it. A plain bracket - a take-profit sell
                        // and a stop sell over the same held shares - is one
                        // exclusive group whose legs cannot both fill, and a
                        // held child cannot fill at all; a sum over the resting
                        // sells counted both and refused the second leg of every
                        // bracket a cash equity account ever placed.
                        //
                        // The group's members travel in as `pending` so this
                        // number is the same on the dry pass and the real one.
                        let mut pending: Vec<SubmitOrder> = group.to_vec();
                        if !pending
                            .iter()
                            .any(|member| member.client_order_id == order.client_order_id)
                        {
                            pending.push(order.clone());
                        }
                        let worst_sells =
                            self.worst_case_leaves(&order.symbol, Side::Sell, &pending);
                        let short = (self.net_position(&order.symbol) - worst_sells)
                            .min(Decimal::ZERO)
                            .abs();
                        if short.is_zero() {
                            Decimal::ZERO
                        } else {
                            // SHORTING IS A MARGIN ACTIVITY. A cash account
                            // cannot do it at any price, which is the whole
                            // cash-versus-margin distinction and not a funding
                            // question - so it is refused by NAME rather than
                            // as an insufficient balance.
                            let Some(policy) = policy else {
                                return Err(format!(
                                    "a cash equity account cannot sell {symbol} short; shorting \
                                     needs a margin account, which is a margin policy on this \
                                     symbol",
                                    symbol = order.symbol
                                ));
                            };
                            // THE LOCATE. A venue that states a borrow has a
                            // finite one, and a name it states as zero cannot be
                            // shorted at all.
                            if let Some(borrowable) = instrument.class.borrowable()
                                && short > borrowable
                            {
                                return Err(format!(
                                    "no shares to borrow: {symbol} allows a short of {borrowable} \
                                     and this order would leave {short}",
                                    symbol = order.symbol
                                ));
                            }
                            policy.initial(instrument, short, price)
                        }
                    }
                };
                let required = required.saturating_add(commission);
                if self.free_balance(currency) < required {
                    return Err(format!("insufficient {currency} balance"));
                }
                return Ok(());
            }
            if order.side == Side::Sell {
                let currency = instrument.class.settlement_currency();
                let commission =
                    self.maximum_commission(order, order.quantity, price, ts, apply_divergences);
                if self.free_balance(currency).saturating_add(notional) < commission {
                    return Err(format!("insufficient {currency} balance"));
                }
            }
            let (currency, required) = match order.side {
                Side::Buy => (
                    instrument.class.settlement_currency(),
                    notional.saturating_add(self.maximum_commission(
                        order,
                        order.quantity,
                        price,
                        ts,
                        apply_divergences,
                    )),
                ),
                Side::Sell => (
                    instrument.class.base_currency().ok_or_else(|| {
                        "cash-settled futures require the margin ledger".to_string()
                    })?,
                    order.quantity,
                ),
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
    #[expect(
        clippy::too_many_arguments,
        reason = "the funds question carries the complete prospective execution"
    )]
    fn validate_fill_funds(
        &self,
        order: &SubmitOrder,
        qty: Decimal,
        fill_px: Decimal,
        held: Decimal,
        liquidity_side: LiquiditySide,
        ts: u64,
        apply_divergences: bool,
    ) -> Result<(), String> {
        if !self.enforce_funds {
            return Ok(());
        }
        let instrument = &self.instruments[&order.symbol];
        // A spot sell normally pays commission from its quote proceeds, but a
        // configured per-unit fee can exceed those proceeds. Check that case
        // explicitly so even an extreme legal schedule cannot overdraw the
        // settlement balance. A future pays fees from settlement cash on
        // either side. A reduce-only future releases maintenance collateral,
        // and that release is included below when checking whether its
        // commission is payable.
        let commission = self.commission_for(
            order,
            qty,
            fill_px,
            liquidity_side,
            // A venue-originated order pays no consumer-armed `FeeSurcharge`,
            // so checking it against one could cancel a forced liquidation
            // and leave the breached position open.
            if apply_divergences {
                self.fee_surcharge_multiplier_for(self.event_sim, ts)
            } else {
                Decimal::ONE
            },
        );
        if !instrument.class.is_future() && order.side == Side::Sell {
            let proceeds = qty
                .checked_mul(fill_px)
                .ok_or_else(|| "order notional exceeds maximum representable value".to_string())?;
            let currency = instrument.class.settlement_currency();
            if self.free_balance(currency).saturating_add(proceeds) < commission {
                return Err(format!("insufficient {currency} balance"));
            }
            return Ok(());
        }
        // A MARGINED EQUITY posts the Reg-T requirement rather than the whole
        // notional, exactly as a future posts its bond, and BORROWS the rest -
        // which is what makes the settlement balance go negative on a margin
        // buy and is the loan itself. A cash equity falls through to the
        // notional branch below, where it belongs.
        let (required, released) = if instrument.class.is_equity()
            && let Some(policy) = self.margin.get(&order.symbol)
        {
            if order.reduce_only {
                (commission, policy.maintenance(instrument, qty, fill_px))
            } else {
                (
                    policy
                        .initial(instrument, qty, fill_px)
                        .saturating_add(commission),
                    Decimal::ZERO,
                )
            }
        } else if instrument.class.is_future() {
            let policy = self
                .margin
                .get(&order.symbol)
                .ok_or_else(|| "cash-settled futures require the margin ledger".to_string())?;
            if order.reduce_only {
                (commission, policy.maintenance(instrument, qty, fill_px))
            } else {
                (
                    policy
                        .initial(instrument, qty, fill_px)
                        .saturating_add(commission),
                    Decimal::ZERO,
                )
            }
        } else {
            (
                qty.checked_mul(fill_px)
                    .ok_or_else(|| {
                        "order notional exceeds maximum representable value".to_string()
                    })?
                    .saturating_add(commission),
                Decimal::ZERO,
            )
        };
        let currency = instrument.class.settlement_currency();
        if self
            .free_balance(currency)
            .saturating_add(held)
            .saturating_add(released)
            < required
        {
            return Err(format!("insufficient {currency} balance"));
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

    /// `apply_divergences` is FALSE for every venue-originated cancel, exactly
    /// as it is for every venue-originated submit.
    ///
    /// `liquidate_all`, `retire_off_river` and `cancel_unreadable_orders` all
    /// flatten the book through here, and an arm the consumer aimed at its own
    /// next cancel must not be spent on one of those. `RejectNextCancel` was
    /// the serious half: the first order a risk-breach liquidation tried to
    /// pull came back `OrderCancelRejected` AND STAYED RESTING, so the
    /// liquidation went on to close the positions while leaving live orders
    /// behind - precisely the hazard `liquidate_all`'s "resting orders go
    /// first" rule exists to prevent. `DropNextAccountUpdate` was the quiet
    /// half: spent on a transition the scenario author never aimed at.
    ///
    /// ONE FLAG, TWO EFFECTS, and the second is worth stating because it
    /// changes what a liquidation puts on the wire. With `apply_divergences`
    /// false the `DropNextAccountUpdate` check is not merely unspent, it is not
    /// consulted, so a flatten emits one `AccountState` per cancelled order
    /// unconditionally. That is the same stream a run with no arm produces, and
    /// it is the right one: a snapshot suppressed here would leave the consumer's
    /// ledger disagreeing with the venue's over a transition the consumer did not
    /// ask for. They stay one flag because both effects answer the same
    /// question - is this cancel the consumer's, or the venue's.
    pub(crate) fn on_cancel(
        &mut self,
        client_order_id: ClientOrderId,
        ts: u64,
        apply_divergences: bool,
    ) -> Vec<VenueMessage> {
        // Divergence: refuse a cancel the venue COULD have honoured, leaving the
        // order resting. Checked before the book is touched, so the refusal
        // really does leave everything where it was - the whole point is that
        // the consumer's model and the book disagree afterwards.
        //
        // Only when the order EXISTS: refusing a cancel for an unknown or
        // already-terminal id would spend the arm on a rejection that was going
        // to happen anyway, which is the failure mode a scenario author would
        // never see and would misread as the arm not firing.
        if apply_divergences
            && let Some(resting) = self
                .open
                .iter()
                .find(|order| order.submit.client_order_id == client_order_id)
                .map(|order| order.venue_order_id.clone())
            && let Some(Divergence::RejectNextCancel { reason }) =
                self.take_armed(|d| matches!(d, Divergence::RejectNextCancel { .. }))
        {
            return vec![VenueMessage::OrderCancelRejected {
                client_order_id,
                venue_order_id: Some(resting),
                reason: mogwai_protocol::truncate_reason(reason),
                ts_event: ts,
            }];
        }
        if let Some(pos) = self.open.position(&client_order_id) {
            let o = self.open[pos].clone();
            // A cancelled parent that never filled can never release its
            // children, and a held child left behind would rest for the life of
            // the run holding a promise nothing can keep. `close_out` owns that
            // rule for every terminal path, this one included.
            let reaped = self.close_out(pos, &o, WireOrderStatus::Canceled, ts);
            let mut out = vec![VenueMessage::OrderCanceled {
                client_order_id: client_order_id.clone(),
                venue_order_id: o.venue_order_id,
                ts_event: ts,
            }];
            out.extend(reaped);
            // SPELLS THE RULE ITSELF rather than calling
            // `push_account_snapshot`, and its doc names this site: a cancel
            // always moves the ledger, so the helper's `account_changed` half
            // would be dead weight, and the `apply_divergences` half is stated
            // here in the negative form this path was fixed into.
            //
            // A cancel takes the order OFF the book and gives its hold back, so
            // the snapshot it owes is exactly the account movement
            // `DropNextAccountUpdate` exists to hide - suppressing it leaves the
            // consumer overstating `locked`, which is the drift the arm is for.
            // The arm is spent on transitions, not on fills specifically; the
            // one carve-out is an order merely COMING TO REST, which `on_submit`
            // holds open for the fill the author is aiming at.
            if !apply_divergences
                || self
                    .take_armed(|d| matches!(d, Divergence::DropNextAccountUpdate))
                    .is_none()
            {
                out.push(VenueMessage::AccountState(self.snapshot(ts)));
            }
            out
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
            vec![VenueMessage::OrderCancelRejected {
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
        trigger_price: Option<Decimal>,
        ts: u64,
        reading: Option<MarketReading>,
    ) -> Vec<VenueMessage> {
        let Some(pos) = self.open.position(&client_order_id) else {
            let (reason, venue_order_id) =
                terminal_or_unknown_reject(&self.seen_client_order_ids, &client_order_id);
            return vec![VenueMessage::OrderModifyRejected {
                reason,
                client_order_id,
                venue_order_id,
                ts_event: ts,
            }];
        };

        let venue_order_id = self.open[pos].venue_order_id.clone();
        if price.is_none() && quantity.is_none() && trigger_price.is_none() {
            return vec![VenueMessage::OrderModifyRejected {
                client_order_id,
                venue_order_id: Some(venue_order_id),
                reason: "empty modify (no price or quantity)".into(),
                ts_event: ts,
            }];
        }

        // A trigger amend is legal on an UNTRIGGERED conditional and on nothing
        // else. After the trigger there is nothing left to trigger; on a plain
        // limit or market there never was, and silently ignoring the field
        // would make the amend a lie either way - so the two cases are refused
        // with the reason that is actually true of each.
        if trigger_price.is_some() && !matches!(self.open[pos].resting, Resting::Conditional { .. })
        {
            let conditional = self.open[pos].submit.order_type.is_conditional();
            // A HELD child has NOT triggered - it has not been released, and
            // saying it triggered is a false statement about the venue's own
            // state on the wire. Its trigger is still amendable in principle;
            // refusing it is the conservative answer while the child holds
            // nothing, and the reason now says which of the two it is.
            let held = conditional && matches!(self.open[pos].resting, Resting::Held);
            return vec![VenueMessage::OrderModifyRejected {
                client_order_id,
                venue_order_id: Some(venue_order_id),
                reason: if held {
                    "order-list child is held: its trigger is not live until its parent executes"
                        .into()
                } else if conditional {
                    "order has already triggered".into()
                } else {
                    "order carries no trigger to amend".into()
                },
                ts_event: ts,
            }];
        }

        // Neither kind of market order has a live limit price. A plain Market
        // can remain open only as an inert partial-fill remainder, and giving
        // that remainder a price must not turn it into a scannable limit.
        if price.is_some()
            && matches!(
                self.open[pos].submit.order_type,
                OrderType::Market | OrderType::StopMarket
            )
        {
            return vec![VenueMessage::OrderModifyRejected {
                client_order_id,
                venue_order_id: Some(venue_order_id),
                reason: if self.open[pos].submit.order_type == OrderType::StopMarket {
                    "StopMarket order must not carry a price".into()
                } else {
                    "Market order must not carry a price amend".into()
                },
                ts_event: ts,
            }];
        }

        let order = &self.open[pos];
        let new_total = quantity.unwrap_or(order.submit.quantity);
        let filled = order.submit.quantity - order.leaves_qty;

        if quantity.is_some() && new_total <= Decimal::ZERO {
            return vec![VenueMessage::OrderModifyRejected {
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
            return vec![VenueMessage::OrderModifyRejected {
                client_order_id,
                venue_order_id: Some(venue_order_id),
                reason: "modify to at or below already-filled quantity".into(),
                ts_event: ts,
            }];
        }

        if let Some(new_price) = price
            && new_price <= Decimal::ZERO
        {
            return vec![VenueMessage::OrderModifyRejected {
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
            return vec![VenueMessage::OrderModifyRejected {
                client_order_id,
                venue_order_id: Some(venue_order_id),
                reason: "unknown instrument".into(),
                ts_event: ts,
            }];
        };

        if let Some(new_trigger) = trigger_price
            && (new_trigger <= Decimal::ZERO
                || !on_increment(new_trigger, instrument.price_increment))
        {
            return vec![VenueMessage::OrderModifyRejected {
                client_order_id,
                venue_order_id: Some(venue_order_id),
                reason: "trigger price violates price increment".into(),
                ts_event: ts,
            }];
        }

        if !on_increment(new_total, instrument.size_increment) {
            return vec![VenueMessage::OrderModifyRejected {
                client_order_id,
                venue_order_id: Some(venue_order_id),
                reason: "quantity violates size increment".into(),
                ts_event: ts,
            }];
        }

        if let Some(new_price) = price
            && order.submit.post_only
            && matches!(order.resting, Resting::Limit { .. })
        {
            let band_draw = order.band_draw.saturating_add(1);
            let band_ticks = reading.map_or(order.band_ticks, |value| value.band_ticks);
            let trigger = draw_trigger(
                self.fill_seed,
                &order.submit,
                new_price,
                instrument.price_increment,
                band_ticks,
                band_draw,
            );
            if reading
                .is_some_and(|value| trades_through(order.submit.side, trigger, value.last_px))
            {
                return vec![VenueMessage::OrderModifyRejected {
                    client_order_id,
                    venue_order_id: Some(venue_order_id),
                    reason: "post-only order would take liquidity".into(),
                    ts_event: ts,
                }];
            }
        }

        // The price this order's hold, notional and funds check are
        // taken against AFTER the amend - `risk_px` under amendment. For a
        // price-less `StopMarket` that is its trigger, new or existing, which
        // is what `held_balances` will hold against; for everything else it
        // is the limit price. Spelled as `or` rather than unwrapped so the
        // validation degrades instead of panicking if the invariant loosens.
        let effective_price = if order.submit.order_type == OrderType::StopMarket {
            trigger_price.or(order.submit.trigger_price)
        } else {
            price.or(order.submit.price)
        };
        if let Some(effective_price) = effective_price {
            if !on_increment(effective_price, instrument.price_increment) {
                return vec![VenueMessage::OrderModifyRejected {
                    client_order_id,
                    venue_order_id: Some(venue_order_id),
                    reason: "price violates price increment".into(),
                    ts_event: ts,
                }];
            }

            // Same overflow guard as `validate_submit`: `held_balances`
            // multiplies `leaves_qty * price` for every open buy order on
            // every snapshot, including the one this very modify emits below,
            // so an unbounded notional here panics immediately rather than in
            // some later, harder-to-trace fill.
            if new_total.checked_mul(effective_price).is_none() {
                return vec![VenueMessage::OrderModifyRejected {
                    client_order_id,
                    venue_order_id: Some(venue_order_id),
                    reason: "order notional exceeds maximum representable value".into(),
                    ts_event: ts,
                }];
            }

            // Funded accounts check the amended hold against free
            // balance, mirroring the submit-side funds check: an amend that
            // grows a hold past what the account holds is refused, or
            // the venue would advertise free < 0 in its own snapshot. The
            // order's CURRENT hold is excluded from the comparison
            // (it is being replaced, not added to), so free-plus-own-hold
            // must cover the new hold. Both products are bounded: the new
            // one by the checked_mul just above (leaves <= total), the old
            // one by the same check at its own submit/amend time.
            if self.enforce_funds && !self.open[pos].submit.reduce_only {
                let order = &self.open[pos];
                let new_leaves = new_total - filled;
                // The amended fill will be charged commission just like a
                // fresh submit, so the amended requirement carries it too -
                // the worse of the maker and taker rate, because which side
                // the order ends up providing is not known until it fills.
                // Without this an amend is admitted that the fill-time check
                // then cancels.
                let commission =
                    self.maximum_commission(&order.submit, new_leaves, effective_price, ts, true);
                // A spot sell places a BASE hold, but pays its fee out of the
                // quote proceeds, so its commission is checked against the
                // settlement balance separately - the same split
                // `validate_submit` makes.
                if !instrument.class.is_future() && order.submit.side == Side::Sell {
                    let settlement = instrument.class.settlement_currency();
                    let proceeds = new_leaves.saturating_mul(effective_price);
                    if self.free_balance(settlement).saturating_add(proceeds) < commission {
                        return vec![VenueMessage::OrderModifyRejected {
                            client_order_id,
                            venue_order_id: Some(venue_order_id),
                            reason: format!("insufficient {settlement} balance"),
                            ts_event: ts,
                        }];
                    }
                }
                // BOTH SIDES OF THE COMPARISON COME FROM `order_hold`,
                // the one home of the hold's shape. This block used to be a
                // fourth hand-rolled copy of that formula, and it had drifted
                // twice over: the futures arm priced the amended requirement at
                // the amend's own optional `price` rather than
                // `effective_price`, so a quantity-only amend of a futures order
                // under a notional policy required `initial(new_leaves, 0)` -
                // zero - and the funds check could not fail however far the
                // amend grew the position; and the buy arm held the full
                // notional even for a margined equity, which posts the Reg-T
                // initial instead. The refusals below stay explicit because they
                // name a MISCONFIGURED instrument rather than a funding
                // shortfall, and `order_hold` answers `None` for both.
                if instrument.class.is_future() && !self.margin.contains_key(&order.submit.symbol) {
                    return vec![VenueMessage::OrderModifyRejected {
                        client_order_id,
                        venue_order_id: Some(venue_order_id),
                        reason: "cash-settled futures require the margin ledger".into(),
                        ts_event: ts,
                    }];
                }
                // A NON-FUTURE, NON-EQUITY SELL places a hold on its BASE asset, so an
                // instrument declaring no base currency has nowhere to take the
                // hold from. The reason string used to say "cash-settled futures
                // require the margin ledger" here, carried over verbatim from
                // the futures arm it shared a block with, which named the wrong
                // class of instrument for the wrong reason - the order being
                // refused is a SPOT sell, not a future.
                if !instrument.class.is_future()
                    && !instrument.class.is_equity()
                    && order.submit.side == Side::Sell
                    && instrument.class.base_currency().is_none()
                {
                    return vec![VenueMessage::OrderModifyRejected {
                        client_order_id,
                        venue_order_id: Some(venue_order_id),
                        reason:
                            "a sell of this instrument places a hold on its base asset, which it \
                                 declares none of"
                                .into(),
                        ts_event: ts,
                    }];
                }
                // A HELD order-list child holds nothing on either side of this
                // comparison, and an amend does not release it - round 1's
                // promotion guard excludes `Held` deliberately - so the amended
                // order is still held and still holds nothing. The rule's home
                // is `order_hold_entry`, which declines a held child
                // before it computes anything; asking `order_hold`
                // directly (as the two calls below do, because they must price
                // an order that is not on the book in this shape yet) BYPASSES
                // that home, so the test is made here rather than left implicit.
                // Without it an amend of a bracket's held exit leg was checked
                // against a hold the venue would never take, and could be
                // refused for funds an unheld order would not have needed.
                if !matches!(order.resting, Resting::Held) {
                    // The AMENDED order, built as a submit and priced at
                    // `effective_price`, is what the venue would hold once the
                    // amend lands - the same construction `hold_for` makes for a
                    // resting order, so an amend can never be checked against a
                    // requirement the hold would not actually take.
                    let mut amended = order.submit.clone();
                    amended.quantity = new_total;
                    if let Some(new_price) = price {
                        amended.price = Some(new_price);
                    }
                    if let Some(new_trigger) = trigger_price {
                        amended.trigger_price = Some(new_trigger);
                    }
                    // `clipped` is DROPPED, exactly as `hold_for` drops it and
                    // every other check-time hold site does. The flag
                    // belongs to the RECORDING path: `add_order_hold`
                    // re-derives it through `order_hold_entry` when the
                    // amend actually lands below, and that is what puts the
                    // currency into `order_holds_clipped` and raises the
                    // saturation warning. Recording it here would warn about a
                    // hold the venue may yet refuse to take.
                    let mut clipped = false;
                    // `unwrap_or_default` rather than `risk_px`: the order being
                    // amended may be a Market remainder resting inert with
                    // neither a price nor a trigger, and its hold is zero either
                    // way.
                    let held_price = order
                        .submit
                        .price
                        .or(order.submit.trigger_price)
                        .unwrap_or_default();
                    let existing_hold =
                        self.order_hold(&order.submit, order.leaves_qty, held_price, &mut clipped);
                    let required_hold =
                        self.order_hold(&amended, new_leaves, effective_price, &mut clipped);
                    let settlement = instrument.class.settlement_currency();
                    let base = instrument.class.base_currency().unwrap_or(settlement);
                    let amount_in = |currency: &str, hold: &crate::account::Hold| match hold {
                        crate::account::Hold::None => Decimal::ZERO,
                        crate::account::Hold::Settlement(amount) => {
                            if currency == settlement {
                                *amount
                            } else {
                                Decimal::ZERO
                            }
                        }
                        crate::account::Hold::Base(amount) => {
                            if currency == base {
                                *amount
                            } else {
                                Decimal::ZERO
                            }
                        }
                    };
                    // The currency the check runs in is whichever one the
                    // AMENDED hold lands in, falling back to the current hold's
                    // when the amend places no hold at all - a comparison
                    // against a currency neither side touches is vacuously true,
                    // which is exactly what it should be.
                    let currency = match (&required_hold, &existing_hold) {
                        (crate::account::Hold::Base(_), _)
                        | (crate::account::Hold::None, crate::account::Hold::Base(_)) => base,
                        _ => settlement,
                    };
                    let held = amount_in(currency, &existing_hold);
                    let required = amount_in(currency, &required_hold).saturating_add(
                        // Commission is paid in settlement cash; a base-currency
                        // hold's fee was already checked against the settlement
                        // balance by the spot-sell branch above.
                        if currency == settlement {
                            commission
                        } else {
                            Decimal::ZERO
                        },
                    );
                    if self.free_balance(currency).saturating_add(held) < required {
                        return vec![VenueMessage::OrderModifyRejected {
                            client_order_id,
                            venue_order_id: Some(venue_order_id),
                            reason: format!("insufficient {currency} balance"),
                            ts_event: ts,
                        }];
                    }
                }
            }
        }

        let before = self.open[pos].clone();
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
                // The re-draw takes a FRESH band when the venue supplies one.
                // An amend arrives over the same path that reads the tape on
                // every limit submit, so a reading is available and cheap next
                // to the amend itself; an order repriced hours after acceptance
                // would otherwise keep a band fitted to a regime that is gone.
                // The stored value is the fallback, and is updated to whatever
                // the re-draw used so a later tranche inherits the current
                // regime rather than the acceptance one.
                order.band_draw = order.band_draw.saturating_add(1);
                order.band_ticks = reading.map_or(order.band_ticks, |value| value.band_ticks);
                // A price amend on an UNTRIGGERED stop-limit changes the limit
                // the order will take once it fires, not the price the tape has
                // to touch. It re-reads the band (which the trigger will draw
                // against) but leaves the order conditional and does NOT
                // restart the trigger window - promoting it here would make the
                // venue fill a stop that never triggered.
                //
                // AND A HELD CHILD STAYS HELD. `Resting::Held` is not
                // `Conditional`, so this used to promote an order-list child
                // still waiting on its parent into a live, scannable limit -
                // taking a hold it had deliberately not taken and
                // letting it fill before its parent ever executed. That is
                // one-triggers-the-other defeated by a consumer amend. The amend
                // moves `submit.price`, which is the price the child will rest
                // at when its parent releases it and re-draws.
                if !matches!(order.resting, Resting::Conditional { .. } | Resting::Held) {
                    order.resting = Resting::Limit {
                        fill_trigger_px: draw_trigger(
                            self.fill_seed,
                            &order.submit,
                            new_price,
                            instrument.price_increment,
                            order.band_ticks,
                            order.band_draw,
                        ),
                    };
                    order.scanned_ns = ts;
                }
            }
            // Amending the trigger restarts the trigger window, exactly as a
            // price amend restarts a limit's band window.
            if let Some(new_trigger) = trigger_price {
                order.submit.trigger_price = Some(new_trigger);
                order.resting = Resting::Conditional {
                    stop_px: new_trigger,
                    toward: order.submit.order_type.triggers_toward(),
                };
                order.scanned_ns = ts;
            }
            order.submit.quantity = new_total;
            order.leaves_qty = new_total - filled;
            order.ts_last = ts;
            (order.submit.quantity, order.submit.price, order.leaves_qty)
        };
        self.refresh_open_hold(pos, &before);

        // MARKED EXEMPTION FROM `push_account_snapshot`, three of three, and the
        // only one whose batch genuinely DOES move the ledger -
        // `refresh_open_hold` above just moved the hold. It is exempt by
        // an existing ruling rather than by the helper's rule:
        // `modify_does_not_consume_armed_drop` pins the arm SURVIVING an amend
        // so it lands on the next FILL, which is what an author arming
        // `DropNextAccountUpdate` was pointing at. Routing this site through the
        // helper is therefore a deliberate behaviour change and an owner
        // question, not a bug fix - the cold review that proposed it had not
        // seen the test.
        vec![
            VenueMessage::OrderUpdated {
                client_order_id,
                venue_order_id,
                quantity,
                price,
                trigger_price,
                leaves_qty,
                ts_event: ts,
            },
            VenueMessage::AccountState(self.snapshot(ts)),
        ]
    }
}

/// FNV-1a over the run's fill seed and the order's identity. Deliberately a
/// pure function of consumer-supplied fields plus `fill_seed`, which means a
/// consumer that dislikes its trigger can cancel and resubmit under a fresh
/// `client_order_id` to re-roll it. For a test venue whose consumers are
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
    feed(&price.normalize().serialize());
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
pub(super) fn draw_trigger(
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
/// The consumer's stated price is ignored for PRICING - answering "what price did
/// this trade at" with the consumer's own number is the same defect the limit
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
    band_draw: u32,
) -> Decimal {
    let offset = increment.checked_mul(Decimal::from(draw_offset(
        fill_seed,
        order,
        risk_px(order),
        band_ticks,
        band_draw,
    )));
    safe_price(
        stated_px,
        offset.and_then(|offset| match order.side {
            Side::Buy => last_px.checked_add(offset),
            Side::Sell => last_px.checked_sub(offset),
        }),
    )
}

/// The one price a price-less `StopMarket` substitutes everywhere the engine
/// assumes an order has one: its hold, its notional overflow guard, its
/// funded-account requirement and its RNG key. Total for every shape
/// `validate_submit_order` admits.
fn risk_px(order: &SubmitOrder) -> Decimal {
    order
        .price
        .or(order.trigger_price)
        .expect("validated submit carries a price or a trigger")
}

/// Whether a batch moved the ledger and therefore owes an account snapshot. A
/// no-fill transition that FREES a hold - a cap-zero reduce-only cancel,
/// a post-only trigger rejection, a trigger-time funds cancel - owes one just
/// as a fill does.
/// EXHAUSTIVE OVER `VenueMessage` deliberately: the predicate claims to answer
/// "did anything move the ledger", and a `matches!` over a hand-written list
/// answers "is it one of the ones I thought of". A variant added later must be
/// classified here rather than inherit `false` silently - which is how
/// `OrderUpdated` and `OrderExpired` came to be missing. An `Ouo` shrink plainly
/// moves the hold, and an expiry releases one; neither is reachable today
/// without an accompanying fill or cancel at either call site, so closing the
/// gap changes no behaviour now and cannot open one later.
pub(crate) fn account_changed(out: &[VenueMessage]) -> bool {
    out.iter().any(|event| match event {
        VenueMessage::OrderFilled(_)
        | VenueMessage::OrderCanceled { .. }
        | VenueMessage::OrderRejected { .. }
        | VenueMessage::OrderUpdated { .. }
        | VenueMessage::OrderExpired { .. } => true,
        // Accepted, triggered and the modify/cancel REFUSALS move nothing: the
        // order is answerable but no balance has changed hands. `AccountState`
        // itself is the snapshot, not a reason to take one.
        VenueMessage::OrderAccepted { .. }
        | VenueMessage::OrderTriggered { .. }
        | VenueMessage::OrderModifyRejected { .. }
        | VenueMessage::OrderCancelRejected { .. }
        | VenueMessage::AccountState(_)
        // Not order lifecycle at all: replies, tape frames and transport
        // notices, none of which an engine batch books a balance through.
        | VenueMessage::RunComplete { .. }
        | VenueMessage::PassengerDurationComplete { .. }
        | VenueMessage::AdmissionRejected { .. }
        | VenueMessage::OrderStatusSnapshot(_)
        | VenueMessage::FillSnapshot(_)
        | VenueMessage::Trade(_)
        | VenueMessage::Quote(_)
        | VenueMessage::Heartbeat { .. }
        | VenueMessage::FeedLagged { .. }
        | VenueMessage::HavocDiagnostic { .. }
        | VenueMessage::ProtocolError { .. } => false,
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

    let symbol = order.symbol.as_ref();
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
