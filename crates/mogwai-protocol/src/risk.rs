// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The account policy a consumer trades under, and what the venue enforces.
//!
//! This is a risk-policy layer, not a prop-firm feature, and reading it the
//! other way builds the wrong thing. A live account has the same machinery: an
//! operator sets "if I lose 200 dollars today, allow no further positions", and
//! that behaves exactly like a liquidation except that it lifts at the next
//! session. A funded-account firm is that engine with stricter numbers and less
//! forgiving breach actions, so there is one mechanism here rather than two.
//!
//! It matters because a forward test against an account whose rules differ from
//! the deployed one tests a different account. A strategy that would have been
//! liquidated must actually be liquidated, or the claim is worth nothing.
//!
//! A rule is a triple: what it measures, on what basis, and what it does on
//! breach. The breach action is the parameter that spans both worlds.
//!
//! The margin ledger's own `breach_action = "liquidate"` is a third instance of
//! that same triple, reached by a parallel mechanism with its own arithmetic.
//! Reconciling the two liquidation paths means expressing a margin breach as
//! one more rule here rather than keeping the second mechanism alongside.

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
    /// Intraday peak equity including unrealized. The harsh and common form: a
    /// spike that is touched and given back has still spent budget.
    #[default]
    PeakEquity,
    /// End-of-day balance only. Much softer, because an intraday spike that is
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
    /// Where the trail stops, if it stops: once the threshold reaches this
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
/// Not derivable from the trailing drawdown and not the same mechanism. This
/// one forgets: crossing a session boundary restores the whole budget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DailyLossLimit {
    /// How far below the day's opening equity the floor sits.
    pub amount: Decimal,
    #[serde(default)]
    pub on_breach: BreachAction,
}

/// A static overall drawdown: a floor measured from opening equity that never
/// ratchets and never resets.
///
/// This is the other common funded-account form. A trailing rule follows the
/// account up; this one does not. Two accounts advertising "10 percent
/// drawdown" are different experiments if one trails and one does not, and a
/// forward test that only models the trail tests the wrong account for the
/// static programme.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverallDrawdown {
    /// How far below the opening equity the floor sits.
    pub amount: Decimal,
    #[serde(default = "terminate")]
    pub on_breach: BreachAction,
}

/// A hard cap on how large a position this account may carry, in the
/// instrument's own size unit.
///
/// One scalar, applied independently to every symbol the account trades: it is
/// a contract count on a future, a share count on an equity, a base-unit size
/// on a perp. The venue refuses an order that would put that symbol's book over
/// the cap - the largest |qty| it can reach given worst-case fill order of the
/// working book, reduce-only excluded - rather than flattening after the fact.
/// Under netting that is the worse extreme net; under hedging, the larger
/// side. Sizing past the firm is a consumer error, not a liquidation.
///
/// Two things follow from it being one number rather than a table, and both are
/// deliberate. An account trading several symbols may hold the cap in each at
/// once, because nothing here bounds the aggregate. And the number is read in
/// each symbol's own size unit, so ten is ten contracts on one and ten coins on
/// the next; a cap meant for one instrument is worth checking against every
/// instrument the account will touch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaxPosition {
    pub quantity: Decimal,
}

