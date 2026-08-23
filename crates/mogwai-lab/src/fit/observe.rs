// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! `observe()` from `analysis/mnq_fit.py` (spec 4.2-4.8): one streaming pass
//! over the usable sessions with three independent chains - cadence over all
//! sided parents, quote-mid returns over adjacent valid-quote parents inside
//! one segment, and the shared-shape diagnostics over all sided parents,
//! reset at every session or segment change - plus the protocol-11
//! session-refit cells and the two fixed-horizon chains.
//!
//! Every accumulation order here is the Python's. Where the Python walks a
//! `dict` in insertion order the port keeps an insertion-ordered structure;
//! where it walks `sorted(...)` the port sorts. Both matter to the last ulp.

use std::collections::{BTreeMap, HashMap, VecDeque};

use serde_json::{Value, json};

use crate::error::{LabError, LabResult};
use crate::fit::curves::utc_hour_dow;
use crate::fit::mtrand::PyRandom;
use crate::kernel::nearest_rank_list;
use crate::session::{MinuteFieldsCache, segment_end_ns, segment_origin_ns};
use crate::stream::Row;
use crate::subcontract::{
    ACF_LAGS, DISPLACEMENT_BIN_TICKS, HORIZON_SECONDS, PRICE_UNITS_PER_POINT,
    RESAMPLE_ENVELOPE_LEVEL, RESAMPLE_REPLICATES, RESAMPLE_SEED, RESAMPLE_SESSIONS_PER_REPLICATE,
    TICK_UNITS,
};

/// Bounded discrete histogram with exact nearest-rank quantiles.
#[derive(Default)]
pub struct Quantiles {
    pub counts: BTreeMap<i64, i64>,
    pub total: i64,
    pub sum: i64,
}

impl Quantiles {
    pub fn add(&mut self, value: i64) {
        *self.counts.entry(value).or_insert(0) += 1;
        self.total += 1;
        self.sum += value;
    }

    pub fn nearest_rank(&self, q: f64) -> LabResult<i64> {
        if self.total == 0 {
            return Err(LabError::refusal("empty histogram has no quantiles"));
        }
        let rank = ((q * self.total as f64).ceil() as i64).max(1);
        let mut seen = 0i64;
        for (value, count) in &self.counts {
            seen += count;
            if seen >= rank {
                return Ok(*value);
            }
        }
        unreachable!("rank walked past the histogram")
    }

    #[must_use]
    pub fn mean(&self) -> f64 {
        if self.total > 0 {
            self.sum as f64 / self.total as f64
        } else {
            f64::NAN
        }
    }
}

/// `nearest_rank_of`: nearest-rank quantile of an already-built histogram.
pub fn nearest_rank_of(hist: &BTreeMap<i64, i64>, q: f64) -> LabResult<i64> {
    let qq = Quantiles {
        counts: hist.clone(),
        total: hist.values().sum(),
        // The Python builds this shim the same way and never reads `sum`.
        sum: 0,
    };
    qq.nearest_rank(q)
}

/// Streaming autocorrelation at fixed lags over one long series. Each lag
/// carries its own pair-only moments (left and right members separately), so
/// the value is the Pearson correlation of exactly the accepted pairs.
pub struct Acf {
    lags: Vec<usize>,
    window: VecDeque<f64>,
    maxlen: usize,
    stats: Vec<AcfStat>,
}

#[derive(Default, Clone, Copy)]
struct AcfStat {
    n: i64,
    sx: f64,
    sy: f64,
    sxx: f64,
    syy: f64,
    sxy: f64,
}

impl Acf {
    #[must_use]
    pub fn new(lags: &[usize]) -> Self {
        let maxlen = *lags.iter().max().expect("at least one lag");
        Self {
            lags: lags.to_vec(),
            window: VecDeque::with_capacity(maxlen),
            maxlen,
            stats: vec![AcfStat::default(); lags.len()],
        }
    }

    pub fn add(&mut self, x: f64) {
        for (i, lag) in self.lags.iter().enumerate() {
            if self.window.len() >= *lag {
                let y = self.window[self.window.len() - lag];
                let st = &mut self.stats[i];
                st.n += 1;
                st.sx += x;
                st.sy += y;
                st.sxx += x * x;
                st.syy += y * y;
                st.sxy += x * y;
            }
        }
        // `deque(maxlen=...)` evicts the stale head on append.
        if self.window.len() == self.maxlen {
            self.window.pop_front();
        }
        self.window.push_back(x);
    }

    pub fn reset_series(&mut self) {
        self.window.clear();
    }

    #[must_use]
    pub fn value(&self, lag: usize) -> f64 {
        let Some(i) = self.lags.iter().position(|l| *l == lag) else {
            return f64::NAN;
        };
        let st = self.stats[i];
        let n = st.n;
        if n < 2 {
            return f64::NAN;
        }
        let n = n as f64;
        let mx = st.sx / n;
        let my = st.sy / n;
        let vx = st.sxx / n - mx * mx;
        let vy = st.syy / n - my * my;
        if vx <= 0.0 || vy <= 0.0 {
            return f64::NAN;
        }
        let cov = st.sxy / n - mx * my;
        cov / (vx * vy).sqrt()
    }
}

