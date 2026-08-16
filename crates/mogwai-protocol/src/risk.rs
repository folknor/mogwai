// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The account policy a client trades under, and what the venue enforces.
//!
//! THIS IS A RISK-POLICY LAYER, NOT A PROP-FIRM FEATURE, and reading it the
//! other way builds the wrong thing. A live account has the same machinery: an
//! operator sets "if I lose 200 dollars today, allow no further positions", and
//! that behaves exactly like a liquidation except that it lifts at the next
//! session. A funded-account firm is that engine with stricter numbers and less
//! forgiving breach actions, so there is ONE mechanism here rather than two.
//!
//! It matters because a forward test against an account whose rules differ from
//! the deployed one tests a different account. A strategy that would have been
//! liquidated must actually be liquidated, or the claim is worth nothing.
//!
//! A RULE IS A TRIPLE: what it measures, on what basis, and WHAT IT DOES ON
//! BREACH. The breach action is the parameter that spans both worlds.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// What a breach does to the account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BreachAction {
    /// Flatten and refuse to open until the next session boundary, then resume
    /// with a fresh budget. A daily loss limit, at a live venue and at most
    /// firms.
    #[default]
    LockUntilReset,
    /// Flatten and end the account. The trailing-drawdown breach: there is no
    /// tomorrow.
    Terminate,
}

/// What the trailing threshold ratchets on.
///
/// The choice is the single largest difference between two accounts advertising
/// the same drawdown number, and it is what makes a day ending in profit still
/// cost budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TrailingBasis {
    /// Intraday PEAK EQUITY including unrealized. The harsh and common form: a
    /// spike that is touched and given back has still spent budget.
    #[default]
    PeakEquity,
    /// End-of-day BALANCE only. Much softer, because an intraday spike that is
    /// given back never counts toward the ratchet at all.
    EndOfDayBalance,
}

/// A trailing drawdown: a floor that follows the account up and never comes
/// back down.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrailingDrawdown {
    /// How far below the ratcheted high-water mark the floor sits.
    pub amount: Decimal,
    #[serde(default)]
    pub basis: TrailingBasis,
    /// Where the trail STOPS, if it stops: once the threshold reaches this
    /// equity it is locked there and no longer follows.
    ///
    /// Many firms trail only until the floor reaches the starting balance plus
    /// a buffer, then freeze it; others trail for the life of the account,
    /// which is `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_at_equity: Option<Decimal>,
    #[serde(default = "terminate")]
    pub on_breach: BreachAction,
}

fn terminate() -> BreachAction {
    BreachAction::Terminate
}

/// A daily loss limit: a non-ratcheting floor measured from the day's opening
/// equity, reset each session.
///
/// NOT derivable from the trailing drawdown and not the same mechanism. This
/// one forgets: crossing a session boundary restores the whole budget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DailyLossLimit {
    /// How far below the day's opening equity the floor sits.
    pub amount: Decimal,
    #[serde(default)]
    pub on_breach: BreachAction,
}

/// The rules an account is enforced under. Every field is optional, and an
/// account naming none is unpoliced - which is the default account's policy and
/// the behaviour every client had before this existed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountPolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trailing_drawdown: Option<TrailingDrawdown>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_loss_limit: Option<DailyLossLimit>,
    /// Minute of the UTC day at which the daily budget resets, if a daily limit
    /// is set.
    ///
    /// THE ACCOUNT DEFINES ITS DAY, not the instrument. A firm's reset is its
    /// own instant and is a property of the account rather than of whatever is
    /// being traded, so it does not come from the instrument calendar even
    /// though that carries a settlement minute and real open windows.
    ///
    /// The reset fires whenever SIM TIME crosses it, which needs no rule about
    /// loops: a one-session loop crosses it once per loop, a multi-day loop as
    /// often as it contains it. THE EDGE THAT DOES NOT RESOLVE ITSELF is a
    /// footprint that never contains the instant at all - an Asia-only loop
    /// under a 22:00 UTC reset never crosses it, so the budget never resets and
    /// a daily limit silently becomes a run-lifetime limit.
    #[serde(default = "default_reset_minute")]
    pub reset_minute_utc: u32,
}