/// The rules an account is enforced under. Every field is optional, and an
/// account naming none is unpoliced - which is the default account's policy and
/// the behaviour every consumer had before this existed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountPolicy {
    /// Opening funding carried by a funded-account programme. An account
    /// request's explicit balances take precedence; these are used when that
    /// request omits balances.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub opening_balances: std::collections::HashMap<String, Decimal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trailing_drawdown: Option<TrailingDrawdown>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_loss_limit: Option<DailyLossLimit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overall_drawdown: Option<OverallDrawdown>,
    /// Refused at entry rather than flattened after the fact. See `MaxPosition`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_position: Option<MaxPosition>,
    /// Minute of the UTC day at which the daily budget resets, if a daily limit
    /// is set.
    ///
    /// The account defines its day, not the instrument. A firm's reset is its
    /// own instant and is a property of the account rather than of whatever is
    /// being traded, so it does not come from the instrument calendar even
    /// though that carries a settlement minute and real open windows.
    ///
    /// The reset fires whenever sim time crosses it, which needs no rule about
    /// loops: a one-session loop crosses it once per loop, a multi-day loop as
    /// often as it contains it. The edge that does not resolve itself is a
    /// footprint that never contains the instant at all - an Asia-only loop
    /// under a 22:00 UTC reset never crosses it, so the budget never resets and
    /// a daily limit silently becomes a run-lifetime limit.
    #[serde(default = "default_reset_minute")]
    pub reset_minute_utc: u32,
    /// The currency every threshold in this policy is stated in, and the only
    /// currency a policed account may hold.
    ///
    /// Required whenever a rule is set, because a threshold has no meaning
    /// without one. Equity is computed in this currency alone: the venue sums
    /// the balance and the unrealized on positions settling in it, and has no
    /// exchange rate for anything else.
    ///
    /// The consequence is that a policed account trades one settlement
    /// currency, which today means futures. A spot fill credits the base asset
    /// as a currency balance and debits the quote - buy one BTC at 60,000 and
    /// the ledger holds `BTC: 1` beside `USDT: -60,000` - so a spot account
    /// holds two currencies from its first fill, and the venue would have to
    /// value the base to state its equity. It cannot: `Engine::mark` refreshes
    /// only futures positions, so a spot position's mark is never live, and
    /// inventing a rate would make every threshold mean something nobody
    /// stated. An order that would open a second currency is therefore refused
    /// at entry, by name, rather than silently mis-valued afterwards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
}

/// 22:00 UTC, which is 17:00 US Eastern in summer - the reset most funded
/// accounts advertise. A convention, not a measurement.
fn default_reset_minute() -> u32 {
    22 * 60
}

impl AccountPolicy {
    /// Whether this policy asks the venue to enforce anything at all.
    pub fn is_unpoliced(&self) -> bool {
        self.trailing_drawdown.is_none()
            && self.daily_loss_limit.is_none()
            && self.overall_drawdown.is_none()
            && self.max_position.is_none()
    }

    /// `Err` naming the first unusable field. Validated where the policy enters
    /// the venue, so a nonsense rule is a refused request rather than an
    /// account that behaves strangely hours later.
    pub fn validate(&self) -> Result<(), String> {
        for (currency, amount) in &self.opening_balances {
            crate::validate_currency_code(currency)
                .map_err(|why| format!("opening_balances.{currency}: {why}"))?;
            if *amount <= Decimal::ZERO {
                return Err(format!("opening_balances.{currency} must be positive"));
            }
        }
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
        if let Some(overall) = &self.overall_drawdown
            && overall.amount <= Decimal::ZERO
        {
            return Err("overall_drawdown.amount must be positive".to_owned());
        }
        if let Some(max_position) = &self.max_position
            && max_position.quantity <= Decimal::ZERO
        {
            return Err("max_position.quantity must be positive".to_owned());
        }
        if self.reset_minute_utc >= 24 * 60 {
            return Err("reset_minute_utc must be a minute of the day, 0 to 1439".to_owned());
        }
        // Blank means trims-to-empty, matching the divergence validators in
        // `havoc.rs`, which refuse a `client_order_id` on `trim().is_empty()`.
        // A currency is a lookup key: equity is summed over the balances
        // carrying exactly this code, so a whitespace code names no currency
        // any balance can ever match and would freeze equity at zero forever
        // rather than refuse the policy at registration.
        if !self.is_unpoliced() && self.currency.as_ref().is_none_or(|c| c.trim().is_empty()) {
            return Err(
                "a policy with any rule must name the currency its thresholds are stated in; \
                 equity is computed in that currency alone and the venue has no exchange rate"
                    .to_owned(),
            );
        }
        // Naming one is not the same as naming a usable one. A padded code is
        // the same defect as a blank one wearing a plausible value: it is a
        // lookup key against balance currencies, `" USD "` equals no balance
        // funded as `USD`, and the account is then policed on an equity that
        // is permanently zero. Refused here, at registration, rather than
        // discovered from a liquidation that had no cause.
        if let Some(currency) = &self.currency {
            crate::validate_currency_code(currency)?;
        }
        // Opening funding in a currency the rules cannot see is refused, not
        // converted. Equity is summed over the policy currency alone and the
        // venue owns no rate surface, so a policy anchored at 50,000 USD plus
        // 50,000 EUR would open at 100,000 and read its first honest
        // observation as a 50,000 loss - a liquidation with no cause, before
        // the account has traded. There is no parity shortcut available: see
        // the one-hop limit recorded against account valuation.
        if !self.is_unpoliced()
            && let Some(policy_currency) = &self.currency
            && let Some(other) = self
                .opening_balances
                .keys()
                .find(|currency| *currency != policy_currency)
        {
            return Err(format!(
                "opening_balances.{other} is not the policy currency {policy_currency}; a policed \
                 account may open only in the currency its thresholds are stated in, because \
                 equity is computed in that currency alone and the venue has no exchange rate"
            ));
        }
        Ok(())
    }
}

