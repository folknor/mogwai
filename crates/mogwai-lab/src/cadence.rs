// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Cadence synthesis over raw Binance trade archives
//! (`analysis/build_cadence.py`, `analysis/probe_binance_trades.py`): the
//! parent/child event-grouping probe, `PAIRS`/`WIDEN`/`band()`, and the
//! parent/child shape solve. `probe_binance_trades.py`/
//! `probe_binance_aggtrades.py` themselves stay Python (KEEP-class,
//! `pair_harness.py`'s serving pair); this module ports only the internal
//! machinery `build_cadence.py` needs: `EventStats` and its byte-line
//! Binance-trades `probe`.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use serde_json::{Value, json};

use crate::characterize::AutoCorr;
use crate::error::{LabError, LabResult};

pub const PAIRS: [&str; 3] = ["BTCUSDT", "ETHUSDT", "SOLUSDT"];
pub const WIDEN: f64 = 1.5;

/// `EventStats`: infers parent match events by consecutive-row grouping,
/// either `(timestamp, side)` (`with_side = true`, the primary rule) or
/// `timestamp` only (matching the aggTrades probe).
pub struct EventStats {
    with_side: bool,
    key_stamp: Option<i64>,
    key_side: bool,
    count: i64,
    prices: std::collections::HashSet<Vec<u8>>,
    events: i64,
    single: i64,
    single_level: i64,
    children_sum: i64,
    // Insertion-ordered like the Python dict: only sorted-key reads happen
    // downstream (the p95 rank walk), so a HashMap is safe here, unlike
    // characterize.rs's tick_counts modal tie.
    children_hist: BTreeMap<i64, i64>,
    children_max: i64,
    levels_sum: i64,
    prev_time: Option<i64>,
    gaps: i64,
    gap_sum: f64,
    gap_sumsq: f64,
    gap_acf: AutoCorr,
    subsecond_distinct_gaps: i64,
    subsecond_gap_sum_us: i64,
}

impl EventStats {
    #[must_use]
    pub fn new(with_side: bool) -> Self {
        Self {
            with_side,
            key_stamp: None,
            key_side: false,
            count: 0,
            prices: std::collections::HashSet::new(),
            events: 0,
            single: 0,
            single_level: 0,
            children_sum: 0,
            children_hist: BTreeMap::new(),
            children_max: 0,
            levels_sum: 0,
            prev_time: None,
            gaps: 0,
            gap_sum: 0.0,
            gap_sumsq: 0.0,
            gap_acf: AutoCorr::new(5),
            subsecond_distinct_gaps: 0,
            subsecond_gap_sum_us: 0,
        }
    }

    fn close(&mut self) {
        let Some(key_stamp) = self.key_stamp else {
            return;
        };
        self.events += 1;
        self.children_sum += self.count;
        self.children_max = self.children_max.max(self.count);
        *self.children_hist.entry(self.count).or_insert(0) += 1;
        let levels = self.prices.len() as i64;
        self.levels_sum += levels;
        self.single += i64::from(self.count == 1);
        self.single_level += i64::from(levels == 1);
        if let Some(prev_time) = self.prev_time {
            let gap_us = key_stamp - prev_time;
            #[expect(
                clippy::cast_precision_loss,
                reason = "microsecond gaps fit f64 exactly"
            )]
            let gap = gap_us as f64 / 1_000_000.0;
            self.gaps += 1;
            self.gap_sum += gap;
            self.gap_sumsq += gap * gap;
            self.gap_acf.push(gap);
            if gap_us > 0 && gap_us < 1_000_000 {
                self.subsecond_distinct_gaps += 1;
                self.subsecond_gap_sum_us += gap_us;
            }
        }
        self.prev_time = Some(key_stamp);
    }

    pub fn push(&mut self, stamp: i64, side: bool, price: &[u8]) {
        if Some(stamp) != self.key_stamp || (self.with_side && side != self.key_side) {
            self.close();
            self.key_stamp = Some(stamp);
            self.key_side = side;
            self.count = 0;
            self.prices.clear();
        }
        self.count += 1;
        self.prices.insert(price.to_vec());
    }

    #[must_use]
    pub fn report(mut self) -> Value {
        self.close();
        let mean_gap = self.gap_sum / self.gaps as f64;
        let var_gap = self.gap_sumsq / self.gaps as f64 - mean_gap * mean_gap;
        #[expect(clippy::cast_precision_loss, reason = "events is a bounded row count")]
        let rank95 = (0.95 * self.events as f64).ceil() as i64;
        let mut seen = 0i64;
        let mut p95 = 0i64;
        for (&count, &freq) in &self.children_hist {
            seen += freq;
            if seen >= rank95 {
                p95 = count;
                break;
            }
        }
        let acf = self.gap_acf.acf();
        let acf1 = acf.first().copied().unwrap_or(0.0);
        let acf5 = acf.get(4).copied().unwrap_or(0.0);
        json!({
            "events": self.events,
            "children": {
                "mean": self.children_sum as f64 / self.events as f64,
                "single_frac": self.single as f64 / self.events as f64,
                "p95": p95,
                "max": self.children_max,
            },
            "levels": {
                "mean": self.levels_sum as f64 / self.events as f64,
                "single_frac": self.single_level as f64 / self.events as f64,
            },
            "parent_gap": {
                "mean_s": mean_gap,
                "var_over_mean": var_gap / mean_gap,
                "cv2": var_gap / (mean_gap * mean_gap),
                "acf_lag1": acf1,
                "acf_lag5": acf5,
            },
            "subsecond_distinct_gap_mean_us": if self.subsecond_distinct_gaps != 0 {
                Some(self.subsecond_gap_sum_us as f64 / self.subsecond_distinct_gaps as f64)
            } else {
                None
            },
        })
    }
}

