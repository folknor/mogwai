// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Enforcing one account's policy against its own equity.
//!
//! The venue ENFORCES rather than reports: a strategy that would have been
//! liquidated must actually be liquidated, or the forward claim is worth
//! nothing. What is published alongside is for the EVALUATOR - mogwai presents
//! no dashboard, so numbers that are not on the wire cannot be judged after the
//! run.
//!
//! EVALUATED AT TICK RESOLUTION, which is the design ruling: peak equity tracks
//! every tick, because at a real venue the ratchet is effectively tick-by-tick
//! and a spike lasting 200 ms still spends budget. This ran on the fill
//! sweeper's mark cadence until 2026-08-16, so a spike entirely between two
//! marks was invisible and the account kept room it should have lost.
//!
//! It is NOT closed by evaluating per tick. Equity is LINEAR in the price of the
//! one instrument an account can be holding - an account is on at most one
//! river, and strategies are single-instrument - so its extreme over a span is
//! attained at a PRICE extreme. The tape thread records the high and the low it
//! reached (`crate::extremes`), and the sweeper replays those two readings in
//! the order the tape reached them before observing the close. Two valuations
//! per pass answer what thousands would have, the engine lock is still taken
//! once, and the tape thread's whole contribution is two comparisons per tick.
//!
//! THE TIME ORDER IS WHAT KEEPS IT HONEST. A span that spiked and then fell is a
//! different run from one that fell and then spiked: in the first the ratchet
//! raises the floor before the fall tests it. Replaying favourable-first would
//! invent breaches that never happened, which is the opposite error to the
//! leniency being removed.

use mogwai_protocol::risk::{
    AccountPolicy, Breach, BreachAction, BreachedRule, RiskState, TrailingBasis,
};
use rust_decimal::Decimal;

/// Nanoseconds in one minute, for the reset-boundary arithmetic.
const NANOS_PER_MINUTE: u64 = 60 * 1_000_000_000;

/// The evolving half of an account's risk position: what the policy needs to
/// remember between evaluations.
#[derive(Debug)]
pub(crate) struct RiskLedger {
    policy: AccountPolicy,
    /// Ratcheted high-water mark. Starts at the opening equity, so an account
    /// that only ever loses still has a meaningful floor.
    peak_equity: Decimal,
    day_open_equity: Decimal,
    /// Which reset period `day_open_equity` was taken in, so a crossing is
    /// detected without keeping a wall clock.
    day_index: u64,
    /// Frozen once a rule fires. A breached account is not re-evaluated: the
    /// first breach is the one that describes the run.
    breach: Option<Breach>,
    /// Set by a `LockUntilReset` breach and cleared at the next boundary.
    locked: bool,
}

/// What the caller must do about an evaluation.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Nothing to do.
    Clear,
    /// A rule fired. Flatten, then apply the action.
    Breached(Breach),
}

impl RiskLedger {
    pub(crate) fn new(policy: AccountPolicy, opening_equity: Decimal, now_ns: u64) -> Self {
        Self {
            peak_equity: opening_equity,
            day_open_equity: opening_equity,
            day_index: day_index(now_ns, policy.reset_minute_utc),
            policy,
            breach: None,
            locked: false,
        }
    }

    /// The currency this policy's thresholds are stated in, if it polices
    /// anything.
    pub(crate) fn currency(&self) -> Option<&str> {
        self.policy.currency.as_deref()
    }

    /// Whether the account may open. A locked account is flat and stays flat
    /// until its next reset; a terminated one never opens again.
    pub(crate) fn is_locked(&self) -> bool {
        self.locked
            || matches!(&self.breach, Some(breach) if breach.action == BreachAction::Terminate)
    }

