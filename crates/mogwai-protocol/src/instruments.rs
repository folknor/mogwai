// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::Symbol;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireAssetClass {
    Fx,
    Equity,
    Commodity,
    Index,
    Cryptocurrency,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OmsType {
    #[default]
    Netting,
    Hedging,
}

/// What an instrument is, which decides how holding it moves the ledger.
///
/// The split that matters is not asset class but settlement shape, and there are
/// three of them:
///
/// - `Spot` credits the base asset as a currency balance. That is right for
///   crypto spot, where you genuinely hold the base as money and can spend it on
///   the next pair.
/// - `Equity` credits a position and never a balance. A share is not money: you
///   cannot pay for anything with AAPL, and modelling it as a currency puts it
///   on the same footing as USD, which is what makes short sales, settlement and
///   round lots inexpressible.
/// - `Future`, `Perpetual` and `Inverse` credit neither. They move only cash in
///   the settlement currency, with the exposure carried as a marked position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "snake_case")]
pub enum InstrumentClass {
    Spot {
        base: String,
        quote: String,
    },
    /// A share. Held as a position, paid for in `currency`.
    ///
    /// Not `Spot { base: "AAPL", quote: "USD" }`, which was the only way to say
    /// this before and is wrong in a way that compounds: it makes the ledger
    /// hold AAPL as a currency balance, so the account can "spend" shares, no
    /// short position is expressible as anything but a negative money balance,
    /// and there is nowhere to hang a settlement period or a borrow.
    Equity {
        currency: String,
        /// Shares per contract. One, on every venue that lists shares, and
        /// present so the notional arithmetic is uniform across classes rather
        /// than special-cased here.
        multiplier: Decimal,
        /// The round lot: orders must be a multiple of this many shares.
        ///
        /// Separate from `size_increment`, which stays at one share, and the
        /// separation is the point: a lot rule governs what may be submitted,
        /// while a partial fill legitimately leaves an odd-lot remainder that
        /// the size grid still has to represent. One means odd lots are
        /// accepted, which is what every retail-facing venue does today.
        #[serde(default = "one_share")]
        lot_size: Decimal,
        /// How many shares this account may be short, or `None` for a venue
        /// that models no borrow constraint.
        ///
        /// A short sale needs a locate at a real venue, and its absence is why
        /// a name goes hard-to-borrow. `Some(0)` states exactly that; `None`
        /// states that the venue is not modelling the borrow market, which is
        /// the honest default for a synthetic tape with no lending desk behind
        /// it. Either way a cash equity account - one with no margin policy -
        /// cannot short at all, because shorting is a margin activity.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        borrowable: Option<Decimal>,
        /// How long sale proceeds take to settle, in simulated nanoseconds.
        ///
        /// Cash from a sale is credited immediately and held unsettled until
        /// this span has passed, which is what a `T+N` convention buys a
        /// strategy: the money is yours and you cannot spend it yet. It appears
        /// as `locked` on the balance row, because that is exactly what it is.
        ///
        /// A fixed sim span rather than N business days, and the simplification
        /// is stated rather than hidden: a real `T+2` counts sessions, so a
        /// weekend stretches it. Expressing it in nanoseconds keeps it on the
        /// one clock everything else here is on, and a preset that wants the
        /// session-counted form owes a calendar-aware successor.
        #[serde(default)]
        settlement_ns: u64,
    },
    Future {
        underlying: String,
        settlement_currency: String,
        multiplier: Decimal,
        asset_class: WireAssetClass,
    },
    /// A perpetual swap: a future with no expiry that pays funding between long
    /// and short at an interval.
    ///
    /// The dominant crypto instrument rather than an exotic one. Modelled as a
    /// future plus the funding mechanism, because that is what it is - the
    /// funding flow is the only thing that makes it track spot without an
    /// expiry to converge at, and a perp without it has forward P and L wrong by
    /// construction rather than by approximation.
    Perpetual {
        underlying: String,
        settlement_currency: String,
        multiplier: Decimal,
        asset_class: WireAssetClass,
        /// How often funding is exchanged, in nanoseconds. Eight hours is the
        /// near-universal convention.
        funding_interval_ns: u64,
        /// Zero-premium interest: the rate a long pays a short when the mark
        /// sits on the index, and the whole rate when no index mark is
        /// available. Negative reverses the direction. A real venue computes
        /// the live rate as this plus the mark-versus-index premium, clamped.
        funding_rate: Decimal,
        /// Symbol whose last mark is the index this perpetual funds against.
        ///
        /// Absent, or present but not yet priced, the premium is zero and the
        /// rate is exactly `funding_rate` - today's constant-rate behaviour,
        /// which is also what a perp-only venue produces because reading an
        /// index must never spend a river nobody asked for.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index_symbol: Option<String>,
        /// Absolute cap on the computed rate per interval. Zero means no cap,
        /// so a configured interest with no index is bit-identical to the
        /// constant rate this field used to be.
        #[serde(default)]
        funding_clamp: Decimal,
    },
    /// A coin-margined (inverse) contract: notional is quoted in `quote_currency`
    /// but margin and P and L settle in the base asset.
    ///
    /// Standard on crypto derivatives venues and nearly free on the consumer
    /// side, since a nautilus host already carries an inverse arm. The
    /// arithmetic is what differs: a position's value is
    /// `multiplier * qty / price` rather than `multiplier * qty * price`, so
    /// P and L is non-linear in price and a long is not the mirror of a short.
    Inverse {
        underlying: String,
        /// The asset margin and settlement are denominated in.
        settlement_currency: String,
        /// The currency the contract is quoted in, which nothing settles in.
        quote_currency: String,
        multiplier: Decimal,
        asset_class: WireAssetClass,
    },
}

