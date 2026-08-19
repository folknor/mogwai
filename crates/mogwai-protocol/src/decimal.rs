// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

use rust_decimal::Decimal;

/// Saturating `Decimal` -> `f64`. `Decimal`'s max magnitude (~7.9e28) sits
/// nowhere near `f64::MAX` (~1.8e308), so `to_f64()` cannot actually fail for
/// any value the type can hold - the `unwrap_or(0.0)` fallback is defensive
/// completeness, not a live safety net today. It is kept anyway because
/// `0.0` is the worst possible sentinel for a price or quantity on the hot
/// fill/balance path (a huge magnitude would silently read as zero rather
/// than surface as an error), so if `Decimal`'s range or `f64`'s ever changed
/// underneath this, this is the one place that assumption needs revisiting.
/// The data crate carries a private reader with this exact contract; the
/// adapter's `convert.rs` already calls this helper directly rather than the
/// panicking `.to_f64().expect(...)` a pathological wire `Decimal` would
/// otherwise take the runtime down with.
#[must_use]
pub fn decimal_to_f64(d: Decimal) -> f64 {
    use rust_decimal::prelude::ToPrimitive;
    d.to_f64().unwrap_or(0.0)
}

/// Saturating `f64` -> `Decimal`: clamps to `Decimal::MAX` / `Decimal::MIN` for
/// out-of-range finite inputs and maps any non-finite input (NaN, +/-inf) to
/// `Decimal::ZERO`. Mirrors the data crate's private writer so the two can be
/// unified, and gives the adapter a total conversion in place of a panicking one.
#[must_use]
pub fn decimal_from_f64(x: f64) -> Decimal {
    use rust_decimal::prelude::FromPrimitive;
    if !x.is_finite() {
        return Decimal::ZERO;
    }
    Decimal::from_f64(x).unwrap_or(if x > 0.0 { Decimal::MAX } else { Decimal::MIN })
}

/// `serde` glue for an OPTIONAL wire `Decimal`: a JSON string or `null`, never
/// a JSON number.
///
/// `rust_decimal::serde::str_option` looks like it would do this and does not:
/// it REFUSES an explicit `null`, and the venue's own frames carry
/// `"price":null` for every priceless order - a stop-market submit, a
/// still-priceless amend. Annotating the wire fields with it made the adapter's
/// stop-market and trigger-amend paths undecodable, which the socket suites
/// caught. The required (non-`Option`) fields keep using
/// `rust_decimal::serde::str` directly, because no `null` can reach them.
///
/// `visit_some` delegates to that same `str` deserializer, so the ONE rule -
/// a number is refused, a string is exact - is stated in one place and the
/// optional case cannot drift away from the required one.
pub(crate) mod str_option {
    use rust_decimal::Decimal;
    use serde::{Deserializer, Serializer, de};

    pub(crate) fn serialize<S>(value: &Option<Decimal>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Some(decimal) => serializer.serialize_str(&decimal.to_string()),
            None => serializer.serialize_none(),
        }
    }

    struct OptionalWireDecimal;

    impl<'de> de::Visitor<'de> for OptionalWireDecimal {
        type Value = Option<Decimal>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("null, or a decimal spelled as a JSON string")
        }

        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
        where
            D: Deserializer<'de>,
        {
            rust_decimal::serde::str::deserialize(deserializer).map(Some)
        }
    }

    pub(crate) fn deserialize<'de, D>(deserializer: D) -> Result<Option<Decimal>, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_option(OptionalWireDecimal)
    }
}

/// `serde` glue for a MAP of wire decimals keyed by currency: every VALUE is a
/// JSON string, never a JSON number.
///
/// Exists for one field, `POST /accounts`'s `balances`, which is the third live
/// decode path carrying money into the venue and was missed when the wire fields
/// in `messages` were annotated. An opening balance is a money quantity in
/// exactly the sense a fill price is, so it takes the same rule; the thresholds
/// and fractions alongside it in that request body (`AccountPolicy`, and the
/// `RiskPolicy` inside it) stay tolerant, because they are also TOML config.
///
/// `serialize` is here for completeness of the `with = ...` pair rather than
/// because a caller needs it today - the request type is deserialize-only.
pub mod str_map {
    use std::collections::HashMap;

    use rust_decimal::Decimal;
    use serde::{Deserialize, Deserializer, Serializer, ser::SerializeMap};

    /// One map value, deserialized by the same rule a required wire decimal
    /// uses, so the map cannot drift away from the scalar case.
    struct WireDecimal(Decimal);

    impl<'de> Deserialize<'de> for WireDecimal {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            rust_decimal::serde::str::deserialize(deserializer).map(WireDecimal)
        }
    }

    /// # Errors
    /// Propagates the serializer's own errors.
    pub fn serialize<S>(value: &HashMap<String, Decimal>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(value.len()))?;
        for (key, decimal) in value {
            map.serialize_entry(key, &decimal.to_string())?;
        }
        map.end()
    }

    /// # Errors
    /// Fails when a value is spelled as anything but a decimal in a JSON string.
    pub fn deserialize<'de, D>(deserializer: D) -> Result<HashMap<String, Decimal>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = HashMap::<String, WireDecimal>::deserialize(deserializer)?;
        Ok(raw.into_iter().map(|(key, value)| (key, value.0)).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_f64_round_trips_and_saturates() {
        // A representable value round-trips through both helpers.
        let d = Decimal::new(12_345, 2);
        assert!((decimal_to_f64(d) - 123.45).abs() < 1e-9);
        assert_eq!(decimal_from_f64(123.45), d);

        // Non-finite inputs collapse to zero rather than panicking.
        assert_eq!(decimal_from_f64(f64::NAN), Decimal::ZERO);
        assert_eq!(decimal_from_f64(f64::INFINITY), Decimal::ZERO);
        assert_eq!(decimal_from_f64(f64::NEG_INFINITY), Decimal::ZERO);

        // Magnitudes past Decimal's range saturate to the signed bound.
        assert_eq!(decimal_from_f64(1e40), Decimal::MAX);
        assert_eq!(decimal_from_f64(-1e40), Decimal::MIN);
    }
}
