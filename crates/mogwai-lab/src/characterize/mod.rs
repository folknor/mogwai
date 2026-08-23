// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The phase-0 stylized-fact characterization estimand layer
//! (`analysis/characterize.py`): streaming ACF ring buffers, histogram
//! quantiles, `LVL_BINS`/`lvl_bin`, duration dispersion, zero-change
//! fraction, per-second counts. This module is the estimand layer the
//! synthesis modules (`fingerprint`, `cadence`) build on. It is a byte-level
//! port; every constant and formula below is named after its Python
//! counterpart in `analysis/characterize.py`. That file is retired and no
//! longer in the tree - the names are the record of how the port was proven,
//! not a pointer to something a reader can open.
//!
//! The Kraken corpus itself lives outside this repository (phase-3a brief),
//! so this module has no corpus-level parity gate: it is verified by unit
//! fixtures mirroring `analysis/test_characterize.py`, and consumed
//! downstream by [`crate::fingerprint`] over the already-committed
//! (gitignored, locally present) `analysis/char_<PAIR>.json` files.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use serde_json::{Value, json};

use crate::error::LabResult;

pub const MAX_LAG: usize = 50;
pub const LOG_DUR_BINS: usize = 40;
pub const DWELL_ERA_START_TS: i64 = 1_546_300_800; // 2019-01-01T00:00:00Z
pub const DWELL_LOG_BINS: usize = 160;
pub const DWELL_LOG_LO_S: f64 = 1.0;
pub const DWELL_LOG_HI_S: f64 = 604_800.0;
pub const TICK_DICT_CAP: usize = 500_000;
pub const LVL_LOG_LO: f64 = 1e-6;
pub const LVL_LOG_HI: f64 = 1e6;
pub const LVL_PER_DEC: f64 = 10.0;
pub const LVL_BINS: usize = 120;

/// `lvl_bin`: regular log bin for era-windowed level-visit volumes and
/// sizes.
#[must_use]
pub fn lvl_bin(value: f64) -> usize {
    if value < LVL_LOG_LO {
        return 0;
    }
    let bucket = (value / LVL_LOG_LO).log10() * LVL_PER_DEC;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "bucket is floored and clamped into [0, LVL_BINS-1]"
    )]
    let bucket = bucket.floor() as i64;
    bucket.clamp(0, LVL_BINS as i64 - 1) as usize
}

/// `histogram_quantile`: geometric bin-centre quantile for the level
/// histogram. `None` mirrors the Python's `None` for an empty histogram.
#[must_use]
pub fn histogram_quantile(hist: &[i64], q: f64) -> Option<f64> {
    let total: i64 = hist.iter().sum();
    if total == 0 {
        return None;
    }
    #[expect(
        clippy::cast_possible_truncation,
        reason = "q*total is nonnegative and total is small"
    )]
    let threshold = (q * total as f64).ceil() as i64;
    let mut cumulative = 0i64;
    for (index, &count) in hist.iter().enumerate() {
        cumulative += count;
        if cumulative >= threshold {
            return Some(LVL_LOG_LO * 10f64.powf((index as f64 + 0.5) / LVL_PER_DEC));
        }
    }
    unreachable!("histogram quantile did not find a bin")
}

/// Streaming O(1) accumulator of at-touch traded volume per level visit, a
/// port of `characterize.py`'s `LevelVisits`.
pub struct LevelVisits {
    era_start_ts: f64,
    px: Option<f64>,
    vol: f64,
    n: i64,
    open_ok: bool,
    pub vol_hist: [i64; LVL_BINS],
    pub n_hist: [i64; 12],
    pub count: i64,
    pub single: i64,
    pub size_hist: [i64; LVL_BINS],
}

impl LevelVisits {
    #[must_use]
    pub fn new(era_start_ts: f64) -> Self {
        Self {
            era_start_ts,
            px: None,
            vol: 0.0,
            n: 0,
            open_ok: false,
            vol_hist: [0; LVL_BINS],
            n_hist: [0; 12],
            count: 0,
            single: 0,
            size_hist: [0; LVL_BINS],
        }
    }