/// 22:00 UTC, which is 17:00 US Eastern in summer - the reset most funded
/// accounts advertise. A convention, not a measurement.
fn default_reset_minute() -> u32 {
    22 * 60
}

impl AccountPolicy {
    /// Whether this policy asks the venue to enforce anything at all.
    pub fn is_unpoliced(&self) -> bool {
        self.trailing_drawdown.is_none() && self.daily_loss_limit.is_none()
    }

    /// `Err` naming the first unusable field. Validated where the policy enters
    /// the venue, so a nonsense rule is a refused request rather than an
    /// account that behaves strangely hours later.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(trailing) = &self.trailing_drawdown
            && trailing.amount <= Decimal::ZERO
        {
            return Err("trailing_drawdown.amount must be positive".to_owned());
        }
        if let Some(daily) = &self.daily_loss_limit
            && daily.amount <= Decimal::ZERO
        {
            return Err("daily_loss_limit.amount must be positive".to_owned());
        }
        if self.reset_minute_utc >= 24 * 60 {
            return Err("reset_minute_utc must be a minute of the day, 0 to 1439".to_owned());
        }
        Ok(())
    }
}

/// What the venue is enforcing against this account right now, published so the
/// run can be JUDGED afterwards.
///
/// The audience is the EVALUATOR, not the strategy. A real trader reads its
/// remaining drawdown off the firm's dashboard; mogwai presents no dashboard,
/// so if these numbers are not on the wire nobody can tell a run that ended flat
/// having spent 90 percent of its budget from one that never came close, and
/// the two are indistinguishable from fills alone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskState {
    /// Equity at the last evaluation: balances plus unrealized.
    pub equity: Decimal,
    /// The high-water mark the trailing threshold has ratcheted on.
    pub peak_equity: Decimal,
    /// Equity at the current day's open, which the daily limit measures from.
    pub day_open_equity: Decimal,
    /// The trailing floor, if a trailing drawdown is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trailing_threshold: Option<Decimal>,
    /// How far equity may fall before the trailing floor is breached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trailing_remaining: Option<Decimal>,
    /// How much more may be lost today before the daily limit is breached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_remaining: Option<Decimal>,
    /// Set once a rule has fired.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub breached: Option<Breach>,
}

/// A rule that fired, and what it did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Breach {
    pub rule: BreachedRule,
    pub action: BreachAction,
    /// Sim instant the rule fired.
    pub ts_event: u64,
    /// Equity at the moment it fired, and the floor it crossed.
    pub equity: Decimal,
    pub threshold: Decimal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BreachedRule {
    TrailingDrawdown,
    DailyLossLimit,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_policy_naming_no_rule_is_unpoliced() {
        assert!(AccountPolicy::default().is_unpoliced());
    }

    #[test]
    fn a_nonpositive_drawdown_is_refused() {
        let policy = AccountPolicy {
            trailing_drawdown: Some(TrailingDrawdown {
                amount: Decimal::ZERO,
                basis: TrailingBasis::PeakEquity,
                lock_at_equity: None,
                on_breach: BreachAction::Terminate,
            }),
            ..AccountPolicy::default()
        };
        assert!(policy.validate().is_err());
    }

    #[test]
    fn a_reset_minute_outside_the_day_is_refused() {
        let policy = AccountPolicy {
            reset_minute_utc: 1_440,
            ..AccountPolicy::default()
        };
        assert!(policy.validate().is_err());
    }

    /// The default reset is a convention and the default breach actions differ
    /// per rule, both of which a client inherits by saying nothing - so they are
    /// pinned rather than left to a reader of the derive.
    #[test]
    fn the_defaults_a_client_inherits_are_the_documented_ones() {
        let policy: AccountPolicy = serde_json::from_str(
            r#"{"trailing_drawdown":{"amount":"2000"},"daily_loss_limit":{"amount":"500"}}"#,
        )
        .expect("a minimal policy deserializes");
        assert_eq!(policy.reset_minute_utc, 22 * 60);
        assert_eq!(
            policy.trailing_drawdown.expect("set").on_breach,
            BreachAction::Terminate,
            "a trailing-drawdown breach ends the account: there is no tomorrow"
        );
        assert_eq!(
            policy.daily_loss_limit.expect("set").on_breach,
            BreachAction::LockUntilReset,
            "a daily limit lifts at the next session"
        );
    }
}
