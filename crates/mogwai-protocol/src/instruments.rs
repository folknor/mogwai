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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "snake_case")]
pub enum InstrumentClass {
    Spot {
        base: String,
        quote: String,
    },
    Future {
        underlying: String,
        settlement_currency: String,
        multiplier: Decimal,
        asset_class: WireAssetClass,
    },
}

impl InstrumentClass {
    #[must_use]
    pub fn settlement_currency(&self) -> &str {
        match self {
            Self::Spot { quote, .. } => quote,
            Self::Future {
                settlement_currency,
                ..
            } => settlement_currency,
        }
    }

    #[must_use]
    pub fn multiplier(&self) -> Decimal {
        match self {
            Self::Spot { .. } => Decimal::ONE,
            Self::Future { multiplier, .. } => *multiplier,
        }
    }

    #[must_use]
    pub fn base_currency(&self) -> Option<&str> {
        match self {
            Self::Spot { base, .. } => Some(base),
            Self::Future { .. } => None,
        }
    }

    #[must_use]
    pub const fn is_future(&self) -> bool {
        matches!(self, Self::Future { .. })
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

    #[must_use]
    pub fn notional(&self, qty: Decimal, px: Decimal) -> Option<Decimal> {
        qty.checked_mul(px)?.checked_mul(self.class.multiplier())
    }
}

/// The canonical default instrument set the venue seeds when none is supplied.
///
/// Today this is the single BTCUSDT instrument. The engine seeds from this
/// function, and the server derives its default generator grid from the same
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

        for (def, tag) in [
            (spot, "\"class\":\"spot\""),
            (future, "\"class\":\"future\""),
        ] {
            let json = serde_json::to_string(&def).unwrap();
            assert!(json.contains(tag), "wire tag must be exact: {json}");
            let decoded: InstrumentDef = serde_json::from_str(&json).unwrap();
            assert_eq!(decoded, def);
        }
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

    #[test]
    fn default_instruments_matches_engine_btcusdt_seed() {
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
