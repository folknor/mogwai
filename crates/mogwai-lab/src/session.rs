// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Session/segment math, the one implementation
//! (the retired rewrite plan, phase 1): a port of `analysis/mnq_fit.py`'s
//! `local_fields`/`assign_session`/`minute_fields`/`segment_origin_ns`/
//! `segment_end_ns`/`segment_labels`, unified with the integer-only
//! `session_segment_at` that `crates/mogwai-cli/src/gen.rs` used to carry.
//! `session_segment_at` here is that same pure-integer
//! algorithm (no `chrono`, no string dates in the hot path); `assign_session`
//! wraps it to produce the (trade-date label, segment name) pairs the
//! preflight stream pass needs.
//!
//! The one implementation is now literal. `gen.rs` was rewired onto
//! `crate::summary` by the Python-to-Rust move, and `summary`'s own copy of
//! this branch structure - which a `mogwai-cli` test pinned against this one
//! over a timestamp sweep, calling itself a bridge until that rewire - is a
//! thin field mapping over `session_segment_at` below. There is no second
//! body to keep in step, so there is no sweep either.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{LabError, LabResult};

use crate::subcontract::{
    HALT_END_LOCAL_MIN, HALT_START_LOCAL_MIN, PARENT_COUNT_BIN_EDGES, PARENT_COUNT_BIN_NAMES,
    SEGMENT_LABEL_EDGES_S, SESSION_CLOSE_LOCAL_MIN, SESSION_OPEN_LOCAL_MIN, SINCE_OPEN_BIN_NAMES,
    UNTIL_CLOSE_BIN_NAMES,
};

const NS_PER_DAY: i128 = 86_400_000_000_000;
const NS_PER_MIN: i128 = 60_000_000_000;
const NS_PER_SEC: i128 = 1_000_000_000;
pub const STAGE_M_TZ_AUTHORITY_SHA256: &str =
    "afe9024d96f492ad6ae821455cb60ff4127b2312acb0bd164003f95c127739bc";

#[derive(Clone, Debug, Serialize)]
pub struct SessionBounds {
    pub session: String,
    pub open_ns: u64,
    pub halt_start_ns: u64,
    pub halt_end_ns: u64,
    pub close_ns: u64,
    pub scheduled_open_seconds: i64,
}

#[derive(Clone, Debug)]
pub enum ScheduleFrame {
    JulyFixed,
    Chicago2026c(TzAuthority),
}

#[derive(Clone, Debug)]
pub struct TzAuthority {
    transitions: Vec<Transition>,
    standard_offset_s: i64,
    daylight_offset_s: i64,
    pub artifact_path: String,
    pub artifact_sha256: String,
}

#[derive(Clone, Debug)]
struct Transition {
    utc_s: i64,
    before_s: i64,
    after_s: i64,
}

#[derive(Deserialize)]
struct AuthorityFile {
    zone: String,
    tzdb_release: String,
    standard_offset_s: i64,
    daylight_offset_s: i64,
    transitions: Vec<AuthorityTransition>,
}

#[derive(Deserialize)]
struct AuthorityTransition {
    utc_instant: String,
    offset_before_s: i64,
    offset_after_s: i64,
}

impl ScheduleFrame {
    pub fn stage_m(path: &Path) -> LabResult<Self> {
        let bytes = std::fs::read(path)?;
        let hash = crate::delivery::sha256_bytes(&bytes);
        if hash != STAGE_M_TZ_AUTHORITY_SHA256 {
            return Err(LabError::refusal(format!(
                "timezone authority hash mismatch: expected {STAGE_M_TZ_AUTHORITY_SHA256}, got {hash}"
            )));
        }
        let raw: AuthorityFile = serde_json::from_slice(&bytes)?;
        if raw.zone != "America/Chicago" || raw.tzdb_release != "2026c" {
            return Err(LabError::refusal("timezone authority identity mismatch"));
        }
        if raw.standard_offset_s != -21_600
            || raw.daylight_offset_s != -18_000
            || raw.transitions.len() != 14
        {
            return Err(LabError::refusal("timezone authority shape mismatch"));
        }
        let transitions = raw
            .transitions
            .into_iter()
            .map(|t| {
                Ok(Transition {
                    utc_s: parse_utc_second(&t.utc_instant)?,
                    before_s: t.offset_before_s,
                    after_s: t.offset_after_s,
                })
            })
            .collect::<LabResult<Vec<_>>>()?;
        if transitions.iter().any(|t| {
            ![-21_600, -18_000].contains(&t.before_s) || ![-21_600, -18_000].contains(&t.after_s)
        }) {
            return Err(LabError::refusal(
                "timezone authority carries an unpermitted offset",
            ));
        }
        Ok(Self::Chicago2026c(TzAuthority {
            transitions,
            standard_offset_s: raw.standard_offset_s,
            daylight_offset_s: raw.daylight_offset_s,
            artifact_path: path.display().to_string(),
            artifact_sha256: hash,
        }))
    }