    /// Fold one equity reading in, and say what it costs.
    ///
    /// ORDER MATTERS HERE. The day boundary is crossed FIRST, so a reading in a
    /// new period is measured against that period's opening equity rather than
    /// yesterday's. The ratchet is applied SECOND, before the thresholds are
    /// compared, because a reading that sets a new peak cannot simultaneously
    /// breach the floor that peak just raised.
    pub(crate) fn observe(&mut self, equity: Decimal, now_ns: u64) -> Verdict {
        if self.breach.is_some() {
            return Verdict::Clear;
        }
        let today = day_index(now_ns, self.policy.reset_minute_utc);
        if today != self.day_index {
            self.day_index = today;
            self.day_open_equity = equity;
            // A daily limit that locked the account lifts here, which is the
            // whole difference between it and a terminating rule.
            self.locked = false;
            if let Some(trailing) = &self.policy.trailing_drawdown
                && trailing.basis == TrailingBasis::EndOfDayBalance
            {
                // The soft basis ratchets ONLY here, on the period's closing
                // equity, so an intraday spike given back never counts.
                self.peak_equity = self.peak_equity.max(equity);
            }
        }
        if let Some(trailing) = &self.policy.trailing_drawdown
            && trailing.basis == TrailingBasis::PeakEquity
        {
            self.peak_equity = self.peak_equity.max(equity);
        }

        if let Some(trailing) = &self.policy.trailing_drawdown {
            let threshold = self.trailing_threshold(trailing.amount, trailing.lock_at_equity);
            if equity < threshold {
                return self.fire(
                    BreachedRule::TrailingDrawdown,
                    trailing.on_breach,
                    equity,
                    threshold,
                    now_ns,
                );
            }
        }
        if let Some(daily) = &self.policy.daily_loss_limit {
            let threshold = self.day_open_equity - daily.amount;
            if equity < threshold {
                return self.fire(
                    BreachedRule::DailyLossLimit,
                    daily.on_breach,
                    equity,
                    threshold,
                    now_ns,
                );
            }
        }
        Verdict::Clear
    }

    /// The trailing floor: the ratcheted peak less the allowance, except that
    /// a `lock_at_equity` freezes it once it reaches that level.
    fn trailing_threshold(&self, amount: Decimal, lock_at: Option<Decimal>) -> Decimal {
        let trailed = self.peak_equity - amount;
        match lock_at {
            Some(locked) if trailed >= locked => locked,
            _ => trailed,
        }
    }

    fn fire(
        &mut self,
        rule: BreachedRule,
        action: BreachAction,
        equity: Decimal,
        threshold: Decimal,
        now_ns: u64,
    ) -> Verdict {
        let breach = Breach {
            rule,
            action,
            ts_event: now_ns,
            equity,
            threshold,
        };
        self.locked = true;
        // A LOCK is remembered as a lock and NOT as a terminal breach, so the
        // next reset can lift it. Only a terminating rule is recorded as the
        // breach that describes the run.
        if action == BreachAction::Terminate {
            self.breach = Some(breach.clone());
        }
        Verdict::Breached(breach)
    }

    /// What to publish. Computed rather than stored, so it can never disagree
    /// with the thresholds the enforcement just used.
    pub(crate) fn state(&self, equity: Decimal) -> RiskState {
        let trailing = self
            .policy
            .trailing_drawdown
            .as_ref()
            .map(|trailing| self.trailing_threshold(trailing.amount, trailing.lock_at_equity));
        RiskState {
            equity,
            peak_equity: self.peak_equity,
            day_open_equity: self.day_open_equity,
            trailing_threshold: trailing,
            trailing_remaining: trailing.map(|threshold| equity - threshold),
            daily_remaining: self
                .policy
                .daily_loss_limit
                .as_ref()
                .map(|daily| equity - (self.day_open_equity - daily.amount)),
            breached: self.breach.clone(),
        }
    }
}

/// Account equity IN ONE CURRENCY, answered by the engine because only it knows
/// which instrument prices which asset.
///
/// ONE CURRENCY AND NEVER A SUM ACROSS THEM. An earlier version summed every
/// balance total, which silently valued one unit of any asset at one unit of
/// any other - and that is not an exotic case, it is the DEFAULT one: a spot
/// fill credits the base asset as a currency balance, so buying one BTC at
/// 60,000 leaves `BTC: 1` beside `USDT: -60,000` and the sum read a 59,999 loss
/// on a trade that changed nothing.
///
/// `None` when the account holds something nothing prices in `currency`, which
/// is the caller's signal to refuse rather than to enforce against a wrong
/// number.
pub(crate) fn equity_in(engine: &mogwai_engine::Engine, currency: &str) -> Option<Decimal> {
    engine.valuation_in(currency)
}

