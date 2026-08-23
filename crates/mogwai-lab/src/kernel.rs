// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The deterministic kernel of the 12a measurement engine
//! (the retired rewrite plan, phase 2a): the frozen bit-exact PRNG
//! derivation (`splitmix64`, `tuple_mix`, `fisher_yates`), the four quantile
//! conventions (`nearest_rank_list`, `nearest_rank_p`,
//! `weighted_nearest_rank`, `median_or_none`) and the type-strict canonical
//! serialization the parity gates compare with.
//!
//! Every function here is a port of the same-named helper in
//! `analysis/mnq_fit.py`; `splitmix64` and `tuple_mix` additionally have a
//! bit-identical twin in `crates/mogwai-cli/src/measure12a.rs` (retiring at
//! phase 2c) and in `crates/mogwai-protocol/src/seeds.rs`. Nothing here is
//! allowed to differ by so much as a rounding convention: these are the
//! functions whose output is compared byte-for-byte across two languages.

/// Bit-identical to `crates/mogwai-protocol/src/seeds.rs`, to
/// `analysis/mnq_fit.py`'s `splitmix64` and to the `mogwai-cli` twin.
#[must_use]
pub const fn splitmix64(x: u64) -> u64 {
    let x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The spec-3.4a multi-field derivation: fold `splitmix64` over the fields in
/// listed order (the order is load-bearing - `tuple_mix(b, [x, y])` and
/// `tuple_mix(b, [y, x])` are different streams).
#[must_use]
pub fn tuple_mix(base: u64, fields: &[u64]) -> u64 {
    let mut x = base;
    for &value in fields {
        x = splitmix64(x ^ value);
    }
    x
}

/// The frozen spec-5.1 shuffle, in place over `values` in original stream
/// order: the state advances by `splitmix64` per step and `j = state % (i+1)`.
/// Note the state advances before the modulus, matching the Python.
pub fn fisher_yates<T>(values: &mut [T], mut state: u64) {
    if values.is_empty() {
        return;
    }
    let mut i = values.len() - 1;
    while i > 0 {
        state = splitmix64(state);
        let j = (state % (i as u64 + 1)) as usize;
        values.swap(i, j);
        i -= 1;
    }
}

/// `nearest_rank_list`: nearest-rank quantile of an ascending list, clamped
/// at rank 1. `None` marks the Python `Refusal("empty list has no
/// quantiles")` - callers that must refuse map it themselves; every current
/// caller guards emptiness first.
#[must_use]
pub fn nearest_rank_list(sorted_values: &[f64], q: f64) -> Option<f64> {
    if sorted_values.is_empty() {
        return None;
    }
    let n = sorted_values.len();
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "q in [0,1] and n is a slice length, so the ceil is in [0, n]"
    )]
    let rank = ((q * n as f64).ceil() as usize).max(1);
    Some(sorted_values[rank - 1])
}

/// `nearest_rank_p`: the same rule with both ends clamped (the Python
/// `min(max(rank, 1), len)`), returning `None` on an empty list rather than
/// refusing. The two exist separately in the Python and are kept separate
/// here so a future mismatch stays visible.
#[must_use]
pub fn nearest_rank_p(sorted_vals: &[f64], q: f64) -> Option<f64> {
    if sorted_vals.is_empty() {
        return None;
    }
    let n = sorted_vals.len();
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "q in [0,1] and n is a slice length"
    )]
    let rank = ((q * n as f64).ceil() as usize).clamp(1, n);
    Some(sorted_vals[rank - 1])
}

