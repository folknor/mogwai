// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::Symbol;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstrumentDef {
    pub symbol: Symbol,
    pub base: String,
    pub quote: String,
    pub price_precision: u8,
    pub size_precision: u8,
    pub price_increment: Decimal,
    pub size_increment: Decimal,
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
        base: "BTC".into(),
        quote: "USDT".into(),
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
        let def = InstrumentDef {
            symbol: "BTCUSDT".into(),
            base: "BTC".into(),
            quote: "USDT".into(),
            price_precision: 2,
            size_precision: 8,
            price_increment: Decimal::new(1, 2),
            size_increment: Decimal::new(1, 8),
        };

        let json = serde_json::to_string(&def).unwrap();
        let decoded: InstrumentDef = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded, def);
    }

    #[test]
    fn default_instruments_matches_engine_btcusdt_seed() {
        let defs = default_instruments();
        assert_eq!(defs.len(), 1);
        assert_eq!(
            defs[0],
            InstrumentDef {
                symbol: "BTCUSDT".into(),
                base: "BTC".into(),
                quote: "USDT".into(),
                price_precision: 2,
                size_precision: 8,
                price_increment: Decimal::new(1, 2),
                size_increment: Decimal::new(1, 8),
            }
        );
    }
}