/// `_byte_lines`: newline-split raw byte lines from a binary reader, no
/// per-row UTF-8 decode.
struct ByteLines<R> {
    reader: R,
    buf: Vec<u8>,
    pos: usize,
    eof: bool,
}

impl<R: Read> ByteLines<R> {
    fn new(reader: R) -> Self {
        Self {
            reader,
            buf: Vec::new(),
            pos: 0,
            eof: false,
        }
    }
}

impl<R: Read> Iterator for ByteLines<R> {
    type Item = Vec<u8>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(rel) = self.buf[self.pos..].iter().position(|&b| b == b'\n') {
                let end = self.pos + rel;
                let line = self.buf[self.pos..end].to_vec();
                self.pos = end + 1;
                return Some(line);
            }
            if self.eof {
                if self.pos < self.buf.len() {
                    let line = self.buf[self.pos..].to_vec();
                    self.pos = self.buf.len();
                    return Some(line);
                }
                return None;
            }
            // Compact and refill.
            self.buf.drain(..self.pos);
            self.pos = 0;
            let mut chunk = vec![0u8; 1 << 22];
            let n = self.reader.read(&mut chunk).unwrap_or(0);
            if n == 0 {
                self.eof = true;
            } else {
                self.buf.extend_from_slice(&chunk[..n]);
            }
        }
    }
}