    pub(crate) fn offset_at_utc_s(&self, utc_s: i64) -> LabResult<i64> {
        match self {
            Self::JulyFixed => Ok(i64::from(crate::subcontract::UTC_OFFSET_MINUTES) * 60),
            Self::Chicago2026c(tz) => {
                let mut offset = tz
                    .transitions
                    .first()
                    .ok_or_else(|| LabError::refusal("timezone authority has no transitions"))?
                    .before_s;
                for transition in &tz.transitions {
                    if utc_s < transition.utc_s {
                        break;
                    }
                    offset = transition.after_s;
                }
                Ok(offset)
            }
        }
    }

    fn civil_to_utc_s(&self, day: i64, minute: i32) -> LabResult<i64> {
        let local_s = day * 86_400 + i64::from(minute) * 60;
        let candidates: &[i64] = match self {
            Self::JulyFixed => &[-18_000],
            Self::Chicago2026c(tz) => &[tz.standard_offset_s, tz.daylight_offset_s],
        };
        let mut valid = candidates
            .iter()
            .copied()
            .map(|offset| local_s - offset)
            .filter(|utc| {
                self.offset_at_utc_s(*utc)
                    .is_ok_and(|offset| *utc + offset == local_s)
            })
            .collect::<Vec<_>>();
        valid.sort_unstable();
        valid.dedup();
        match valid.as_slice() {
            [utc] => Ok(*utc),
            [] => Err(LabError::refusal(format!(
                "nonexistent civil instant at day {day}, minute {minute}"
            ))),
            _ => Err(LabError::refusal(format!(
                "ambiguous civil instant at day {day}, minute {minute}"
            ))),
        }
    }

    pub fn bounds(&self, session: &str) -> LabResult<SessionBounds> {
        let day = days_from_iso(session);
        let open = self.civil_to_utc_s(day - 1, SESSION_OPEN_LOCAL_MIN)?;
        let halt_start = self.civil_to_utc_s(day, HALT_START_LOCAL_MIN)?;
        let halt_end = self.civil_to_utc_s(day, HALT_END_LOCAL_MIN)?;
        let close = self.civil_to_utc_s(day, SESSION_CLOSE_LOCAL_MIN)?;
        if !(open < halt_start && halt_start < halt_end && halt_end < close) {
            return Err(LabError::refusal(format!(
                "session {session} UTC bounds are not strictly increasing"
            )));
        }
        if halt_end - halt_start != 900 {
            return Err(LabError::refusal(format!(
                "session {session} halt is not 15 minutes"
            )));
        }
        let exposure = (halt_start - open) + (close - halt_end);
        if exposure != 81_900 {
            return Err(LabError::refusal(format!(
                "session {session} scheduled exposure is {exposure}, not 81900 seconds"
            )));
        }
        Ok(SessionBounds {
            session: session.to_string(),
            open_ns: to_ns(open)?,
            halt_start_ns: to_ns(halt_start)?,
            halt_end_ns: to_ns(halt_end)?,
            close_ns: to_ns(close)?,
            scheduled_open_seconds: exposure,
        })
    }