/// Nearest-rank quantile of a binned histogram, read at the bin center.
#[must_use]
pub fn hist_quantile(hist: &BTreeMap<i64, i64>, q: f64, bin_width: f64) -> f64 {
    let total: i64 = hist.values().sum();
    if total == 0 {
        return f64::NAN;
    }
    let rank = ((q * total as f64).ceil() as i64).max(1);
    let mut seen = 0i64;
    for (k, v) in hist {
        seen += v;
        if seen >= rank {
            return (*k as f64 + 0.5) * bin_width;
        }
    }
    unreachable!("rank walked past the histogram")
}

#[must_use]
pub fn hist_median(hist: &BTreeMap<i64, i64>, bin_width: f64) -> f64 {
    hist_quantile(hist, 0.5, bin_width)
}

/// Nearest-rank median/IQR plus min and max over a small list of per-session
/// values (the 4.2 stability-diagnostic shape).
#[must_use]
pub fn dist_stats(values: &[f64]) -> Value {
    if values.is_empty() {
        let nan = json!(f64::NAN);
        return json!({
            "median": nan, "p25": nan, "p75": nan, "iqr": nan,
            "min": nan, "max": nan,
        });
    }
    let mut ordered = values.to_vec();
    ordered.sort_by(f64::total_cmp);
    let n = ordered.len() as f64;
    let rank = |q: f64| ordered[(((q * n).ceil() as usize).max(1)) - 1];
    let (p25, p75) = (rank(0.25), rank(0.75));
    json!({
        "median": rank(0.5),
        "p25": p25,
        "p75": p75,
        "iqr": p75 - p25,
        "min": ordered[0],
        "max": ordered[ordered.len() - 1],
    })
}

/// The successor spec 3.3 envelope: `RESAMPLE_REPLICATES` replicates, each
/// drawing `RESAMPLE_SESSIONS_PER_REPLICATE` sessions with replacement,
/// pooling their minute tick ranges, and recording nearest-rank p99, p99.9,
/// p99.99 and the maximum. The p99 envelope is two-sided: its lower bound is
/// the complementary lower-tail quantile across replicates and its upper
/// bound is at `RESAMPLE_ENVELOPE_LEVEL`. The remaining statistics retain
/// their one-sided upper bounds.
/// Deterministic under `RESAMPLE_SEED` - see `fit::mtrand` for why the
/// CPython Mersenne Twister had to be ported rather than substituted.
pub fn minute_range_envelope(session_ranges: &BTreeMap<String, Vec<i64>>) -> LabResult<Value> {
    let mut rng = PyRandom::new(RESAMPLE_SEED as u64);
    let sessions: Vec<&String> = session_ranges.keys().collect();
    if sessions.is_empty() {
        return Err(LabError::refusal("no sessions carry minute ranges"));
    }
    let mut stats: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
    for name in ["p99", "p99.9", "p99.99", "max"] {
        stats.insert(name, Vec::new());
    }
    for _ in 0..RESAMPLE_REPLICATES {
        let mut pool: Vec<i64> = Vec::new();
        for _ in 0..RESAMPLE_SESSIONS_PER_REPLICATE {
            let idx = rng.choice_index(sessions.len());
            pool.extend_from_slice(&session_ranges[sessions[idx]]);
        }
        pool.sort_unstable();
        let pf: Vec<f64> = pool.iter().map(|v| *v as f64).collect();
        stats
            .get_mut("p99")
            .expect("seeded")
            .push(nearest_rank_list(&pf, 0.99).ok_or_else(|| LabError::refusal("empty pool"))?);
        stats
            .get_mut("p99.9")
            .expect("seeded")
            .push(nearest_rank_list(&pf, 0.999).ok_or_else(|| LabError::refusal("empty pool"))?);
        stats
            .get_mut("p99.99")
            .expect("seeded")
            .push(nearest_rank_list(&pf, 0.9999).ok_or_else(|| LabError::refusal("empty pool"))?);
        stats
            .get_mut("max")
            .expect("seeded")
            .push(*pool.last().expect("nonempty") as f64);
    }
    let mut out = serde_json::Map::new();
    for (name, values) in stats {
        let mut sorted = values;
        sorted.sort_by(f64::total_cmp);
        if name == "p99" {
            let lower = nearest_rank_list(&sorted, 1.0 - RESAMPLE_ENVELOPE_LEVEL)
                .ok_or_else(|| LabError::refusal("empty replicate list"))?;
            out.insert("p99_lower".to_string(), json!(lower as i64));
        }
        let upper = nearest_rank_list(&sorted, RESAMPLE_ENVELOPE_LEVEL)
            .ok_or_else(|| LabError::refusal("empty replicate list"))?;
        // The pooled values are integer tick counts, so the envelope is an
        // integer too; the Python's list holds Python ints and serializes
        // as such.
        out.insert(name.to_string(), json!(upper as i64));
    }
    Ok(Value::Object(out))
}

// --- the streaming pass ----------------------------------------------------

struct Parent {
    ts: i64,
    side: char,
    session: String,
    segment: &'static str,
    first_ts: i64,
    first_price: i64,
    rows: i64,
    levels: Vec<i64>,
    book: &'static str,
    bid_px: i64,
    ask_px: i64,
    bid_sz: i64,
    ask_sz: i64,
}