/// `weighted_nearest_rank`: exact nearest rank over `(value, weight)` pairs -
/// sort ascending by value, return the first value whose cumulative weight
/// reaches `q * total` (spec 5.2's literal rule, shared by every quantile
/// over histogram mass). The target is an f64 product and the comparison is
/// `cum >= target` in f64, exactly as the Python computes it.
#[must_use]
pub fn weighted_nearest_rank(pairs: &[(i64, i64)], q: f64) -> Option<i64> {
    if pairs.is_empty() {
        return None;
    }
    let mut sorted = pairs.to_vec();
    sorted.sort_unstable();
    let total: i64 = sorted.iter().map(|&(_, w)| w).sum();
    if total <= 0 {
        return None;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "histogram masses stay far below 2^53"
    )]
    let target = q * total as f64;
    let mut cum: i64 = 0;
    for &(v, w) in &sorted {
        cum += w;
        #[expect(clippy::cast_precision_loss, reason = "see above")]
        let cum_f = cum as f64;
        if cum_f >= target {
            return Some(v);
        }
    }
    sorted.last().map(|&(v, _)| v)
}

/// The same rule over `f64` support values (the `trade_range_sqrt_n`
/// histograms of phase 2b use it); kept beside its integer twin so the
/// convention lives in exactly one place.
#[must_use]
pub fn weighted_nearest_rank_f64(pairs: &[(f64, i64)], q: f64) -> Option<f64> {
    if pairs.is_empty() {
        return None;
    }
    let mut sorted = pairs.to_vec();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let total: i64 = sorted.iter().map(|&(_, w)| w).sum();
    if total <= 0 {
        return None;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "histogram masses stay far below 2^53"
    )]
    let target = q * total as f64;
    let mut cum: i64 = 0;
    for &(v, w) in &sorted {
        cum += w;
        #[expect(clippy::cast_precision_loss, reason = "see above")]
        let cum_f = cum as f64;
        if cum_f >= target {
            return Some(v);
        }
    }
    sorted.last().map(|&(v, _)| v)
}

/// `median_or_none`: drop the `None`s, sort ascending, then take the
/// `(n-1)/2`-th order statistic for odd n and the `n/2 - 1`-th for even n -
/// i.e. the ceil(n/2)-th value in both cases, matching
/// `nearest_rank_list(.., 0.5)`. This is not the arithmetic midpoint median:
/// an even-length population takes the lower of the two middles.
#[must_use]
pub fn median_or_none(values: &[Option<f64>]) -> Option<f64> {
    let mut vals: Vec<f64> = values.iter().filter_map(|v| *v).collect();
    if vals.is_empty() {
        return None;
    }
    vals.sort_by(f64::total_cmp);
    let n = vals.len();
    let idx = if n % 2 == 1 { (n - 1) / 2 } else { n / 2 - 1 };
    Some(vals[idx])
}

/// CPython's builtin `sum()` over floats, which is not a naive left fold:
/// since 3.12 it applies the improved Kahan-Babuska (Neumaier) compensated
/// summation and adds the compensation term at the end.
///
/// This is not a refinement the port chose - it is a parity requirement. A
/// naive fold over the 10,000 bootstrap replicates lands several ulps away
/// from the committed artifact's `se`, and through the simultaneous critical
/// value that error reaches every interval in every family. Use this
/// wherever the Python writes `sum(...)` over floats; a Python `+=` loop is
/// a naive fold and must stay one.
#[must_use]
pub fn py_sum(values: impl IntoIterator<Item = f64>) -> f64 {
    let mut sum = 0.0f64;
    let mut c = 0.0f64;
    for x in values {
        let t = sum + x;
        if sum.abs() >= x.abs() {
            c += (sum - t) + x;
        } else {
            c += (x - t) + sum;
        }
        sum = t;
    }
    sum + c
}

