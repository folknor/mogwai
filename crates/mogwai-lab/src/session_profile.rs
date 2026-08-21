// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! `analysis/fit_session_profile.py`: the calendar-conditional
//! `SessionProfile` fit for MNQ from the NQ one-minute archive.
//!
//! Ownership model, assumed here as it is there: `SessionCalendar` owns hard
//! closure, `SessionProfile` owns relative arrival and volatility WHILE
//! open, `mean_event_duration_s` the unconditional baseline. So the fitted
//! factors are conditional on being open, exposure is calendar-open minutes,
//! and closed minutes contribute neither activity nor exposure.
//!
//! Every threshold in the `preregistered` block below was fixed before the
//! fit was first run; changing one after reading a result invalidates the
//! acceptance claim it supports.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::error::{LabError, LabResult};
use crate::kernel::py_sum;

// --- preregistered constants ----------------------------------------------

/// A cell is material when observed/fitted leaves `[1/R, R]`.
pub const INTERACTION_RATIO_LIMIT: f64 = 1.25;
/// Separability is rejected when more than this share of open exposure sits
/// in material cells.
pub const MAX_MATERIAL_EXPOSURE_SHARE: f64 = 0.05;
/// Calendar-defined, never selected from results. MNQ and MES did not list
/// until May 2019, so the instrument being modelled exists only in the third.
pub const ERAS: [(&str, i32, i32); 3] = [
    ("early", 2009, 2014),
    ("middle", 2015, 2019),
    ("recent", 2020, 2026),
];
pub const DESIGNATED_FIT_ERA: &str = "recent";
/// A session whose last observed minute precedes its calendar close by more
/// than this is an exchange-scheduled early close, not thin trading.
pub const EARLY_CLOSE_TOLERANCE_MINUTES: i64 = 60;
/// How a historical civil timestamp lands on the model's permanent
/// UTC-05:00 clock. `civil` preserves the civil label (the default, because
/// the preset declares permanent-CDT CIVIL hours); `instant` preserves the
/// true instant, moving a CST civil time forward 60 minutes.
pub const MODEL_CLOCK_ALIGNMENT_DEFAULT: Alignment = Alignment::Civil;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Alignment {
    Civil,
    Instant,
}

impl Alignment {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Civil => "civil",
            Self::Instant => "instant",
        }
    }
}

const MINUTES_PER_WEEK: i64 = 7 * 24 * 60;
const HOURS: usize = 24;
const DAYS: usize = 7;

// --- the calendar, read from the shipped preset rather than restated -------

/// The venue's own weekly grid, in local minutes from Sunday 00:00.
pub struct Calendar {
    pub utc_offset_minutes: i64,
    pub windows: Vec<(i64, i64)>,
}

impl Calendar {
    /// The MNQ preset's calendar, through the venue's own preset loader -
    /// the Python re-parsed `presets/mnq.toml` with `tomllib` to reach the
    /// same table.
    pub fn from_preset(name: &str) -> LabResult<Self> {
        let profile = mogwai_server::config::profile_from_preset(name)
            .map_err(|e| LabError::refusal(format!("loading preset {name}: {e}")))?;
        let calendar = profile
            .calendar
            .ok_or_else(|| LabError::refusal(format!("preset {name} carries no calendar")))?;
        // `expect` below is on integer conversions from a validated
        // calendar; the fallible parts already returned above.
        let mut windows = Vec::new();
        for w in &calendar.open_windows {
            let (start, end) = (i64::from(w.start_minute), i64::from(w.end_minute));
            if start >= end {
                // The Python's `range(start, end)` would silently produce an
                // empty window here; refusing is the honest port of a case
                // the shipped calendars do not contain.
                return Err(LabError::refusal(
                    "a wrapping open window has no week-minute range under this estimator",
                ));
            }
            windows.push((start, end));
        }
        Ok(Self {
            utc_offset_minutes: i64::from(calendar.utc_offset_minutes),
            windows,
        })
    }

    #[must_use]
    pub fn open_mask(&self) -> Vec<bool> {
        let mut mask = vec![false; MINUTES_PER_WEEK as usize];
        for (start, end) in &self.windows {
            for minute in *start..*end {
                mask[minute as usize] = true;
            }
        }
        mask
    }

    /// Contiguous trading cycles: windows separated only by the 15-minute
    /// halt belong to one session; the 60-minute break starts a new one.
    #[must_use]
    pub fn sessions(&self) -> Vec<(i64, i64)> {
        let mut sorted = self.windows.clone();
        sorted.sort_unstable();
        let mut merged: Vec<(i64, i64)> = Vec::new();
        for (start, end) in sorted {
            match merged.last_mut() {
                Some(last) if start - last.1 <= 15 => last.1 = end,
                _ => merged.push((start, end)),
            }
        }
        merged
    }