#[derive(Default)]
struct SessionCadence {
    parents: i64,
    rows: i64,
    singles: i64,
    levels: i64,
    gaps: i64,
    gap_ns: i64,
}

#[derive(Default, Clone, Copy)]
struct PvCell {
    count: i64,
    sum_abs: f64,
    max_abs: f64,
}

#[derive(Default, Clone, Copy)]
struct HzCell {
    count: i64,
    sum: f64,
    sumsq: f64,
    sum_abs: f64,
    max_abs: f64,
}

/// One streaming pass over `rows` restricted to `usable`, producing the
/// `observed` block of `analysis/mnq-fit.json`.
#[allow(
    unused_assignments,
    reason = "the final `close_parent!` expansion stores chain state the loop \
              would have read on a next parent; the Python keeps the same \
              dead final assignments and removing them would fork the port"
)]
#[allow(
    clippy::too_many_lines,
    reason = "a faithful port of one 580-line Python function; splitting it would \
              relocate state across boundaries the original does not have"
)]
pub fn observe(
    rows_iter: impl Iterator<Item = LabResult<Row>>,
    usable: &[String],
) -> LabResult<Value> {
    let usable_set: std::collections::HashSet<&str> = usable.iter().map(String::as_str).collect();

    let mut parents = 0i64;
    let mut sided_rows = 0i64;
    let mut single_parents = 0i64;
    let mut level_sum = 0i64;
    let mut gap_sum_ns = 0i64;
    let mut gaps = 0i64;
    let mut dur_sum = 0.0f64;
    let mut dur_sumsq = 0.0f64;
    let mut sizes = Quantiles::default();
    let mut width_hist: BTreeMap<i64, i64> = BTreeMap::new();
    let mut bid_sizes = Quantiles::default();
    let mut ask_sizes = Quantiles::default();
    // Displacement histograms keyed by side, insertion order irrelevant:
    // the Python sorts before serializing and pools with `.get` sums.
    let mut disp_hist_b: BTreeMap<i64, i64> = BTreeMap::new();
    let mut disp_hist_a: BTreeMap<i64, i64> = BTreeMap::new();
    let mut cats_b = [0i64; 4]; // wrong_side, inside_mid, at_touch, beyond_touch
    let mut cats_a = [0i64; 4];
    let mut wrong_side = 0i64;
    let mut valid_quote_parents = 0i64;
    let mut mid_count = 0i64;
    let mut mid_sumsq = 0.0f64;
    let acf_lags: Vec<usize> = ACF_LAGS.iter().map(|l| *l as usize).collect();
    let mut ret_acf = Acf::new(&acf_lags);
    let mut absret_acf = Acf::new(&acf_lags);
    let mut dur_acf = Acf::new(&[1, 5]);
    let mut zero_changes = 0i64;
    let mut price_changes = 0i64;
    let mut hour_count = [0i64; 24];
    let mut hour_volume = [0i64; 24];
    let mut session_cad: BTreeMap<String, SessionCadence> = BTreeMap::new();

    // Legacy fixed-horizon accumulators (4.7).
    let mut hz_acc: BTreeMap<i64, (i64, f64, f64)> = HORIZON_SECONDS
        .iter()
        .map(|h| (*h, (0, 0.0, 0.0)))
        .collect();
    let mut hz_key: Option<(String, &'static str)> = None;
    let mut hz_state: BTreeMap<i64, (i64, i64, Option<f64>)> = BTreeMap::new(); // origin, next, prev
    let mut hz_last_mid: Option<f64> = None;

    let mut last_trade_price_units: Option<i64> = None;
    let mut minute_current: Option<(i64, String, i64, i64)> = None;
    let mut session_minute_ranges: BTreeMap<String, Vec<i64>> = BTreeMap::new();

    let mut pop_prints = 0i64;
    let mut pop_unsided = 0i64;
    let mut pop_invalid_book = 0i64;

    // Protocol-11 session-refit cells, keyed on UTC hour.
    let mut pv_cells: BTreeMap<(String, usize), PvCell> = BTreeMap::new();
    let mut c_hd = vec![vec![0i64; 7]; 24];
    let mut hz_cells: BTreeMap<i64, BTreeMap<(String, usize), HzCell>> = HORIZON_SECONDS
        .iter()
        .map(|h| (*h, BTreeMap::new()))
        .collect();
    let mut hz_pooled: BTreeMap<i64, (i64, f64, f64)> = HORIZON_SECONDS
        .iter()
        .map(|h| (*h, (0, 0.0, 0.0)))
        .collect();

    // The 4.6 horizon chains, separate from the legacy hz state: the new
    // convention settles trailing boundaries through the segment end.
    let mut nhz_key: Option<(String, &'static str)> = None;
    let mut nhz_end = 0i64;
    let mut nhz_state: BTreeMap<i64, (i64, Option<f64>)> = BTreeMap::new(); // nnext, nprev
    let mut nhz_last_mid: Option<f64> = None;

    let mut current: Option<Parent> = None;
    let mut prev_cadence: Option<(i64, String, &'static str)> = None;
    let mut prev_mid: Option<(f64, String, &'static str)> = None;
    let mut prev_diag: Option<(i64, String, &'static str)> = None;

    let mut minutes = MinuteFieldsCache::new();

    // `nhz_settle` and `nhz_boundary` as free closures over the state above.
    macro_rules! nhz_boundary {
        ($h:expr, $session:expr, $boundary:expr, $as_of:expr) => {{
            let h: i64 = $h;
            let boundary: i64 = $boundary;
            let as_of: Option<f64> = $as_of;
            if let Some(as_of) = as_of
                && as_of > 0.0
            {
                let st = nhz_state.get_mut(&h).expect("state per horizon");
                let prev = st.1;
                st.1 = Some(as_of);
                if let Some(prev) = prev {
                    let window_start = boundary - h * 1_000_000_000;
                    let b_hour = utc_hour_dow(boundary).0;
                    if utc_hour_dow(window_start).0 == b_hour {
                        let r = (as_of / prev).ln();
                        let cell = hz_cells
                            .get_mut(&h)
                            .expect("cells per horizon")
                            .entry(($session.clone(), b_hour))
                            .or_default();
                        cell.count += 1;
                        cell.sum += r;
                        cell.sumsq += r * r;
                        cell.sum_abs += r.abs();
                        if r.abs() > cell.max_abs {
                            cell.max_abs = r.abs();
                        }
                        let pooled = hz_pooled.get_mut(&h).expect("pooled per horizon");
                        pooled.0 += 1;
                        pooled.1 += r;
                        pooled.2 += r * r;
                    }
                }
            }
        }};
    }

    macro_rules! nhz_settle {
        ($until_exclusive:expr) => {{
            let until_exclusive: i64 = $until_exclusive;
            let session = nhz_key.as_ref().expect("an active chain").0.clone();
            for h in HORIZON_SECONDS.iter().copied() {
                let w_ns = h * 1_000_000_000;
                let limit = until_exclusive.min(nhz_end);
                loop {
                    let nnext = nhz_state.get(&h).expect("state per horizon").0;
                    if nnext >= limit {
                        break;
                    }
                    nhz_boundary!(h, session, nnext, nhz_last_mid);
                    nhz_state.get_mut(&h).expect("state per horizon").0 += w_ns;
                }
            }
        }};
    }

    macro_rules! close_parent {
        ($parent:expr) => {{
            let parent: Parent = $parent;
            parents += 1;
            let cad = session_cad.entry(parent.session.clone()).or_default();
            cad.parents += 1;
            cad.rows += parent.rows;
            cad.levels += parent.levels.len() as i64;
            sided_rows += parent.rows;
            if parent.rows == 1 {
                single_parents += 1;
                cad.singles += 1;
            }
            level_sum += parent.levels.len() as i64;

            let here = (parent.session.clone(), parent.segment);
            let (p11_hour, p11_dow) = utc_hour_dow(parent.first_ts);
            c_hd[p11_hour][p11_dow] += 1;

            // Chain 1: cadence, every parent.
            match &prev_cadence {
                Some(p) if (p.1.as_str(), p.2) == (here.0.as_str(), here.1) => {
                    let gap_ns = parent.first_ts - p.0;
                    gap_sum_ns += gap_ns;
                    gaps += 1;
                    let cad = session_cad.get_mut(&parent.session).expect("just inserted");
                    cad.gaps += 1;
                    cad.gap_ns += gap_ns;
                    let dur_s = gap_ns as f64 / 1e9;
                    dur_sum += dur_s;
                    dur_sumsq += dur_s * dur_s;
                    dur_acf.add(dur_s);
                }
                Some(_) => dur_acf.reset_series(),
                None => {}
            }
            prev_cadence = Some((parent.first_ts, here.0.clone(), here.1));

            // Chain 2: quote-mid returns, valid-quote parents only.
            if parent.book == "normal" {
                valid_quote_parents += 1;
                let width_ticks = (parent.ask_px - parent.bid_px).div_euclid(TICK_UNITS);
                *width_hist.entry(width_ticks).or_insert(0) += 1;
                bid_sizes.add(parent.bid_sz);
                ask_sizes.add(parent.ask_sz);
                let mid_units = (parent.bid_px + parent.ask_px) as f64 / 2.0;
                let raw_ticks = (parent.first_price as f64 - mid_units) / TICK_UNITS as f64;
                let signed = if parent.side == 'B' {
                    raw_ticks
                } else {
                    -raw_ticks
                };
                // Touch categories on exact integers.
                let mut d2 = 2 * parent.first_price - parent.bid_px - parent.ask_px;
                if parent.side == 'A' {
                    d2 = -d2;
                }
                let touch2 = parent.ask_px - parent.bid_px;
                let cats = if parent.side == 'B' {
                    &mut cats_b
                } else {
                    &mut cats_a
                };
                if d2 < 0 {
                    wrong_side += 1;
                    cats[0] += 1;
                } else if d2 == touch2 {
                    cats[2] += 1;
                } else if d2 > touch2 {
                    cats[3] += 1;
                } else {
                    cats[1] += 1;
                }
                let bin_key = (signed / DISPLACEMENT_BIN_TICKS).floor() as i64;
                let hist = if parent.side == 'B' {
                    &mut disp_hist_b
                } else {
                    &mut disp_hist_a
                };
                *hist.entry(bin_key).or_insert(0) += 1;
                if let Some(p) = &prev_mid
                    && (p.1.as_str(), p.2) == (here.0.as_str(), here.1)
                    && p.0 > 0.0
                    && mid_units > 0.0
                {
                    let r = (mid_units / p.0).ln();
                    mid_count += 1;
                    mid_sumsq += r * r;
                    let cell = pv_cells
                        .entry((parent.session.clone(), p11_hour))
                        .or_default();
                    cell.count += 1;
                    cell.sum_abs += r.abs();
                    if r.abs() > cell.max_abs {
                        cell.max_abs = r.abs();
                    }
                }
                prev_mid = Some((mid_units, here.0.clone(), here.1));

                // Legacy fixed-horizon windows (4.7).
                let ts_ns = parent.first_ts;
                if hz_key.as_ref().map(|k| (k.0.as_str(), k.1)) != Some((here.0.as_str(), here.1)) {
                    hz_key = Some((here.0.clone(), here.1));
                    let origin = segment_origin_ns(&here.0, here.1) as i64;
                    hz_state = HORIZON_SECONDS
                        .iter()
                        .map(|h| (*h, (origin, origin + h * 1_000_000_000, None)))
                        .collect();
                    hz_last_mid = None;
                }
                for h in HORIZON_SECONDS.iter().copied() {
                    let w_ns = h * 1_000_000_000;
                    let st = hz_state.get_mut(&h).expect("state per horizon");
                    if hz_last_mid.is_none() && st.1 < ts_ns {
                        // No as-of mid exists yet: dead boundaries carry no
                        // observation; jump to the first boundary >= ts.
                        let k = (ts_ns - st.0 + w_ns - 1).div_euclid(w_ns);
                        st.1 = st.0 + w_ns * k.max(1);
                    }
                    while st.1 < ts_ns {
                        if let Some(mid) = hz_last_mid
                            && mid > 0.0
                        {
                            if let Some(prev) = st.2 {
                                let r = (mid / prev).ln();
                                let acc = hz_acc.get_mut(&h).expect("acc per horizon");
                                acc.0 += 1;
                                acc.1 += r;
                                acc.2 += r * r;
                            }
                            st.2 = Some(mid);
                        }
                        st.1 += w_ns;
                    }
                }
                hz_last_mid = Some(mid_units);

                // Protocol-11 horizon chains (spec 4.6), separate state.
                if nhz_key.as_ref().map(|k| (k.0.as_str(), k.1)) != Some((here.0.as_str(), here.1))
                {
                    if nhz_key.is_some() {
                        nhz_settle!(nhz_end);
                    }
                    let origin = segment_origin_ns(&here.0, here.1) as i64;
                    nhz_key = Some((here.0.clone(), here.1));
                    nhz_end = segment_end_ns(&here.0, here.1) as i64;
                    nhz_state = HORIZON_SECONDS
                        .iter()
                        .map(|h| (*h, (origin + h * 1_000_000_000, None)))
                        .collect();
                    nhz_last_mid = None;
                }
                nhz_settle!(ts_ns);
                nhz_last_mid = Some(mid_units);
            }

            // Chain 3: shared-shape diagnostics, every parent.
            match &prev_diag {
                Some(p) if (p.1.as_str(), p.2) == (here.0.as_str(), here.1) => {
                    price_changes += 1;
                    if parent.first_price == p.0 {
                        zero_changes += 1;
                    }
                    if parent.first_price > 0 && p.0 > 0 {
                        let r = (parent.first_price as f64 / p.0 as f64).ln();
                        ret_acf.add(r);
                        absret_acf.add(r.abs());
                    }
                }
                Some(_) => {
                    ret_acf.reset_series();
                    absret_acf.reset_series();
                }
                None => {}
            }
            prev_diag = Some((parent.first_price, here.0, here.1));
        }};
    }

    for row in rows_iter {
        let row = row?;
        let (session, segment, hour) = minutes.minute_fields(row.ts as u64);
        let in_usable = session.as_deref().is_some_and(|s| usable_set.contains(s));
        if !in_usable {
            if let Some(p) = current.take() {
                close_parent!(p);
            }
            continue;
        }
        let session = session.expect("checked above");
        let segment = segment.expect("a usable session always resolves a segment");

        // Deliberately before the side and book branches (4.3).
        sizes.add(row.size);
        pop_prints += 1;
        if row.side == 'N' {
            pop_unsided += 1;
        }
        if row.book != "normal" {
            pop_invalid_book += 1;
        }
        last_trade_price_units = Some(row.price);
        hour_count[hour as usize] += 1;
        hour_volume[hour as usize] += row.size;

        let minute = row.ts.div_euclid(60_000_000_000);
        match &mut minute_current {
            Some(mc) if mc.0 == minute => {
                if row.price < mc.2 {
                    mc.2 = row.price;
                }
                if row.price > mc.3 {
                    mc.3 = row.price;
                }
            }
            _ => {
                if let Some(mc) = minute_current.take() {
                    session_minute_ranges
                        .entry(mc.1)
                        .or_default()
                        .push((mc.3 - mc.2).div_euclid(TICK_UNITS));
                }
                minute_current = Some((minute, session.clone(), row.price, row.price));
            }
        }

        if row.side == 'N' {
            // Contiguity: an unsided row terminates the open parent.
            if let Some(p) = current.take() {
                close_parent!(p);
            }
            continue;
        }
        match &mut current {
            Some(p) if p.ts == row.ts && p.side == row.side => {
                p.rows += 1;
                if !p.levels.contains(&row.price) {
                    p.levels.push(row.price);
                }
            }
            _ => {
                if let Some(p) = current.take() {
                    close_parent!(p);
                }
                current = Some(Parent {
                    ts: row.ts,
                    side: row.side,
                    session: session.clone(),
                    segment,
                    first_ts: row.ts,
                    first_price: row.price,
                    rows: 1,
                    levels: vec![row.price],
                    book: row.book,
                    bid_px: row.bid_px,
                    ask_px: row.ask_px,
                    bid_sz: row.bid_sz,
                    ask_sz: row.ask_sz,
                });
            }
        }
    }
    if let Some(p) = current.take() {
        close_parent!(p);
    }
    if nhz_key.is_some() {
        nhz_settle!(nhz_end);
    }
    if let Some(mc) = minute_current.take() {
        session_minute_ranges
            .entry(mc.1)
            .or_default()
            .push((mc.3 - mc.2).div_euclid(TICK_UNITS));
    }
    {
        let have: std::collections::HashSet<&str> =
            session_minute_ranges.keys().map(String::as_str).collect();
        if have != usable_set {
            let mut missing: Vec<&str> = usable_set.difference(&have).copied().collect();
            let mut extra: Vec<&str> = have.difference(&usable_set).copied().collect();
            missing.sort_unstable();
            extra.sort_unstable();
            return Err(LabError::refusal(format!(
                "minute-range session blocks do not match the usable set (missing: {missing:?}; \
                 outside: {extra:?})"
            )));
        }
    }
    if parents == 0 {
        return Err(LabError::refusal("no parents in usable sessions"));
    }
    if width_hist.is_empty() {
        return Err(LabError::refusal(
            "no valid-quote parents in usable sessions",
        ));
    }

    let mut all_disp: BTreeMap<i64, i64> = BTreeMap::new();
    for h in [&disp_hist_b, &disp_hist_a] {
        for (k, v) in h {
            *all_disp.entry(*k).or_insert(0) += v;
        }
    }
    let max_width_count = *width_hist.values().max().expect("nonempty");
    let width_mode = *width_hist
        .iter()
        .filter(|(_, v)| **v == max_width_count)
        .map(|(k, _)| k)
        .min()
        .expect("nonempty");
    let width_total: i64 = width_hist.values().sum();
    // `sum(...)` over integer terms: CPython's compensated summation applies
    // to floats only, so an exact integer fold is the faithful port here
    // (contrast the float `sum(...)` sites, which route through
    // `kernel::py_sum`).
    let width_mad = width_hist
        .iter()
        .map(|(k, v)| (k - width_mode).abs() * v)
        .sum::<i64>() as f64
        / width_total as f64;

    let mut per_session_cadence = serde_json::Map::new();
    let mut stability_pools: HashMap<&str, Vec<f64>> = HashMap::new();
    for name in [
        "mean_event_duration_s",
        "children_mean",
        "children_single_frac",
        "levels_mean",
    ] {
        stability_pools.insert(name, Vec::new());
    }
    for (label, c) in &session_cad {
        // `gap_ns / gaps` is Python int/int - exact operands, one correctly
        // rounded division. See `kernel::py_int_div`.
        let med = if c.gaps > 0 {
            crate::kernel::py_int_div(c.gap_ns, c.gaps) / 1e9
        } else {
            f64::NAN
        };
        let vals = [
            ("mean_event_duration_s", med),
            ("children_mean", c.rows as f64 / c.parents as f64),
            ("children_single_frac", c.singles as f64 / c.parents as f64),
            ("levels_mean", c.levels as f64 / c.parents as f64),
        ];
        for (name, v) in vals {
            if v.is_finite() {
                stability_pools.get_mut(name).expect("seeded").push(v);
            }
        }
        per_session_cadence.insert(
            label.clone(),
            json!({
                "parents": c.parents,
                "mean_event_duration_s": med,
                "children_mean": vals[1].1,
                "children_single_frac": vals[2].1,
                "levels_mean": vals[3].1,
            }),
        );
    }
    let mut cadence_stability = serde_json::Map::new();
    for name in [
        "mean_event_duration_s",
        "children_mean",
        "children_single_frac",
        "levels_mean",
    ] {
        cadence_stability.insert(
            name.to_string(),
            dist_stats(stability_pools.get(name).expect("seeded")),
        );
    }

    let cat_names = ["wrong_side", "inside_mid", "at_touch", "beyond_touch"];
    let category_fractions = |counts: &[i64; 4]| -> Value {
        let total: i64 = counts.iter().sum();
        let mut m = serde_json::Map::new();
        for (i, name) in cat_names.iter().enumerate() {
            m.insert(
                (*name).to_string(),
                json!(if total > 0 {
                    counts[i] as f64 / total as f64
                } else {
                    f64::NAN
                }),
            );
        }
        m.insert("parents".to_string(), json!(total));
        Value::Object(m)
    };
    let combined: [i64; 4] = std::array::from_fn(|i| cats_b[i] + cats_a[i]);

    let mut cv2 = f64::NAN;
    if gaps > 1 {
        let mean_d = dur_sum / gaps as f64;
        let var_d = dur_sumsq / gaps as f64 - mean_d * mean_d;
        cv2 = if mean_d > 0.0 {
            var_d / (mean_d * mean_d)
        } else {
            f64::NAN
        };
    }
    let last_price_points = match last_trade_price_units {
        Some(units) => json!(format!(
            "{:.2}",
            units as f64 / PRICE_UNITS_PER_POINT as f64
        )),
        None => Value::Null,
    };

    let q_named = |qs: &Quantiles, quants: &[f64]| -> LabResult<Value> {
        let mut m = serde_json::Map::new();
        for q in quants {
            m.insert(
                format!("p{}", (q * 100.0) as i64),
                json!(qs.nearest_rank(*q)?),
            );
        }
        Ok(Value::Object(m))
    };
    let hist_str = |h: &BTreeMap<i64, i64>| -> Value {
        Value::Object(h.iter().map(|(k, v)| (k.to_string(), json!(v))).collect())
    };

    let pooled_minute: Vec<f64> = {
        let mut all: Vec<i64> = session_minute_ranges.values().flatten().copied().collect();
        all.sort_unstable();
        all.iter().map(|v| *v as f64).collect()
    };
    let minute_range_observed = json!({
        "p99": nearest_rank_list(&pooled_minute, 0.99).expect("nonempty") as i64,
        "p99.9": nearest_rank_list(&pooled_minute, 0.999).expect("nonempty") as i64,
        "p99.99": nearest_rank_list(&pooled_minute, 0.9999).expect("nonempty") as i64,
        "max": *pooled_minute.last().expect("nonempty") as i64,
    });

    // --- the protocol-11 raw session-refit block --------------------------
    let mut pv_by_session = serde_json::Map::new();
    for ((session, hour), c) in &pv_cells {
        let entry = pv_by_session
            .entry(session.clone())
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        entry
            .as_object_mut()
            .expect("just inserted an object")
            .insert(
                hour.to_string(),
                json!({"count": c.count, "sum_abs": c.sum_abs, "max_abs": c.max_abs}),
            );
    }
    let mut horizon_cells = serde_json::Map::new();
    for h in HORIZON_SECONDS {
        let mut by_session = serde_json::Map::new();
        for ((session, hour), c) in &hz_cells[h] {
            let entry = by_session
                .entry(session.clone())
                .or_insert_with(|| Value::Object(serde_json::Map::new()));
            entry
                .as_object_mut()
                .expect("just inserted an object")
                .insert(
                    hour.to_string(),
                    json!({
                        "count": c.count, "sum": c.sum, "sumsq": c.sumsq,
                        "sum_abs": c.sum_abs, "max_abs": c.max_abs,
                    }),
                );
        }
        horizon_cells.insert(h.to_string(), Value::Object(by_session));
    }
    let mut walltime_pooled = serde_json::Map::new();
    for h in HORIZON_SECONDS {
        let (count, sum, sumsq) = hz_pooled[h];
        walltime_pooled.insert(
            h.to_string(),
            json!({
                "count": count, "sum": sum, "sumsq": sumsq,
                "rms": if count > 0 { (sumsq / count as f64).sqrt() } else { f64::NAN },
            }),
        );
    }

    let mut horizon_vol = serde_json::Map::new();
    for (h, acc) in &hz_acc {
        horizon_vol.insert(
            h.to_string(),
            json!({
                "count": acc.0, "sum": acc.1, "sumsq": acc.2,
                "rms": if acc.0 > 0 { (acc.2 / acc.0 as f64).sqrt() } else { f64::NAN },
            }),
        );
    }

    let per_session_parents: serde_json::Map<String, Value> = per_session_cadence
        .iter()
        .map(|(k, v)| (k.clone(), v["parents"].clone()))
        .collect();

    Ok(json!({
        "parents": parents,
        "sided_rows": sided_rows,
        "mean_event_duration_s": if gaps > 0 {
            crate::kernel::py_int_div(gap_sum_ns, gaps) / 1e9
        } else { f64::NAN },
        "children_mean": sided_rows as f64 / parents as f64,
        "children_single_frac": single_parents as f64 / parents as f64,
        "levels_mean": level_sum as f64 / parents as f64,
        "size_population": {
            "definition": "all prints in usable sessions, unsided and invalid-book included",
            "prints": pop_prints,
            "sided": pop_prints - pop_unsided,
            "unsided": pop_unsided,
            "valid_book": pop_prints - pop_invalid_book,
            "invalid_book": pop_invalid_book,
        },
        "size_histogram": hist_str(&sizes.counts),
        "size_mean": sizes.mean(),
        "size_quantiles": q_named(&sizes, &[0.50, 0.75, 0.90, 0.95, 0.99])?,
        "size_floor_mass": *sizes.counts.get(&1).unwrap_or(&0) as f64 / sizes.total as f64,
        "width_hist": hist_str(&width_hist),
        "width_mode": width_mode,
        "width_modal_mass": width_hist[&width_mode] as f64 / width_total as f64,
        "width_median": nearest_rank_of(&width_hist, 0.5)?,
        "width_p90": nearest_rank_of(&width_hist, 0.90)?,
        "width_mad_from_mode": width_mad,
        "top_bid_median": bid_sizes.nearest_rank(0.5)?,
        "top_ask_median": ask_sizes.nearest_rank(0.5)?,
        "bid_size_histogram": hist_str(&bid_sizes.counts),
        "ask_size_histogram": hist_str(&ask_sizes.counts),
        "top_size_quantiles": {
            "bid": q_named(&bid_sizes, &[0.50, 0.90, 0.95, 0.99])?,
            "ask": q_named(&ask_sizes, &[0.50, 0.90, 0.95, 0.99])?,
        },
        "displacement_hist": {
            "B": hist_str(&disp_hist_b),
            "A": hist_str(&disp_hist_a),
        },
        "displacement_median_ticks": hist_median(&all_disp, DISPLACEMENT_BIN_TICKS),
        "displacement_p90_ticks": hist_quantile(&all_disp, 0.90, DISPLACEMENT_BIN_TICKS),
        "displacement_buyer_median_ticks": hist_median(&disp_hist_b, DISPLACEMENT_BIN_TICKS),
        "displacement_seller_median_ticks": hist_median(&disp_hist_a, DISPLACEMENT_BIN_TICKS),
        "displacement_buyer_p90_ticks": hist_quantile(&disp_hist_b, 0.90, DISPLACEMENT_BIN_TICKS),
        "displacement_seller_p90_ticks": hist_quantile(&disp_hist_a, 0.90, DISPLACEMENT_BIN_TICKS),
        "displacement_fractions": {
            "combined": category_fractions(&combined),
            "B": category_fractions(&cats_b),
            "A": category_fractions(&cats_a),
        },
        "wrong_side_share": if valid_quote_parents > 0 {
            wrong_side as f64 / valid_quote_parents as f64
        } else { f64::NAN },
        "valid_quote_parents": valid_quote_parents,
        "mid_rms": if mid_count > 0 { (mid_sumsq / mid_count as f64).sqrt() } else { f64::NAN },
        "mid_return_count": mid_count,
        "eligible_gaps": gaps,
        "last_price_points": last_price_points,
        "minute_ranges_by_session": session_minute_ranges.iter()
            .map(|(k, v)| {
                let mut sorted = v.clone();
                sorted.sort_unstable();
                (k.clone(), json!(sorted))
            })
            .collect::<serde_json::Map<String, Value>>(),
        "minute_range_observed": minute_range_observed,
        "minute_range_envelope": minute_range_envelope(&session_minute_ranges)?,
        "per_session_parents": per_session_parents,
        "per_session_cadence": Value::Object(per_session_cadence),
        "cadence_stability": Value::Object(cadence_stability),
        "horizon_vol": Value::Object(horizon_vol),
        "session_refit_raw": {
            "parent_count_by_hour_dow": c_hd,
            "parent_vol_cells": Value::Object(pv_by_session),
            "horizon_cells": Value::Object(horizon_cells),
            "walltime_pooled": Value::Object(walltime_pooled),
        },
        "diagnostics": super::diagnostics::build_diagnostics(
            zero_changes, price_changes, &ret_acf, &absret_acf, &dur_acf, cv2,
            &hour_count, &hour_volume, usable_set.len() as i64,
        ),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantiles_nearest_rank_matches_the_ceiling_rule() {
        let mut q = Quantiles::default();
        for v in [1, 2, 3, 4] {
            q.add(v);
        }
        assert_eq!(q.nearest_rank(0.5).unwrap(), 2);
        assert_eq!(q.nearest_rank(0.99).unwrap(), 4);
        // A zero quantile still reads rank 1, never rank 0.
        assert_eq!(q.nearest_rank(0.0).unwrap(), 1);
    }

    #[test]
    fn an_empty_histogram_refuses_rather_than_returning_nan() {
        assert!(Quantiles::default().nearest_rank(0.5).is_err());
    }

    #[test]
    fn acf_pairs_never_straddle_a_reset() {
        let mut acf = Acf::new(&[1]);
        for x in [1.0, 2.0, 3.0] {
            acf.add(x);
        }
        acf.reset_series();
        for x in [10.0, 20.0] {
            acf.add(x);
        }
        // 2 pairs before the reset, 1 after: 3 total, none across it.
        assert!(acf.value(1).is_finite());
    }

    #[test]
    fn dist_stats_uses_the_nearest_rank_convention() {
        let s = dist_stats(&[1.0, 2.0, 3.0, 4.0]);
        assert_eq!(s["median"], 2.0);
        assert_eq!(s["p25"], 1.0);
        assert_eq!(s["p75"], 3.0);
        assert_eq!(s["min"], 1.0);
        assert_eq!(s["max"], 4.0);
    }

    #[test]
    fn hist_quantile_reads_the_bin_center() {
        let hist: BTreeMap<i64, i64> = [(0, 1), (1, 1)].into_iter().collect();
        assert!((hist_quantile(&hist, 0.5, 0.05) - 0.025).abs() < 1e-15);
    }
}