/// CPython's `math.fsum`: exact summation via Shewchuk partials, rounded once
/// at the end.
///
/// Distinct from [`py_sum`], and the distinction is load-bearing rather than
/// stylistic. Builtin `sum()` is Neumaier-compensated - very accurate, not
/// exact. `math.fsum` maintains a list of non-overlapping partial sums whose
/// exact total equals the true sum, then rounds that total once, half-even.
/// The two disagree on inputs where compensation is not enough, so a port that
/// routes both through one helper is wrong wherever Python distinguishes them.
/// `analysis/build_fingerprint.py`'s `hour_vol` does exactly that: line 168
/// uses builtin `sum` and line 170 uses `statistics.fmean`, which on 3.14 is
/// `fsum(data) / n`.
///
/// The algorithm is CPython's `math_fsum`: for each input, add it into the
/// running partials with two-sum error tracking, keeping only the nonzero
/// partials. Because every partial is exact and they do not overlap, the final
/// left-to-right fold is the correctly rounded total.
#[must_use]
pub fn py_fsum(values: impl IntoIterator<Item = f64>) -> f64 {
    let mut partials: Vec<f64> = Vec::new();
    for value in values {
        let mut x = value;
        let mut i = 0usize;
        for index in 0..partials.len() {
            let mut y = partials[index];
            if x.abs() < y.abs() {
                core::mem::swap(&mut x, &mut y);
            }
            // Two-sum: `hi` is the rounded sum, `lo` the exact residual.
            let hi = x + y;
            let lo = y - (hi - x);
            if lo != 0.0 {
                partials[i] = lo;
                i += 1;
            }
            x = hi;
        }
        partials.truncate(i);
        partials.push(x);
    }
    // The final rounding is not a fold over the partials. CPython walks them
    // from the largest down, stops at the first inexact addition, and then
    // applies an explicit round-half-even correction: when the residual `lo`
    // and the next partial share a sign, the true total is more than halfway
    // to the next representable value, so doubling `lo` and re-adding it
    // rounds the tie the way exact arithmetic would. A left-to-right fold
    // skips that correction and lands one ulp off on ties.
    let mut n = partials.len();
    if n == 0 {
        return 0.0;
    }
    n -= 1;
    let mut hi = partials[n];
    let mut lo = 0.0f64;
    while n > 0 {
        n -= 1;
        let x = hi;
        let y = partials[n];
        hi = x + y;
        let yr = hi - x;
        lo = y - yr;
        if lo != 0.0 {
            break;
        }
    }
    if n > 0 && ((lo < 0.0 && partials[n - 1] < 0.0) || (lo > 0.0 && partials[n - 1] > 0.0)) {
        let y = lo * 2.0;
        let x = hi + y;
        let yr = x - hi;
        if y == yr {
            hi = x;
        }
    }
    hi
}

/// CPython's `int.__truediv__`: the correctly rounded quotient of two
/// arbitrary-precision integers.
///
/// This is a parity requirement, not a nicety. `a as f64 / b as f64` rounds
/// the numerator to binary64 before dividing, and the fit's pooled
/// `mean_event_duration_s` divides a nanosecond gap sum of order 2e16 - past
/// 2^53 - by an eligible-gap count. That pre-rounding lands one ulp off the
/// committed artifact. Python never pre-rounds: its ints are exact and the
/// division is a single correctly-rounded operation.
///
/// The implementation shifts the numerator until the integer quotient
/// carries at least 54 significant bits, then rounds half-to-even on the
/// remainder, which is exactly what CPython's `long_true_divide` does.
#[must_use]
pub fn py_int_div(n: i64, d: i64) -> f64 {
    assert!(d != 0, "division by zero has no float value");
    let sign = if (n < 0) != (d < 0) { -1.0 } else { 1.0 };
    let n_abs = u128::from(n.unsigned_abs());
    let d_abs = u128::from(d.unsigned_abs());
    if n_abs == 0 {
        return sign * 0.0;
    }
    let bitlen = |x: u128| -> i32 { 128 - i32::try_from(x.leading_zeros()).expect("a bit count") };
    // Scale so the integer quotient carries exactly 53 significant bits;
    // then one half-to-even rounding decision on the exact remainder gives
    // the correctly-rounded result, and the final scaling by a power of two
    // is exact.
    let mut shift = 53 - (bitlen(n_abs) - bitlen(d_abs));
    let (mut q, mut rem, mut den) = (0u128, 0u128, 0u128);
    for _ in 0..3 {
        let sn = n_abs << u32::try_from(shift.max(0)).expect("a shift");
        let sd = d_abs << u32::try_from((-shift).max(0)).expect("a shift");
        q = sn / sd;
        rem = sn % sd;
        den = sd;
        if q >= 1u128 << 53 {
            shift -= 1;
        } else if q < 1u128 << 52 {
            shift += 1;
        } else {
            break;
        }
    }
    if rem * 2 > den || (rem * 2 == den && q % 2 == 1) {
        q += 1;
    }
    sign * (q as f64) * (2f64).powi(-shift)
}

