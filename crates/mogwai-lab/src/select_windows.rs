// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! `analysis/select_windows.py`: the bar-frame intake station.
//!
//! Chooses which tick-data windows to buy, using cheap 1-minute bars as a
//! sampling frame. Bars cannot see microstructure - aggregation destroys
//! arrival burstiness, bounce and size dispersion - so this stratifies on what
//! a bar CAN measure and picks the calendar months whose feature vectors are
//! farthest apart. Whether those proxies coincided with microstructure regimes
//! is then a finding rather than an assumption.
//!
//! PORTED WHOLE, all four phases, because the corpus is OPEN: every new
//! instrument re-runs the purchase question, so this is reusable intake
//! machinery rather than one instrument's spent history. The BTCUSDT rejection
//! of volatility-stratified selection travels as a recorded prior on `select`
//! and `plan`, not as grounds to drop them - and per the preregistration's
//! section 7.1 only the `rv`-rank association was ever tested, so `plan` carries
//! a real rejection while `select`'s five-feature farthest-point method carries
//! no verdict at all.
//!
//! NO FROZEN ARTIFACT EXISTED to port against: the Python prints its results
//! and writes only a regenerable gitignored cache, and `targets-frozen.json` -
//! called its gate in three documents - is the BTCUSDT target set this never
//! touches. So `analysis/select-windows-blessed.json` was blessed from the
//! Python first (`scripts/bless_select_windows.py`) and this is matched against
//! it.
//!
//! ONE APPROVED DEVIATION, and it is NOT cosmetic - see [`squared`]. The Python
//! squares with `** 2`, which routes through libm's `pow` and is not correctly
//! rounded; this squares with a multiply, which always is. Eleven of the
//! 111,396 cached feature values differ by one or two ULPs as a result. No
//! month median moves on today's archives, so the blessed gate passes - but it
//! passes by coincidence rather than by construction, which is stated here
//! rather than left for someone to discover.
//!
//! ORDERING IS LOAD-BEARING THROUGHOUT. CPython's `sum()` over floats is
//! Neumaier-compensated, so every float accumulation here depends on the order
//! its terms arrive in: the hourly volatility buckets keep first-seen hour
//! order, the month table keeps first-seen month order, and both feed
//! [`crate::kernel::py_sum`]. Sorting either would move the last ulp of a
//! z-score and can reshuffle a selection.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::Path;

use crate::error::{LabError, LabResult};
use crate::kernel::{py_int_div, py_sum};

/// `ARCHIVES`, in the Python's dict-literal order. That order IS the cache's
/// key order, which is the month table's first-seen order, which is the term
/// order of every z-score sum - so it is a numerical input, not presentation.
pub const ARCHIVES: [(&str, &str); 4] = [
    ("NQ", "nq-1m_bk.zip"),
    ("ES", "es-1m_bk.zip"),
    ("CL", "cl-1m_bk.zip"),
    ("GC", "gc-1m.zip"),
];

/// `FEATURES`, in the Python's list order.
pub const FEATURES: [&str; 6] = [
    "rv",
    "vol_of_vol",
    "volume",
    "volume_cv",
    "zero_change",
    "gap",
];

/// Databento GLBX.MDP3 history does not reach back to 2007. Months before this
/// stay in the features - they describe the instrument - but cannot be
/// selected.
///
/// LOAD-BEARING BEYOND ELIGIBILITY, and the Python says so in as many words:
/// every z-score is computed over the eligible months ONLY, so moving this
/// constant does not merely admit or exclude candidates, it re-centres and
/// re-scales the whole feature space and can reshuffle a selection that looks
/// stable. Change it and re-read the entire selection.
pub const DATABENTO_START: &str = "2010-06";

/// How many month-equivalents of budget.
pub const BUDGET_MONTHS: usize = 9;

/// A full CME session is about 1380 one-minute bars. `rv` is an UNNORMALISED
/// sum over however many bars exist, so a stub session understates volatility
/// purely by bar count; features are scaled to this.
const FULL_SESSION: i64 = 1380;

/// Below this many bars a session is a holiday or half day rather than a regime
/// observation. The Python notes the old threshold of 200 let early closes
/// through.
const MIN_SESSION_BARS: i64 = 1000;

/// A month needs this many sessions per symbol to contribute.
const MIN_MONTH_SESSIONS: usize = 15;

/// Squares a deviation, with ONE multiply.
///
/// THE APPROVED DEVIATION FROM `select_windows.py`. The Python writes
/// `(v - mean) ** 2`, and CPython's `float ** int` calls libm's `pow`, which is
/// NOT correctly rounded: over the domain these features occupy it disagrees
/// with the correctly rounded product in roughly one value in 1,163. A single
/// IEEE multiply always is correctly rounded, so this function is exact where
/// the Python is not - verified against exact rational arithmetic, not merely
/// asserted: at `x = -783721.5825316639`, `x ** 2` gives
/// `614219518925.9357` while `x * x` gives `614219518925.9358`, and the
/// rational product rounds to the latter.
///
/// APPROVED BY REVIEW 2026-08-08, session 019fe1ff, on the ground that the
/// tool being OFFLINE makes correctness more important rather than less: a
/// purchase decision should not depend on the host libm when a correctly
/// rounded and portable operation exists. Reproducing `pow`'s error bug-for-bug
/// would make this tool's output a function of whichever libm the machine
/// happens to carry.
///
/// The approval was sought rather than assumed, and that distinction is worth
/// keeping: the wording here originally cited the `sqrt` precedent and declared
/// itself approved, which inherited that ruling's reasoning while helping
/// itself to its authority. The signature this program runs under says any new
/// parity deviation reopens the gate, so the ruling had to be someone else's.
///
/// MEASURED CONSEQUENCE, so nobody has to guess at the blast radius: of the
/// 111,396 values in a full feature sweep, ELEVEN differ from the Python's
/// cache, all `volume_cv` or `vol_of_vol`, all by one or two ULPs. None of them
/// moves a monthly median on the committed archives, so
/// `analysis/select-windows-blessed.json` still reproduces exactly. That is
/// luck rather than design - a different corpus could put one of these sessions
/// on a median's middle - and `scripts/compare_cme_caches.py` is the tool that
/// re-measures it.
fn squared(deviation: f64) -> f64 {
    deviation * deviation
}