    pub fn session_segment_at(&self, ts_ns: u64) -> LabResult<Option<SessionSegment>> {
        let utc_s = i64::try_from(ts_ns / 1_000_000_000)
            .map_err(|_| LabError::refusal("timestamp exceeds i64 seconds"))?;
        let offset_s = self.offset_at_utc_s(utc_s)?;
        let local_s = utc_s + offset_s;
        let day = local_s.div_euclid(86_400);
        let minute = local_s.rem_euclid(86_400) / 60;
        let trade_day = if minute >= i64::from(SESSION_OPEN_LOCAL_MIN) {
            day + 1
        } else {
            day
        };
        let label = format_trade_date(trade_day);
        let bounds = self.bounds(&label)?;
        let segment = if ts_ns >= bounds.open_ns && ts_ns < bounds.halt_start_ns {
            Some(("overnight", bounds.open_ns, bounds.halt_start_ns))
        } else if ts_ns >= bounds.halt_end_ns && ts_ns <= bounds.close_ns {
            Some(("post_halt", bounds.halt_end_ns, bounds.close_ns))
        } else {
            None
        };
        Ok(segment.map(|(name, origin, end)| SessionSegment {
            session_start_ns: bounds.open_ns,
            session_end_ns: bounds.close_ns,
            segment_origin_ns: origin,
            segment_end_ns: end,
            trade_day,
            segment: name,
        }))
    }
}

fn to_ns(seconds: i64) -> LabResult<u64> {
    u64::try_from(i128::from(seconds) * NS_PER_SEC)
        .map_err(|_| LabError::refusal("session boundary is outside u64 nanoseconds"))
}

fn parse_utc_second(text: &str) -> LabResult<i64> {
    let body = text
        .strip_suffix('Z')
        .ok_or_else(|| LabError::refusal("timezone transition is not UTC"))?;
    let (date, time) = body
        .split_once('T')
        .ok_or_else(|| LabError::refusal("invalid timezone transition"))?;
    let mut d = date.split('-');
    let year = d
        .next()
        .and_then(|x| x.parse().ok())
        .ok_or_else(|| LabError::refusal("invalid transition year"))?;
    let month = d
        .next()
        .and_then(|x| x.parse().ok())
        .ok_or_else(|| LabError::refusal("invalid transition month"))?;
    let day = d
        .next()
        .and_then(|x| x.parse().ok())
        .ok_or_else(|| LabError::refusal("invalid transition day"))?;
    let mut t = time.split(':');
    let hour: i64 = t
        .next()
        .and_then(|x| x.parse().ok())
        .ok_or_else(|| LabError::refusal("invalid transition hour"))?;
    let minute: i64 = t
        .next()
        .and_then(|x| x.parse().ok())
        .ok_or_else(|| LabError::refusal("invalid transition minute"))?;
    let second: i64 = t
        .next()
        .and_then(|x| x.parse().ok())
        .ok_or_else(|| LabError::refusal("invalid transition second"))?;
    Ok(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second)
}

/// The calendar boundaries of the session/segment a timestamp falls in, as
/// UTC epoch-ns instants, plus the trade-date civil day (days since the
/// 1970-01-01 epoch) and segment name that produced them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionSegment {
    pub session_start_ns: u64,
    pub session_end_ns: u64,
    pub segment_origin_ns: u64,
    pub segment_end_ns: u64,
    pub trade_day: i64,
    pub segment: &'static str,
}