// -- Type-strict canonical serialization ------------------------------------

/// CPython's `repr(float)`: shortest round-trip digits, fixed notation when
/// `-4 < decpt <= 16`, scientific otherwise with a signed two-digit exponent.
#[must_use]
pub fn py_float_repr(x: f64) -> String {
    if x == 0.0 {
        return if x.is_sign_negative() {
            "-0.0".to_string()
        } else {
            "0.0".to_string()
        };
    }
    if x.is_nan() {
        return "nan".to_string();
    }
    if x.is_infinite() {
        return if x < 0.0 {
            "-inf".to_string()
        } else {
            "inf".to_string()
        };
    }
    let neg = x < 0.0;
    let ax = x.abs();
    let sci = format!("{ax:e}");
    let (mantissa, exp_str) = sci.split_once('e').expect("LowerExp always emits 'e'");
    let exp: i32 = exp_str.parse().expect("LowerExp exponent is an integer");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let decpt = exp + 1;
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    // CPython's `format_float_short` in 'r' mode switches to the exponent
    // form when `decpt <= -4 || decpt > 16`. Sixteen, not seventeen: 1e16
    // reprs as `1e+16` while 9.9e15 reprs as `9900000000000000.0`.
    if decpt > -4 && decpt <= 16 {
        if decpt <= 0 {
            out.push_str("0.");
            for _ in 0..(-decpt) {
                out.push('0');
            }
            out.push_str(&digits);
        } else if (decpt as usize) >= digits.len() {
            out.push_str(&digits);
            for _ in 0..(decpt as usize - digits.len()) {
                out.push('0');
            }
            out.push_str(".0");
        } else {
            out.push_str(&digits[..decpt as usize]);
            out.push('.');
            out.push_str(&digits[decpt as usize..]);
        }
    } else {
        out.push(digits.as_bytes()[0] as char);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        let e = decpt - 1;
        out.push('e');
        out.push(if e < 0 { '-' } else { '+' });
        out.push_str(&format!("{:02}", e.abs()));
    }
    out
}

fn escape_py_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c if (c as u32) < 0x80 => out.push(c),
            // `json.dumps` defaults to ensure_ascii=True.
            c => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
    }
    out.push('"');
}

fn canon_node(v: &serde_json::Value, out: &mut String) {
    match v {
        serde_json::Value::Bool(b) => {
            out.push_str("[\"b\", ");
            out.push_str(if *b { "true" } else { "false" });
            out.push(']');
        }
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                out.push_str("[\"i\", ");
                out.push_str(&i.to_string());
                out.push(']');
            } else if let Some(u) = n.as_u64() {
                out.push_str("[\"i\", ");
                out.push_str(&u.to_string());
                out.push(']');
            } else {
                out.push_str("[\"f\", ");
                escape_py_str(&py_float_repr(n.as_f64().unwrap_or(f64::NAN)), out);
                out.push(']');
            }
        }
        serde_json::Value::String(s) => {
            out.push_str("[\"s\", ");
            escape_py_str(s, out);
            out.push(']');
        }
        serde_json::Value::Null => out.push_str("[\"n\"]"),
        serde_json::Value::Array(items) => {
            out.push_str("[\"l\", [");
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                canon_node(item, out);
            }
            out.push_str("]]");
        }
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push_str("[\"d\", [");
            for (i, key) in keys.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push('[');
                escape_py_str(key, out);
                out.push_str(", ");
                canon_node(&map[*key], out);
                out.push(']');
            }
            out.push_str("]]");
        }
    }
}