/// `probe`: streams a Binance raw-trades archive and infers parent match
/// events under both grouping rules, matching `probe_binance_trades.py`'s
/// `probe`.
///
/// # Errors
/// Propagates zip/I/O failure or a malformed archive (no member, or a row
/// that fails to parse as expected).
pub fn probe(path: &Path) -> LabResult<Value> {
    let file = std::fs::File::open(path)?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| LabError::refusal(e.to_string()))?;
    let info_size = archive
        .by_index(0)
        .map_err(|e| LabError::refusal(e.to_string()))?
        .size();
    let stream = archive
        .by_index(0)
        .map_err(|e| LabError::refusal(e.to_string()))?;

    let mut primary = EventStats::new(true);
    let mut timestamp = EventStats::new(false);
    let mut rows: i64 = 0;
    let mut quote = 0.0f64;
    let mut first: Option<i64> = None;
    let mut last: Option<i64> = None;
    let mut second_counts: BTreeMap<i64, i64> = BTreeMap::new();

    for line in ByteLines::new(stream) {
        let line: &[u8] = if line.last() == Some(&b'\r') {
            &line[..line.len() - 1]
        } else {
            &line
        };
        if line.is_empty() {
            continue;
        }
        let row: Vec<&[u8]> = line.split(|&b| b == b',').collect();
        if row.is_empty() {
            continue;
        }
        let first_field = row[0];
        let digits = if first_field.first() == Some(&b'-') {
            &first_field[1..]
        } else {
            first_field
        };
        if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
            continue;
        }
        if row.len() < 6 {
            return Err(LabError::refusal(
                "malformed Binance trades row: fewer than 6 fields",
            ));
        }
        let price = row[1];
        let stamp: i64 = std::str::from_utf8(row[4])
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| LabError::refusal("malformed Binance trades timestamp field"))?;
        let side_text = row[5];
        let side = side_text == b"True" || side_text == b"true";
        rows += 1;
        quote += std::str::from_utf8(row[3])
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .ok_or_else(|| LabError::refusal("malformed Binance trades quote field"))?;
        if first.is_none() {
            first = Some(stamp);
        }
        last = Some(stamp);
        let second = stamp.div_euclid(1_000_000);
        *second_counts.entry(second).or_insert(0) += 1;
        primary.push(stamp, side, price);
        timestamp.push(stamp, side, price);
    }

    let first = first.ok_or_else(|| LabError::refusal("empty Binance trades archive"))?;
    let last = last.expect("last set alongside first");
    let span_seconds = (last.div_euclid(1_000_000) - first.div_euclid(1_000_000)) + 1;
    let mut histogram: BTreeMap<i64, i64> = BTreeMap::new();
    for &value in second_counts.values() {
        *histogram.entry(value).or_insert(0) += 1;
    }
    let missing = span_seconds - second_counts.len() as i64;
    histogram.insert(0, missing);
    #[expect(
        clippy::cast_precision_loss,
        reason = "span_seconds is a bounded per-archive second count"
    )]
    let rank50 = (0.50 * span_seconds as f64).ceil() as i64;
    #[expect(clippy::cast_precision_loss, reason = "same bound as above")]
    let rank95 = (0.95 * span_seconds as f64).ceil() as i64;
    let mut seen = 0i64;
    let (mut median, mut p95) = (0i64, 0i64);
    for (&count, &freq) in &histogram {
        seen += freq;
        if seen >= rank50 && median == 0 {
            median = count;
        }
        if seen >= rank95 {
            p95 = count;
            break;
        }
    }

    Ok(json!({
        "rows": rows,
        "bytes": info_size,
        "span_days": span_seconds as f64 / 86400.0,
        "raw_fills_per_second": rows as f64 / span_seconds as f64,
        "mean_notional": quote / rows as f64,
        "per_second_counts": {
            "mean": rows as f64 / span_seconds as f64,
            "median": median,
            "p95": p95,
            "zero_frac": missing as f64 / span_seconds as f64,
        },
        "timestamp_and_side": primary.report(),
        "timestamp_only": timestamp.report(),
    }))
}

/// `band`: per-pair extremes widened by `WIDEN` on each side, floor-clamped
/// on the low edge.
#[must_use]
pub fn band(values: &[f64], floor: Option<f64>) -> Value {
    let low = values.iter().copied().fold(f64::INFINITY, f64::min) / WIDEN;
    let high = values.iter().copied().fold(f64::NEG_INFINITY, f64::max) * WIDEN;
    let low = floor.map_or(low, |f| f.max(low));
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    json!({"min": low, "median": sorted[sorted.len() / 2], "max": high})
}

/// `solve_shape`: the parent/child geometric-mixture shape solve.
#[must_use]
pub fn solve_shape(children_mean: f64, single_frac: f64, levels_mean: f64) -> Value {
    let fallback = single_frac < 1.0 / children_mean;
    let (q, m) = if fallback {
        (0.0, children_mean)
    } else {
        let m = (children_mean - 1.0) / (1.0 - single_frac);
        let q = 1.0 - (children_mean - 1.0) / (m - 1.0);
        (q, m)
    };
    json!({
        "q": q,
        "m": m,
        "level_step_prob": (levels_mean - 1.0) / (children_mean - 1.0),
        "fallback_pure_geometric": fallback,
    })
}

const FLOORS: &[(&str, f64)] = &[
    ("duration_dispersion_cv2", 1.0),
    ("duration_acf_lag1", 1e-6),
    ("duration_acf_lag5", 0.0),
    ("levels_mean", 1.0),
    ("mean_event_duration_s", 1e-9),
    ("children_mean", 1.0 + 1e-9),
    ("children_single_frac", 0.0),
    ("mean_trade_notional", 1e-9),
];