    #[must_use]
    pub fn n_bin(prints: i64) -> usize {
        if prints <= 10 {
            #[expect(clippy::cast_sign_loss, reason = "prints in 1..=10 here")]
            return (prints - 1) as usize;
        }
        if prints <= 20 { 10 } else { 11 }
    }

    pub fn push(&mut self, ts: f64, px: f64, sz: f64) {
        if ts >= self.era_start_ts && sz > 0.0 {
            self.size_hist[lvl_bin(sz)] += 1;
        }
        if Some(px) == self.px {
            self.vol += sz;
            self.n += 1;
        } else {
            self.close();
            self.px = Some(px);
            self.vol = sz;
            self.n = 1;
            self.open_ok = ts >= self.era_start_ts;
        }
    }

    /// Bins the open visit. Called on every price change and once at EOF.
    pub fn close(&mut self) {
        if self.px.is_none() || !self.open_ok {
            return;
        }
        self.vol_hist[lvl_bin(self.vol)] += 1;
        self.n_hist[Self::n_bin(self.n)] += 1;
        self.count += 1;
        self.single += i64::from(self.n == 1);
        self.open_ok = false; // binned once, even if close() is called again
    }

    #[must_use]
    pub fn report(&self) -> Value {
        let size_p50 = histogram_quantile(&self.size_hist, 0.5);
        let size_p90 = histogram_quantile(&self.size_hist, 0.9);
        let vol_p50 = histogram_quantile(&self.vol_hist, 0.5);
        let vol_p90 = histogram_quantile(&self.vol_hist, 0.9);
        // Both of these are integers in the Python - `DWELL_ERA_START_TS` and
        // `LVL_PER_DEC` are int literals - and the typed-canonical comparator
        // distinguishes int from float, so emitting them as floats is a real
        // difference rather than a formatting one. They are held as f64
        // internally only because every comparison they take part in is
        // against a float timestamp or a float bin index.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "an exact integral epoch second and an exact small integer"
        )]
        let (era_start_ts, bins_per_decade) = (self.era_start_ts as i64, LVL_PER_DEC as i64);
        json!({
            "era_start_ts": era_start_ts,
            "n_visits": self.count,
            "single_print_frac": if self.count != 0 { Some(self.single as f64 / self.count as f64) } else { None },
            "bin_lo": LVL_LOG_LO,
            "bin_hi": LVL_LOG_HI,
            "bins_per_decade": bins_per_decade,
            "vol_hist": self.vol_hist.to_vec(),
            "n_hist": self.n_hist.to_vec(),
            "size_median": size_p50,
            "vol_p50_norm": match (vol_p50, size_p50) {
                (Some(v), Some(s)) if v != 0.0 && s != 0.0 => Some(v / s),
                _ => None,
            },
            "vol_p90_norm": match (vol_p90, size_p50) {
                (Some(v), Some(s)) if v != 0.0 && s != 0.0 => Some(v / s),
                _ => None,
            },
            "vol_dispersion": match (vol_p90, vol_p50) {
                (Some(a), Some(b)) if a != 0.0 && b != 0.0 => Some(a / b),
                _ => None,
            },
            "size_dispersion": match (size_p90, size_p50) {
                (Some(a), Some(b)) if a != 0.0 && b != 0.0 => Some(a / b),
                _ => None,
            },
        })
    }
}

/// Streaming autocorrelation up to `max_lag` via ring buffer + cross-sums, a
/// port of `characterize.py`'s `AutoCorr` (every lag retained, matching the
/// default `lags=None` behaviour in `probe_binance_aggtrades.py`'s twin).
pub struct AutoCorr {
    k: usize,
    ring: Vec<f64>,
    pos: usize,
    filled: usize,
    n: i64,
    sum: f64,
    sumsq: f64,
    cross: Vec<f64>,
}

impl AutoCorr {
    #[must_use]
    pub fn new(max_lag: usize) -> Self {
        Self {
            k: max_lag,
            ring: vec![0.0; max_lag],
            pos: 0,
            filled: 0,
            n: 0,
            sum: 0.0,
            sumsq: 0.0,
            cross: vec![0.0; max_lag + 1],
        }
    }