/// The pure-integer session/segment rule, and the workspace's only body of
/// it: `crate::summary::session_segment_at` is a field mapping over this, and
/// `gen.rs` reaches it through that. It reports the trade-date day number and
/// segment name too, so callers here need no second pass. `None` inside the
/// 15:15-15:30 halt or the 16:00-17:00 daily break.
pub fn session_segment_at(ts_ns: u64, offset_minutes: i32) -> Option<SessionSegment> {
    let offset_ns = i128::from(offset_minutes) * NS_PER_MIN;
    let local = i128::from(ts_ns) + offset_ns;
    let day = local.div_euclid(NS_PER_DAY);
    let minute_of_day = local.rem_euclid(NS_PER_DAY) / NS_PER_MIN;
    let to_utc = |local_ns: i128| -> u64 { u64::try_from(local_ns - offset_ns).unwrap_or(0) };
    let open_of = |d: i128| d * NS_PER_DAY + i128::from(SESSION_OPEN_LOCAL_MIN) * NS_PER_MIN;
    let close_of = |d: i128| d * NS_PER_DAY + i128::from(SESSION_CLOSE_LOCAL_MIN) * NS_PER_MIN;
    let halt_start = i128::from(HALT_START_LOCAL_MIN);
    let halt_end = i128::from(HALT_END_LOCAL_MIN);
    let open_min = i128::from(SESSION_OPEN_LOCAL_MIN);
    if minute_of_day >= open_min {
        Some(SessionSegment {
            session_start_ns: to_utc(open_of(day)),
            session_end_ns: to_utc(close_of(day + 1)),
            segment_origin_ns: to_utc(open_of(day)),
            segment_end_ns: to_utc((day + 1) * NS_PER_DAY + halt_start * NS_PER_MIN),
            trade_day: (day + 1) as i64,
            segment: "overnight",
        })
    } else if minute_of_day < halt_start {
        Some(SessionSegment {
            session_start_ns: to_utc(open_of(day - 1)),
            session_end_ns: to_utc(close_of(day)),
            segment_origin_ns: to_utc(open_of(day - 1)),
            segment_end_ns: to_utc(day * NS_PER_DAY + halt_start * NS_PER_MIN),
            trade_day: day as i64,
            segment: "overnight",
        })
    } else if (halt_end..i128::from(SESSION_CLOSE_LOCAL_MIN)).contains(&minute_of_day) {
        Some(SessionSegment {
            session_start_ns: to_utc(open_of(day - 1)),
            session_end_ns: to_utc(close_of(day)),
            segment_origin_ns: to_utc(day * NS_PER_DAY + halt_end * NS_PER_MIN),
            segment_end_ns: to_utc(close_of(day)),
            trade_day: day as i64,
            segment: "post_halt",
        })
    } else {
        None
    }
}

/// Howard Hinnant's `civil_from_days`: proleptic-Gregorian (year, month, day)
/// from a day count since the 1970-01-01 epoch. Avoids a `chrono` dependency
/// for the one thing this crate needs a calendar for.
pub(crate) fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `"YYYY-MM-DD"` for a day count since the epoch - the session label format.
pub fn format_trade_date(day: i64) -> String {
    let (y, m, d) = civil_from_days(day);
    format!("{y:04}-{m:02}-{d:02}")
}

/// `session_date_int`: `"YYYY-MM-DD"` with the dashes stripped, as an
/// integer.
pub fn session_date_int(label: &str) -> i64 {
    label
        .replace('-', "")
        .parse()
        .expect("session labels are always YYYY-MM-DD")
}

/// `assign_session`: (trade-date label, segment name) for a UTC-ns instant
/// under the permanent `UTC_OFFSET_MINUTES` calendar, or `(None, None)`
/// outside every session window.
pub fn assign_session(ts_ns: u64) -> (Option<String>, Option<&'static str>) {
    match session_segment_at(ts_ns, crate::subcontract::UTC_OFFSET_MINUTES) {
        Some(seg) => (Some(format_trade_date(seg.trade_day)), Some(seg.segment)),
        None => (None, None),
    }
}

/// Howard Hinnant's `days_from_civil`: the inverse of `civil_from_days`.
#[must_use]
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let mp = u64::from(if m > 2 { m - 3 } else { m + 9 });
    let doy = (153 * mp + 2) / 5 + u64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

/// `"YYYY-MM-DD"` to a day count since the epoch.
#[must_use]
pub fn days_from_iso(label: &str) -> i64 {
    let mut parts = label.split('-');
    let mut next = || {
        parts
            .next()
            .unwrap_or_else(|| panic!("a YYYY-MM-DD session label, got {label:?}"))
    };
    let y: i64 = next().parse().expect("a four-digit year");
    let m: u32 = next().parse().expect("a two-digit month");
    let d: u32 = next().parse().expect("a two-digit day");
    days_from_civil(y, m, d)
}

fn local_instant_ns(day: i64, local_min: i64) -> u64 {
    let seconds =
        day * 86_400 + local_min * 60 - i64::from(crate::subcontract::UTC_OFFSET_MINUTES) * 60;
    u64::try_from(seconds).expect("session instants are after the epoch") * 1_000_000_000
}

/// `segment_origin_ns`: the calendar start of a session segment as a UTC
/// epoch-ns instant - the previous civil day's 17:00 local for `overnight`,
/// the trade date's 15:30 local for `post_halt`. Derived from the label
/// alone, so the expected-exposure judge never consults a candidate's data.
#[must_use]
pub fn segment_origin_ns(session: &str, segment: &str) -> u64 {
    let day = days_from_iso(session);
    if segment == "overnight" {
        local_instant_ns(day - 1, i64::from(SESSION_OPEN_LOCAL_MIN))
    } else {
        local_instant_ns(day, i64::from(HALT_END_LOCAL_MIN))
    }
}