    #[must_use]
    pub fn open_minutes(&self) -> i64 {
        self.windows.iter().map(|(s, e)| e - s).sum()
    }

    /// Open minutes per session, which is NOT the session's span: sessions
    /// merge across the daily halt, so the span overstates exposure.
    #[must_use]
    pub fn session_open_minutes(&self) -> Vec<i64> {
        let mask = self.open_mask();
        self.sessions()
            .into_iter()
            .map(|(start, end)| (start..end).filter(|m| mask[*m as usize]).count() as i64)
            .collect()
    }
}

/// Minutes from local Sunday 00:00, matching the calendar's own convention.
/// 1970-01-01 was a Thursday, so `(days + 4) % 7` is Python's
/// `(weekday() + 1) % 7`.
#[must_use]
pub fn local_week_minute(local_min: i64) -> i64 {
    let days = local_min.div_euclid(1440);
    let sunday_index = (days + 4).rem_euclid(7);
    sunday_index * 1440 + local_min.rem_euclid(1440)
}

// --- the archive -----------------------------------------------------------

/// One archive minute, already placed on the model clock. `local_min` is
/// minutes since the epoch on the model-local clock.
pub struct Row {
    pub local_min: i64,
    pub year: i32,
    pub close: f64,
    pub volume: f64,
    pub was_cst: bool,
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The nth `weekday` (Sunday = 0) of a month, as a day index.
fn nth_weekday(y: i64, m: i64, weekday: i64, n: i64) -> i64 {
    let first = days_from_civil(y, m, 1);
    let first_dow = (first + 4).rem_euclid(7);
    let delta = (weekday - first_dow).rem_euclid(7);
    first + delta + 7 * (n - 1)
}

/// Is a naive Chicago civil timestamp on CDT? The rule CPython's
/// `zoneinfo` applies with `fold=0`: daylight time runs from the second
/// Sunday in March at 03:00 civil - the gap hour 02:00-02:59 resolves to
/// the offset BEFORE the transition, i.e. standard time - through the first
/// Sunday in November at 02:00 civil, so the ambiguous 01:00-01:59 hour
/// resolves to daylight time.
#[must_use]
pub fn chicago_is_dst(year: i64, day: i64, minute_of_day: i64) -> bool {
    let start = nth_weekday(year, 3, 0, 2);
    let end = nth_weekday(year, 11, 0, 1);
    if day < start || day > end {
        return false;
    }
    if day == start {
        return minute_of_day >= 3 * 60;
    }
    if day == end {
        return minute_of_day < 2 * 60;
    }
    true
}

/// Stream the single member out of the zip without extracting it.
///
/// Format: no header, `DD/MM/YYYY;HH:MM;O;H;L;C;V`, semicolon-delimited,
/// LF endings, timestamps CME local civil time observing DST.
pub fn read_archive(path: &std::path::Path, alignment: Alignment) -> LabResult<Vec<Row>> {
    use std::io::BufRead;

    let file = std::fs::File::open(path)?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| LabError::refusal(format!("opening {}: {e}", path.display())))?;
    let member = archive
        .by_index(0)
        .map_err(|e| LabError::refusal(format!("reading the archive member: {e}")))?;
    let reader = std::io::BufReader::with_capacity(1 << 20, member);
    let mut rows = Vec::new();
    for raw in reader.lines() {
        let line = raw?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split(';').collect();
        if parts.len() != 7 {
            return Err(LabError::refusal(format!(
                "unexpected field count in the archive: {line:?}"
            )));
        }
        let date = parts[0];
        let time = parts[1];
        let day_n: i64 = date[0..2]
            .parse()
            .map_err(|_| LabError::refusal(format!("bad day in {date:?}")))?;
        let month: i64 = date[3..5]
            .parse()
            .map_err(|_| LabError::refusal(format!("bad month in {date:?}")))?;
        let year: i64 = date[6..10]
            .parse()
            .map_err(|_| LabError::refusal(format!("bad year in {date:?}")))?;
        let hour: i64 = time[0..2]
            .parse()
            .map_err(|_| LabError::refusal(format!("bad hour in {time:?}")))?;
        let minute: i64 = time[3..5]
            .parse()
            .map_err(|_| LabError::refusal(format!("bad minute in {time:?}")))?;
        let civil_day = days_from_civil(year, month, day_n);
        let minute_of_day = hour * 60 + minute;
        let was_cst = !chicago_is_dst(year, civil_day, minute_of_day);
        let mut local_min = civil_day * 1440 + minute_of_day;
        if alignment == Alignment::Instant && was_cst {
            local_min += 60;
        }
        let close: f64 = parts[5]
            .parse()
            .map_err(|_| LabError::refusal(format!("bad close in {line:?}")))?;
        let volume: f64 = parts[6]
            .parse()
            .map_err(|_| LabError::refusal(format!("bad volume in {line:?}")))?;
        // The era filter reads the MODEL-LOCAL year, which under `instant`
        // can differ from the civil year at a new-year boundary.
        let model_year = year_of_day(local_min.div_euclid(1440));
        rows.push(Row {
            local_min,
            year: i32::try_from(model_year)
                .map_err(|_| LabError::refusal(format!("year {model_year} is out of range")))?,
            close,
            volume,
            was_cst,
        });
    }
    Ok(rows)
}