/// One share per lot: odd lots accepted, which is what every retail-facing
/// venue does.
fn one_share() -> Decimal {
    Decimal::ONE
}

impl InstrumentClass {
    /// The round lot an order's quantity must be a multiple of. One for
    /// everything that is not an equity stating otherwise.
    #[must_use]
    pub fn lot_size(&self) -> Decimal {
        match self {
            Self::Equity { lot_size, .. } => *lot_size,
            _ => Decimal::ONE,
        }
    }

    /// How many units this account may be short, or `None` where the venue
    /// models no borrow constraint. Only an equity states one.
    #[must_use]
    pub fn borrowable(&self) -> Option<Decimal> {
        match self {
            Self::Equity { borrowable, .. } => *borrowable,
            _ => None,
        }
    }

    /// How long sale proceeds take to settle, in simulated nanoseconds. Zero
    /// everywhere but an equity that states a period.
    #[must_use]
    pub fn settlement_ns(&self) -> u64 {
        match self {
            Self::Equity { settlement_ns, .. } => *settlement_ns,
            _ => 0,
        }
    }

    /// Whether holding this is a share rather than a claim on cash - the class
    /// whose position is the asset itself and whose cash leg is the full
    /// notional on both sides.
    #[must_use]
    pub const fn is_equity(&self) -> bool {
        matches!(self, Self::Equity { .. })
    }

    #[must_use]
    pub fn settlement_currency(&self) -> &str {
        match self {
            Self::Spot { quote, .. } => quote,
            Self::Equity { currency, .. } => currency,
            Self::Future {
                settlement_currency,
                ..
            }
            | Self::Perpetual {
                settlement_currency,
                ..
            }
            | Self::Inverse {
                settlement_currency,
                ..
            } => settlement_currency,
        }
    }

    #[must_use]
    pub fn multiplier(&self) -> Decimal {
        match self {
            Self::Spot { .. } => Decimal::ONE,
            Self::Equity { multiplier, .. }
            | Self::Future { multiplier, .. }
            | Self::Perpetual { multiplier, .. }
            | Self::Inverse { multiplier, .. } => *multiplier,
        }
    }

    /// The asset a fill credits as a currency balance, if any.
    ///
    /// `Spot` alone. An equity's shares are a position, and every derivative
    /// moves only cash - which is exactly the distinction that makes an equity
    /// account valuable in one currency where a spot account needs its base
    /// priced.
    #[must_use]
    pub fn base_currency(&self) -> Option<&str> {
        match self {
            Self::Spot { base, .. } => Some(base),
            _ => None,
        }
    }

