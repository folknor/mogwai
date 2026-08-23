// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Aggregation and inference over the protocol-12a per-session records
//! (the retired rewrite plan, phase 2b): the monthly pooling, the
//! vote-caching [`context::ObsContext`], the fixed-seed bootstrap, the
//! Amendment-D family envelopes, the 5.2 count substitution, the gap
//! closures, the forensic subchecks and the fail-closed 6.2 ladder.
//!
//! The Python reference is `analysis/mnq_fit.py` and the binding contract is
//! `notes/protocol-12a-measurement-spec.md` revision 12. Where the two
//! disagree the Python wins - it produced the committed
//! `analysis/mnq-measure-12a.json` that the parity gate compares against.
//!
//! ## What this module owns, and what it does not
//!
//! Everything here is a pure function of the cached per-session records: the
//! observed records (`analysis/out/mnq-measure12a-observed.json`), the eight
//! generated walk records (`analysis/out/measure12a-cache/`) and the
//! bootstrap multiplicities derived from the session count. Artifact
//! assembly - `binding`, `constants`, `cost`, `diagnostics.refused_cells`,
//! `diagnostics.empty_bins` and the schema/semantic validators - is phase
//! 2c's; the outputs below are shaped so that assembly is a paste, never a
//! re-derivation. In particular [`ladder::LadderOutcome`] carries the
//! envelope and rung refusal records in the order 2c must collect them.
//!
//! ## Why every statistic is a function of a multiplicity vector
//!
//! The point estimate (all ones), each of the 10,000 bootstrap replicates
//! and each leave-one-ISO-week-out fold run the same `stat_fn(ctx, mult)`
//! code path, so a resampling convention cannot drift from the point
//! convention. That design is also what makes 10,000 replicates tractable:
//! `ObsContext` extracts each cell's per-session votes (or a cumulative
//! count array over a shared quantile support) from the JSON exactly once
//! and caches it, so a replicate costs a weighted median or a binary search
//! rather than a re-walk of the records.

use serde_json::Value;

pub mod artifact;
pub mod assemble;
pub mod bootstrap;
pub mod context;
pub mod countsub;
pub mod family;
pub mod ladder;
pub mod monthly;

#[cfg(test)]
mod tests;

// -- Small JSON accessors ---------------------------------------------------
//
// The records are read back from committed JSON, so every access is a
// contract read: a shape violation is a defect in the producer, not data, and
// panicking names the exact field. The Python indexes the same fields with
// `[]` and raises KeyError identically.

/// An integer field. Python holds these as `int`; a float here would be a
/// producer defect, so the read refuses rather than truncating.
#[must_use]
pub(crate) fn ji(v: &Value, key: &str) -> i64 {
    v.get(key)
        .and_then(Value::as_i64)
        .unwrap_or_else(|| panic!("{key} is missing or not an integer in {v}"))
}

/// A nullable float field, read as `Option<f64>`. An integer JSON number
/// widens (the Python arithmetic is identical either way).
#[must_use]
pub(crate) fn jf_opt(v: &Value, key: &str) -> Option<f64> {
    match v.get(key) {
        None | Some(Value::Null) => None,
        Some(n) => Some(
            n.as_f64()
                .unwrap_or_else(|| panic!("{key} is not a number in {v}")),
        ),
    }
}

/// A nullable integer field.
#[must_use]
pub(crate) fn ji_opt(v: &Value, key: &str) -> Option<i64> {
    match v.get(key) {
        None | Some(Value::Null) => None,
        Some(n) => Some(
            n.as_i64()
                .unwrap_or_else(|| panic!("{key} is not an integer in {v}")),
        ),
    }
}

/// A string field.
#[must_use]
pub(crate) fn js<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{key} is missing or not a string in {v}"))
}

/// An array field.
#[must_use]
pub(crate) fn ja<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key).and_then(Value::as_array).map_or_else(
        || panic!("{key} is missing or not an array in {v}"),
        Vec::as_slice,
    )
}

/// `Some(f)` when the JSON value is a finite-or-not number, `None` for
/// `null` - the Python `x is None` test, kept separate from finiteness.
#[must_use]
pub(crate) fn opt_num(v: Option<&Value>) -> Option<f64> {
    match v {
        None | Some(Value::Null) => None,
        Some(n) => n.as_f64(),
    }
}

/// `Option<f64>` to JSON, as the Python emits it: `None` becomes `null` and
/// a value becomes a float leaf. Never an integer leaf - the typed-canonical
/// comparator separates `1` from `1.0`.
#[must_use]
pub(crate) fn jnum(v: Option<f64>) -> Value {
    match v {
        Some(x) => serde_json::json!(x),
        None => Value::Null,
    }
}

/// `Option<i64>` to JSON, preserving the integer leaf type.
#[must_use]
pub(crate) fn jint(v: Option<i64>) -> Value {
    match v {
        Some(x) => serde_json::json!(x),
        None => Value::Null,
    }
}

/// `Option<bool>` to JSON.
#[must_use]
pub(crate) fn jbool(v: Option<bool>) -> Value {
    match v {
        Some(x) => Value::Bool(x),
        None => Value::Null,
    }
}

/// One `RefusalRec`: the three-field record the whole 12a artifact refuses
/// with (spec section 10). Kept as a struct rather than a `Value` so the
/// deduplicating collection 2c performs can key on the triple.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RefusalRec {
    pub scope: String,
    pub cell: String,
    pub reason: String,
}

impl RefusalRec {
    pub fn new(
        scope: impl Into<String>,
        cell: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            scope: scope.into(),
            cell: cell.into(),
            reason: reason.into(),
        }
    }

    #[must_use]
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "scope": self.scope,
            "cell": self.cell,
            "reason": self.reason,
        })
    }
}

/// `stdev_ddof1`: the sample standard deviation over the finite values,
/// `None` below two of them. Non-finite entries are dropped here; every
/// caller has already refused on them, so the filter is belt and braces.
#[must_use]
pub fn stdev_ddof1(values: &[Option<f64>]) -> Option<f64> {
    let vals: Vec<f64> = values
        .iter()
        .filter_map(|v| *v)
        .filter(|v| v.is_finite())
        .collect();
    if vals.len() < 2 {
        return None;
    }
    #[expect(clippy::cast_precision_loss, reason = "populations stay tiny")]
    let n = vals.len() as f64;
    // Both sums are CPython `sum()` calls, so both are compensated.
    let mean = crate::kernel::py_sum(vals.iter().copied()) / n;
    let var = crate::kernel::py_sum(vals.iter().map(|v| (v - mean).powi(2))) / (n - 1.0);
    Some(var.sqrt())
}