fn year_of_day(day: i64) -> i64 {
    // Inverse of `days_from_civil`, civil_from_days' year component.
    let z = day + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    if mp >= 10 { y + 1 } else { y }
}

// --- session eligibility ---------------------------------------------------

#[derive(Default, Clone)]
pub struct SessionState {
    pub minutes_present: i64,
    pub volume: f64,
    pub last_offset: i64,
    pub span: i64,
}

/// `(close day index, session slot)`. The Python keys on the ISO date the
/// close falls on; a day index is the same key, cheaper.
pub type SessionKey = (i64, usize);

/// Bucket rows into trading cycles, returning the state per session and the
/// count of rows falling outside every declared open window.
#[must_use]
pub fn group_sessions(
    rows: &[Row],
    calendar: &Calendar,
) -> (BTreeMap<SessionKey, SessionState>, u64) {
    let sessions_by_slot = calendar.sessions();
    let mask = calendar.open_mask();
    let mut states: BTreeMap<SessionKey, SessionState> = BTreeMap::new();
    let mut outside = 0u64;
    for row in rows {
        let minute = local_week_minute(row.local_min);
        if !mask[minute as usize] {
            outside += 1;
            continue;
        }
        let Some(slot) = sessions_by_slot
            .iter()
            .position(|(s, e)| *s <= minute && minute < *e)
        else {
            continue;
        };
        let (start, end) = sessions_by_slot[slot];
        // A cycle can straddle midnight and the week boundary, so it is
        // keyed on the day its CLOSE falls on, not the row's own day.
        let close_day = (row.local_min + (end - minute)).div_euclid(1440);
        let state = states
            .entry((close_day, slot))
            .or_insert_with(|| SessionState {
                last_offset: -1,
                ..SessionState::default()
            });
        state.minutes_present += 1;
        state.volume += row.volume;
        state.last_offset = state.last_offset.max(minute - start);
        state.span = end - start;
    }
    (states, outside)
}

/// Ordinary full sessions only. A session with no rows never appears here at
/// all - a full holiday is invisible rather than excluded.
#[must_use]
pub fn eligible_sessions(
    states: &BTreeMap<SessionKey, SessionState>,
) -> (std::collections::BTreeSet<SessionKey>, u64) {
    let mut eligible = std::collections::BTreeSet::new();
    let mut excluded_early = 0u64;
    for (key, state) in states {
        let shortfall = state.span - 1 - state.last_offset;
        if shortfall > EARLY_CLOSE_TOLERANCE_MINUTES {
            excluded_early += 1;
            continue;
        }
        eligible.insert(*key);
    }
    (eligible, excluded_early)
}

// --- the estimator ---------------------------------------------------------

/// Volume and exposure on the `(UTC hour, UTC day-of-week)` grid. Exposure
/// is calendar-open minutes, never row presence.
pub struct Grid {
    pub volume: Vec<Vec<f64>>,
    pub exposure: Vec<Vec<f64>>,
}

impl Default for Grid {
    fn default() -> Self {
        Self {
            volume: vec![vec![0.0; DAYS]; HOURS],
            exposure: vec![vec![0.0; DAYS]; HOURS],
        }
    }
}

fn utc_cell(local_min: i64, calendar: &Calendar) -> (usize, usize) {
    let utc = local_min - calendar.utc_offset_minutes;
    let hour = (utc.rem_euclid(1440) / 60) as usize;
    let day = ((utc.div_euclid(1440) + 4).rem_euclid(7)) as usize;
    (hour, day)
}

