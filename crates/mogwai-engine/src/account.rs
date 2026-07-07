//! The account ledger: balances, positions, locked reservations, and the
//! `AccountState` snapshot. Includes the saturating arithmetic that keeps
//! unbounded cross-fill accumulation from panicking the engine.

use std::collections::{HashMap, HashSet};

use mogwai_protocol::{AccountState, Balance, OrderFilled, Side, Symbol};
use rust_decimal::Decimal;

use crate::Engine;

#[derive(Debug, Default)]
pub(crate) struct Account {
    pub(crate) balances: HashMap<String, Decimal>,
    pub(crate) positions: HashMap<Symbol, PositionState>,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct PositionState {
    pub(crate) qty: Decimal,
    pub(crate) avg_px: Decimal,
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
}

impl Engine {
    pub(crate) fn apply_fill(&mut self, fill: &OrderFilled) {
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