    /// Whether exposure is carried as a marked position with unrealized P and L,
    /// rather than as an asset the account holds.
    ///
    /// True for every derivative and for equity. This is the predicate the
    /// margin ledger, the mark path and the settlement walk actually want;
    /// `is_future` answered it only while futures were the one such class.
    #[must_use]
    pub const fn is_marked(&self) -> bool {
        matches!(
            self,
            Self::Equity { .. }
                | Self::Future { .. }
                | Self::Perpetual { .. }
                | Self::Inverse { .. }
        )
    }

    /// Whether this is a derivative: marked, and margin-eligible.
    ///
    /// Equity is marked but is not this - a cash equity account posts no
    /// collateral per contract, and a margin policy attached to one must move no
    /// number, which is the same rule spot already had.
    #[must_use]
    pub const fn is_future(&self) -> bool {
        matches!(
            self,
            Self::Future { .. } | Self::Perpetual { .. } | Self::Inverse { .. }
        )
    }

    /// Whether P and L is inverse in price: value is `multiplier * qty / px`.
    #[must_use]
    pub const fn is_inverse(&self) -> bool {
        matches!(self, Self::Inverse { .. })
    }

    /// The funding terms, for a class that exchanges funding.
    #[must_use]
    pub fn funding(&self) -> Option<FundingTerms> {
        match self {
            Self::Perpetual {
                funding_interval_ns,
                funding_rate,
                index_symbol,
                funding_clamp,
                ..
            } => Some(FundingTerms {
                interval_ns: *funding_interval_ns,
                interest: *funding_rate,
                index_symbol: index_symbol.clone(),
                clamp: *funding_clamp,
            }),
            _ => None,
        }
    }
}

/// How a perpetual exchanges funding: the clock, the interest, and the
/// optional mark-versus-index premium.
///
/// The rate is computed, not configured. A real venue pays
/// `clamp(interest + (mark - index) / index, +/- clamp)`. `interest` is
/// `funding_rate` on the class, the zero-premium term. When no index mark is
/// available the premium is zero and the rate collapses to that term, which is
/// why existing configs that state only a rate keep their previous P and L.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FundingTerms {
    pub interval_ns: u64,
    pub interest: Decimal,
    pub index_symbol: Option<String>,
    pub clamp: Decimal,
}