/// `segment_end_ns`: the calendar end - the trade date's 15:15 local (halt
/// start) for `overnight`, its 16:00 local (close) for `post_halt`.
#[must_use]
pub fn segment_end_ns(session: &str, segment: &str) -> u64 {
    let day = days_from_iso(session);
    let local_min = if segment == "overnight" {
        HALT_START_LOCAL_MIN
    } else {
        SESSION_CLOSE_LOCAL_MIN
    };
    local_instant_ns(day, i64::from(local_min))
}

/// `expected_scheduled_windows`: the calendar-derived scheduled-window count
/// for one `(session, hour, window length)`, independent of any candidate's
/// own data (spec 3.3 scheduled-exposure completeness) - 59 per full hour at
/// 60 s, 44 at the halt hour.
///
/// Judging exposure by a max over the candidate's own sessions would be
/// self-referential: a systematically short generator would define its own
/// shortfall as complete. The count comes from the same
/// [`crate::measure12a::window_schedule`] iterator the block-2 accumulator
/// consumes, so the endpoint-hour rule cannot drift between them.
#[must_use]
pub fn expected_scheduled_windows(session: &str, hour: i64, w: i64) -> i64 {
    #[expect(clippy::cast_sign_loss, reason = "window lengths are 1, 5 and 60")]
    let w_ns = w as u64 * 1_000_000_000;
    ["overnight", "post_halt"]
        .into_iter()
        .map(|segment| {
            let origin = segment_origin_ns(session, segment);
            let end = segment_end_ns(session, segment);
            crate::measure12a::window_schedule(origin, end, w_ns)
                .into_iter()
                .filter(|(_, _, h)| *h == Some(hour.unsigned_abs()))
                .count() as i64
        })
        .sum()
}

/// `parent_count_bin`: the half-open parent-count-bin name for `n`.
pub fn parent_count_bin(n: i64) -> &'static str {
    if n == 0 {
        return "0";
    }
    for (edge, name) in PARENT_COUNT_BIN_EDGES
        .iter()
        .rev()
        .zip(PARENT_COUNT_BIN_NAMES.iter().rev())
    {
        if n >= *edge {
            return name;
        }
    }
    unreachable!("every n >= 1 has a bin")
}

/// `segment_labels`: (since_open_bin, until_close_bin) evaluated at minute
/// start, per spec 3.2.
pub fn segment_labels(
    minute_start_ns: u64,
    origin_ns: u64,
    end_ns: u64,
) -> (&'static str, &'static str) {
    let since_s = (minute_start_ns as i128 - origin_ns as i128) as f64 / 1e9;
    let until_s = (end_ns as i128 - minute_start_ns as i128) as f64 / 1e9;
    let lo = SEGMENT_LABEL_EDGES_S[0] as f64;
    let hi = SEGMENT_LABEL_EDGES_S[1] as f64;
    let since = if since_s < lo {
        SINCE_OPEN_BIN_NAMES[0]
    } else if since_s < hi {
        SINCE_OPEN_BIN_NAMES[1]
    } else {
        SINCE_OPEN_BIN_NAMES[2]
    };
    let until = if until_s > hi {
        UNTIL_CLOSE_BIN_NAMES[0]
    } else if until_s > lo {
        UNTIL_CLOSE_BIN_NAMES[1]
    } else {
        UNTIL_CLOSE_BIN_NAMES[2]
    };
    (since, until)
}