/// `build`: synthesizes `cadence.json` from the archives under
/// `data_dir/<PAIR>-trades-2026-06.zip` for `PAIRS`, matching
/// `build_cadence.py`'s `build()` (minus the `provenance.generated_utc`
/// timestamp, which is intentionally excluded here - a caller that writes
/// the artifact stamps it itself, keeping this function pure/deterministic
/// for the parity gate).
///
/// # Errors
/// [`LabError::Refusal`] if an expected archive is missing; propagates
/// zip/I/O/JSON failure.
pub fn build(data_dir: &Path) -> LabResult<Value> {
    let mut reports: BTreeMap<&str, Value> = BTreeMap::new();
    let mut archives = Vec::new();
    for &pair in &PAIRS {
        let path = data_dir.join(format!("{pair}-trades-2026-06.zip"));
        if !path.exists() {
            return Err(LabError::refusal(format!("{}", path.display())));
        }
        let report = probe(&path)?;
        let meta = std::fs::metadata(&path)?;
        archives.push(json!({
            "name": path.file_name().unwrap().to_string_lossy(),
            "bytes": meta.len(),
            "rows": report["rows"],
            "span_days": report["span_days"],
        }));
        reports.insert(pair, report);
    }
    let primary: BTreeMap<&str, Value> = PAIRS
        .iter()
        .map(|&p| (p, reports[p]["timestamp_and_side"].clone()))
        .collect();

    let mut targets = serde_json::Map::new();
    let field_values: [(&str, Vec<f64>); 8] = [
        (
            "mean_event_duration_s",
            PAIRS
                .iter()
                .map(|&p| primary[p]["parent_gap"]["mean_s"].as_f64().unwrap())
                .collect(),
        ),
        (
            "children_mean",
            PAIRS
                .iter()
                .map(|&p| primary[p]["children"]["mean"].as_f64().unwrap())
                .collect(),
        ),
        (
            "children_single_frac",
            PAIRS
                .iter()
                .map(|&p| primary[p]["children"]["single_frac"].as_f64().unwrap())
                .collect(),
        ),
        (
            "levels_mean",
            PAIRS
                .iter()
                .map(|&p| primary[p]["levels"]["mean"].as_f64().unwrap())
                .collect(),
        ),
        (
            "mean_trade_notional",
            PAIRS
                .iter()
                .map(|&p| reports[p]["mean_notional"].as_f64().unwrap())
                .collect(),
        ),
        (
            "duration_dispersion_cv2",
            PAIRS
                .iter()
                .map(|&p| primary[p]["parent_gap"]["cv2"].as_f64().unwrap())
                .collect(),
        ),
        (
            "duration_acf_lag1",
            PAIRS
                .iter()
                .map(|&p| primary[p]["parent_gap"]["acf_lag1"].as_f64().unwrap())
                .collect(),
        ),
        (
            "duration_acf_lag5",
            PAIRS
                .iter()
                .map(|&p| primary[p]["parent_gap"]["acf_lag5"].as_f64().unwrap())
                .collect(),
        ),
    ];
    for (name, values) in &field_values {
        let floor = FLOORS.iter().find(|(n, _)| *n == *name).map(|(_, f)| *f);
        targets.insert(
            (*name).to_string(),
            json!({"anchor": values[0], "range": band(values, floor)}),
        );
    }
    targets.insert(
        "per_second_counts".to_string(),
        reports["BTCUSDT"]["per_second_counts"].clone(),
    );

    let anchor = &primary["BTCUSDT"];
    Ok(json!({
        "provenance": {"archives": archives},
        "anchor": "BTCUSDT",
        "pairs": reports,
        "targets": Value::Object(targets),
        "shape": solve_shape(
            anchor["children"]["mean"].as_f64().unwrap(),
            anchor["children"]["single_frac"].as_f64().unwrap(),
            anchor["levels"]["mean"].as_f64().unwrap(),
        ),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_grouping_rules_are_distinct() {
        let mut side = EventStats::new(true);
        let mut stamp = EventStats::new(false);
        for &(ts, taker_side, price) in &[
            (10i64, false, b"1".as_slice()),
            (10, true, b"2"),
            (11, true, b"2"),
        ] {
            side.push(ts, taker_side, price);
            stamp.push(ts, taker_side, price);
        }
        assert_eq!(side.report()["events"].as_i64().unwrap(), 3);
        assert_eq!(stamp.report()["events"].as_i64().unwrap(), 2);
    }

    #[test]
    fn mixture_solution_and_fallback() {
        let mixed = solve_shape(5.0, 0.5, 2.0);
        assert!(!mixed["fallback_pure_geometric"].as_bool().unwrap());
        assert!((mixed["m"].as_f64().unwrap() - 8.0).abs() < 1e-9);
        let fallback = solve_shape(5.0, 0.1, 2.0);
        assert!(fallback["fallback_pure_geometric"].as_bool().unwrap());
        assert_eq!(fallback["q"].as_f64().unwrap(), 0.0);
        assert_eq!(fallback["m"].as_f64().unwrap(), 5.0);
    }
}