/// Numerator from observed rows, denominator from the calendar. Every
/// eligible session contributes its FULL open-minute grid as exposure.
#[must_use]
pub fn build_grid(
    rows: &[Row],
    calendar: &Calendar,
    eligible: &std::collections::BTreeSet<SessionKey>,
    era: Option<(i32, i32)>,
) -> Grid {
    let mut grid = Grid::default();
    let sessions_by_slot = calendar.sessions();
    let mask = calendar.open_mask();
    let mut seen: std::collections::BTreeSet<SessionKey> = std::collections::BTreeSet::new();
    for row in rows {
        if let Some((lo, hi)) = era
            && !(lo <= row.year && row.year <= hi)
        {
            continue;
        }
        let minute = local_week_minute(row.local_min);
        if !mask[minute as usize] {
            continue;
        }
        let Some(slot) = sessions_by_slot
            .iter()
            .position(|(s, e)| *s <= minute && minute < *e)
        else {
            continue;
        };
        let (start, end) = sessions_by_slot[slot];
        let close_day = (row.local_min + (end - minute)).div_euclid(1440);
        let key = (close_day, slot);
        if !eligible.contains(&key) {
            continue;
        }
        let (hour, day) = utc_cell(row.local_min, calendar);
        grid.volume[hour][day] += row.volume;
        if seen.insert(key) {
            let anchor = row.local_min - (minute - start);
            for offset in 0..(end - start) {
                let stamp = anchor + offset;
                if !mask[local_week_minute(stamp) as usize] {
                    continue;
                }
                let (eh, ed) = utc_cell(stamp, calendar);
                grid.exposure[eh][ed] += 1.0;
            }
        }
    }
    grid
}

pub struct Fit {
    pub hour: Vec<f64>,
    pub day: Vec<f64>,
    pub alpha: f64,
    pub sweeps: usize,
}

/// Maximum likelihood for a multiplicative Poisson rate with an exposure
/// offset, by alternating closed-form updates.
///
/// Saturday is pinned to 1.0 and never updated: with the shipped calendar it
/// has zero exposure in every cell, so its update is 0/0 and its value is a
/// declared convention rather than an estimate.
#[must_use]
pub fn fit_grid(grid: &Grid, sweeps: usize, tolerance: f64) -> Fit {
    let mut hour = vec![1.0f64; HOURS];
    let mut day = vec![1.0f64; DAYS];
    let exposed_days: Vec<usize> = (0..DAYS)
        .filter(|d| (0..HOURS).any(|h| grid.exposure[h][*d] > 0.0))
        .collect();
    let exposed_hours: Vec<usize> = (0..HOURS)
        .filter(|h| (0..DAYS).any(|d| grid.exposure[*h][d] > 0.0))
        .collect();
    let total_exposure = py_sum(grid.exposure.iter().map(|r| py_sum(r.iter().copied())));
    let total_volume = py_sum(grid.volume.iter().map(|r| py_sum(r.iter().copied())));
    if total_exposure <= 0.0 {
        return Fit {
            hour,
            day,
            alpha: 0.0,
            sweeps: 0,
        };
    }
    // `alpha` is constant under the chosen identification and must appear in
    // BOTH denominators: omit it and the two arrays drift reciprocally while
    // their product stays right, and the convergence test never fires.
    let alpha = total_volume / total_exposure;

    let mut completed = 0usize;
    for sweep in 0..sweeps {
        let previous: Vec<f64> = hour.iter().chain(day.iter()).copied().collect();
        for h in &exposed_hours {
            let numerator = py_sum(exposed_days.iter().map(|d| grid.volume[*h][*d]));
            let denominator =
                alpha * py_sum(exposed_days.iter().map(|d| grid.exposure[*h][*d] * day[*d]));
            if denominator > 0.0 {
                hour[*h] = numerator / denominator;
            }
        }
        for d in &exposed_days {
            let numerator = py_sum(exposed_hours.iter().map(|h| grid.volume[*h][*d]));
            let denominator = alpha
                * py_sum(
                    exposed_hours
                        .iter()
                        .map(|h| grid.exposure[*h][*d] * hour[*h]),
                );
            if denominator > 0.0 {
                day[*d] = numerator / denominator;
            }
        }
        let composite = py_sum(
            (0..HOURS)
                .flat_map(|h| (0..DAYS).map(move |d| (h, d)))
                .map(|(h, d)| grid.exposure[h][d] * hour[h] * day[d]),
        );
        let scale = composite / total_exposure;
        if scale > 0.0 {
            let root = scale.sqrt();
            for h in &exposed_hours {
                hour[*h] /= root;
            }
            for d in &exposed_days {
                day[*d] /= root;
            }
        }
        completed = sweep + 1;
        let moved = hour
            .iter()
            .chain(day.iter())
            .zip(previous.iter())
            .map(|(a, b)| (a - b).abs())
            .fold(f64::NEG_INFINITY, f64::max);
        if moved < tolerance {
            break;
        }
    }
    Fit {
        hour,
        day,
        alpha,
        sweeps: completed,
    }
}

