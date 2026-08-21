// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The account ledger: balances, positions, locked reservations, and the
//! `AccountState` snapshot. Includes the saturating arithmetic that keeps
//! unbounded cross-fill accumulation from panicking the engine.

use std::collections::{HashMap, HashSet};

use mogwai_protocol::{AccountState, Balance, OrderFilled, Side, SubmitOrder, Symbol};
use rust_decimal::Decimal;

use crate::Engine;

/// What one resting order holds, and in which of the instrument's two
/// currencies. Returned by `Engine::order_reservation`, the single derivation
/// both the account snapshot and the order-path funds checks read.
pub(crate) enum Reservation {
    /// Nothing is held: a reduce-only order, or a future with no margin policy.
    None,
    /// Settlement (quote) cash: futures initial margin, or a spot buy notional.
    Settlement(Decimal),
    /// Base currency: a spot sell delivering what it already holds.
    Base(Decimal),
}

#[derive(Debug, Default)]
pub(crate) struct Account {
    pub(crate) balances: HashMap<String, Decimal>,
    pub(crate) positions: HashMap<(Symbol, Option<String>), PositionState>,
    /// Sale proceeds credited but NOT YET SETTLED, each with the instant it
    /// becomes spendable.
    ///
    /// This is what a `T+N` convention actually is: the money is yours the
    /// moment the trade prints, and you cannot use it to buy anything until
    /// settlement. It rides `balances` as a full credit and is subtracted by
    /// `free_balance`, so it appears on the wire as `locked` - which is exactly
    /// what it is, and needs no new balance field for a consumer to understand.
    pub(crate) unsettled: Vec<UnsettledCredit>,
}

/// One sale's proceeds waiting out its settlement period.
#[derive(Debug, Clone)]
pub(crate) struct UnsettledCredit {
    pub(crate) currency: String,
    pub(crate) amount: Decimal,
    /// Sim instant this becomes spendable. Released by the sweeper's pass.
    pub(crate) settles_at_ns: u64,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct PositionState {
    pub(crate) qty: Decimal,
    pub(crate) avg_px: Decimal,
    pub(crate) mark_px: Decimal,
}

#[derive(Debug, Default)]
pub(crate) struct Warned {
    pub(crate) missing_instrument: HashSet<Symbol>,
    pub(crate) zero_px: HashSet<Symbol>,
    /// Symbols/currencies whose ledger arithmetic has saturated at the Decimal
    /// range boundary (see `add_clamped` and friends). Once a key clips, its
    /// stored value is a boundary, not a true total, so the condition persists
    /// across every later snapshot - warn once per key, not once per event.
    pub(crate) saturated: HashSet<String>,
    /// Inverse symbols a caller has offered a non-positive settlement price
    /// for. The mark is refused rather than applied, and the condition is a
    /// property of the caller rather than of one instant, so it would repeat on
    /// every settlement instant of the run - warn once per symbol.
    pub(crate) unpriceable_settlement: HashSet<Symbol>,
}

impl Engine {
    pub(crate) fn rest_open(&mut self, order: crate::OpenOrder) {
        self.add_order_reservation(&order);
        self.open.push(order);
    }

    pub(crate) fn take_open(&mut self, pos: usize) -> crate::OpenOrder {
        let order = self.open.remove(pos);
        self.remove_order_reservation(&order);
        order
    }

    pub(crate) fn refresh_open_reservation(&mut self, pos: usize, before: &crate::OpenOrder) {
        self.remove_order_reservation(before);
        let after = self.open[pos].clone();
        self.add_order_reservation(&after);
    }

    pub(crate) fn order_reservation_entry(
        &self,
        order: &crate::OpenOrder,
    ) -> Option<(String, Decimal, bool)> {
        // A HELD order-list child reserves nothing. It cannot execute until its
        // parent fills, and holding funds against it would let a bracket's exit
        // legs starve the entry that has to fill before either can do anything.
        // The hold appears at RELEASE, through the same `refresh_open_reservation`
        // any other state change goes through.
        if matches!(order.resting, crate::Resting::Held) {
            return None;
        }
        let instrument = self.instruments.get(&order.submit.symbol)?;
        let price = order.submit.price.or(order.submit.trigger_price)?;
        let mut clipped = false;
        let reservation =
            self.order_reservation(&order.submit, order.leaves_qty, price, &mut clipped);
        let entry = match reservation {
            Reservation::None => None,
            Reservation::Settlement(amount) => Some((
                instrument.class.settlement_currency().to_owned(),
                amount,
                clipped,
            )),
            Reservation::Base(amount) => instrument
                .class
                .base_currency()
                .map(|currency| (currency.to_owned(), amount, clipped)),
        };
        // A zero hold (a margin policy with `initial_per_contract` of zero)
        // contributes nothing, so it is treated as no entry at all. Without
        // this the incremental cache and the reconciliation fold could
        // disagree about whether a currency KEY exists while agreeing on
        // every amount: the fold inserts a zero entry per zero-hold order,
        // but the incremental remove deletes a key the moment its total hits
        // zero - a spurious debug panic on states that are economically equal.
        entry.filter(|(_, amount, clipped)| !amount.is_zero() || *clipped)
    }

