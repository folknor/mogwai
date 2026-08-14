// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Observable top-of-book model and its calibration seams.

use std::num::NonZeroU32;

use rust_decimal::Decimal;
use serde::Deserialize;

use super::numeric::decimal_from_f64;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum CalibrationProvenance {
    #[default]
    Uncalibrated,
    Fitted {
        corpus: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct QuotedWidth {
    ticks: NonZeroU32,
    #[serde(default)]
    provenance: CalibrationProvenance,
}

impl QuotedWidth {
    #[must_use]
    pub fn new(ticks: NonZeroU32, provenance: CalibrationProvenance) -> Self {
        Self { ticks, provenance }
    }

    #[must_use]
    pub fn uncalibrated(ticks: NonZeroU32) -> Self {
        Self::new(ticks, CalibrationProvenance::Uncalibrated)
    }

    #[must_use]
    pub fn ticks(&self) -> NonZeroU32 {
        self.ticks
    }

    #[must_use]
    pub fn provenance(&self) -> &CalibrationProvenance {
        &self.provenance
    }
}

impl Default for QuotedWidth {
    fn default() -> Self {
        Self::uncalibrated(NonZeroU32::MIN)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TopOfBookSizes {
    pub bid: Decimal,
    pub ask: Decimal,
    #[serde(default)]
    pub provenance: CalibrationProvenance,
}

impl TopOfBookSizes {
    #[must_use]
    pub fn uncalibrated(min_size: Decimal) -> Self {
        Self {
            bid: min_size,
            ask: min_size,
            provenance: CalibrationProvenance::Uncalibrated,
        }
    }
}

impl Default for TopOfBookSizes {
    fn default() -> Self {
        Self::uncalibrated(Decimal::ONE)
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TradeDisplacement {
    ticks: f64,
    #[serde(default)]
    provenance: CalibrationProvenance,
}

impl TradeDisplacement {
    #[must_use]
    pub fn new(ticks: f64, provenance: CalibrationProvenance) -> Self {
        Self { ticks, provenance }
    }

    #[must_use]
    pub fn uncalibrated(ticks: f64) -> Self {
        Self::new(ticks, CalibrationProvenance::Uncalibrated)
    }

    #[must_use]
    pub fn ticks(&self) -> f64 {
        self.ticks
    }

    #[must_use]
    pub fn provenance(&self) -> &CalibrationProvenance {
        &self.provenance
    }
}

impl Default for TradeDisplacement {
    fn default() -> Self {
        Self::uncalibrated(super::consts::TRADE_BOUNCE_HALF_WIDTH_TICKS)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PublishedBook {
    pub bid_ticks: f64,
    pub ask_ticks: f64,
    pub bid_size: Decimal,
    pub ask_size: Decimal,
}

#[must_use]
pub fn place_book(
    drifted_mid_ticks: f64,
    width: &QuotedWidth,
    sizes: &TopOfBookSizes,
) -> PublishedBook {
    let width_ticks = f64::from(width.ticks().get());
    let bid_ticks = (drifted_mid_ticks - width_ticks / 2.0).round().max(1.0);
    PublishedBook {
        bid_ticks,
        ask_ticks: bid_ticks + width_ticks,
        bid_size: sizes.bid,
        ask_size: sizes.ask,
    }
}

#[must_use]
pub fn book_mid_ticks(book: &PublishedBook) -> f64 {
    (book.bid_ticks + book.ask_ticks) / 2.0
}

impl PublishedBook {
    #[must_use]
    pub fn bid_price(&self, tick: f64, decimals: u32) -> Decimal {
        decimal_from_f64(self.bid_ticks * tick).round_dp(decimals)
    }

    #[must_use]
    pub fn ask_price(&self, tick: f64, decimals: u32) -> Decimal {
        decimal_from_f64(self.ask_ticks * tick).round_dp(decimals)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn book_width_is_exact_at_every_phase() {
        let sizes = TopOfBookSizes::uncalibrated(Decimal::ONE);
        for width in 1..=8 {
            let width = QuotedWidth::uncalibrated(NonZeroU32::new(width).unwrap());
            for phase in 0..64 {
                let book = place_book(100.0 + f64::from(phase) / 64.0, &width, &sizes);
                assert_eq!(
                    book.ask_ticks - book.bid_ticks,
                    f64::from(width.ticks().get())
                );
            }
        }
    }

    #[test]
    fn published_book_carries_values_without_calibration_metadata() {
        assert_eq!(std::mem::size_of::<PublishedBook>(), 48);
        assert!(
            std::mem::size_of::<PublishedBook>()
                < 2 * std::mem::size_of::<f64>() + std::mem::size_of::<TopOfBookSizes>()
        );
        let sizes = TopOfBookSizes {
            bid: Decimal::from(2),
            ask: Decimal::from(3),
            provenance: CalibrationProvenance::Fitted {
                corpus: "large fitted corpus identity".repeat(100),
            },
        };
        let book = place_book(100.0, &QuotedWidth::default(), &sizes);
        assert_eq!((book.bid_size, book.ask_size), (sizes.bid, sizes.ask));
    }

    #[test]
    fn book_mid_tracks_the_drifted_mid_within_half_a_tick() {
        let sizes = TopOfBookSizes::uncalibrated(Decimal::ONE);
        for width in 1..=8 {
            let width = QuotedWidth::uncalibrated(NonZeroU32::new(width).unwrap());
            for phase in 0..64 {
                let mid = 100.0 + f64::from(phase) / 64.0;
                let book = place_book(mid, &width, &sizes);
                assert!(book.bid_ticks > 1.0);
                assert!((book_mid_ticks(&book) - mid).abs() <= 0.5);
            }
        }
    }

    #[test]
    fn odd_widths_straddle_a_tick_and_even_widths_sit_on_one() {
        let sizes = TopOfBookSizes::uncalibrated(Decimal::ONE);
        for width in 1..=8 {
            let configured = QuotedWidth::uncalibrated(NonZeroU32::new(width).unwrap());
            let mid = book_mid_ticks(&place_book(100.0, &configured, &sizes));
            assert_eq!(mid.fract().abs(), if width % 2 == 0 { 0.0 } else { 0.5 });
        }
    }

    #[test]
    fn the_one_tick_floor_preserves_the_width() {
        let sizes = TopOfBookSizes::uncalibrated(Decimal::ONE);
        let width = QuotedWidth::uncalibrated(NonZeroU32::new(7).unwrap());
        let book = place_book(-100.0, &width, &sizes);
        assert_eq!(book.bid_ticks, 1.0);
        assert_eq!(book.ask_ticks, 8.0);
    }
}