/// `vol_hour`, the per-mean RMS return ratio, conditional on being open.
pub struct VolFit {
    pub ratio: Vec<f64>,
    pub returns: Vec<i64>,
    pub trimmed_sessions: u64,
    pub excluded_non_adjacent: u64,
}

/// RMS of close-to-close minute returns per UTC hour, normalized to an
/// observation-weighted mean of one.
///
/// A return is formed ONLY between two adjacent present open minutes inside
/// one eligible session - that single rule excludes closure crossings, the
/// halt, the break and every missing minute at once. The roll trim drops the
/// single largest squared return per session.
#[must_use]
pub fn build_vol_hour(
    rows: &[Row],
    calendar: &Calendar,
    eligible: &std::collections::BTreeSet<SessionKey>,
    era: Option<(i32, i32)>,
) -> VolFit {
    let mask = calendar.open_mask();
    let sessions_by_slot = calendar.sessions();
    let mut sum_sq = vec![0.0f64; HOURS];
    let mut counts = vec![0i64; HOURS];
    let mut trimmed = 0u64;
    let mut non_adjacent = 0u64;

    let mut current_key: Option<SessionKey> = None;
    let mut session_returns: Vec<(usize, f64)> = Vec::new();
    let mut previous: Option<(i64, f64)> = None;

    let mut order: Vec<usize> = (0..rows.len()).collect();
    // `sorted(rows, key=...)` is a STABLE sort on the model-local stamp.
    order.sort_by_key(|i| rows[*i].local_min);

    let flush = |session_returns: &mut Vec<(usize, f64)>,
                 sum_sq: &mut Vec<f64>,
                 counts: &mut Vec<i64>,
                 trimmed: &mut u64| {
        if session_returns.is_empty() {
            return;
        }
        // `max(range(n), key=lambda i: abs(...))` keeps the FIRST maximum.
        let mut largest = 0usize;
        for i in 1..session_returns.len() {
            if session_returns[i].1.abs() > session_returns[largest].1.abs() {
                largest = i;
            }
        }
        *trimmed += 1;
        for (index, (hour, value)) in session_returns.iter().enumerate() {
            if index == largest {
                continue;
            }
            sum_sq[*hour] += value * value;
            counts[*hour] += 1;
        }
        session_returns.clear();
    };

    for i in order {
        let row = &rows[i];
        if let Some((lo, hi)) = era
            && !(lo <= row.year && row.year <= hi)
        {
            previous = None;
            continue;
        }
        let minute = local_week_minute(row.local_min);
        if !mask[minute as usize] {
            previous = None;
            continue;
        }
        let Some(slot) = sessions_by_slot
            .iter()
            .position(|(s, e)| *s <= minute && minute < *e)
        else {
            previous = None;
            continue;
        };
        let (start, end) = sessions_by_slot[slot];
        let _ = start;
        let close_day = (row.local_min + (end - minute)).div_euclid(1440);
        let key = (close_day, slot);
        if !eligible.contains(&key) {
            previous = None;
            continue;
        }
        if Some(key) != current_key {
            flush(&mut session_returns, &mut sum_sq, &mut counts, &mut trimmed);
            current_key = Some(key);
            previous = None;
        }
        if let Some((prev_min, prev_close)) = previous {
            if row.local_min - prev_min == 1 && prev_close > 0.0 {
                let (hour, _) = utc_cell(row.local_min, calendar);
                session_returns.push((hour, (row.close - prev_close) / prev_close));
            } else {
                non_adjacent += 1;
            }
        }
        previous = Some((row.local_min, row.close));
    }
    flush(&mut session_returns, &mut sum_sq, &mut counts, &mut trimmed);

    let total: i64 = counts.iter().sum();
    if total == 0 {
        return VolFit {
            ratio: vec![1.0; HOURS],
            returns: counts,
            trimmed_sessions: trimmed,
            excluded_non_adjacent: non_adjacent,
        };
    }
    let rms: Vec<f64> = (0..HOURS)
        .map(|h| {
            if counts[h] > 0 {
                (sum_sq[h] / counts[h] as f64).sqrt()
            } else {
                0.0
            }
        })
        .collect();
    let mean = py_sum((0..HOURS).map(|h| rms[h] * counts[h] as f64)) / total as f64;
    let ratio: Vec<f64> = (0..HOURS)
        .map(|h| {
            if counts[h] == 0 {
                // An unexposed hour carries no evidence; one is the neutral
                // convention, the same treatment Saturday's day factor gets.
                1.0
            } else if mean > 0.0 {
                rms[h] / mean
            } else {
                1.0
            }
        })
        .collect();
    VolFit {
        ratio,
        returns: counts,
        trimmed_sessions: trimmed,
        excluded_non_adjacent: non_adjacent,
    }
}