    pub fn push(&mut self, x: f64) {
        self.n += 1;
        self.sum += x;
        self.sumsq += x * x;
        self.cross[0] += x * x;
        let mut d = 1usize;
        let mut i = self.pos;
        while d <= self.filled {
            i = if i == 0 { self.k - 1 } else { i - 1 };
            self.cross[d] += x * self.ring[i];
            d += 1;
        }
        self.ring[self.pos] = x;
        self.pos = (self.pos + 1) % self.k;
        if self.filled < self.k {
            self.filled += 1;
        }
    }

    /// Known numerical hazard, deliberately left in place. The variance guard
    /// below fires only when `sumsq / n - mean * mean` lands at zero or below,
    /// so a series held constant at a value the mean cannot represent exactly
    /// leaves a tiny positive residue, the guard misses it, and the returned
    /// ACF is the ratio of two cancellation errors rather than a measurement.
    /// Both branches substitute a number where the honest answer is that
    /// autocorrelation is undefined for a constant series.
    ///
    /// It is not fixed because this same routine computes the F1 duration ACFs
    /// and is bit-exact against `analysis/cadence.json`, the lineage the
    /// fingerprint's cadence half rests on: changing the estimator invalidates
    /// that equivalence. A fix owes the analysis of what moves in the cadence
    /// targets and whether the fingerprint must be refitted, not just the code
    /// change. A reader who spots this and quietly repairs it breaks the
    /// lineage; the repair has to be taken as a piece of work with that
    /// analysis attached.
    #[must_use]
    pub fn acf(&self) -> Vec<f64> {
        if self.n < 2 {
            return Vec::new();
        }
        #[expect(clippy::cast_precision_loss, reason = "n is a bounded row count")]
        let n_f = self.n as f64;
        let mean = self.sum / n_f;
        let var = self.sumsq / n_f - mean * mean;
        if var <= 0.0 {
            return vec![0.0; self.k];
        }
        let mut out = Vec::with_capacity(self.k);
        for d in 1..=self.k {
            #[expect(
                clippy::cast_possible_wrap,
                reason = "d is bounded by max_lag, far under i64::MAX"
            )]
            let m = self.n - d as i64;
            if m <= 0 {
                out.push(0.0);
                continue;
            }
            let cov = self.cross[d] / m as f64 - mean * mean;
            out.push(cov / var);
        }
        out
    }
}

/// `log_bin`: shared duration/level log-histogram bucketer.
#[must_use]
pub fn log_bin(value: f64, lo: f64, hi: f64, nbins: usize) -> usize {
    if value <= lo {
        return 0;
    }
    if value >= hi {
        return nbins - 1;
    }
    let frac = (value.ln() - lo.ln()) / (hi.ln() - lo.ln());
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "frac in [0,1), nbins is small"
    )]
    let bucket = (frac * nbins as f64) as usize;
    bucket.min(nbins - 1)
}

/// `decimals_used`: significant decimal places after stripping trailing
/// zeros, over a raw ASCII numeric field.
#[must_use]
pub fn decimals_used(field: &str) -> usize {
    let s = field.trim();
    match s.split_once('.') {
        None => 0,
        Some((_, frac)) => frac.trim_end_matches('0').len(),
    }
}

/// `dwell_stats`: complete-era-hour dwell statistics from occupied UTC
/// hours. Returns `(empty_hour_frac, max_empty_hour_run_h)`.
#[must_use]
pub fn dwell_stats(
    first_ts: Option<f64>,
    last_ts: Option<f64>,
    seen_hours: &std::collections::HashSet<i64>,
) -> (f64, i64) {
    let (Some(first_ts), Some(last_ts)) = (first_ts, last_ts) else {
        return (0.0, 0);
    };
    let start_hour = (first_ts.max(DWELL_ERA_START_TS as f64) / 3600.0).ceil() as i64;
    let end_hour = (last_ts / 3600.0).floor() as i64 - 1;
    if end_hour < start_hour {
        return (0.0, 0);
    }
    let mut empty = 0i64;
    let mut longest = 0i64;
    let mut run = 0i64;
    for hour in start_hour..=end_hour {
        if seen_hours.contains(&hour) {
            run = 0;
        } else {
            empty += 1;
            run += 1;
            longest = longest.max(run);
        }
    }
    let total = end_hour - start_hour + 1;
    (empty as f64 / total as f64, longest)
}