impl FundingTerms {
    /// The rate a long pays a short at these marks.
    ///
    /// `index` absent or zero leaves the premium at zero: a missing index is
    /// not a market event, it is a tape that was never asked for, and inventing
    /// a basis against nothing would move cash for a reason nobody configured.
    #[must_use]
    pub fn rate(&self, mark: Decimal, index: Option<Decimal>) -> Decimal {
        let premium = match index {
            Some(index) if !index.is_zero() => (mark - index) / index,
            _ => Decimal::ZERO,
        };
        let raw = self.interest + premium;
        if self.clamp.is_zero() {
            raw
        } else {
            raw.clamp(-self.clamp, self.clamp)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstrumentDef {
    pub symbol: Symbol,
    pub class: InstrumentClass,
    pub price_precision: u8,
    pub size_precision: u8,
    pub price_increment: Decimal,
    pub size_increment: Decimal,
}

impl InstrumentDef {
    #[must_use]
    pub fn tick_value(&self) -> Decimal {
        self.price_increment
            .checked_mul(self.class.multiplier())
            .unwrap_or(Decimal::MAX)
    }

    /// What `qty` at `px` is worth in the settlement currency.
    ///
    /// Linear for every class but `Inverse`, where the contract is quoted in one
    /// currency and settles in another, so value is `multiplier * qty / px` -
    /// one contract is a fixed amount of quote currency, and what varies with
    /// price is how much of the settlement asset that costs. Getting this wrong
    /// does not merely mis-size a position: it inverts the sign of the P and L
    /// response to price.
    #[must_use]
    pub fn notional(&self, qty: Decimal, px: Decimal) -> Option<Decimal> {
        if self.class.is_inverse() {
            if px.is_zero() {
                return None;
            }
            return qty.checked_mul(self.class.multiplier())?.checked_div(px);
        }
        qty.checked_mul(px)?.checked_mul(self.class.multiplier())
    }

    /// Unrealized P and L on `qty` bought at `avg_px` and marked at `mark_px`,
    /// in the settlement currency.
    ///
    /// The one place the inverse arithmetic is spelled out, so no caller has to
    /// remember it: a linear contract gains `(mark - avg) * qty * multiplier`,
    /// an inverse one gains `multiplier * qty * (1/avg - 1/mark)`. The inverse
    /// form is what makes a coin-margined long lose more from a fall than it
    /// gains from an equal rise.
    #[must_use]
    pub fn unrealized(&self, qty: Decimal, avg_px: Decimal, mark_px: Decimal) -> Option<Decimal> {
        if self.class.is_inverse() {
            if avg_px.is_zero() || mark_px.is_zero() {
                return None;
            }
            let entry = Decimal::ONE.checked_div(avg_px)?;
            let now = Decimal::ONE.checked_div(mark_px)?;
            return entry
                .checked_sub(now)?
                .checked_mul(qty)?
                .checked_mul(self.class.multiplier());
        }
        mark_px
            .checked_sub(avg_px)?
            .checked_mul(qty)?
            .checked_mul(self.class.multiplier())
    }
}

/// The canonical default instrument set the venue seeds when none is supplied.
///
/// Today this is the single `BTCUSDT` instrument. The engine seeds from this
/// function, and the venue derives its default generator grid from the same
/// definition, so order validation and generated prices agree on tick size and
/// precision. The field values are price precision 2, size precision 8, with
/// `1e-2` / `1e-8` increments.
#[must_use]
pub fn default_instruments() -> Vec<InstrumentDef> {
    vec![InstrumentDef {
        symbol: "BTCUSDT".into(),
        class: InstrumentClass::Spot {
            base: "BTC".into(),
            quote: "USDT".into(),
        },
        price_precision: 2,
        size_precision: 8,
        price_increment: Decimal::new(1, 2),
        size_increment: Decimal::new(1, 8),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instrument_def_round_trips() {
        let spot = InstrumentDef {
            symbol: "BTCUSDT".into(),
            class: InstrumentClass::Spot {
                base: "BTC".into(),
                quote: "USDT".into(),
            },
            price_precision: 2,
            size_precision: 8,
            price_increment: Decimal::new(1, 2),
            size_increment: Decimal::new(1, 8),
        };

        let future = InstrumentDef {
            symbol: "MNQ".into(),
            class: InstrumentClass::Future {
                underlying: "NQ".into(),
                settlement_currency: "USD".into(),
                multiplier: Decimal::from(2),
                asset_class: WireAssetClass::Index,
            },
            price_precision: 2,
            size_precision: 0,
            price_increment: Decimal::new(25, 2),
            size_increment: Decimal::ONE,
        };

        let equity = InstrumentDef {
            symbol: "AAPL".into(),
            class: InstrumentClass::Equity {
                currency: "USD".into(),
                multiplier: Decimal::ONE,
                lot_size: Decimal::from(100),
                borrowable: Some(Decimal::from(500)),
                settlement_ns: 2 * 24 * 3_600 * 1_000_000_000,
            },
            price_precision: 2,
            size_precision: 0,
            price_increment: Decimal::new(1, 2),
            size_increment: Decimal::ONE,
        };

        let perpetual = InstrumentDef {
            symbol: "BTCUSDT.P".into(),
            class: InstrumentClass::Perpetual {
                underlying: "BTC".into(),
                settlement_currency: "USDT".into(),
                multiplier: Decimal::ONE,
                asset_class: WireAssetClass::Cryptocurrency,
                funding_interval_ns: 8 * 3_600 * 1_000_000_000,
                funding_rate: Decimal::new(1, 4),
                index_symbol: Some("BTCUSDT".into()),
                funding_clamp: Decimal::new(5, 3),
            },
            price_precision: 2,
            size_precision: 0,
            price_increment: Decimal::new(1, 2),
            size_increment: Decimal::ONE,
        };

        let inverse = InstrumentDef {
            symbol: "BTCUSD-INV".into(),
            class: InstrumentClass::Inverse {
                underlying: "BTC".into(),
                settlement_currency: "BTC".into(),
                quote_currency: "USD".into(),
                multiplier: Decimal::from(100),
                asset_class: WireAssetClass::Cryptocurrency,
            },
            price_precision: 1,
            size_precision: 0,
            price_increment: Decimal::new(1, 1),
            size_increment: Decimal::ONE,
        };

        for (def, tag) in [
            (spot, "\"class\":\"spot\""),
            (future, "\"class\":\"future\""),
            (equity, "\"class\":\"equity\""),
            (perpetual, "\"class\":\"perpetual\""),
            (inverse, "\"class\":\"inverse\""),
        ] {
            let json = serde_json::to_string(&def).unwrap();
            assert!(json.contains(tag), "wire tag must be exact: {json}");
            let decoded: InstrumentDef = serde_json::from_str(&json).unwrap();
            assert_eq!(decoded, def);
        }
    }

    /// `lot_size` is the one equity field with a non-zero serde default, so a
    /// config omitting it must decode to one share rather than to zero - which
    /// would make every quantity a whole number of nothing. Its neighbours
    /// `borrowable` and `settlement_ns` default to absent and zero, and those
    /// two defaults are load-bearing in the other direction: they are what says
    /// "this venue models no borrow market and settles instantly".
    #[test]
    fn an_equity_omitting_its_optional_terms_takes_the_documented_defaults() {
        let json = r#"{"class":"equity","currency":"USD","multiplier":1}"#;
        let decoded: InstrumentClass = serde_json::from_str(json).unwrap();
        assert_eq!(
            decoded,
            InstrumentClass::Equity {
                currency: "USD".into(),
                multiplier: Decimal::ONE,
                lot_size: Decimal::ONE,
                borrowable: None,
                settlement_ns: 0,
            }
        );
        assert_eq!(decoded.lot_size(), Decimal::ONE, "odd lots are accepted");
        assert_eq!(decoded.borrowable(), None);
        assert_eq!(decoded.settlement_ns(), 0);
    }

    #[test]
    fn tick_value_derives_from_increment_and_multiplier() {
        for (symbol, multiplier, expected) in [
            ("MNQ", Decimal::from(2), Decimal::new(50, 2)),
            ("MES", Decimal::from(5), Decimal::new(125, 2)),
        ] {
            let def = InstrumentDef {
                symbol: symbol.into(),
                class: InstrumentClass::Future {
                    underlying: symbol.into(),
                    settlement_currency: "USD".into(),
                    multiplier,
                    asset_class: WireAssetClass::Index,
                },
                price_precision: 2,
                size_precision: 0,
                price_increment: Decimal::new(25, 2),
                size_increment: Decimal::ONE,
            };
            assert_eq!(def.tick_value(), expected);
        }
    }

    fn inverse(multiplier: i64) -> InstrumentDef {
        InstrumentDef {
            symbol: "BTCUSD-INV".into(),
            class: InstrumentClass::Inverse {
                underlying: "BTC".into(),
                settlement_currency: "BTC".into(),
                quote_currency: "USD".into(),
                multiplier: Decimal::from(multiplier),
                asset_class: WireAssetClass::Cryptocurrency,
            },
            price_precision: 1,
            size_precision: 0,
            price_increment: Decimal::new(1, 1),
            size_increment: Decimal::ONE,
        }
    }

    /// An inverse contract is a fixed amount of quote currency, so its value in
    /// the settlement asset falls as price rises. Getting this backwards does
    /// not mis-size a position, it inverts the sign of its response to price.
    #[test]
    fn an_inverse_contract_is_worth_less_of_its_settlement_asset_as_price_rises() {
        let def = inverse(100);
        // 100 USD of contract at 50,000 USD/BTC is 0.002 BTC.
        assert_eq!(
            def.notional(Decimal::ONE, Decimal::from(50_000)),
            Some(Decimal::new(2, 3))
        );
        // The same contract at 100,000 is half as much BTC.
        assert_eq!(
            def.notional(Decimal::ONE, Decimal::from(100_000)),
            Some(Decimal::ONE / Decimal::from(1_000))
        );
    }

    /// A coin-margined long gains settlement asset when price rises, and the
    /// gain is not symmetric with an equal fall - which is the whole reason the
    /// arithmetic cannot be shared with the linear form.
    #[test]
    fn inverse_unrealized_is_non_linear_in_price() {
        let def = inverse(100);
        let up = def
            .unrealized(Decimal::ONE, Decimal::from(50_000), Decimal::from(60_000))
            .expect("representable");
        let down = def
            .unrealized(Decimal::ONE, Decimal::from(50_000), Decimal::from(40_000))
            .expect("representable");
        assert!(up > Decimal::ZERO, "a long gains when price rises: {up}");
        assert!(down < Decimal::ZERO, "and loses when it falls: {down}");
        assert!(
            down.abs() > up,
            "an equal fall must cost more than the rise gains, or this is the linear \
             formula wearing an inverse label: up {up}, down {down}"
        );
    }

    /// The linear form stays exactly what it was, so the inverse arm cannot have
    /// been reached by a class that should not use it.
    #[test]
    fn a_linear_contract_keeps_its_multiplier_arithmetic() {
        let def = InstrumentDef {
            symbol: "MNQ".into(),
            class: InstrumentClass::Future {
                underlying: "NQ".into(),
                settlement_currency: "USD".into(),
                multiplier: Decimal::from(2),
                asset_class: WireAssetClass::Index,
            },
            price_precision: 2,
            size_precision: 0,
            price_increment: Decimal::new(25, 2),
            size_increment: Decimal::ONE,
        };
        assert_eq!(
            def.notional(Decimal::from(3), Decimal::from(20_000)),
            Some(Decimal::from(120_000))
        );
        assert_eq!(
            def.unrealized(
                Decimal::from(3),
                Decimal::from(20_000),
                Decimal::from(20_010)
            ),
            Some(Decimal::from(60))
        );
    }

    /// An equity holds shares, so nothing about it credits a currency balance -
    /// which is the distinction the class exists for.
    #[test]
    fn an_equity_credits_no_currency_balance() {
        let equity = InstrumentClass::Equity {
            currency: "USD".into(),
            multiplier: Decimal::ONE,
            lot_size: Decimal::ONE,
            borrowable: None,
            settlement_ns: 0,
        };
        assert_eq!(equity.base_currency(), None);
        assert_eq!(equity.settlement_currency(), "USD");
        assert!(equity.is_marked(), "shares are marked, not held as money");
        assert!(
            !equity.is_future(),
            "a cash equity posts no per-contract collateral"
        );
    }

    /// The three equity-only accessors read their field, and nothing else in
    /// this crate said so: the fixture above states all three at their
    /// defaults, so an implementation returning `Decimal::ONE` / `None` / `0`
    /// and ignoring the class entirely passed every protocol test. The engine's
    /// `Shares` fixture does parameterize them, but that is coverage of the
    /// behaviour in another crate, not of the accessor.
    ///
    /// The non-equity direction is the other half: every other class answers
    /// the same three questions with the documented constants, which is what
    /// lets the ledger ask them without matching on the class first.
    #[test]
    fn the_equity_accessors_report_the_terms_the_class_states() {
        let equity = InstrumentClass::Equity {
            currency: "USD".into(),
            multiplier: Decimal::ONE,
            lot_size: Decimal::from(100),
            borrowable: Some(Decimal::from(500)),
            settlement_ns: 2 * 24 * 3_600 * 1_000_000_000,
        };
        assert_eq!(equity.lot_size(), Decimal::from(100), "a round-lot name");
        assert_eq!(
            equity.borrowable(),
            Some(Decimal::from(500)),
            "a stated borrow, which is not the same fact as no borrow market"
        );
        assert_eq!(equity.settlement_ns(), 2 * 24 * 3_600 * 1_000_000_000);

        // `Some(0)` is hard-to-borrow and must not collapse to `None`.
        let hard = InstrumentClass::Equity {
            currency: "USD".into(),
            multiplier: Decimal::ONE,
            lot_size: Decimal::ONE,
            borrowable: Some(Decimal::ZERO),
            settlement_ns: 0,
        };
        assert_eq!(hard.borrowable(), Some(Decimal::ZERO));

        for other in [
            InstrumentClass::Spot {
                base: "BTC".into(),
                quote: "USDT".into(),
            },
            InstrumentClass::Future {
                underlying: "NQ".into(),
                settlement_currency: "USD".into(),
                multiplier: Decimal::from(2),
                asset_class: WireAssetClass::Index,
            },
            InstrumentClass::Perpetual {
                underlying: "BTC".into(),
                settlement_currency: "USDT".into(),
                multiplier: Decimal::ONE,
                asset_class: WireAssetClass::Cryptocurrency,
                funding_interval_ns: 8 * 3_600 * 1_000_000_000,
                funding_rate: Decimal::new(1, 4),
                index_symbol: None,
                funding_clamp: Decimal::ZERO,
            },
            InstrumentClass::Inverse {
                underlying: "BTC".into(),
                settlement_currency: "BTC".into(),
                quote_currency: "USD".into(),
                multiplier: Decimal::from(100),
                asset_class: WireAssetClass::Cryptocurrency,
            },
        ] {
            assert_eq!(other.lot_size(), Decimal::ONE, "{other:?}");
            assert_eq!(other.borrowable(), None, "{other:?}");
            assert_eq!(other.settlement_ns(), 0, "{other:?}");
        }
    }

    #[test]
    fn a_perpetual_states_its_funding_and_a_future_states_none() {
        let perp = InstrumentClass::Perpetual {
            underlying: "BTC".into(),
            settlement_currency: "USDT".into(),
            multiplier: Decimal::ONE,
            asset_class: WireAssetClass::Cryptocurrency,
            funding_interval_ns: 8 * 3_600 * 1_000_000_000,
            funding_rate: Decimal::new(1, 4),
            index_symbol: None,
            funding_clamp: Decimal::ZERO,
        };
        assert_eq!(
            perp.funding(),
            Some(FundingTerms {
                interval_ns: 8 * 3_600 * 1_000_000_000,
                interest: Decimal::new(1, 4),
                index_symbol: None,
                clamp: Decimal::ZERO,
            })
        );
        assert!(perp.is_future(), "a perpetual posts margin like any future");
        assert_eq!(
            InstrumentClass::Spot {
                base: "BTC".into(),
                quote: "USDT".into()
            }
            .funding(),
            None
        );
    }

    /// A real venue pays interest plus the mark-versus-index premium. No index
    /// is a zero premium, not a refusal: a perp-only venue has no spot tape to
    /// read and must keep the configured rate.
    #[test]
    fn funding_rate_is_interest_plus_clamped_premium() {
        let terms = FundingTerms {
            interval_ns: 1,
            interest: Decimal::new(1, 4),
            index_symbol: Some("BTCUSDT".into()),
            clamp: Decimal::new(5, 3),
        };
        assert_eq!(
            terms.rate(Decimal::from(60_000), None),
            Decimal::new(1, 4),
            "no index mark: the rate is the configured interest"
        );
        // Mark 0.1 percent above the index: premium 0.001 plus interest 0.0001,
        // under the 0.005 cap.
        assert_eq!(
            terms.rate(Decimal::from(60_060), Some(Decimal::from(60_000))),
            Decimal::new(11, 4)
        );
        // A 10 percent basis would be 0.1001 unclamped; the cap holds it.
        assert_eq!(
            terms.rate(Decimal::from(66_000), Some(Decimal::from(60_000))),
            Decimal::new(5, 3)
        );
    }

    /// A value pin on the shipped default set, and nothing more: this crate
    /// cannot depend on `mogwai-engine`, so it has no way to compare against
    /// what the engine does with these terms. The name used to promise exactly
    /// that comparison; the cross-check that really has two sides lives in the
    /// engine, as `the_default_seed_puts_the_engine_on_a_btcusdt_cent_and
    /// _satoshi_grid`, where the increments are read back out of the engine's
    /// own order validation.
    #[test]
    fn default_instruments_ships_one_btcusdt_spot_definition() {
        let defs = default_instruments();
        assert_eq!(defs.len(), 1);
        assert_eq!(
            defs[0],
            InstrumentDef {
                symbol: "BTCUSDT".into(),
                class: InstrumentClass::Spot {
                    base: "BTC".into(),
                    quote: "USDT".into(),
                },
                price_precision: 2,
                size_precision: 8,
                price_increment: Decimal::new(1, 2),
                size_increment: Decimal::new(1, 8),
            }
        );
    }
}