/// One session's bar-frame features.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DayFeatures {
    pub rv: f64,
    pub vol_of_vol: f64,
    pub volume: f64,
    pub volume_cv: f64,
    pub zero_change: f64,
    pub gap: f64,
}

impl DayFeatures {
    /// By `FEATURES` name, for building the `SYMBOL.feature` rows.
    #[must_use]
    pub fn get(&self, feature: &str) -> Option<f64> {
        Some(match feature {
            "rv" => self.rv,
            "vol_of_vol" => self.vol_of_vol,
            "volume" => self.volume,
            "volume_cv" => self.volume_cv,
            "zero_change" => self.zero_change,
            "gap" => self.gap,
            _ => return None,
        })
    }
}

/// A parsed bar: timestamp parts, open, close, volume.
struct Bar {
    /// SECONDS since the epoch, not minutes.
    ///
    /// The Python builds a full `datetime` including seconds, compares whole
    /// timestamps for the duplicate/backwards check, and derives missing
    /// minutes from the full difference. Holding minute precision here made two
    /// valid rows at `17:00:00` and `17:00:30` behave differently: the Python
    /// accepts both and computes the second return, while a minute-resolution
    /// clock reads the second as a duplicate and drops it. The committed
    /// archives are minute-aligned, so no corpus gate could expose that -
    /// found by review instead.
    second: i64,
    hour: u32,
    /// Days since 1970-01-01 of the SESSION this bar belongs to.
    session_day: i64,
    open: f64,
    close: f64,
    volume: i64,
}

/// Days from 1970-01-01 for a proleptic Gregorian civil date. Howard Hinnant's
/// `days_from_civil`, which is exact for the range these archives cover.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day = i64::from(day);
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Civil date from days since 1970-01-01, the inverse of the above.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "month and day are bounded by construction"
    )]
    (
        if month <= 2 { year + 1 } else { year },
        month as u32,
        day as u32,
    )
}

/// Day of week, 0 = Monday, matching Python's `date.weekday()`.
fn weekday(days: i64) -> i64 {
    (days + 3).rem_euclid(7)
}

/// `parse_line`: seven semicolon-separated fields, `d/m/Y;H:M[:S];open;...;close;volume`.
///
/// Returns `None` on anything malformed, exactly as the Python does - it
/// catches `ValueError` AND `IndexError`, the latter because a time field with
/// no colon leaves the parts one element long, which used to kill a whole
/// feature sweep on one bad row.
fn parse_line(line: &str) -> Option<Bar> {
    let parts: Vec<&str> = line.trim_end_matches(['\r', '\n']).split(';').collect();
    if parts.len() != 7 {
        return None;
    }
    let mut date = parts[0].split('/');
    let day: u32 = date.next()?.parse().ok()?;
    let month: u32 = date.next()?.parse().ok()?;
    let year: i64 = date.next()?.parse().ok()?;
    if date.next().is_some() {
        // The Python unpacks into exactly three names, so a fourth field raises.
        return None;
    }
    let mut hms = parts[1].split(':');
    let hour: u32 = hms.next()?.parse().ok()?;
    let minute_of_hour: u32 = hms.next()?.parse().ok()?;
    // Seconds are optional; anything past them is ignored, as the Python's
    // indexing does.
    let second: u32 = match hms.next() {
        Some(raw) => raw.parse().ok()?,
        None => 0,
    };
    // `datetime` refuses these outright and the Python turns that into a
    // skipped row. Day-of-month is validated against the actual month LENGTH,
    // not against 31: `31/02/2024` is a `ValueError` in Python, and accepting
    // it here would have manufactured a bar on a date that does not exist and
    // silently shifted it into March.
    if !(1..=12).contains(&month)
        || day < 1
        || day > days_in_month(year, month)
        || hour > 23
        || minute_of_hour > 59
        || second > 59
    {
        return None;
    }
    let open: f64 = parts[2].parse().ok()?;
    let close: f64 = parts[5].parse().ok()?;
    let volume: i64 = parts[6].parse().ok()?;

    let civil = days_from_civil(year, month, day);
    // Full SECOND resolution: the Python compares whole `datetime`s, so two
    // rows inside the same minute are distinct rather than duplicates.
    let stamp = civil * 86_400
        + i64::from(hour) * 3_600
        + i64::from(minute_of_hour) * 60
        + i64::from(second);
    // `session_date`: CME runs 17:00 to 16:00 US Central, so a bar at or after
    // 17:00 belongs to the NEXT calendar day's session.
    let session_day = if hour >= 17 { civil + 1 } else { civil };
    Some(Bar {
        second: stamp,
        hour,
        session_day,
        open,
        close,
        volume,
    })
}