/// `_typed_canon`: type-strict canonical serialization for equality gates.
/// Python equality treats `1 == True == 1.0`, so a mutation from `1` to
/// `true` escapes a plain `!=`; every leaf carries an explicit type tag and
/// floats compare by `repr`. Byte-identical to the Python
/// `json.dumps(tag(x))` (default `", "` / `": "` separators, ensure_ascii).
#[must_use]
pub fn typed_canon(value: &serde_json::Value) -> String {
    let mut out = String::new();
    canon_node(value, &mut out);
    out
}

/// The first path (dotted / bracketed, `_typed_canon`-style leaf compare) at
/// which two JSON trees differ, or `None` when they are canonically equal.
/// The parity gates need the location, not just the verdict: a per-session
/// diff over a 22-session artifact is otherwise a needle in a haystack.
#[must_use]
pub fn first_canon_difference(
    left: &serde_json::Value,
    right: &serde_json::Value,
    path: &str,
) -> Option<String> {
    canon_differences(left, right, path, 1).into_iter().next()
}

/// Up to `limit` differing paths, in canonical (sorted-key, index) order. The
/// parity gates report a handful rather than one, because a single reported
/// leaf tells you nothing about whether the mismatch is a lone last-ulp
/// float or a whole block off its convention.
#[must_use]
pub fn canon_differences(
    left: &serde_json::Value,
    right: &serde_json::Value,
    path: &str,
    limit: usize,
) -> Vec<String> {
    let mut out = Vec::new();
    collect_differences(left, right, path, limit, &mut out);
    out
}