pub struct Separability {
    pub material_share: f64,
    pub material_cells: u64,
    pub residuals: Vec<Vec<Option<f64>>>,
    pub passes: bool,
}

/// Materiality, not statistical rejection: with seventeen years of minutes
/// any deviance test rejects separability regardless of operational impact.
#[must_use]
pub fn assess(grid: &Grid, fit: &Fit) -> Separability {
    let mut material_exposure = 0.0f64;
    let mut total_exposure = 0.0f64;
    let mut material_cells = 0u64;
    let mut residuals = vec![vec![None; DAYS]; HOURS];
    for (h, residual_row) in residuals.iter_mut().enumerate() {
        for (d, residual) in residual_row.iter_mut().enumerate() {
            let exposure = grid.exposure[h][d];
            if exposure <= 0.0 {
                continue;
            }
            total_exposure += exposure;
            let observed = grid.volume[h][d] / exposure;
            let fitted = fit.alpha * fit.hour[h] * fit.day[d];
            if fitted <= 0.0 {
                continue;
            }
            let ratio = observed / fitted;
            *residual = Some(ratio);
            if ratio > INTERACTION_RATIO_LIMIT || ratio < 1.0 / INTERACTION_RATIO_LIMIT {
                material_exposure += exposure;
                material_cells += 1;
            }
        }
    }
    let share = if total_exposure > 0.0 {
        material_exposure / total_exposure
    } else {
        0.0
    };
    Separability {
        material_share: share,
        material_cells,
        residuals,
        passes: share <= MAX_MATERIAL_EXPOSURE_SHARE,
    }
}

pub struct EraStability {
    pub divergent_share: f64,
    pub divergent_cells: u64,
    pub passes: bool,
}

/// Does the full-corpus curve represent the designated era? Deliberately the
/// SAME rule and the same two constants as the separability test, so no new
/// threshold enters after results exist.
#[must_use]
pub fn era_stability(full: &Fit, designated: &Fit, designated_grid: &Grid) -> EraStability {
    let mut divergent_exposure = 0.0f64;
    let mut total_exposure = 0.0f64;
    let mut divergent_cells = 0u64;
    for h in 0..HOURS {
        for d in 0..DAYS {
            let exposure = designated_grid.exposure[h][d];
            if exposure <= 0.0 {
                continue;
            }
            total_exposure += exposure;
            let reference = designated.hour[h] * designated.day[d];
            let candidate = full.hour[h] * full.day[d];
            if reference <= 0.0 {
                continue;
            }
            let ratio = candidate / reference;
            if ratio > INTERACTION_RATIO_LIMIT || ratio < 1.0 / INTERACTION_RATIO_LIMIT {
                divergent_exposure += exposure;
                divergent_cells += 1;
            }
        }
    }
    let share = if total_exposure > 0.0 {
        divergent_exposure / total_exposure
    } else {
        0.0
    };
    EraStability {
        divergent_share: share,
        divergent_cells,
        passes: share <= MAX_MATERIAL_EXPOSURE_SHARE,
    }
}

/// The quantity most directly connected to the visible defect: a flat 1.78x
/// crypto curve standing in for a real index future's intraday swing.
#[must_use]
pub fn peak_to_trough(fit: &Fit, grid: &Grid) -> (f64, f64) {
    let exposed: Vec<usize> = (0..HOURS)
        .filter(|h| (0..DAYS).any(|d| grid.exposure[*h][d] > 0.0))
        .collect();
    let fitted: Vec<f64> = exposed.iter().map(|h| fit.hour[*h]).collect();
    let observed: Vec<f64> = exposed
        .iter()
        .map(|h| {
            py_sum((0..DAYS).map(|d| grid.volume[*h][d]))
                / py_sum((0..DAYS).map(|d| grid.exposure[*h][d]))
        })
        .collect();
    let ratio = |v: &[f64]| {
        let lo = v.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        if lo > 0.0 { hi / lo } else { f64::INFINITY }
    };
    (ratio(&fitted), ratio(&observed))
}

// --- modes -----------------------------------------------------------------