/// Days in a civil month, so an impossible date is refused rather than rolled
/// forward.
fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

/// Formats days-since-epoch as `YYYY-MM-DD`, the cache's session key.
fn iso_date(days: i64) -> String {
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// One session under construction.
struct Slot {
    ret2: f64,
    n: i64,
    volume: i64,
    vols: Vec<i64>,
    max_r2: f64,
    zero: i64,
    /// INSERTION-ORDERED hourly squared-return buckets. A `HashMap` here would
    /// change the term order of the `py_sum` over hourly volatilities and move
    /// the last ulp of `vol_of_vol`.
    hourly: Vec<(u32, f64)>,
    hourly_index: HashMap<u32, usize>,
    gap: f64,
}

impl Slot {
    fn new() -> Self {
        Self {
            ret2: 0.0,
            n: 0,
            volume: 0,
            vols: Vec::new(),
            max_r2: 0.0,
            zero: 0,
            hourly: Vec::new(),
            hourly_index: HashMap::new(),
            gap: 0.0,
        }
    }

    fn add_hourly(&mut self, hour: u32, value: f64) {
        if let Some(&idx) = self.hourly_index.get(&hour) {
            self.hourly[idx].1 += value;
        } else {
            self.hourly_index.insert(hour, self.hourly.len());
            self.hourly.push((hour, value));
        }
    }
}

/// `build_features`: one streaming pass over an archive, session by session.
///
/// # Errors
/// Propagates I/O and zip failures, and refuses an archive with no members.
pub fn build_features(path: &Path) -> LabResult<Vec<(String, DayFeatures)>> {
    let file = std::fs::File::open(path)?;
    let mut archive = zip::ZipArchive::new(BufReader::new(file))
        .map_err(|e| LabError::refusal(format!("{}: {e}", path.display())))?;
    if archive.is_empty() {
        return Err(LabError::refusal(format!(
            "{}: archive carries no members",
            path.display()
        )));
    }
    // `z.infolist()[0].filename`: the first member, in archive order.
    let member = archive
        .by_index(0)
        .map_err(|e| LabError::refusal(format!("{}: {e}", path.display())))?;

    let mut days: Vec<(i64, Slot)> = Vec::new();
    let mut day_index: HashMap<i64, usize> = HashMap::new();
    let mut prev_stamp: Option<i64> = None;
    let mut prev_close: Option<f64> = None;
    let mut prev_session: Option<i64> = None;

    for raw in BufReader::new(member).lines() {
        // `decode("ascii", "replace")`: undecodable bytes become replacement
        // characters, which then fail the numeric parse and skip the row - the
        // same outcome as reading them lossily here.
        let line = match raw {
            Ok(line) => line,
            Err(_) => continue,
        };
        let Some(bar) = parse_line(&line) else {
            continue;
        };
        if prev_stamp.is_some_and(|prev| bar.second <= prev) {
            continue; // duplicate or backwards timestamp
        }
        if bar.close <= 0.0 || bar.open <= 0.0 {
            continue;
        }
        if weekday(bar.session_day) == 5 {
            continue; // stray Saturday bars
        }

        let slot_idx = if let Some(&idx) = day_index.get(&bar.session_day) {
            idx
        } else {
            let mut slot = Slot::new();
            if let (Some(close), Some(previous)) = (prev_close, prev_session)
                && previous != bar.session_day
            {
                slot.gap = (bar.open / close).ln().abs();
            }
            day_index.insert(bar.session_day, days.len());
            days.push((bar.session_day, slot));
            days.len() - 1
        };

        if let (Some(close), Some(previous), Some(prev_seconds)) =
            (prev_close, prev_session, prev_stamp)
            && previous == bar.session_day
        {
            // The vendor omits no-trade minutes rather than writing a
            // zero-volume bar. Those minutes are real zero-return, zero-volume
            // observations, and dropping them inflates `zero_change` and
            // `volume_cv` in exactly the illiquid regimes this cares about.
            // `int((stamp - prev_stamp).total_seconds() // 60) - 1`: FLOOR
            // division on the second difference, so two bars inside one minute
            // contribute no missing minutes rather than a negative count.
            let missing = (bar.second - prev_seconds).div_euclid(60) - 1;
            let slot = &mut days[slot_idx].1;
            if missing > 0 {
                slot.zero += missing;
                slot.n += missing;
                slot.vols.extend(std::iter::repeat_n(0, missing as usize));
            }
            let r = (bar.close / close).ln();
            slot.ret2 += r * r;
            // `gc-1m.zip` is RAW rather than back-adjusted, so its roll sessions
            // carry a contract switch as one enormous one-minute return.
            // Tracking the session maximum lets it be dropped below.
            if r * r > slot.max_r2 {
                slot.max_r2 = r * r;
            }
            slot.add_hourly(bar.hour, r * r);
            if r == 0.0 {
                slot.zero += 1;
            }
            slot.n += 1;
        }
        let slot = &mut days[slot_idx].1;
        slot.volume += bar.volume;
        slot.vols.push(bar.volume);

        prev_stamp = Some(bar.second);
        prev_close = Some(bar.close);
        prev_session = Some(bar.session_day);
    }

    let mut out = Vec::new();
    for (session, slot) in &days {
        if slot.n < MIN_SESSION_BARS {
            continue; // holiday or half session, not a regime observation
        }
        let count = i64::try_from(slot.vols.len())
            .map_err(|_| LabError::refusal("session bar count overflows an i64"))?;
        let volume_sum: i64 = slot.vols.iter().sum();
        let mean_v = py_int_div(volume_sum, count);
        #[expect(clippy::cast_precision_loss, reason = "bar counts are far below 2^53")]
        let var_v = py_sum(slot.vols.iter().map(|v| squared(*v as f64 - mean_v))) / count as f64;
        // Buckets hold squared returns, so take the root to get an hourly
        // VOLATILITY before measuring its dispersion; the coefficient of
        // variation of a variance is a different, more skewed quantity.
        let hourly: Vec<f64> = slot.hourly.iter().map(|(_, v)| v.sqrt()).collect();
        #[expect(clippy::cast_precision_loss, reason = "at most 24 buckets")]
        let mean_h = if hourly.is_empty() {
            0.0
        } else {
            py_sum(hourly.iter().copied()) / hourly.len() as f64
        };
        #[expect(clippy::cast_precision_loss, reason = "at most 24 buckets")]
        let var_h = if hourly.is_empty() {
            0.0
        } else {
            py_sum(hourly.iter().map(|h| squared(h - mean_h))) / hourly.len() as f64
        };
        // Drop the single largest squared return (the roll artifact in raw GC)
        // and scale to a full session so short days compare like for like.
        let trimmed = (slot.ret2 - slot.max_r2).max(0.0);
        #[expect(
            clippy::cast_precision_loss,
            reason = "session bar counts are far below 2^53"
        )]
        let n_float = slot.n as f64;
        #[expect(clippy::cast_precision_loss, reason = "1380")]
        let full = FULL_SESSION as f64;
        #[expect(
            clippy::cast_precision_loss,
            reason = "session volume is far below 2^53"
        )]
        let volume_float = slot.volume as f64;
        out.push((
            iso_date(*session),
            DayFeatures {
                rv: (trimmed * full / n_float).sqrt(),
                vol_of_vol: if mean_h > 0.0 {
                    var_h.sqrt() / mean_h
                } else {
                    0.0
                },
                volume: volume_float * full / n_float,
                volume_cv: if mean_v > 0.0 {
                    var_v.sqrt() / mean_v
                } else {
                    0.0
                },
                zero_change: py_int_div(slot.zero, slot.n),
                gap: slot.gap,
            },
        ));
    }
    Ok(out)
}