fn collect_differences(
    left: &serde_json::Value,
    right: &serde_json::Value,
    path: &str,
    limit: usize,
    out: &mut Vec<String>,
) {
    if out.len() >= limit {
        return;
    }
    match (left, right) {
        (serde_json::Value::Object(a), serde_json::Value::Object(b)) => {
            let mut keys: Vec<&String> = a.keys().chain(b.keys()).collect();
            keys.sort();
            keys.dedup();
            for key in keys {
                if out.len() >= limit {
                    return;
                }
                let sub = format!("{path}.{key}");
                match (a.get(key), b.get(key)) {
                    (Some(x), Some(y)) => collect_differences(x, y, &sub, limit, out),
                    (Some(_), None) => out.push(format!("{sub}: only in left")),
                    (None, Some(_)) => out.push(format!("{sub}: only in right")),
                    (None, None) => unreachable!("key came from one of the two maps"),
                }
            }
        }
        (serde_json::Value::Array(a), serde_json::Value::Array(b)) => {
            if a.len() != b.len() {
                out.push(format!("{path}: length {} vs {}", a.len(), b.len()));
                return;
            }
            for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
                if out.len() >= limit {
                    return;
                }
                collect_differences(x, y, &format!("{path}[{i}]"), limit, out);
            }
        }
        _ => {
            let (lc, rc) = (typed_canon(left), typed_canon(right));
            if lc != rc {
                out.push(format!("{path}: {lc} vs {rc}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The discriminator between [`py_sum`] and [`py_fsum`]. Both helpers must
    /// exist because CPython distinguishes builtin `sum` from `math.fsum`, and
    /// this is an input where that distinction is observable - found by search,
    /// not constructed, since Neumaier compensation is good enough to agree
    /// with exact summation across millions of well-conditioned draws.
    ///
    /// Note what is not a discriminator any more: `sum([0.1] * 10)` returns
    /// exactly 1.0 on CPython 3.12 and later, because builtin `sum` became
    /// compensated. The textbook example stopped separating these two.
    #[test]
    fn py_sum_and_py_fsum_disagree_by_one_ulp_on_a_cancelling_input() {
        let values = [
            f64::from_bits(0xc3c0_11d8_ac03_41e7), // -0x1.011d8ac0341e7p+61
            f64::from_bits(0x43da_2149_ecaf_9f0b), //  0x1.a2149ecaf9f0bp+62
            f64::from_bits(0xbc2e_1e05_cd67_c281), // -0x1.e1e05cd67c281p-61
            f64::from_bits(0xbc24_3c30_5483_d94f), // -0x1.43c305483d94fp-61
        ];
        // Reference values taken from CPython 3.14.6 directly.
        assert_eq!(py_sum(values).to_bits(), 0x43d2_185d_96ad_fe18);
        assert_eq!(py_fsum(values).to_bits(), 0x43d2_185d_96ad_fe17);
        assert_ne!(py_sum(values), py_fsum(values));
    }

    /// `math.fsum` is exact regardless of magnitude spread. A naive fold loses
    /// the first `1.0` entirely - it is absorbed into `1e100` and never comes
    /// back - so it returns 1.0 where the true total is 2.0.
    #[test]
    fn py_fsum_recovers_a_term_a_naive_fold_annihilates() {
        assert_eq!(py_fsum([1e100, 1.0, -1e100, 1.0]), 2.0);
        let naive = [1e100, 1.0, -1e100, 1.0].iter().sum::<f64>();
        assert_eq!(naive, 1.0);
    }

    /// The `statistics.fmean` path the fingerprint's `hour_vol` normalization
    /// takes: `fsum(data) / n`, not builtin `sum` over the length.
    #[test]
    fn py_fsum_over_the_count_is_the_fmean_contract() {
        let tenths = [0.1f64; 10];
        assert_eq!(py_fsum(tenths), 1.0);
        assert_eq!(py_fsum(tenths) / 10.0, 0.1);
    }

    #[test]
    fn py_fsum_handles_the_empty_and_single_cases() {
        assert_eq!(py_fsum(std::iter::empty()), 0.0);
        assert_eq!(py_fsum([2.5]), 2.5);
    }

    /// The parity case that forced `py_int_div` into existence: the fit's
    /// pooled `mean_event_duration_s` over eight final seeds. Pre-rounding
    /// the 2.1e16-nanosecond numerator to binary64 lands one ulp off what
    /// CPython's exact int/int returns.
    #[test]
    fn py_int_div_is_correctly_rounded_past_two_to_the_fifty_third() {
        // CPython: 20222644286440873 / 353454709 == 57214244.913174644,
        // while float(n)/float(d) == 57214244.91317464 - one ulp low.
        let n = 20_222_644_286_440_873i64;
        let d = 353_454_709i64;
        assert_eq!(py_float_repr(n as f64 / d as f64), "57214244.91317464");
        assert_eq!(py_float_repr(py_int_div(n, d)), "57214244.913174644");
    }

    #[test]
    fn py_int_div_agrees_with_plain_division_on_small_operands() {
        for (n, d) in [(1i64, 3i64), (7, 2), (-7, 2), (10, 5), (0, 9), (123, 456)] {
            assert_eq!(py_int_div(n, d), n as f64 / d as f64, "{n}/{d}");
        }
    }

    #[test]
    fn splitmix_and_tuple_mix_match_the_frozen_vectors() {
        // The vectors the Python harness selftest and the mogwai-cli twin
        // both pin.
        assert_eq!(splitmix64(0), 0xE220_A839_7B1D_CDAF);
        assert_eq!(splitmix64(1), 0x910A_2DEC_8902_5CC1);
        assert_eq!(tuple_mix(7, &[1, 2]), splitmix64(splitmix64(7 ^ 1) ^ 2));
        assert_ne!(tuple_mix(7, &[2, 1]), tuple_mix(7, &[1, 2]));
        assert_eq!(tuple_mix(7, &[]), 7);
    }

    #[test]
    fn fisher_yates_matches_the_python_step_order() {
        // The Python reference, transcribed: state advances first, then
        // j = state % (i+1), then swap.
        let mut values = vec![0usize, 1, 2, 3, 4];
        let mut expect = values.clone();
        let mut state = 12_345u64;
        for i in (1..expect.len()).rev() {
            state = splitmix64(state);
            let j = (state % (i as u64 + 1)) as usize;
            expect.swap(i, j);
        }
        fisher_yates(&mut values, 12_345);
        assert_eq!(values, expect);
        // A one-element or empty list is untouched and must not underflow.
        let mut one = vec![9u8];
        fisher_yates(&mut one, 1);
        assert_eq!(one, vec![9]);
        let mut none: Vec<u8> = Vec::new();
        fisher_yates(&mut none, 1);
        assert!(none.is_empty());
    }

    #[test]
    fn nearest_rank_conventions_are_ceil_based_and_one_clamped() {
        let v = [1.0, 2.0, 3.0, 4.0];
        assert_eq!(nearest_rank_list(&v, 0.0), Some(1.0)); // rank clamps to 1
        assert_eq!(nearest_rank_list(&v, 0.25), Some(1.0));
        assert_eq!(nearest_rank_list(&v, 0.26), Some(2.0)); // ceil, not round
        assert_eq!(nearest_rank_list(&v, 0.50), Some(2.0));
        assert_eq!(nearest_rank_list(&v, 1.0), Some(4.0));
        assert_eq!(nearest_rank_p(&v, 1.0), Some(4.0));
        assert_eq!(nearest_rank_p(&[], 0.5), None);
    }

    #[test]
    fn weighted_nearest_rank_is_first_cumulative_reach() {
        // 5.2's literal rule: the first value whose cumulative weight
        // reaches q * total, with `>=` (not `>`).
        let pairs = [(0i64, 90i64), (1, 9), (2, 1)];
        assert_eq!(weighted_nearest_rank(&pairs, 0.90), Some(0));
        assert_eq!(weighted_nearest_rank(&pairs, 0.99), Some(1));
        assert_eq!(weighted_nearest_rank(&pairs, 0.999), Some(2));
        assert_eq!(weighted_nearest_rank(&[], 0.5), None);
        assert_eq!(weighted_nearest_rank(&[(3, 0)], 0.5), None);
        // Unsorted input sorts first.
        assert_eq!(
            weighted_nearest_rank(&[(2, 1), (0, 90), (1, 9)], 0.90),
            Some(0)
        );
    }

    #[test]
    fn median_or_none_takes_the_lower_middle_on_even_n() {
        assert_eq!(median_or_none(&[Some(1.0), Some(2.0)]), Some(1.0));
        assert_eq!(
            median_or_none(&[Some(1.0), Some(2.0), Some(3.0)]),
            Some(2.0)
        );
        // The None drops out first, so this is an odd population of three.
        assert_eq!(
            median_or_none(&[Some(3.0), None, Some(1.0), Some(2.0)]),
            Some(2.0)
        );
        assert_eq!(median_or_none(&[None, None]), None);
        // The frozen equivalence with nearest_rank_list(0.5).
        for n in 1..12usize {
            let vals: Vec<f64> = (0..n).map(|i| i as f64).collect();
            let opts: Vec<Option<f64>> = vals.iter().map(|v| Some(*v)).collect();
            assert_eq!(
                median_or_none(&opts),
                nearest_rank_list(&vals, 0.5),
                "n={n}"
            );
        }
    }

    #[test]
    fn typed_canon_separates_one_true_and_one_point_zero() {
        let one: serde_json::Value = serde_json::from_str("1").unwrap();
        let tru: serde_json::Value = serde_json::from_str("true").unwrap();
        let onef: serde_json::Value = serde_json::from_str("1.0").unwrap();
        assert_eq!(typed_canon(&one), "[\"i\", 1]");
        assert_eq!(typed_canon(&tru), "[\"b\", true]");
        assert_eq!(typed_canon(&onef), "[\"f\", \"1.0\"]");
        assert_ne!(typed_canon(&one), typed_canon(&tru));
        assert_ne!(typed_canon(&one), typed_canon(&onef));
        // Null, nesting and key sorting.
        let v: serde_json::Value = serde_json::from_str(r#"{"b":null,"a":[1,2.5]}"#).unwrap();
        assert_eq!(
            typed_canon(&v),
            "[\"d\", [[\"a\", [\"l\", [[\"i\", 1], [\"f\", \"2.5\"]]]], [\"b\", [\"n\"]]]]"
        );
        // Key order never matters; key set does.
        let a: serde_json::Value = serde_json::from_str(r#"{"x":1,"y":2}"#).unwrap();
        let b: serde_json::Value = serde_json::from_str(r#"{"y":2,"x":1}"#).unwrap();
        assert_eq!(typed_canon(&a), typed_canon(&b));
    }

    #[test]
    fn json_float_parsing_is_correctly_rounded() {
        // The `float_roundtrip` feature on serde_json (workspace Cargo.toml)
        // is load-bearing for every parity gate: without it serde_json's fast
        // float parser lands one ULP off the correctly rounded value for
        // inputs like these, and the gate reports mismatches that exist only
        // in the reader. Each literal below is a real block-2/block-3 value
        // from the committed 12a records that the default parser gets wrong.
        for s in [
            "12.344540150041679",
            "0.00011171621088374059",
            "0.015004167824395665",
            "-0.15580499894864477",
        ] {
            let parsed: serde_json::Value = serde_json::from_str(s).expect("valid JSON number");
            let via_json = parsed.as_f64().expect("a float");
            let via_std: f64 = s.parse().expect("a float");
            assert_eq!(via_json.to_bits(), via_std.to_bits(), "{s}");
            assert_eq!(py_float_repr(via_json), s, "{s}");
        }
    }

    #[test]
    fn py_sum_is_compensated_and_a_naive_fold_is_not() {
        // The textbook exhibit: a naive left fold loses the two ones
        // entirely, CPython's `sum` does not. Verified against
        // `sum([1.0, 1e100, 1.0, -1e100])` on the reference interpreter.
        let values = [1.0, 1e100, 1.0, -1e100];
        assert_eq!(py_sum(values), 2.0);
        assert_eq!(values.iter().sum::<f64>(), 0.0);
        // Exact arithmetic is untouched, and the empty sum is zero.
        assert_eq!(py_sum([0.5, 0.25, 0.125]), 0.875);
        assert_eq!(py_sum([]), 0.0);
        assert_eq!(py_sum([1.5]), 1.5);
    }

    #[test]
    fn py_float_repr_matches_cpython_shapes() {
        assert_eq!(py_float_repr(1.0), "1.0");
        assert_eq!(py_float_repr(0.1), "0.1");
        assert_eq!(py_float_repr(-0.0), "-0.0");
        assert_eq!(py_float_repr(1e-5), "1e-05");
        assert_eq!(py_float_repr(1e17), "1e+17");
        assert_eq!(py_float_repr(1e16), "1e+16");
        assert_eq!(py_float_repr(0.0001), "0.0001");
        assert_eq!(py_float_repr(1.5e-7), "1.5e-07");
        assert_eq!(py_float_repr(3.55e-15), "3.55e-15");
    }

    #[test]
    fn first_canon_difference_points_at_the_leaf() {
        let a: serde_json::Value = serde_json::from_str(r#"{"k":[1,{"z":2}]}"#).unwrap();
        let b: serde_json::Value = serde_json::from_str(r#"{"k":[1,{"z":3}]}"#).unwrap();
        assert_eq!(
            first_canon_difference(&a, &b, "r"),
            Some("r.k[1].z: [\"i\", 2] vs [\"i\", 3]".to_string())
        );
        assert_eq!(first_canon_difference(&a, &a, "r"), None);
    }
}