/// `preflight`: what the archive actually contains.
pub fn preflight_report(
    archive: &std::path::Path,
    alignment: Alignment,
    preset: &str,
) -> LabResult<Value> {
    let calendar = Calendar::from_preset(preset)?;
    let rows = read_archive(archive, alignment)?;
    let (states, outside) = group_sessions(&rows, &calendar);
    let (eligible, excluded_early) = eligible_sessions(&states);
    let zero_volume_rows = rows.iter().filter(|r| r.volume == 0.0).count();
    let cst_rows = rows.iter().filter(|r| r.was_cst).count();
    // Open minutes, not span: the span of a merged session includes the
    // daily 15-minute halt, which is closed and carries no exposure.
    let slot_open = calendar.session_open_minutes();
    let present: i64 = states
        .iter()
        .filter(|(k, _)| eligible.contains(k))
        .map(|(_, s)| s.minutes_present)
        .sum();
    let exposed: i64 = states
        .keys()
        .filter(|k| eligible.contains(k))
        .map(|k| slot_open[k.1])
        .sum();
    Ok(json!({
        "alignment": alignment.as_str(),
        "rows": rows.len(),
        "present_zero_volume_rows": zero_volume_rows,
        "rows_outside_declared_calendar": outside,
        "cst_rows_remapped": cst_rows,
        "cdt_rows": rows.len() - cst_rows,
        "sessions_observed": states.len(),
        "sessions_eligible": eligible.len(),
        "early_close_excluded": excluded_early,
        "calendar_open_minutes_per_week": calendar.open_minutes(),
        "eligible_open_minutes_expected": exposed,
        "eligible_minutes_present": present,
        "missing_minutes_inside_eligible_sessions": exposed - present,
    }))
}