/// What the venue is enforcing against this account right now, published so the
/// run can be judged afterwards.
///
/// The audience is the evaluator, not the strategy. A real trader reads its
/// remaining drawdown off the firm's dashboard; mogwai presents no dashboard,
/// so if these numbers are not on the wire nobody can tell a run that ended flat
/// having spent 90 percent of its budget from one that never came close, and
/// the two are indistinguishable from fills alone.
///
/// Every decimal here is string-spelled on the wire, and a JSON number is
/// refused, for the same reason the execution and market-data frames in
/// `messages` are: these are money quantities, and a bare number decodes
/// through `f64`. This type is output-only in-tree - the venue builds it and
/// publishes it on `GET /account`, nothing decodes it here - but it derives
/// `Deserialize` for consumers, and a consumer's decoder is exactly where the
/// tolerance would have bitten unobserved. It is a deliberate inclusion, not
/// an oversight: [`AccountPolicy`] beside it stays number-tolerant because a
/// policy is also TOML config, while a published state is only ever wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskState {
    /// Equity at the last evaluation: balances plus unrealized.
    #[serde(with = "rust_decimal::serde::str")]
    pub equity: Decimal,
    /// The high-water mark the trailing threshold has ratcheted on.
    #[serde(with = "rust_decimal::serde::str")]
    pub peak_equity: Decimal,
    /// Equity at the current day's open, which the daily limit measures from.
    #[serde(with = "rust_decimal::serde::str")]
    pub day_open_equity: Decimal,
    /// The trailing floor, if a trailing drawdown is set.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::decimal::str_option"
    )]
    pub trailing_threshold: Option<Decimal>,
    /// How far equity may fall before the trailing floor is breached.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::decimal::str_option"
    )]
    pub trailing_remaining: Option<Decimal>,
    /// How much more may be lost today before the daily limit is breached.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::decimal::str_option"
    )]
    pub daily_remaining: Option<Decimal>,
    /// The static overall floor, if an overall drawdown is set.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::decimal::str_option"
    )]
    pub overall_threshold: Option<Decimal>,
    /// How far equity may fall before the overall floor is breached.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::decimal::str_option"
    )]
    pub overall_remaining: Option<Decimal>,
    /// The position cap, if one is set. Published so the evaluator can see
    /// the size the run was allowed, which fills alone cannot reconstruct.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::decimal::str_option"
    )]
    pub max_position: Option<Decimal>,
    /// Set once a rule has fired.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub breached: Option<Breach>,
}

/// A rule that fired, and what it did. Published inside [`RiskState`] and
/// string-spelled on the same grounds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Breach {
    pub rule: BreachedRule,
    pub action: BreachAction,
    /// Sim instant the rule fired.
    pub ts_event: u64,
    /// Equity at the moment it fired, and the floor it crossed.
    #[serde(with = "rust_decimal::serde::str")]
    pub equity: Decimal,
    #[serde(with = "rust_decimal::serde::str")]
    pub threshold: Decimal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BreachedRule {
    TrailingDrawdown,
    DailyLossLimit,
    OverallDrawdown,
}

/// The names this build ships a policy under.
///
/// Authoritative, not a parallel listing: [`shipped_policy`] refuses any name
/// that is not in here before it reaches its match, so the two cannot disagree
/// about what ships. Nothing can enumerate a `match`'s arms, so the direction
/// that used to be unpinned - an arm added here but forgotten in the list -
/// is closed structurally instead of by a test: such an arm is simply
/// unreachable, and the list stays the one place a name is announced.
pub const SHIPPED_POLICIES: &[&str] = &[
    "intraday-trail",
    "eod-trail",
    "daily-limit-only",
    "static-drawdown",
    "intraday-trail-sized",
];