    fn add_order_reservation(&mut self, order: &crate::OpenOrder) {
        let Some((currency, amount, mut clipped)) = self.order_reservation_entry(order) else {
            return;
        };
        if let Some(total) = self.order_locked.get_mut(currency.as_str()) {
            *total = add_clamped(*total, amount, &mut clipped);
        } else {
            self.order_locked.insert(currency.clone(), amount);
        }
        if clipped {
            self.order_locked_clipped.insert(currency);
        }
    }

    fn remove_order_reservation(&mut self, order: &crate::OpenOrder) {
        let Some((currency, amount, _)) = self.order_reservation_entry(order) else {
            return;
        };
        if self.order_locked_clipped.contains(&currency) {
            self.rebuild_order_locked_excluding(Some(&order.submit.client_order_id));
            return;
        }
        let Some(total) = self.order_locked.get_mut(&currency) else {
            debug_assert!(false, "reservation cache missing currency");
            return;
        };
        *total = total.saturating_sub(amount);
        if total.is_zero() {
            self.order_locked.remove(&currency);
        }
    }

    pub(crate) fn rebuild_order_locked_excluding(&mut self, excluded: Option<&str>) {
        let (locked, clipped) = self.compute_order_locked(excluded);
        self.order_locked = locked;
        self.order_locked_clipped = clipped;
    }

    fn compute_order_locked(
        &self,
        excluded: Option<&str>,
    ) -> (HashMap<String, Decimal>, HashSet<String>) {
        let mut locked = HashMap::<String, Decimal>::new();
        let mut clipped_currencies = HashSet::new();
        for order in self
            .open
            .iter()
            .filter(|order| excluded != Some(order.submit.client_order_id.as_str()))
        {
            let Some((currency, amount, mut clipped)) = self.order_reservation_entry(order) else {
                continue;
            };
            let total = locked.entry(currency.clone()).or_default();
            *total = add_clamped(*total, amount, &mut clipped);
            if clipped {
                clipped_currencies.insert(currency);
            }
        }
        (locked, clipped_currencies)
    }

    /// Compare the fast aggregate with a fresh fold through the authoritative
    /// order book, and PANIC on any difference.
    ///
    /// This is a debug-only check, called under `cfg!(debug_assertions)` at
    /// every command and sweep boundary, and the choice is deliberate on both
    /// halves. It panics rather than repairing because a drifted hold is a
    /// logic defect in incremental maintenance, and a silent repair would
    /// convert that defect into a log line nobody reads while leaving every
    /// funds decision taken before the repair already wrong. It is debug-only
    /// because the fold is exactly the `O(open orders)` walk with its
    /// per-order allocation that this cache exists to remove: running it in
    /// release would reinstate that cost per command on the funded venue,
    /// which is the only configuration that ships.
    ///
    /// What holds the invariant in release is construction, not verification.
    /// `OpenBook`'s storage is private and every mutation of a resting order's
    /// reservation inputs goes through `rest_open`, `take_open` or
    /// `refresh_open_reservation`. A new mutation path that bypasses those
    /// three is the defect this design is exposed to, and the debug assertion
    /// is what catches it: the whole test suite runs under it.
    pub(crate) fn reconcile_order_locked(&self) {
        let (locked, clipped) = self.compute_order_locked(None);
        assert_eq!(
            (&self.order_locked, &self.order_locked_clipped),
            (&locked, &clipped),
            "resting-order reservation cache drifted from the book"
        );
    }

    #[cfg(all(test, debug_assertions))]
    pub(crate) fn corrupt_order_locked_for_test(&mut self, currency: &str, amount: Decimal) {
        self.order_locked.insert(currency.to_owned(), amount);
    }