/// Which reset period a sim instant falls in.
///
/// The period is a whole day offset by the account's reset minute, so crossing
/// the minute increments the index whatever the calendar underneath is doing.
/// That is what lets the daily budget reset without a rule about loops.
fn day_index(now_ns: u64, reset_minute_utc: u32) -> u64 {
    let offset = u64::from(reset_minute_utc) * NANOS_PER_MINUTE;
    now_ns.saturating_sub(offset) / (24 * 60 * NANOS_PER_MINUTE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mogwai_protocol::risk::{DailyLossLimit, TrailingDrawdown};

    const DAY: u64 = 24 * 60 * NANOS_PER_MINUTE;

    fn trailing(amount: u64, basis: TrailingBasis) -> AccountPolicy {
        AccountPolicy {
            trailing_drawdown: Some(TrailingDrawdown {
                amount: Decimal::from(amount),
                basis,
                lock_at_equity: None,
                on_breach: BreachAction::Terminate,
            }),
            daily_loss_limit: None,
            reset_minute_utc: 0,
            currency: Some("USD".to_owned()),
        }
    }

    /// The case that motivated the whole feature: a day ending 3,000 up having
    /// SPENT 700 of drawdown budget, because the floor ratcheted on a peak that
    /// was touched and not held.
    #[test]
    fn a_peak_that_is_given_back_still_spends_budget() {
        let mut ledger =
            RiskLedger::new(trailing(2_000, TrailingBasis::PeakEquity), 50_000.into(), 0);
        assert_eq!(ledger.observe(53_700.into(), 1), Verdict::Clear);
        let state = ledger.state(53_000.into());
        assert_eq!(
            state.trailing_threshold,
            Some(51_700.into()),
            "the floor ratcheted to the peak less the allowance"
        );
        assert_eq!(
            state.trailing_remaining,
            Some(1_300.into()),
            "up 3,000 on the day and 700 of the 2,000 budget is gone"
        );
    }

    /// The soft basis is the reason two accounts advertising the same number
    /// are different accounts: the same intraday spike costs nothing.
    #[test]
    fn an_end_of_day_basis_ignores_the_intraday_spike() {
        let mut ledger = RiskLedger::new(
            trailing(2_000, TrailingBasis::EndOfDayBalance),
            50_000.into(),
            0,
        );
        assert_eq!(ledger.observe(53_700.into(), 1), Verdict::Clear);
        assert_eq!(
            ledger.state(53_000.into()).trailing_threshold,
            Some(48_000.into()),
            "an intraday peak does not ratchet an end-of-day trail"
        );
    }

    #[test]
    fn a_trailing_breach_terminates() {
        let mut ledger =
            RiskLedger::new(trailing(2_000, TrailingBasis::PeakEquity), 50_000.into(), 0);
        let Verdict::Breached(breach) = ledger.observe(47_999.into(), 1) else {
            panic!("crossing the floor breaches");
        };
        assert_eq!(breach.rule, BreachedRule::TrailingDrawdown);
        assert_eq!(breach.action, BreachAction::Terminate);
        assert!(ledger.is_locked(), "a terminated account never opens again");
    }

    /// A locked account is not a dead one, which is the whole difference
    /// between the two breach actions.
    #[test]
    fn a_daily_lock_lifts_at_the_next_reset() {
        let policy = AccountPolicy {
            trailing_drawdown: None,
            daily_loss_limit: Some(DailyLossLimit {
                amount: Decimal::from(500),
                on_breach: BreachAction::LockUntilReset,
            }),
            reset_minute_utc: 0,
            currency: Some("USD".to_owned()),
        };
        let mut ledger = RiskLedger::new(policy, 50_000.into(), 0);
        let Verdict::Breached(breach) = ledger.observe(49_499.into(), 1) else {
            panic!("crossing the daily floor breaches");
        };
        assert_eq!(breach.rule, BreachedRule::DailyLossLimit);
        assert!(ledger.is_locked());

        assert_eq!(ledger.observe(49_499.into(), DAY + 1), Verdict::Clear);
        assert!(
            !ledger.is_locked(),
            "the next session restores the daily budget"
        );
        assert_eq!(
            ledger.state(49_499.into()).day_open_equity,
            49_499.into(),
            "the new day measures from where the account actually opened it"
        );
    }

    /// A firm that trails only to breakeven stops there rather than following
    /// the account up forever.
    #[test]
    fn a_locked_trail_stops_following() {
        let policy = AccountPolicy {
            trailing_drawdown: Some(TrailingDrawdown {
                amount: Decimal::from(2_000),
                basis: TrailingBasis::PeakEquity,
                lock_at_equity: Some(Decimal::from(50_000)),
                on_breach: BreachAction::Terminate,
            }),
            daily_loss_limit: None,
            reset_minute_utc: 0,
            currency: Some("USD".to_owned()),
        };
        let mut ledger = RiskLedger::new(policy, 50_000.into(), 0);
        assert_eq!(ledger.observe(60_000.into(), 1), Verdict::Clear);
        assert_eq!(
            ledger.state(60_000.into()).trailing_threshold,
            Some(50_000.into()),
            "the trail froze at its lock level instead of reaching 58,000"
        );
    }

    #[test]
    fn an_unpoliced_account_never_breaches() {
        let mut ledger = RiskLedger::new(AccountPolicy::default(), 50_000.into(), 0);
        assert_eq!(ledger.observe(Decimal::ZERO, 1), Verdict::Clear);
        assert!(!ledger.is_locked());
    }
}