/// `fit`: the estimator and its report, as a machine-readable record rather
/// than the Python's printed text. Every number the printed report showed is
/// here; the ASCII residual matrix is a rendering of `residuals`.
pub fn fit_report(
    archive: &std::path::Path,
    alignment: Alignment,
    preset: &str,
) -> LabResult<Value> {
    let calendar = Calendar::from_preset(preset)?;
    let rows = read_archive(archive, alignment)?;
    let (states, _) = group_sessions(&rows, &calendar);
    let (eligible, _) = eligible_sessions(&states);

    let mut scopes: Vec<(&str, Option<(i32, i32)>)> = vec![("full", None)];
    for (name, lo, hi) in ERAS {
        scopes.push((name, Some((lo, hi))));
    }

    let mut outcomes = serde_json::Map::new();
    let mut fits: BTreeMap<&str, (Fit, Grid)> = BTreeMap::new();
    let mut verdicts: BTreeMap<&str, bool> = BTreeMap::new();
    for (name, era) in scopes {
        let grid = build_grid(&rows, &calendar, &eligible, era);
        let fit = fit_grid(&grid, 200, 1e-12);
        let verdict = assess(&grid, &fit);
        let (fitted_ptt, observed_ptt) = peak_to_trough(&fit, &grid);
        let vol = build_vol_hour(&rows, &calendar, &eligible, era);
        outcomes.insert(
            name.to_string(),
            json!({
                "alpha": fit.alpha,
                "sweeps": fit.sweeps,
                "hour": fit.hour.clone(),
                "day": fit.day.clone(),
                "hour_exposure": (0..HOURS)
                    .map(|h| (0..DAYS).map(|d| grid.exposure[h][d]).sum::<f64>())
                    .collect::<Vec<f64>>(),
                "day_exposure": (0..DAYS)
                    .map(|d| (0..HOURS).map(|h| grid.exposure[h][d]).sum::<f64>())
                    .collect::<Vec<f64>>(),
                "separable": verdict.passes,
                "material_exposure_share": verdict.material_share,
                "material_cells": verdict.material_cells,
                "residuals": verdict.residuals.clone(),
                "peak_to_trough": {"fitted": fitted_ptt, "observed": observed_ptt},
                "vol_hour": vol.ratio.clone(),
                "vol_returns": vol.returns.clone(),
                "roll_trimmed_sessions": vol.trimmed_sessions,
                "non_adjacent_pairs_skipped": vol.excluded_non_adjacent,
            }),
        );
        verdicts.insert(name, verdict.passes);
        fits.insert(name, (fit, grid));
    }

    let (full_fit, _) = &fits["full"];
    let (era_fit, era_grid) = &fits[DESIGNATED_FIT_ERA];
    let stability = era_stability(full_fit, era_fit, era_grid);
    let era_passes = verdicts[DESIGNATED_FIT_ERA];
    let full_passes = verdicts["full"];
    let outcome = if !era_passes {
        "Outcome 3: material hour-by-day interaction in the designated era."
    } else if full_passes && stability.passes {
        "Outcome 1: separable and era-stable. Fit the full archive."
    } else if full_passes {
        "Outcome 2: the full corpus misrepresents the designated era."
    } else {
        "Outcome 2: the full corpus is not separable."
    };

    Ok(json!({
        "preregistered": {
            "INTERACTION_RATIO_LIMIT": INTERACTION_RATIO_LIMIT,
            "MAX_MATERIAL_EXPOSURE_SHARE": MAX_MATERIAL_EXPOSURE_SHARE,
            "DESIGNATED_FIT_ERA": DESIGNATED_FIT_ERA,
            "MODEL_CLOCK_ALIGNMENT": alignment.as_str(),
            "EARLY_CLOSE_TOLERANCE_MINUTES": EARLY_CLOSE_TOLERANCE_MINUTES,
            "ERAS": ERAS.iter().map(|(n, a, b)| json!([n, a, b])).collect::<Vec<Value>>(),
        },
        "scopes": Value::Object(outcomes),
        "era_stability": {
            "divergent_share": stability.divergent_share,
            "divergent_cells": stability.divergent_cells,
            "verdict": if stability.passes { "STABLE" } else { "ERA-DEPENDENT" },
        },
        "outcome": outcome,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_week_minute_counts_from_local_sunday() {
        // 1970-01-04 was a Sunday.
        let sunday = days_from_civil(1970, 1, 4) * 1440;
        assert_eq!(local_week_minute(sunday), 0);
        assert_eq!(local_week_minute(sunday + 17 * 60), 17 * 60);
        assert_eq!(local_week_minute(sunday + 6 * 1440 + 5), 6 * 1440 + 5);
    }

    #[test]
    fn chicago_dst_follows_the_fold_zero_boundaries() {
        // 2026: second Sunday in March is the 8th, first Sunday in November
        // the 1st.
        let mar8 = days_from_civil(2026, 3, 8);
        let nov1 = days_from_civil(2026, 11, 1);
        assert!(!chicago_is_dst(2026, mar8, 60));
        // The 02:00-02:59 gap resolves to the offset BEFORE the transition.
        assert!(!chicago_is_dst(2026, mar8, 2 * 60 + 30));
        assert!(chicago_is_dst(2026, mar8, 3 * 60));
        // The 01:00-01:59 ambiguous hour resolves to daylight time.
        assert!(chicago_is_dst(2026, nov1, 90));
        assert!(!chicago_is_dst(2026, nov1, 2 * 60));
        assert!(!chicago_is_dst(2026, days_from_civil(2026, 1, 15), 12 * 60));
    }

    #[test]
    fn the_mnq_calendar_merges_across_the_halt_but_not_the_break() {
        let cal = Calendar::from_preset("MNQ").expect("the shipped preset");
        // Five weekday cycles, each 17:00 -> 16:00 with the halt inside.
        assert_eq!(cal.sessions().len(), 5);
        for minutes in cal.session_open_minutes() {
            assert_eq!(minutes, 23 * 60 - 15);
        }
    }

    #[test]
    fn a_session_short_by_more_than_the_tolerance_is_an_early_close() {
        let mut states: BTreeMap<SessionKey, SessionState> = BTreeMap::new();
        states.insert(
            (1, 0),
            SessionState {
                minutes_present: 10,
                volume: 1.0,
                last_offset: 1000,
                span: 1380,
            },
        );
        states.insert(
            (2, 0),
            SessionState {
                minutes_present: 10,
                volume: 1.0,
                last_offset: 1330,
                span: 1380,
            },
        );
        let (eligible, excluded) = eligible_sessions(&states);
        assert_eq!(excluded, 1);
        assert!(eligible.contains(&(2, 0)));
        assert!(!eligible.contains(&(1, 0)));
    }

    /// The identification the sweep enforces: an exposure-weighted composite
    /// mean of one, with alpha carrying the corpus-wide rate.
    #[test]
    fn fit_grid_normalizes_the_composite_to_an_exposure_weighted_mean_of_one() {
        let mut grid = Grid::default();
        for h in 0..HOURS {
            for d in 1..6 {
                grid.exposure[h][d] = 60.0;
                grid.volume[h][d] = 60.0 * (1.0 + h as f64 / 24.0) * (1.0 + d as f64 / 10.0);
            }
        }
        let fit = fit_grid(&grid, 200, 1e-12);
        let composite = py_sum(
            (0..HOURS)
                .flat_map(|h| (0..DAYS).map(move |d| (h, d)))
                .map(|(h, d)| grid.exposure[h][d] * fit.hour[h] * fit.day[d]),
        );
        let total = py_sum(grid.exposure.iter().map(|r| py_sum(r.iter().copied())));
        assert!((composite / total - 1.0).abs() < 1e-9);
        // Saturday and Sunday are unexposed and keep the declared 1.0.
        assert_eq!(fit.day[0], 1.0);
        assert_eq!(fit.day[6], 1.0);
    }
}