    pub(crate) fn apply_fill(&mut self, fill: &OrderFilled) {
        let previous_position = self
            .account
            .positions
            .get(&(
                std::sync::Arc::clone(&fill.symbol),
                self.position_key_id(fill.position_id.as_ref()),
            ))
            .cloned()
            .unwrap_or_default();
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

        if instrument.class.is_future() {
            let settlement = instrument.class.settlement_currency().to_owned();
            let before = previous_position;
            let reducing = (before.qty > Decimal::ZERO && fill.side == Side::Sell)
                || (before.qty < Decimal::ZERO && fill.side == Side::Buy);
            if reducing {
                let closed = before.qty.abs().min(fill.last_qty);
                let direction = if before.qty > Decimal::ZERO {
                    Decimal::ONE
                } else {
                    -Decimal::ONE
                };
                // Through `InstrumentDef::unrealized`, which is the one place
                // the INVERSE arithmetic lives: a coin-margined contract's P and
                // L is `multiplier * qty * (1/entry - 1/exit)`, not
                // `(exit - entry) * qty * multiplier`. Realized and unrealized
                // must come from the same expression or a position's value would
                // jump the moment it closed.
                let pnl = instrument
                    .unrealized(
                        closed.saturating_mul(direction),
                        before.avg_px,
                        fill.last_px,
                    )
                    .unwrap_or(Decimal::ZERO);
                let total = self.account.balances.entry(settlement).or_default();
                *total = total.saturating_add(pnl).saturating_sub(fill.commission);
            } else if !fill.commission.is_zero() {
                let total = self.account.balances.entry(settlement).or_default();
                *total = total.saturating_sub(fill.commission);
            }
            return;
        }
        // EQUITY moves cash only. The shares are the POSITION, which
        // `apply_position` above already recorded, and crediting them as a
        // balance too would both double-count them and make them spendable as
        // money. This is the spot branch's quote leg without its base leg, and
        // the difference is exactly what makes an equity account statable in one
        // currency where a spot account needs its base priced.
        if matches!(
            instrument.class,
            mogwai_protocol::InstrumentClass::Equity { .. }
        ) {
            let currency = instrument.class.settlement_currency().to_owned();
            let settlement_ns = instrument.class.settlement_ns();
            let mut clipped = false;
            let notional = mul_clamped(
                mul_clamped(fill.last_qty, fill.last_px, &mut clipped),
                instrument.class.multiplier(),
                &mut clipped,
            );
            let proceeds = sub_clamped(notional, fill.commission, &mut clipped);
            let total = self.account.balances.entry(currency.clone()).or_default();
            *total = match fill.side {
                Side::Buy => sub_clamped(
                    *total,
                    add_clamped(notional, fill.commission, &mut clipped),
                    &mut clipped,
                ),
                Side::Sell => add_clamped(
                    *total,
                    sub_clamped(notional, fill.commission, &mut clipped),
                    &mut clipped,
                ),
            };
            // THE SETTLEMENT PERIOD. A sale's proceeds are credited above and
            // held unspendable until the instrument's period has run - which is
            // the whole of what `T+N` means to a strategy, and is invisible to
            // one that never tries to spend the money before it lands.
            if fill.side == Side::Sell && settlement_ns > 0 && proceeds > Decimal::ZERO {
                self.account.unsettled.push(UnsettledCredit {
                    currency,
                    amount: proceeds,
                    settles_at_ns: fill.ts_event.saturating_add(settlement_ns),
                });
            }
            if clipped {
                self.warn_saturated(&fill.symbol);
            }
            return;
        }
        let Some(base) = instrument.class.base_currency().map(str::to_owned) else {
            return;
        };
        let quote = instrument.class.settlement_currency().to_owned();
        // Fees are charged in the settlement currency. A buy adds commission
        // to its quote spend; a sell subtracts commission from its proceeds.
        // The order path includes the same charge in its funded-account checks
        // before this ledger mutation can run.

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
            .get(&(
                std::sync::Arc::clone(&fill.symbol),
                self.position_key_id(fill.position_id.as_ref()),
            ))
            .cloned()
            .unwrap_or_default();

        let mut clipped = false;
        let next = next_position(&current, delta, fill.last_px, &mut clipped);
        if clipped {
            self.warn_saturated(&fill.symbol);
        }
        if next.qty == Decimal::ZERO {
            self.account.positions.remove(&(
                std::sync::Arc::clone(&fill.symbol),
                self.position_key_id(fill.position_id.as_ref()),
            ));
        } else {
            self.account.positions.insert(
                (
                    std::sync::Arc::clone(&fill.symbol),
                    self.position_key_id(fill.position_id.as_ref()),
                ),
                next,
            );
        }
    }