/// A policy this build ships, by name.
///
/// Illustrative rather than authoritative as to terms - a different axis
/// from the one [`SHIPPED_POLICIES`] above calls authoritative, which is the
/// set of names. The list settles which names exist; this doc disclaims that
/// the numbers behind them describe any real programme. Both hold at once.
///
/// Deliberately few, too. These show the
/// shapes a funded-account programme comes in - a hard intraday trail, a softer
/// end-of-day trail that stops at breakeven, a daily limit that locks rather
/// than kills - so an operator has something to copy and a test something to
/// name. They do not track any real firm's current terms and must not be read
/// as doing so: those change without notice, which is exactly why registration
/// is a runtime path and why a name an operator registers shadows a shipped one.
///
/// Built in code rather than parsed from embedded text, so this crate needs no
/// TOML dependency to state a handful of constants.
#[must_use]
pub fn shipped_policy(name: &str) -> Option<AccountPolicy> {
    // `SHIPPED_POLICIES` is the announcement; this match is the construction.
    // Gating on the list makes a name that is not announced unresolvable, so
    // the pair cannot drift in the direction no test can see.
    if !SHIPPED_POLICIES.contains(&name) {
        return None;
    }
    let usd = || Some("USD".to_owned());
    match name {
        "intraday-trail" => Some(AccountPolicy {
            opening_balances: Default::default(),
            currency: usd(),
            trailing_drawdown: Some(TrailingDrawdown {
                amount: Decimal::from(2_000),
                basis: TrailingBasis::PeakEquity,
                lock_at_equity: None,
                on_breach: BreachAction::Terminate,
            }),
            daily_loss_limit: Some(DailyLossLimit {
                amount: Decimal::from(1_000),
                on_breach: BreachAction::LockUntilReset,
            }),
            overall_drawdown: None,
            max_position: None,
            reset_minute_utc: default_reset_minute(),
        }),
        "eod-trail" => Some(AccountPolicy {
            opening_balances: Default::default(),
            currency: usd(),
            trailing_drawdown: Some(TrailingDrawdown {
                amount: Decimal::from(2_000),
                basis: TrailingBasis::EndOfDayBalance,
                lock_at_equity: Some(Decimal::from(50_000)),
                on_breach: BreachAction::Terminate,
            }),
            daily_loss_limit: None,
            overall_drawdown: None,
            max_position: None,
            reset_minute_utc: default_reset_minute(),
        }),
        "daily-limit-only" => Some(AccountPolicy {
            opening_balances: Default::default(),
            currency: usd(),
            trailing_drawdown: None,
            daily_loss_limit: Some(DailyLossLimit {
                amount: Decimal::from(500),
                on_breach: BreachAction::LockUntilReset,
            }),
            overall_drawdown: None,
            max_position: None,
            reset_minute_utc: default_reset_minute(),
        }),
        // The static form: a floor measured from opening equity that never
        // follows the account up. 5,000 off a 50k start is the 10 percent
        // overall many forex programmes advertise, with a 2,500 daily lock.
        "static-drawdown" => Some(AccountPolicy {
            opening_balances: Default::default(),
            currency: usd(),
            trailing_drawdown: None,
            daily_loss_limit: Some(DailyLossLimit {
                amount: Decimal::from(2_500),
                on_breach: BreachAction::LockUntilReset,
            }),
            overall_drawdown: Some(OverallDrawdown {
                amount: Decimal::from(5_000),
                on_breach: BreachAction::Terminate,
            }),
            max_position: None,
            reset_minute_utc: default_reset_minute(),
        }),
        // The sized form: the hard intraday trail plus a 10-contract cap, which
        // is the typical 50k micros evaluation size. Without the cap a
        // strategy can pass the dollar rules at a size no firm would have
        // let it carry.
        "intraday-trail-sized" => Some(AccountPolicy {
            opening_balances: Default::default(),
            currency: usd(),
            trailing_drawdown: Some(TrailingDrawdown {
                amount: Decimal::from(2_000),
                basis: TrailingBasis::PeakEquity,
                lock_at_equity: None,
                on_breach: BreachAction::Terminate,
            }),
            daily_loss_limit: Some(DailyLossLimit {
                amount: Decimal::from(1_000),
                on_breach: BreachAction::LockUntilReset,
            }),
            overall_drawdown: None,
            max_position: Some(MaxPosition {
                quantity: Decimal::from(10),
            }),
            reset_minute_utc: default_reset_minute(),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_policy_naming_no_rule_is_unpoliced() {
        assert!(AccountPolicy::default().is_unpoliced());
    }

    /// `RiskState` is on the wire - `GET /account` publishes it as
    /// `AccountSnapshot.risk` - so it takes the same string-only decimal rule
    /// the execution and market-data frames do, and this is the row that says
    /// the inclusion was decided rather than forgotten.
    ///
    /// It is output-only in this workspace, which is exactly why the tolerance
    /// needed pinning here: no in-tree decode would ever have exercised it, and
    /// a consumer's would.
    #[test]
    fn a_published_risk_state_refuses_a_numeric_decimal() {
        let string_spelled = r#"{"equity":"1000.5","peak_equity":"1200","day_open_equity":"1100","trailing_remaining":"50.25","breached":{"rule":"daily_loss_limit","action":"lock_until_reset","ts_event":7,"equity":"900","threshold":"950"}}"#;
        let state: RiskState =
            serde_json::from_str(string_spelled).expect("the string spelling must decode");
        assert_eq!(state.equity, Decimal::new(10_005, 1));
        assert_eq!(state.trailing_remaining, Some(Decimal::new(5025, 2)));
        assert_eq!(
            state.breached.as_ref().map(|breach| breach.threshold),
            Some(Decimal::from(950))
        );

        // Every decimal in the frame, required and optional, on the state and
        // on the nested breach. Each is unquoted in turn so a refusal can only
        // be about that field.
        for field in [
            "equity",
            "peak_equity",
            "day_open_equity",
            "trailing_remaining",
            "threshold",
        ] {
            let needle = format!("\"{field}\":\"");
            let at = string_spelled
                .find(&needle)
                .unwrap_or_else(|| panic!("{field} is not spelled as a string"));
            let value_start = at + needle.len();
            let value_end = value_start
                + string_spelled[value_start..]
                    .find('"')
                    .expect("the value's closing quote");
            let mut numeric = String::with_capacity(string_spelled.len());
            numeric.push_str(&string_spelled[..value_start - 1]);
            numeric.push_str(&string_spelled[value_start..value_end]);
            numeric.push_str(&string_spelled[value_end + 1..]);
            assert!(
                serde_json::from_str::<RiskState>(&numeric).is_err(),
                "{field}: a numeric spelling must be refused"
            );
        }

        // An absent optional is still absent, which is what the `default`s
        // alongside the `with = ...` annotations buy.
        assert_eq!(state.max_position, None);
    }

    /// A policy carrying exactly one rule, with the currency named so the
    /// currency check below cannot fire and take the credit for a rule check.
    ///
    /// That shadowing is why this helper exists. Setting any rule makes
    /// `is_unpoliced()` false, so a fixture leaving `currency: None` is refused
    /// whatever the rule's own amount says - and a test asserting only
    /// `is_err()` on such a fixture stays green with the amount branch deleted.
    fn policed(mutate: impl FnOnce(&mut AccountPolicy)) -> AccountPolicy {
        let mut policy = AccountPolicy {
            currency: Some("USD".to_owned()),
            ..AccountPolicy::default()
        };
        mutate(&mut policy);
        policy
    }

    fn trailing(amount: Decimal) -> TrailingDrawdown {
        TrailingDrawdown {
            amount,
            basis: TrailingBasis::PeakEquity,
            lock_at_equity: None,
            on_breach: BreachAction::Terminate,
        }
    }

    /// Each of the four rules that carries an amount refuses a nonpositive one
    /// by name, and accepts a positive one. The exact message is asserted
    /// because it is the only thing separating these four branches from the
    /// currency branch that fires for every policed fixture.
    #[test]
    fn every_rule_carrying_an_amount_refuses_a_nonpositive_one_by_name() {
        let nonpositive = [Decimal::ZERO, Decimal::from(-1)];
        for bad in nonpositive {
            assert_eq!(
                policed(|p| p.trailing_drawdown = Some(trailing(bad))).validate(),
                Err("trailing_drawdown.amount must be positive".to_owned())
            );
            assert_eq!(
                policed(|p| p.daily_loss_limit = Some(DailyLossLimit {
                    amount: bad,
                    on_breach: BreachAction::LockUntilReset,
                }))
                .validate(),
                Err("daily_loss_limit.amount must be positive".to_owned())
            );
            assert_eq!(
                policed(|p| p.overall_drawdown = Some(OverallDrawdown {
                    amount: bad,
                    on_breach: BreachAction::Terminate,
                }))
                .validate(),
                Err("overall_drawdown.amount must be positive".to_owned())
            );
            assert_eq!(
                policed(|p| p.max_position = Some(MaxPosition { quantity: bad })).validate(),
                Err("max_position.quantity must be positive".to_owned())
            );
        }

        // The positive case: the same four rules, each on its own, validate.
        // Without this a validator refusing every amount would pass above.
        let ok = Decimal::from(1);
        policed(|p| p.trailing_drawdown = Some(trailing(ok)))
            .validate()
            .expect("a positive trailing drawdown is usable");
        policed(|p| {
            p.daily_loss_limit = Some(DailyLossLimit {
                amount: ok,
                on_breach: BreachAction::LockUntilReset,
            });
        })
        .validate()
        .expect("a positive daily loss limit is usable");
        policed(|p| {
            p.overall_drawdown = Some(OverallDrawdown {
                amount: ok,
                on_breach: BreachAction::Terminate,
            });
        })
        .validate()
        .expect("a positive overall drawdown is usable");
        policed(|p| p.max_position = Some(MaxPosition { quantity: ok }))
            .validate()
            .expect("a positive position cap is usable");
    }

    /// The currency requirement itself, which nothing tested directly: it binds
    /// exactly when a rule is set, and an empty string does not satisfy it.
    #[test]
    fn a_policy_naming_any_rule_must_name_its_currency() {
        let expected = "a policy with any rule must name the currency its thresholds are \
             stated in; equity is computed in that currency alone and the venue has no \
             exchange rate";
        let policed_no_currency = AccountPolicy {
            trailing_drawdown: Some(trailing(Decimal::from(2_000))),
            ..AccountPolicy::default()
        };
        assert_eq!(
            policed_no_currency.validate(),
            Err(expected.to_owned()),
            "a policed account naming no currency is refused by name"
        );

        // An empty currency is the same defect wearing a value - and so is a
        // whitespace one, which is not a narrower case but the same one: the
        // code is a lookup key against balance currencies, and no balance
        // carries a blank code. `havoc.rs` refuses a blank client_order_id on
        // `trim().is_empty()`; the two validators mean the same thing by blank.
        for blank in ["", " ", "   ", "\t", "\n", " \t\n "] {
            let policy = AccountPolicy {
                currency: Some(blank.to_owned()),
                ..policed_no_currency.clone()
            };
            assert_eq!(
                policy.validate(),
                Err(expected.to_owned()),
                "a currency of {blank:?} names nothing"
            );
        }

        // A padded code is the subtler half, and the one that reaches a live
        // run: it is non-blank, so the rule above admits it, and it matches no
        // balance, so the account is policed on an equity frozen at zero.
        for padded in [" USD", "USD ", " USD "] {
            let policy = AccountPolicy {
                currency: Some(padded.to_owned()),
                ..policed_no_currency.clone()
            };
            assert!(
                policy.validate().is_err(),
                "a currency of {padded:?} matches no balance and must be refused"
            );
        }

        // Naming it is what makes the same policy usable ...
        AccountPolicy {
            currency: Some("USD".to_owned()),
            ..policed_no_currency
        }
        .validate()
        .expect("a named currency satisfies the rule");

        // ... and an unpoliced account owes no currency at all, which is the
        // default account and every consumer that predates this policy layer.
        assert!(AccountPolicy::default().currency.is_none());
        AccountPolicy::default()
            .validate()
            .expect("an unpoliced account needs no currency");
    }

    /// Opening funding outside the policy currency, which is the shape that
    /// liquidates an account before it trades. A USD policy funded with 50,000
    /// USD and 50,000 EUR would anchor at 100,000 if the two were summed, then
    /// read its first honest observation - 50,000, the USD balance alone - as a
    /// loss of the whole EUR leg. The venue has no rate surface to value the
    /// EUR with, so the only two honest answers are refuse the configuration or
    /// silently ignore half the funding, and a refusal by name is the one that
    /// tells the operator what they got wrong.
    #[test]
    fn policed_opening_balances_may_not_leave_the_policy_currency() {
        let funded = |opening: Vec<(&str, i64)>| {
            policed(|policy| {
                policy.trailing_drawdown = Some(trailing(Decimal::from(5_000)));
                policy.opening_balances = opening
                    .into_iter()
                    .map(|(code, amount)| (code.to_owned(), Decimal::from(amount)))
                    .collect();
            })
        };
        let refusal = funded(vec![("USD", 50_000), ("EUR", 50_000)])
            .validate()
            .expect_err("a policed account may not open in two currencies");
        assert!(
            refusal.contains("EUR") && refusal.contains("USD"),
            "the refusal must name the offending currency and the policy one, got {refusal:?}"
        );
        funded(vec![("USD", 50_000)])
            .validate()
            .expect("opening in the policy currency alone is the supported shape");

        // An unpoliced policy enforces nothing and values nothing, so its
        // opening funding is not policed into one currency: no rule ever reads
        // the anchor, and refusing here would refuse a shape that has always
        // worked.
        AccountPolicy {
            opening_balances: std::collections::HashMap::from([
                ("USD".to_owned(), Decimal::from(50_000)),
                ("EUR".to_owned(), Decimal::from(50_000)),
            ]),
            ..Default::default()
        }
        .validate()
        .expect("an unpoliced account is anchored by nothing and may hold anything");
    }

    /// The inverse of `every_shipped_policy_is_usable`: a name this build does
    /// not ship resolves to nothing. Nothing asserted this, so a match arm
    /// resolving a name absent from `SHIPPED_POLICIES` was invisible - the
    /// membership gate in `shipped_policy` is what makes that unrepresentable
    /// rather than merely untested.
    #[test]
    fn a_name_this_build_does_not_ship_resolves_to_nothing() {
        for absent in ["", "nonsense", "Intraday-Trail", "intraday-trail ", "trail"] {
            assert!(
                shipped_policy(absent).is_none(),
                "{absent} is not in SHIPPED_POLICIES and must not resolve"
            );
            assert!(!SHIPPED_POLICIES.contains(&absent));
        }
    }

    /// The sixth branch of `validate`, held to the same standard as the five
    /// above: the exact message, and the boundary pinned from both sides.
    /// 1439 is the last minute of a UTC day and must be accepted; 1440 is the
    /// first that is not and must be refused. Without both, an off-by-one from
    /// `>=` to `>` admits a reset minute that no day contains.
    #[test]
    fn a_reset_minute_outside_the_day_is_refused() {
        let expected = "reset_minute_utc must be a minute of the day, 0 to 1439";
        // A real rule is set as well, so the fixture is genuinely policed and
        // the accepted cases below prove the reset branch passed rather than
        // that no branch was reachable.
        let at = |minute: u32| {
            policed(|p| {
                p.daily_loss_limit = Some(DailyLossLimit {
                    amount: Decimal::from(500),
                    on_breach: BreachAction::LockUntilReset,
                });
                p.reset_minute_utc = minute;
            })
        };
        for outside in [1_440, 1_441, u32::MAX] {
            assert_eq!(
                at(outside).validate(),
                Err(expected.to_owned()),
                "{outside} is not a minute of the day"
            );
        }
        for inside in [0, 1, 22 * 60, 1_439] {
            at(inside)
                .validate()
                .expect("a minute of the day is accepted");
        }
    }

    /// The default reset is a convention and the default breach actions differ
    /// per rule, both of which a consumer inherits by saying nothing - so they are
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

    /// Every shipped name resolves, validates, and is policed. A name that
    /// does not is a broken binary, not a consumer error.
    #[test]
    fn every_shipped_policy_is_usable() {
        for name in SHIPPED_POLICIES {
            let policy = shipped_policy(name).expect(name);
            assert!(!policy.is_unpoliced(), "{name} must enforce something");
            policy.validate().expect(name);
        }
        let static_dd = shipped_policy("static-drawdown").expect("static");
        assert_eq!(
            static_dd.overall_drawdown.expect("set").amount,
            Decimal::from(5_000)
        );
        let sized = shipped_policy("intraday-trail-sized").expect("sized");
        assert_eq!(sized.max_position.expect("set").quantity, Decimal::from(10));
    }
}