/// One resolved minute's `(session, segment, exchange-local hour)`.
pub type MinuteFields = (Option<String>, Option<&'static str>, i32);

/// `minute_fields`: memoized on the UTC minute, mirroring the Python memo -
/// a month is ~44k distinct minutes against 35M rows.
#[derive(Default)]
pub struct MinuteFieldsCache {
    memo: HashMap<i64, MinuteFields>,
    frame: Option<ScheduleFrame>,
}

impl MinuteFieldsCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_frame(frame: ScheduleFrame) -> Self {
        Self {
            memo: HashMap::new(),
            frame: Some(frame),
        }
    }

    pub fn minute_fields(&mut self, ts_ns: u64) -> MinuteFields {
        let key = (ts_ns / 60_000_000_000) as i64;
        if let Some(hit) = self.memo.get(&key) {
            return hit.clone();
        }
        let (session, segment, offset_minutes) = if let Some(frame) = &self.frame {
            let seg = frame
                .session_segment_at(ts_ns)
                .expect("a validated Stage M timezone frame");
            let pair = seg.map_or((None, None), |s| {
                (Some(format_trade_date(s.trade_day)), Some(s.segment))
            });
            let utc_s = i64::try_from(ts_ns / 1_000_000_000).expect("timestamp seconds fit i64");
            let offset = frame
                .offset_at_utc_s(utc_s)
                .expect("a validated Stage M timezone frame")
                / 60;
            (pair.0, pair.1, offset as i32)
        } else {
            let pair = assign_session(ts_ns);
            (pair.0, pair.1, crate::subcontract::UTC_OFFSET_MINUTES)
        };
        let offset_ns = i128::from(offset_minutes) * NS_PER_MIN;
        let local = i128::from(ts_ns) + offset_ns;
        let local_hour = (local.rem_euclid(NS_PER_DAY) / NS_PER_MIN / 60) as i32;
        let value = (session, segment, local_hour);
        self.memo.insert(key, value.clone());
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_trade_date_matches_known_dates() {
        assert_eq!(format_trade_date(0), "1970-01-01");
        // 2026-07-01 is 20636 days after the epoch.
        assert_eq!(format_trade_date(20635), "2026-07-01");
    }

    #[test]
    fn session_date_int_strips_dashes() {
        assert_eq!(session_date_int("2026-07-01"), 20_260_701);
    }

    #[test]
    fn amendment4_authority_derives_daylight_standard_and_transition_sessions() {
        let authority = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../analysis/tz-america-chicago-2026c.json");
        let frame = ScheduleFrame::stage_m(&authority).expect("frozen authority");
        let september = frame.bounds("2025-09-15").unwrap();
        let november = frame.bounds("2025-11-03").unwrap();
        let before = frame.bounds("2026-03-06").unwrap();
        let after = frame.bounds("2026-03-09").unwrap();
        assert_eq!(
            september.open_ns,
            ScheduleFrame::JulyFixed
                .bounds("2025-09-15")
                .unwrap()
                .open_ns
        );
        assert_eq!(
            november.open_ns
                - ScheduleFrame::JulyFixed
                    .bounds("2025-11-03")
                    .unwrap()
                    .open_ns,
            3_600_000_000_000
        );
        assert_eq!(before.scheduled_open_seconds, 81_900);
        assert_eq!(after.scheduled_open_seconds, 81_900);
        assert_eq!(after.open_ns - before.open_ns, 71 * 3_600_000_000_000);
    }

    #[test]
    fn amendment4_authority_refuses_a_hash_mismatch() {
        let path = Path::new("Cargo.toml");
        assert!(
            ScheduleFrame::stage_m(path)
                .unwrap_err()
                .to_string()
                .contains("hash mismatch")
        );
    }

    #[test]
    fn parent_count_bin_matches_python_edges() {
        assert_eq!(parent_count_bin(0), "0");
        assert_eq!(parent_count_bin(1), "1-64");
        assert_eq!(parent_count_bin(64), "1-64");
        assert_eq!(parent_count_bin(65), "65-256");
        assert_eq!(parent_count_bin(4096), "1025-4096");
        assert_eq!(parent_count_bin(4097), "4097+");
    }

    #[test]
    fn assign_session_matches_the_halt_and_break_gaps() {
        // 2026-07-01 15:20 local (in the halt) -> None. Local noon UTC
        // offset -300 min means local = utc - 5h, so 15:20 local is 20:20
        // UTC on the same civil day.
        let day0 = 20635i64; // 2026-07-01
        let halt_ts = ((day0 * 86_400 + 15 * 3600 + 20 * 60) as i128
            - i128::from(crate::subcontract::UTC_OFFSET_MINUTES) * 60) as u64
            * 1_000_000_000;
        assert_eq!(assign_session(halt_ts), (None, None));
    }
}