    // `&mut self` purely for the once-per-currency saturation warning below;
    // the account state itself is only read.
    pub(crate) fn snapshot(&mut self, ts: u64) -> AccountState {
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
            account_id: self.account_id.clone(),
            balances,
            positions: self.positions(),
            margins: self.margin_requirement(),
            ts_event: ts,
        }
    }

    /// The spendable amount of one currency: the booked total minus every
    /// resting order's reservation. This is what the funds checks in
    /// `validate_submit` and `on_modify` compare an order's requirement
    /// against, and it matches the `free` the snapshot reports for the same
    /// currency (both derive from `locked_balances` with clamped arithmetic).
    /// The account's NET quantity in one symbol, across every position key. The
    /// number a short-sale check is against: under hedging two legs of opposite
    /// sign are one net exposure to the borrow desk.
    pub(crate) fn net_position(&self, symbol: &str) -> Decimal {
        self.account
            .positions
            .iter()
            .filter(|((position_symbol, _), _)| position_symbol.as_ref() == symbol)
            .fold(Decimal::ZERO, |sum, (_, state)| {
                sum.saturating_add(state.qty)
            })
    }

    pub(crate) fn free_balance(&self, currency: &str) -> Decimal {
        let total = *self
            .account
            .balances
            .get(currency)
            .unwrap_or(&Decimal::ZERO);
        let mut clipped = false;
        let mut locked = *self.order_locked.get(currency).unwrap_or(&Decimal::ZERO);
        // Unsettled sale proceeds are HELD, not absent: the account owns them
        // and cannot spend them until their instant.
        for credit in self
            .account
            .unsettled
            .iter()
            .filter(|credit| credit.currency == currency)
        {
            locked = add_clamped(locked, credit.amount, &mut clipped);
        }
        for ((symbol, _), position) in &self.account.positions {
            let Some(policy) = self.margin.get(symbol) else {
                continue;
            };
            // MARKED, not merely a future. An equity carrying a margin policy is
            // a Reg-T margin account and posts maintenance collateral exactly as
            // a future does; a SPOT symbol carrying one still moves no number,
            // which is what the class check is here for.
            let Some(instrument) = self
                .instruments
                .get(symbol)
                .filter(|instrument| instrument.class.is_marked())
            else {
                continue;
            };
            if instrument.class.settlement_currency() == currency {
                // At the position's MARK, which is what makes a notional-basis
                // requirement move with the price the way a leveraged account's
                // actually does. A per-contract policy ignores the price, so
                // this is the same number it always was for futures.
                let reserve = policy.maintenance(instrument, position.qty, position.mark_px);
                locked = add_clamped(locked, reserve, &mut clipped);
            }
        }
        sub_clamped(total, locked, &mut clipped)
    }

    /// The reservation ONE resting order contributes, as a currency kind and
    /// an amount. The single authority on the shape of an open order's hold:
    /// `locked_balances` folds it into the account totals and `held_for` adds
    /// the same number back before a funds check, so the two cannot disagree
    /// about branch order. A future posts initial margin in settlement cash on
    /// either side (and needs a margin policy to post anything at all); a spot
    /// buy reserves quote notional, a spot sell reserves base quantity. The
    /// instrument CLASS is consulted before the margin map, so a margin policy
    /// configured against a spot symbol - which venue config rejects at boot,
    /// but the public `set_margin_policy` cannot - moves neither number.
    pub(crate) fn order_reservation(
        &self,
        submit: &SubmitOrder,
        leaves: Decimal,
        price: Decimal,
        clipped: &mut bool,
    ) -> Reservation {
        if submit.reduce_only {
            return Reservation::None;
        }
        let Some(instrument) = self.instruments.get(&submit.symbol) else {
            return Reservation::None;
        };
        if instrument.class.is_future() {
            return match self.margin.get(&submit.symbol) {
                // Priced at the CALLER'S `price`, since that is the notional the
                // account is committing to; there is no mark for an order that
                // has not filled. It is the argument and not `submit.price`
                // because a price-less `StopMarket` commits at its TRIGGER, and
                // every caller already resolves that fallback - reaching back
                // into `submit` discarded the resolution and reserved
                // `initial(qty, 0)`, which under `MarginBasis::Notional` is
                // nothing at all, so a resting stop held no collateral.
                Some(policy) => Reservation::Settlement(policy.initial(instrument, leaves, price)),
                None => Reservation::None,
            };
        }
        // AN EQUITY IS A CASH ACCOUNT OR A MARGIN ACCOUNT, and the margin policy
        // is which. A cash account commits the whole notional on a buy, the way
        // a spot buy does, and reserves nothing on a sell because it can only
        // ever sell shares it already holds. A MARGIN account commits the
        // Reg-T initial requirement on either side - and on a sell, only for the
        // portion the account is not already long, since selling what you hold
        // is not a short.
        if instrument.class.is_equity() {
            let policy = self.margin.get(&submit.symbol);
            return match (submit.side, policy) {
                (Side::Buy, Some(policy)) => {
                    Reservation::Settlement(policy.initial(instrument, leaves, price))
                }
                (Side::Buy, None) => Reservation::Settlement(mul_clamped(leaves, price, clipped)),
                (Side::Sell, Some(policy)) => {
                    let uncovered = leaves - self.net_position(&submit.symbol).max(Decimal::ZERO);
                    if uncovered <= Decimal::ZERO {
                        Reservation::None
                    } else {
                        Reservation::Settlement(policy.initial(instrument, uncovered, price))
                    }
                }
                (Side::Sell, None) => Reservation::None,
            };
        }
        match submit.side {
            // One order's `leaves * price` is representable (submit and modify
            // both `checked_mul` the full notional), but the SUM across all
            // resting buys is unbounded - saturate instead of letting `*`
            // panic.
            Side::Buy => Reservation::Settlement(mul_clamped(leaves, price, clipped)),
            Side::Sell => Reservation::Base(leaves),
        }
    }