/// `characterize`: the streaming single-pass pair characterization. `path`
/// is a Kraken `PAIR.csv` file: `ts,px,sz[,...]` per line.
///
/// # Errors
/// Propagates I/O failure opening `path`.
pub fn characterize(path: &Path) -> LabResult<Value> {
    let pair = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();

    let mut n: i64 = 0;
    let mut first_ts: Option<f64> = None;
    let mut last_ts: Option<f64> = None;
    let mut prev_ts: Option<f64> = None;
    let mut prev_px: Option<f64> = None;

    let (mut dur_sum, mut dur_sumsq) = (0.0f64, 0.0f64);
    let mut dur_n: i64 = 0;
    let mut dur_hist = vec![0i64; LOG_DUR_BINS];
    let mut dwell_hist = vec![0i64; DWELL_LOG_BINS];
    let mut dwell_n: i64 = 0;
    let (mut dwell_sum, mut dwell_sumsq, mut dwell_max) = (0.0f64, 0.0f64, 0.0f64);
    let mut dwell_seen_hours: std::collections::HashSet<i64> = std::collections::HashSet::new();

    let mut ret_acf = AutoCorr::new(MAX_LAG);
    let mut abs_acf = AutoCorr::new(MAX_LAG);
    let mut dur_acf = AutoCorr::new(MAX_LAG);
    let mut dwell_acf = AutoCorr::new(MAX_LAG);

    let mut zero_change: i64 = 0;
    let mut change_n: i64 = 0;
    // Insertion-ordered: `max(items(), key=count)` on ties keeps the first
    // inserted key in CPython dict iteration order, so an ordered map
    // (rather than a HashMap) is load-bearing for `modal_tick` on a tie.
    let mut tick_counts: Vec<(String, i64)> = Vec::new();
    let mut tick_index: HashMap<String, usize> = HashMap::new();
    let mut tick_capped = false;
    // Insertion-ordered for the same reason as `tick_counts`:
    // `characterize.py:387` takes `max(price_dec_hist.items(), key=count)` over
    // a plain dict, so a tie keeps the first decimal count seen. A `HashMap`
    // here made the tie-break not merely divergent but nondeterministic across
    // runs of the same input.
    let mut price_dec_counts: Vec<(usize, i64)> = Vec::new();
    let mut price_dec_index: HashMap<usize, usize> = HashMap::new();

    let mut size_log_hist = vec![0i64; 30];
    let mut size_dec_hist = vec![0i64; 9];
    let mut size_n: i64 = 0;
    let mut visits = LevelVisits::new(DWELL_ERA_START_TS as f64);

    let mut sess_count = vec![vec![0i64; 7]; 24];
    let mut sess_sumsq_ret = vec![vec![0.0f64; 7]; 24];

    let file = File::open(path)?;
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split(',').collect();
        if parts.len() < 3 {
            continue;
        }
        let (Ok(ts), Ok(px), Ok(sz)) = (
            parts[0].parse::<f64>(),
            parts[1].parse::<f64>(),
            parts[2].parse::<f64>(),
        ) else {
            continue;
        };
        if px <= 0.0 {
            continue;
        }
        n += 1;
        if first_ts.is_none() {
            first_ts = Some(ts);
        }
        last_ts = Some(ts);

        if ts >= DWELL_ERA_START_TS as f64 {
            dwell_seen_hours.insert((ts as i64).div_euclid(3600));
        }

        visits.push(ts, px, sz);

        let tsec = ts as i64;
        let hour = (tsec.div_euclid(3600)).rem_euclid(24) as usize;
        let dow = ((tsec.div_euclid(86400)) + 4).rem_euclid(7) as usize;

        let pd = decimals_used(parts[1]);
        if let Some(&idx) = price_dec_index.get(&pd) {
            price_dec_counts[idx].1 += 1;
        } else {
            price_dec_index.insert(pd, price_dec_counts.len());
            price_dec_counts.push((pd, 1));
        }

        size_n += 1;
        if sz > 0.0 {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "log10 bucket clamped into [0,29]"
            )]
            let idx = (sz.log10() as i64 + 9).clamp(0, 29) as usize;
            size_log_hist[idx] += 1;
        }
        size_dec_hist[decimals_used(parts[2]).min(8)] += 1;

        if let Some(pts) = prev_ts {
            let dt = ts - pts;
            if dt >= 0.0 {
                dur_n += 1;
                dur_sum += dt;
                dur_sumsq += dt * dt;
                dur_hist[log_bin(dt.max(1e-3), 1e-3, 86400.0, LOG_DUR_BINS)] += 1;
                dur_acf.push(dt);
                if ts >= DWELL_ERA_START_TS as f64 {
                    dwell_n += 1;
                    dwell_sum += dt;
                    dwell_sumsq += dt * dt;
                    dwell_acf.push(dt);
                    dwell_max = dwell_max.max(dt);
                    dwell_hist[log_bin(
                        dt.max(DWELL_LOG_LO_S),
                        DWELL_LOG_LO_S,
                        DWELL_LOG_HI_S,
                        DWELL_LOG_BINS,
                    )] += 1;
                }
            }
        }

        if let Some(ppx) = prev_px {
            let dpx = px - ppx;
            change_n += 1;
            if dpx == 0.0 {
                zero_change += 1;
            } else {
                let q = python_round8(dpx.abs());
                let key = crate::kernel::py_float_repr(q);
                if let Some(&idx) = tick_index.get(&key) {
                    tick_counts[idx].1 += 1;
                } else if !tick_capped {
                    tick_index.insert(key.clone(), tick_counts.len());
                    tick_counts.push((key, 1));
                    if tick_counts.len() >= TICK_DICT_CAP {
                        tick_capped = true;
                    }
                }
            }
            let ret = px.ln() - ppx.ln();
            ret_acf.push(ret);
            abs_acf.push(ret.abs());
            sess_sumsq_ret[hour][dow] += ret * ret;
        }
        sess_count[hour][dow] += 1;

        prev_ts = Some(ts);
        prev_px = Some(px);
    }
    visits.close();

    let mut modal_tick: Option<f64> = None;
    let mut tick_p10: Option<f64> = None;
    let mut tick_p50: Option<f64> = None;
    if !tick_counts.is_empty() {
        // `max_by_key` returns the last maximal element, which defeats the
        // insertion-ordered `tick_counts` above: CPython's `max` keeps the
        // first. Fold explicitly with a strict `>` so a tie holds the
        // earliest-inserted key, matching `max(items(), key=count)`.
        let (mtkey, _) = tick_counts
            .iter()
            .fold(None::<&(String, i64)>, |best, cur| match best {
                Some(b) if b.1 >= cur.1 => Some(b),
                _ => Some(cur),
            })
            .expect("nonempty");
        modal_tick = mtkey.parse::<f64>().ok();
        let mut items: Vec<(f64, i64)> = tick_counts
            .iter()
            .map(|(k, c)| (k.parse::<f64>().unwrap_or(0.0), *c))
            .collect();
        items.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        let tot: i64 = items.iter().map(|(_, c)| c).sum();
        let mut cum = 0i64;
        for (v, c) in &items {
            cum += c;
            #[expect(
                clippy::cast_precision_loss,
                reason = "tot is a bounded distinct-tick count"
            )]
            if tick_p10.is_none() && cum as f64 >= 0.10 * tot as f64 {
                tick_p10 = Some(*v);
            }
            #[expect(clippy::cast_precision_loss, reason = "same bound as above")]
            if cum as f64 >= 0.50 * tot as f64 {
                tick_p50 = Some(*v);
                break;
            }
        }
    }
    // Same first-wins fold as `modal_tick`, over the insertion-ordered counts.
    let price_dec_mode = price_dec_counts
        .iter()
        .fold(None::<&(usize, i64)>, |best, cur| match best {
            Some(b) if b.1 >= cur.1 => Some(b),
            _ => Some(cur),
        })
        .map(|(k, _)| *k);

    let dur_mean = if dur_n != 0 {
        dur_sum / dur_n as f64
    } else {
        0.0
    };
    let dur_var = if dur_n != 0 {
        dur_sumsq / dur_n as f64 - dur_mean * dur_mean
    } else {
        0.0
    };
    let dwell_mean = if dwell_n != 0 {
        dwell_sum / dwell_n as f64
    } else {
        0.0
    };
    let dwell_var = if dwell_n != 0 {
        dwell_sumsq / dwell_n as f64 - dwell_mean * dwell_mean
    } else {
        0.0
    };
    let (empty_hour_frac, max_empty_hour_run_h) = dwell_stats(first_ts, last_ts, &dwell_seen_hours);
    let mut dwell_p999_s: Option<f64> = None;
    if dwell_n != 0 {
        #[expect(clippy::cast_precision_loss, reason = "dwell_n is a bounded row count")]
        let threshold = (0.999 * dwell_n as f64).ceil() as i64;
        let mut cumulative = 0i64;
        for (index, &count) in dwell_hist.iter().enumerate() {
            cumulative += count;
            if cumulative >= threshold {
                assert!(
                    index != DWELL_LOG_BINS - 1,
                    "dwell p999 landed in the saturated bin"
                );
                dwell_p999_s = Some(
                    (DWELL_LOG_LO_S.ln()
                        + (index as f64 + 1.0) * (DWELL_LOG_HI_S.ln() - DWELL_LOG_LO_S.ln())
                            / DWELL_LOG_BINS as f64)
                        .exp(),
                );
                break;
            }
        }
    }

    Ok(json!({
        "pair": pair,
        "path": path.to_string_lossy(),
        "n_trades": n,
        "first_ts": first_ts,
        "last_ts": last_ts,
        "span_days": match (first_ts, last_ts) {
            (Some(f), Some(l)) => Some(((l - f) / 86400.0 * 10.0).round() / 10.0),
            _ => None,
        },
        "duration": {
            "mean_s": dur_mean,
            "var_s2": dur_var,
            "dispersion_index": if dur_mean != 0.0 { Some(dur_var / dur_mean) } else { None },
            "log_hist": dur_hist,
            "acf": dur_acf.acf(),
            "dwell": {
                "era_start_ts": DWELL_ERA_START_TS,
                "n_gaps": dwell_n,
                "mean_s": dwell_mean,
                "var_s2": dwell_var,
                "dispersion_index": if dwell_mean != 0.0 { Some(dwell_var / dwell_mean) } else { None },
                "acf": dwell_acf.acf(),
                "max_gap_s": dwell_max,
                "gap_p999_s": dwell_p999_s,
                "dwell_hist": dwell_hist,
                "empty_hour_frac": empty_hour_frac,
                "max_empty_hour_run_h": max_empty_hour_run_h,
            },
        },
        "returns": {
            "acf": ret_acf.acf(),
            "abs_acf": abs_acf.acf(),
            "zero_change_frac": if change_n != 0 { Some(zero_change as f64 / change_n as f64) } else { None },
            "modal_tick": modal_tick,
            "tick_p10": tick_p10,
            "tick_p50": tick_p50,
            "tick_dict_capped": tick_capped,
            "price_decimals_mode": price_dec_mode,
        },
        "size": {
            "log10_hist": size_log_hist,
            "decimals_used_hist": size_dec_hist,
            "round_frac": if size_n != 0 {
                Some(size_dec_hist[..3].iter().sum::<i64>() as f64 / size_n as f64)
            } else { None },
        },
        "level": visits.report(),
        "session": {
            "count_hour_dow": sess_count,
            "sumsq_ret_hour_dow": sess_sumsq_ret,
        },
    }))
}

/// Correctly-rounded decimal rounding to 8 places, matching CPython's
/// `round(x, 8)` (rounds the exact decimal value, not a naive
/// multiply-and-round which can misfire on the binary representation
/// error). Rust's fixed-precision float formatting is itself correctly
/// rounded, so formatting then reparsing reproduces the same decimal digit
/// CPython's `_Py_dg_dtoa`-based `round()` would choose.
fn python_round8(x: f64) -> f64 {
    format!("{x:.8}").parse().unwrap_or(x)
}

#[cfg(test)]
mod tests;