/// One symbol's sessions, in archive order.
pub type SymbolSessions = Vec<(String, DayFeatures)>;

/// The feature cache: symbols in `ARCHIVES` order, sessions in archive order.
pub type Cache = Vec<(String, SymbolSessions)>;

/// One month's cross-symbol feature row, keyed `SYMBOL.feature`.
pub type MonthRow = std::collections::BTreeMap<String, f64>;

/// One month's sessions grouped by symbol, both insertion-ordered.
pub type PerSymbolRows = Vec<(String, Vec<DayFeatures>)>;

/// A month table: months in FIRST-SEEN order, each with its feature row.
pub type MonthTable = Vec<(String, MonthRow)>;

/// A month's z-scored feature vector, laid out in the sorted key order.
pub type MonthVectors = Vec<(String, Vec<f64>)>;

/// True median. Taking the upper middle value on an even-sized month is a real
/// bias here: months run 19 to 23 sessions, so the even case is common, and it
/// moved which months the stratification picked.
fn median(values: &mut [f64]) -> LabResult<f64> {
    let n = values.len();
    if n == 0 {
        return Err(LabError::refusal("median of empty sequence"));
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = n / 2;
    Ok(if n % 2 == 1 {
        values[mid]
    } else {
        0.5 * (values[mid - 1] + values[mid])
    })
}

/// `monthly`: collapse sessions to months, per symbol, as medians.
///
/// Returns months in FIRST-SEEN order, which is the term order every later
/// `py_sum` walks.
///
/// # Errors
/// [`LabError::Refusal`] if a month's feature list is empty.
pub fn monthly(cache: &Cache) -> LabResult<MonthTable> {
    // month -> symbol -> rows, both insertion-ordered.
    let mut months: Vec<(String, PerSymbolRows)> = Vec::new();
    let mut month_index: HashMap<String, usize> = HashMap::new();
    for (symbol, sessions) in cache {
        for (day, feats) in sessions {
            let key = day[..7].to_string();
            let idx = *month_index.entry(key.clone()).or_insert_with(|| {
                months.push((key, Vec::new()));
                months.len() - 1
            });
            let by_symbol = &mut months[idx].1;
            if let Some(entry) = by_symbol.iter_mut().find(|(s, _)| s == symbol) {
                entry.1.push(*feats);
            } else {
                by_symbol.push((symbol.clone(), vec![*feats]));
            }
        }
    }

    let mut out = Vec::new();
    for (month, by_symbol) in &months {
        if by_symbol.len() < ARCHIVES.len() {
            continue;
        }
        let mut row = MonthRow::new();
        let mut ok = true;
        for (symbol, rows) in by_symbol {
            if rows.len() < MIN_MONTH_SESSIONS {
                ok = false;
                break;
            }
            for feature in FEATURES {
                let mut values: Vec<f64> = rows
                    .iter()
                    .map(|r| r.get(feature).expect("named feature"))
                    .collect();
                row.insert(format!("{symbol}.{feature}"), median(&mut values)?);
            }
        }
        if ok {
            out.push((month.clone(), row));
        }
    }
    Ok(out)
}

/// `zscore`: centre and scale each feature over the months given.
///
/// Returns the vectors in the same order as the input and the sorted key order
/// they are laid out in.
///
/// # Errors
/// [`LabError::Refusal`] on an empty month table.
pub fn zscore(months: &[(String, MonthRow)]) -> LabResult<(MonthVectors, Vec<String>)> {
    let first = months
        .first()
        .ok_or_else(|| LabError::refusal("z-score of an empty month table"))?;
    let keys: Vec<String> = first.1.keys().cloned().collect();

    let mut stats: HashMap<&str, (f64, f64)> = HashMap::new();
    #[expect(clippy::cast_precision_loss, reason = "a few hundred months")]
    let count = months.len() as f64;
    for key in &keys {
        let values: Vec<f64> = months.iter().map(|(_, row)| row[key.as_str()]).collect();
        let mean = py_sum(values.iter().copied()) / count;
        let var = py_sum(values.iter().map(|v| squared(v - mean))) / count;
        stats.insert(key, (mean, if var > 0.0 { var.sqrt() } else { 1.0 }));
    }

    let vectors = months
        .iter()
        .map(|(month, row)| {
            let vector = keys
                .iter()
                .map(|key| {
                    let (mean, scale) = stats[key.as_str()];
                    (row[key.as_str()] - mean) / scale
                })
                .collect();
            (month.clone(), vector)
        })
        .collect();
    Ok((vectors, keys))
}

/// `farthest_point`: greedy max-min selection from the seeds outward.
///
/// # Errors
/// [`LabError::Refusal`] if the pool runs out before `k` are chosen.
pub fn farthest_point(
    vectors: &MonthVectors,
    k: usize,
    seeds: &[String],
) -> LabResult<Vec<String>> {
    let lookup: HashMap<&str, &Vec<f64>> = vectors.iter().map(|(m, v)| (m.as_str(), v)).collect();
    let mut chosen: Vec<String> = seeds.to_vec();
    while chosen.len() < k {
        let mut best: Option<String> = None;
        let mut best_distance = -1.0f64;
        for (month, vector) in vectors {
            if chosen.iter().any(|c| c == month) {
                continue;
            }
            let distance = chosen
                .iter()
                .map(|c| {
                    let other = lookup[c.as_str()];
                    py_sum(vector.iter().zip(other.iter()).map(|(a, b)| squared(a - b))).sqrt()
                })
                .fold(f64::INFINITY, f64::min);
            // Strict `>`: on a tie the Python keeps the month it saw FIRST,
            // which is the month table's insertion order.
            if distance > best_distance {
                best = Some(month.clone());
                best_distance = distance;
            }
        }
        let Some(pick) = best else {
            return Err(LabError::refusal(
                "farthest-point ran out of candidates before the budget was filled",
            ));
        };
        chosen.push(pick);
    }
    Ok(chosen)
}

/// The `select` phase's result.
pub struct Selection {
    pub eligible_count: usize,
    pub eligible_first: String,
    pub eligible_last: String,
    pub keys: Vec<String>,
    pub vectors: MonthVectors,
    pub seeds: Vec<String>,
    /// In pick order, seeds first.
    pub chosen: Vec<String>,
}

/// `phase_select`: eligible months, z-scores, seeds, farthest-point basket.
///
/// # Errors
/// Propagates the refusals of the stages it drives.
pub fn select(months: &[(String, MonthRow)]) -> LabResult<Selection> {
    let eligible: Vec<(String, MonthRow)> = months
        .iter()
        .filter(|(m, _)| m.as_str() >= DATABENTO_START)
        .cloned()
        .collect();
    if eligible.is_empty() {
        return Err(LabError::refusal("no months are eligible"));
    }
    let (vectors, keys) = zscore(&eligible)?;

    let nq_rv = keys
        .iter()
        .position(|k| k == "NQ.rv")
        .ok_or_else(|| LabError::refusal("NQ.rv is missing from the feature keys"))?;
    // Seed with the two months that must be in any sample: the most volatile
    // (stress regime) and the most recent (current algo population).
    //
    // `max()` on a tie keeps the FIRST maximal element in CPython, and
    // `max(vectors)` over a dict maxes the KEYS, which is the lexicographic
    // month string rather than any feature.
    let stress = vectors
        .iter()
        .fold(None::<&(String, Vec<f64>)>, |best, cur| match best {
            Some(b) if b.1[nq_rv] >= cur.1[nq_rv] => Some(b),
            _ => Some(cur),
        })
        .expect("nonempty")
        .0
        .clone();
    let recent = vectors
        .iter()
        .map(|(m, _)| m.clone())
        .max()
        .expect("nonempty");
    // Dedupe: if the most volatile month IS the most recent one, seeding with
    // both yields a basket one month short while looking full.
    let seeds = if stress == recent {
        vec![stress.clone()]
    } else {
        vec![stress.clone(), recent.clone()]
    };
    let chosen = farthest_point(&vectors, BUDGET_MONTHS, &seeds)?;

    let mut sorted_months: Vec<&String> = eligible.iter().map(|(m, _)| m).collect();
    sorted_months.sort();
    Ok(Selection {
        eligible_count: eligible.len(),
        eligible_first: (*sorted_months.first().expect("nonempty")).clone(),
        eligible_last: (*sorted_months.last().expect("nonempty")).clone(),
        keys,
        vectors,
        seeds,
        chosen,
    })
}

/// The columns `phase_drift` reports, in its order.
pub const DRIFT_COLUMNS: [&str; 6] = [
    "NQ.zero_change",
    "NQ.volume_cv",
    "NQ.volume",
    "ES.zero_change",
    "CL.zero_change",
    "GC.zero_change",
];

/// `phase_drift`: yearly medians, answering whether the microstructure proxies
/// are era-stable, which decides how much budget may go on old data.
///
/// NOTE THE DIFFERENT MEDIAN. This phase takes `vals[len(vals) // 2]`, the
/// UPPER middle on an even count, where [`monthly`] uses the true median. That
/// is a real difference in the Python rather than a slip to be tidied: the two
/// are separate estimators and only the monthly one was ever argued about, so
/// the port keeps both exactly as they are.
///
/// Returns years in ascending order with one value per [`DRIFT_COLUMNS`] entry.
///
/// # Errors
/// [`LabError::Refusal`] if a year carries no months.
pub fn drift(months: &[(String, MonthRow)]) -> LabResult<Vec<(String, Vec<f64>)>> {
    let mut years: Vec<(String, Vec<&MonthRow>)> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for (month, row) in months {
        let year = month[..4].to_string();
        let at = *index.entry(year.clone()).or_insert_with(|| {
            years.push((year, Vec::new()));
            years.len() - 1
        });
        years[at].1.push(row);
    }
    years.sort_by(|a, b| a.0.cmp(&b.0));

    years
        .into_iter()
        .map(|(year, rows)| {
            if rows.is_empty() {
                return Err(LabError::refusal(format!("{year} carries no months")));
            }
            let values = DRIFT_COLUMNS
                .iter()
                .map(|column| {
                    let mut column_values: Vec<f64> = rows.iter().map(|row| row[*column]).collect();
                    column_values
                        .sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                    column_values[column_values.len() / 2]
                })
                .collect();
            Ok((year, values))
        })
        .collect()
}

/// CPython's `round()`: half to EVEN, unlike Rust's `round`, which goes half
/// away from zero. `phase_plan` indexes with it, so the two disagree on an
/// exact half and pick different months.
fn py_round(value: f64) -> f64 {
    let floor = value.floor();
    let diff = value - floor;
    // Round up on a clear majority, or on an exact half when the floor is ODD -
    // which is what carries a tie to the even neighbour.
    if diff > 0.5 || (diff == 0.5 && floor % 2.0 != 0.0) {
        floor + 1.0
    } else {
        floor
    }
}

/// The percentile rungs `phase_plan` reports.
pub const PLAN_PERCENTILES: [i64; 6] = [0, 20, 40, 60, 80, 100];

/// The eras `phase_plan` probes for historical drift.
pub const PLAN_ERAS: [(&str, &str); 3] = [
    ("2010-06", "2013-12"),
    ("2014-01", "2017-12"),
    ("2018-01", "2021-12"),
];

/// One era's stress and calm probe.
pub struct EraProbe {
    pub lo: String,
    pub hi: String,
    pub stress: String,
    pub stress_rv: f64,
    pub calm: String,
    pub calm_rv: f64,
}

/// `phase_plan`'s result.
pub struct Plan {
    pub pool_first: String,
    pub pool_last: String,
    pub pool_len: usize,
    /// One `(percentile, month)` per [`PLAN_PERCENTILES`] rung.
    pub stratified: Vec<(i64, String)>,
    pub eras: Vec<EraProbe>,
}

/// `phase_plan`: budget-aware plan - the bulk of the spend inside the current
/// microstructure era, stratified by volatility, plus older probes for drift.
///
/// # Errors
/// [`LabError::Refusal`] if no months are eligible or an era pool is empty.
pub fn plan(months: &[(String, MonthRow)]) -> LabResult<Plan> {
    let table: HashMap<&str, &MonthRow> = months.iter().map(|(m, r)| (m.as_str(), r)).collect();
    let mut eligible: Vec<&str> = months
        .iter()
        .map(|(m, _)| m.as_str())
        .filter(|m| *m >= DATABENTO_START)
        .collect();
    eligible.sort_unstable();
    if eligible.is_empty() {
        return Err(LabError::refusal("no months are eligible"));
    }

    // The last 30 ELIGIBLE months, which is not the last 30 CALENDAR months: a
    // month absent from the features silently extends the pool further back.
    // On the committed archives the two coincide, so the Python states this
    // rather than fixing it - and prints the pool's true span precisely so a
    // divergence is visible rather than assumed away.
    let recent: Vec<&str> = eligible.iter().rev().take(30).rev().copied().collect();
    let mut ranked = recent.clone();
    // `sorted` is STABLE, so ties keep the ascending month order `recent`
    // already carries.
    ranked.sort_by(|a, b| {
        table[a]["NQ.rv"]
            .partial_cmp(&table[b]["NQ.rv"])
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    #[expect(clippy::cast_precision_loss, reason = "at most 30 months")]
    let span = (ranked.len() - 1) as f64;
    let stratified = PLAN_PERCENTILES
        .iter()
        .map(|percentile| {
            #[expect(clippy::cast_precision_loss, reason = "0 to 100")]
            let raw = py_round(*percentile as f64 / 100.0 * span);
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "bounded by the pool length"
            )]
            let index = (raw as usize).min(ranked.len() - 1);
            (*percentile, ranked[index].to_string())
        })
        .collect();

    let eras = PLAN_ERAS
        .iter()
        .map(|(lo, hi)| {
            let pool: Vec<&str> = eligible
                .iter()
                .copied()
                .filter(|m| *m >= *lo && *m <= *hi)
                .collect();
            if pool.is_empty() {
                return Err(LabError::refusal(format!("{lo}..{hi} has no months")));
            }
            // `max`/`min` keep the FIRST extremal element on a tie.
            let stress = pool
                .iter()
                .copied()
                .fold(None::<&str>, |best, cur| match best {
                    Some(b) if table[b]["NQ.rv"] >= table[cur]["NQ.rv"] => Some(b),
                    _ => Some(cur),
                })
                .expect("nonempty");
            let calm = pool
                .iter()
                .copied()
                .fold(None::<&str>, |best, cur| match best {
                    Some(b) if table[b]["NQ.rv"] <= table[cur]["NQ.rv"] => Some(b),
                    _ => Some(cur),
                })
                .expect("nonempty");
            Ok(EraProbe {
                lo: (*lo).to_string(),
                hi: (*hi).to_string(),
                stress: stress.to_string(),
                stress_rv: table[stress]["NQ.rv"],
                calm: calm.to_string(),
                calm_rv: table[calm]["NQ.rv"],
            })
        })
        .collect::<LabResult<Vec<EraProbe>>>()?;

    Ok(Plan {
        pool_first: recent.first().expect("nonempty").to_string(),
        pool_last: recent.last().expect("nonempty").to_string(),
        pool_len: recent.len(),
        stratified,
        eras,
    })
}

/// The percentile of each pick within the eligible span, by `NQ.rv`.
///
/// # Errors
/// [`LabError::Refusal`] if a chosen month is not in the eligible table.
pub fn nq_rv_percentiles(
    months: &[(String, MonthRow)],
    chosen: &[String],
) -> LabResult<Vec<(String, f64)>> {
    let eligible: Vec<&(String, MonthRow)> = months
        .iter()
        .filter(|(m, _)| m.as_str() >= DATABENTO_START)
        .collect();
    // `sorted(eligible, key=...)` is STABLE, so ties keep the month table's
    // order rather than falling back to the month name.
    let mut ordered: Vec<&str> = eligible.iter().map(|(m, _)| m.as_str()).collect();
    ordered.sort_by(|a, b| {
        let left = eligible.iter().find(|(m, _)| m == a).expect("present");
        let right = eligible.iter().find(|(m, _)| m == b).expect("present");
        left.1["NQ.rv"]
            .partial_cmp(&right.1["NQ.rv"])
            .expect("finite")
    });
    #[expect(clippy::cast_precision_loss, reason = "a few hundred months")]
    let span = (ordered.len() - 1) as f64;
    let mut sorted_chosen: Vec<&String> = chosen.iter().collect();
    sorted_chosen.sort();
    sorted_chosen
        .into_iter()
        .map(|month| {
            let index = ordered
                .iter()
                .position(|m| m == month)
                .ok_or_else(|| LabError::refusal(format!("{month} is not eligible")))?;
            #[expect(clippy::cast_precision_loss, reason = "a few hundred months")]
            Ok((month.clone(), 100.0 * index as f64 / span))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE DISCRIMINATING CASE FOR THE SQUARING DEVIATION. Pinned so nobody
    /// "restores parity" by reaching for `powf(2.0)` - which is the same libm
    /// `pow` CPython calls, and therefore the same wrong answer.
    ///
    /// At this input the two disagree by one ULP, and exact rational
    /// arithmetic says the multiply is the correctly rounded one. CPython's
    /// `x ** 2` here gives `614219518925.9357`.
    #[test]
    fn squaring_uses_a_multiply_rather_than_pow() {
        let x = -783_721.582_531_663_9_f64;
        let by_multiply = squared(x);
        let by_pow = x.powf(2.0);
        assert_ne!(
            by_multiply.to_bits(),
            by_pow.to_bits(),
            "if these agree the case has stopped discriminating and the deviation needs \
             re-deriving on this platform, not deleting"
        );
        assert_eq!(by_multiply.to_bits(), 614_219_518_925.935_8_f64.to_bits());
        assert_eq!(by_pow.to_bits(), 614_219_518_925.935_7_f64.to_bits());
    }

    /// `py_round` is half-to-EVEN, like CPython's `round`, not half-away-from-
    /// zero like Rust's. `phase_plan` indexes with it, so the two rules pick
    /// different months on an exact half.
    #[test]
    fn rounding_is_half_to_even() {
        assert_eq!(py_round(0.5), 0.0);
        assert_eq!(py_round(1.5), 2.0);
        assert_eq!(py_round(2.5), 2.0);
        assert_eq!(py_round(3.5), 4.0);
        assert_eq!(py_round(-0.5), -0.0);
        assert_eq!(py_round(-1.5), -2.0);
        assert_eq!(py_round(2.4), 2.0);
        assert_eq!(py_round(2.6), 3.0);
        // Rust's own `round` disagrees on every exact half, which is the point.
        assert_ne!(py_round(2.5), 2.5_f64.round());
    }

    /// The session label follows the CME convention: 17:00 CT or later belongs
    /// to the NEXT calendar day's session, and the weekday helper has to agree
    /// with Python's `date.weekday()` so the Saturday filter drops the right
    /// bars.
    #[test]
    fn session_labelling_and_weekdays_match_the_convention() {
        // 1970-01-01 was a Thursday, which is 3 under Python's Monday-zero rule.
        assert_eq!(weekday(0), 3);
        // 2024-01-06 was a Saturday.
        assert_eq!(weekday(days_from_civil(2024, 1, 6)), 5);

        let evening = parse_line("05/01/2024;17:00;100.0;0;0;100.5;7").expect("parses");
        assert_eq!(evening.session_day, days_from_civil(2024, 1, 6));
        let morning = parse_line("05/01/2024;16:59;100.0;0;0;100.5;7").expect("parses");
        assert_eq!(morning.session_day, days_from_civil(2024, 1, 5));
    }

    /// SECONDS PARTICIPATE IN ORDER. The Python compares whole `datetime`s, so
    /// two rows inside one minute are distinct observations. Holding minute
    /// precision made the second one look like a duplicate and dropped it,
    /// which no corpus gate could expose because the committed archives are
    /// minute-aligned.
    ///
    /// The assertion is on the ORDERING CONSEQUENCE rather than on the field,
    /// because that is what the feature loop acts on.
    #[test]
    fn seconds_distinguish_two_bars_inside_one_minute() {
        let first = parse_line("05/01/2024;17:00:00;100.0;0;0;100.5;7").expect("parses");
        let second = parse_line("05/01/2024;17:00:30;100.0;0;0;100.6;7").expect("parses");
        assert!(
            second.second > first.second,
            "the later bar must sort after the earlier one, not equal to it"
        );
        // And the missing-minute arithmetic must floor to zero across them
        // rather than going negative.
        assert_eq!((second.second - first.second).div_euclid(60) - 1, -1);

        // A full minute apart contributes no missing minutes either; two
        // minutes apart contributes exactly one.
        let next_minute = parse_line("05/01/2024;17:01:00;100.0;0;0;100.6;7").expect("parses");
        assert_eq!((next_minute.second - first.second).div_euclid(60) - 1, 0);
        let two_minutes = parse_line("05/01/2024;17:02:00;100.0;0;0;100.6;7").expect("parses");
        assert_eq!((two_minutes.second - first.second).div_euclid(60) - 1, 1);
    }

    /// `datetime` refuses impossible civil dates and out-of-range times, and
    /// the Python turns that into a skipped row. Validating the day against
    /// `1..=31` alone accepted `31/02`, which would have manufactured a bar on
    /// a date that does not exist and silently shifted it into March.
    #[test]
    fn impossible_dates_and_times_are_refused() {
        assert!(parse_line("31/02/2024;12:00;100.0;0;0;100.5;7").is_none());
        assert!(parse_line("30/02/2024;12:00;100.0;0;0;100.5;7").is_none());
        assert!(parse_line("31/04/2024;12:00;100.0;0;0;100.5;7").is_none());
        assert!(parse_line("00/01/2024;12:00;100.0;0;0;100.5;7").is_none());
        assert!(parse_line("01/13/2024;12:00;100.0;0;0;100.5;7").is_none());
        assert!(parse_line("01/01/2024;24:00;100.0;0;0;100.5;7").is_none());
        assert!(parse_line("01/01/2024;12:60;100.0;0;0;100.5;7").is_none());
        assert!(parse_line("01/01/2024;12:00:60;100.0;0;0;100.5;7").is_none());

        // Leap years, both directions: 2024 has a 29 February and 2023 does
        // not, and 1900 is not a leap year while 2000 is.
        assert!(parse_line("29/02/2024;12:00;100.0;0;0;100.5;7").is_some());
        assert!(parse_line("29/02/2023;12:00;100.0;0;0;100.5;7").is_none());
        assert!(parse_line("29/02/1900;12:00;100.0;0;0;100.5;7").is_none());
        assert!(parse_line("29/02/2000;12:00;100.0;0;0;100.5;7").is_some());
    }

    /// `parse_line` returns nothing rather than raising, on every shape the
    /// Python's `except (ValueError, IndexError)` swallows. The `IndexError`
    /// half is the one that matters: a time field with no colon used to kill a
    /// whole feature sweep on one malformed row.
    #[test]
    fn malformed_rows_are_skipped_not_fatal() {
        assert!(parse_line("05/01/2024;17:00;100.0;0;0;100.5;7").is_some());
        // Too few fields.
        assert!(parse_line("05/01/2024;17:00;100.0").is_none());
        // Time with no colon - the IndexError case.
        assert!(parse_line("05/01/2024;1700;100.0;0;0;100.5;7").is_none());
        // Date with too few slashes.
        assert!(parse_line("05-01-2024;17:00;100.0;0;0;100.5;7").is_none());
        // Non-numeric price and volume.
        assert!(parse_line("05/01/2024;17:00;abc;0;0;100.5;7").is_none());
        assert!(parse_line("05/01/2024;17:00;100.0;0;0;100.5;x").is_none());
        // Seconds are optional and ignored.
        assert!(parse_line("05/01/2024;17:00:30;100.0;0;0;100.5;7").is_some());
    }

    /// The true median, which differs from `drift`'s upper-middle rule on an
    /// even count. Months run 19 to 23 sessions, so the even case is common and
    /// the Python records that taking the upper middle moved which months the
    /// stratification picked.
    #[test]
    fn the_monthly_median_is_the_true_one() {
        assert_eq!(median(&mut [3.0, 1.0, 2.0]).unwrap(), 2.0);
        assert_eq!(median(&mut [4.0, 1.0, 2.0, 3.0]).unwrap(), 2.5);
        assert!(median(&mut []).is_err());
    }
}