    /// Sums the per-order reservations. Returns the locked totals plus the
    /// currencies whose accumulation clipped at the Decimal boundary, so the
    /// caller (`snapshot`, which holds `&mut self`) can warn once per key -
    /// this stays `&self` because it runs inside iteration over `self.open`.
    fn locked_balances(&self) -> (HashMap<String, Decimal>, Vec<String>) {
        let mut locked = self.order_locked.clone();
        // Sorted because the source is a `HashSet`: the only consumer is the
        // once-per-currency saturation warning, and an unordered log is a
        // needless nondeterminism in an otherwise deterministic venue.
        let mut clipped_currencies: Vec<_> = self.order_locked_clipped.iter().cloned().collect();
        clipped_currencies.sort();

        for ((symbol, _), position) in &self.account.positions {
            let Some(policy) = self.margin.get(symbol) else {
                continue;
            };
            let Some(instrument) = self.instruments.get(symbol) else {
                continue;
            };
            // Only a MARKED class posts maintenance collateral - futures,
            // perpetuals, inverses, and an equity whose margin policy makes it a
            // Reg-T margin account. The class is checked here for the same
            // reason `order_reservation` checks it: a margin policy attached to
            // a spot symbol must move no number.
            if !instrument.class.is_marked() {
                continue;
            }
            let currency = instrument.class.settlement_currency().to_owned();
            let reserve = policy.maintenance(instrument, position.qty, position.mark_px);
            let total = locked.entry(currency).or_default();
            *total = total.saturating_add(reserve);
        }

        // Unsettled sale proceeds, so the `locked` a client reads is the same
        // number `free_balance` subtracts. A wire snapshot that disagreed with
        // the venue's own funds check would be the worst of both.
        for credit in &self.account.unsettled {
            let total = locked.entry(credit.currency.clone()).or_default();
            *total = total.saturating_add(credit.amount);
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

    /// An inverse position cannot be settled at a non-positive price, so the
    /// mark is DECLINED and the position is left where it was. See
    /// [`Engine::settle`] for why a price like that is a caller defect rather
    /// than a market event.
    pub(crate) fn warn_unpriceable_settlement(&mut self, symbol: &Symbol) {
        if self
            .warned
            .unpriceable_settlement
            .insert(Symbol::clone(symbol))
        {
            tracing::warn!(
                %symbol,
                "settlement declined: an inverse contract has no value at a non-positive price, \
                 so the position keeps the mark it had"
            );
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
            mark_px: px,
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
            mark_px: current.mark_px,
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
            mark_px: current.mark_px,
        }
    } else if delta_abs == current_abs {
        PositionState {
            qty: Decimal::ZERO,
            avg_px: Decimal::ZERO,
            mark_px: Decimal::ZERO,
        }
    } else {
        PositionState {
            qty,
            avg_px: px,
            mark_px: current.mark_px,
        }
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
//
// The wrongness is STICKY, and the warning must not be read as a transient:
// once a value has clipped, a later opposite-direction fill moves off the
// clipped base rather than back toward the true one, so the ledger stays wrong
// for the rest of the run. The saturation is a loud failure mode, not a
// self-healing one.

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
