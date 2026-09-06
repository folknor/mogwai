// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

use serde::Deserialize;

const MINUTES_PER_DAY: i128 = 1_440;
const MINUTES_PER_WEEK: i128 = 10_080;
const NS_PER_MINUTE: u64 = 60_000_000_000;
const UNIX_EPOCH_LOCAL_WEEK_MINUTE: i128 = 4 * MINUTES_PER_DAY;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WeeklyWindow {
    pub start_minute: u32,
    pub end_minute: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CalendarError(pub &'static str);

/// The activity envelope: how intense the market is at each minute of the
/// session the calendar defines, given that it is open at all.
///
/// The calendar owns whether an event may exist; this owns how much is
/// happening when one may. It is keyed by minute of session in the
/// calendar's local frame, from `session_open_minute_of_day` on the local
/// clock, so a fitted shape says "the cash open ignites 930 minutes after
/// the reopen" rather than naming a UTC hour that is wrong for half the
/// year. A calendar without one falls back to the hourly `SessionProfile`
/// curves, which is what every calendar-less instrument uses.
///
/// `volume[m]` is the arrival-rate shape (1.0 is the session's mean minute)
/// and `range[m]` the minute-range shape (1.0 is the session's median
/// minute); the per-parent volatility multiplier the walk applies is derived
/// as `range / sqrt(volume)`, because a minute's range grows with the square
/// root of its arrivals when parents move independently, so the range shape
/// already contains most of the volume shape. `weekday_weight` scales the
/// level by the session's weekday (Sunday = 0; a session belongs to the
/// civil day it closes on, so Sunday evening is Monday's) and is 1.0 by
/// convention on days the calendar never opens.
///
/// Both arrays run from the reopen to the end of the longest session and are
/// the same length; a minute at or past that length is a closure minute the
/// calendar never lets through, and reads as neutral if it is ever asked.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionEnvelope {
    pub session_open_minute_of_day: u16,
    pub weekday_weight: [f64; 7],
    pub volume: Vec<f64>,
    pub range: Vec<f64>,
}

impl SessionEnvelope {
    pub fn validate(&self) -> Result<(), CalendarError> {
        if self.session_open_minute_of_day >= MINUTES_PER_DAY as u16 {
            return Err(CalendarError("envelope.session_open_minute_of_day"));
        }
        let minutes = self.volume.len();
        if minutes == 0 || minutes > MINUTES_PER_DAY as usize || self.range.len() != minutes {
            return Err(CalendarError("envelope.length"));
        }
        let positive = |value: &f64| value.is_finite() && *value > 0.0;
        if !self.volume.iter().all(positive) {
            return Err(CalendarError("envelope.volume"));
        }
        if !self.range.iter().all(positive) {
            return Err(CalendarError("envelope.range"));
        }
        if !self.weekday_weight.iter().all(positive) {
            return Err(CalendarError("envelope.weekday_weight"));
        }
        Ok(())
    }

    #[must_use]
    pub fn minutes(&self) -> usize {
        self.volume.len()
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionCalendar {
    pub utc_offset_minutes: i16,
    pub open_windows: Vec<WeeklyWindow>,
    pub settlement_minute_of_day: Option<u16>,
    #[serde(default)]
    pub envelope: Option<SessionEnvelope>,
}

impl SessionCalendar {
    /// The session weekday (Sunday = 0) and minute of session at a UTC
    /// instant, under this calendar's envelope. `None` when the calendar has
    /// no envelope, or when the instant falls at or past the envelope's
    /// length, which is a closure minute.
    #[must_use]
    pub fn session_position(&self, clock_ns: u64) -> Option<(usize, usize)> {
        let envelope = self.envelope.as_ref()?;
        let week_minute = self.local_week_minute(clock_ns);
        let day = (week_minute / MINUTES_PER_DAY) as usize;
        let minute_of_day = (week_minute % MINUTES_PER_DAY) as usize;
        let open = usize::from(envelope.session_open_minute_of_day);
        let (session_day, session_minute) = if minute_of_day >= open {
            ((day + 1) % 7, minute_of_day - open)
        } else {
            (day, minute_of_day + MINUTES_PER_DAY as usize - open)
        };
        (session_minute < envelope.minutes()).then_some((session_day, session_minute))
    }

    pub fn validate(&self) -> Result<(), CalendarError> {
        if let Some(envelope) = &self.envelope {
            envelope.validate()?;
        }
        if !(-720..=840).contains(&self.utc_offset_minutes) {
            return Err(CalendarError("utc_offset_minutes"));
        }
        if self.open_windows.is_empty() {
            return Err(CalendarError("open_windows"));
        }
        let mut open = [false; MINUTES_PER_WEEK as usize];
        let mut wraps = 0;
        for window in &self.open_windows {
            if window.start_minute >= MINUTES_PER_WEEK as u32
                || window.end_minute >= MINUTES_PER_WEEK as u32
                || window.start_minute == window.end_minute
            {
                return Err(CalendarError("open_windows"));
            }
            if window.start_minute > window.end_minute {
                wraps += 1;
            }
            for minute in 0..MINUTES_PER_WEEK as u32 {
                let inside = if window.start_minute < window.end_minute {
                    (window.start_minute..window.end_minute).contains(&minute)
                } else {
                    minute >= window.start_minute || minute < window.end_minute
                };
                if inside && std::mem::replace(&mut open[minute as usize], true) {
                    return Err(CalendarError("open_windows"));
                }
            }
        }
        if wraps > 1 {
            return Err(CalendarError("open_windows"));
        }
        if let Some(settlement) = self.settlement_minute_of_day
            && (settlement >= MINUTES_PER_DAY as u16
                || !(0..7).any(|day| open[day * MINUTES_PER_DAY as usize + settlement as usize]))
        {
            return Err(CalendarError("settlement_minute_of_day"));
        }
        Ok(())
    }

    #[must_use]
    pub fn is_open(&self, clock_ns: u64) -> bool {
        let minute = self.local_week_minute(clock_ns) as u32;
        self.open_windows.iter().any(|window| {
            if window.start_minute < window.end_minute {
                (window.start_minute..window.end_minute).contains(&minute)
            } else {
                minute >= window.start_minute || minute < window.end_minute
            }
        })
    }

    /// Whether this weekly footprint contains a given UTC minute-of-day on at
    /// least one day. Daily account rules cannot silently become run-lifetime
    /// rules merely because their reset instant is absent from the water.
    #[must_use]
    pub fn contains_utc_minute_of_day(&self, minute_utc: u32) -> bool {
        if minute_utc >= MINUTES_PER_DAY as u32 {
            return false;
        }
        const NS_PER_DAY: u64 = 86_400_000_000_000;
        (0..7).any(|day| {
            self.is_open(day * NS_PER_DAY + u64::from(minute_utc).saturating_mul(NS_PER_MINUTE))
        })
    }

    #[must_use]
    pub fn next_open_ns(&self, clock_ns: u64) -> u64 {
        if self.is_open(clock_ns) {
            return clock_ns;
        }
        let minute_floor = clock_ns / NS_PER_MINUTE;
        for delta in 1..=MINUTES_PER_WEEK as u64 {
            let candidate = minute_floor
                .saturating_add(delta)
                .saturating_mul(NS_PER_MINUTE);
            if self.is_open(candidate) {
                return candidate;
            }
        }
        u64::MAX
    }

    #[must_use]
    pub fn settlement_instants(&self, from_ns: u64, to_ns: u64) -> Vec<u64> {
        self.settlement_scan(from_ns, to_ns).0
    }

    /// The scan behind `settlement_instants`, returning the crossings and how
    /// many day-step candidates it considered to find them.
    ///
    /// The count is what separates "steps a day at a time from the first
    /// crossing" from "steps a minute at a time and filters", and only a test
    /// wants it - but it is returned rather than recorded in a counter. The
    /// previous shape was a `#[cfg(test)]` `thread_local!` incremented from
    /// this loop and read one line after a `.set(0)` in the single test that
    /// looked at it. That was safe only for as long as libtest gives each test
    /// its own thread and no second test reads it without resetting, and both
    /// of those failures are silent. A return value cannot be shared.
    fn settlement_scan(&self, from_ns: u64, to_ns: u64) -> (Vec<u64>, usize) {
        let Some(target) = self.settlement_minute_of_day else {
            return (Vec::new(), 0);
        };
        if to_ns <= from_ns {
            return (Vec::new(), 0);
        }
        let first_minute = from_ns / NS_PER_MINUTE + 1;
        let last_minute = to_ns / NS_PER_MINUTE;
        let local_offset = i128::from(self.utc_offset_minutes) + UNIX_EPOCH_LOCAL_WEEK_MINUTE;
        let target_utc_day_minute = (i128::from(target) - local_offset).rem_euclid(MINUTES_PER_DAY);
        let first_remainder = i128::from(first_minute).rem_euclid(MINUTES_PER_DAY);
        let delta = (target_utc_day_minute - first_remainder).rem_euclid(MINUTES_PER_DAY);
        let Ok(mut utc_minute) = u64::try_from(i128::from(first_minute) + delta) else {
            return (Vec::new(), 0);
        };
        let mut result = Vec::new();
        let mut candidates = 0_usize;
        while utc_minute <= last_minute {
            candidates += 1;
            let instant = utc_minute.saturating_mul(NS_PER_MINUTE);
            if instant > from_ns && instant <= to_ns && self.is_open(instant) {
                result.push(instant);
            }
            let Some(next) = utc_minute.checked_add(MINUTES_PER_DAY as u64) else {
                break;
            };
            utc_minute = next;
        }
        (result, candidates)
    }

    fn local_week_minute(&self, clock_ns: u64) -> i128 {
        (i128::from(clock_ns / NS_PER_MINUTE)
            + i128::from(self.utc_offset_minutes)
            + UNIX_EPOCH_LOCAL_WEEK_MINUTE)
            .rem_euclid(MINUTES_PER_WEEK)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calendar() -> SessionCalendar {
        SessionCalendar {
            utc_offset_minutes: 0,
            open_windows: vec![WeeklyWindow {
                start_minute: 0,
                end_minute: 10_079,
            }],
            settlement_minute_of_day: Some(960),
            envelope: None,
        }
    }

    fn enveloped(minutes: usize) -> SessionCalendar {
        let mut value = calendar();
        value.envelope = Some(SessionEnvelope {
            session_open_minute_of_day: 1_020,
            weekday_weight: [1.0; 7],
            volume: vec![1.0; minutes],
            range: vec![1.0; minutes],
        });
        value
    }

    #[test]
    fn an_envelope_maps_the_local_open_to_minute_zero_of_the_next_days_session() {
        let value = enveloped(1_380);
        value.validate().unwrap();
        // Thursday 1970-01-01 is local day 4 at offset 0. 17:00 local is
        // minute 0 of Friday's session; 16:59 is minute 1_439 of Thursday's,
        // which lies past the envelope and is a closure minute.
        let open = 1_020 * NS_PER_MINUTE;
        assert_eq!(value.session_position(open), Some((5, 0)));
        assert_eq!(value.session_position(open - NS_PER_MINUTE), None);
        assert_eq!(
            value.session_position(open + 1_379 * NS_PER_MINUTE),
            Some((5, 1_379))
        );
        assert_eq!(value.session_position(open + 1_380 * NS_PER_MINUTE), None);
        // A minute before the open belongs to the session that opened the
        // previous local day: 00:00 Thursday is minute 420 of Thursday's.
        assert_eq!(value.session_position(0), Some((4, 420)));
    }

    #[test]
    fn a_calendar_without_an_envelope_has_no_session_position() {
        assert_eq!(calendar().session_position(0), None);
    }

    #[test]
    fn an_envelope_with_mismatched_or_nonpositive_arrays_is_refused() {
        let mut value = enveloped(10);
        value.envelope.as_mut().unwrap().range.pop();
        assert_eq!(value.validate(), Err(CalendarError("envelope.length")));
        let mut value = enveloped(10);
        value.envelope.as_mut().unwrap().volume[3] = 0.0;
        assert_eq!(value.validate(), Err(CalendarError("envelope.volume")));
        let mut value = enveloped(10);
        value.envelope.as_mut().unwrap().weekday_weight[6] = f64::NAN;
        assert_eq!(
            value.validate(),
            Err(CalendarError("envelope.weekday_weight"))
        );
        let mut value = enveloped(10);
        value.envelope.as_mut().unwrap().session_open_minute_of_day = 1_440;
        assert_eq!(
            value.validate(),
            Err(CalendarError("envelope.session_open_minute_of_day"))
        );
    }

    #[test]
    fn a_calendar_with_no_open_window_is_refused() {
        let mut value = calendar();
        value.open_windows.clear();
        assert_eq!(value.validate(), Err(CalendarError("open_windows")));
    }

    #[test]
    fn a_settlement_minute_outside_every_window_is_refused() {
        let value = SessionCalendar {
            utc_offset_minutes: 0,
            open_windows: vec![WeeklyWindow {
                start_minute: 0,
                end_minute: 60,
            }],
            settlement_minute_of_day: Some(960),
            envelope: None,
        };
        assert_eq!(
            value.validate(),
            Err(CalendarError("settlement_minute_of_day"))
        );
    }

    #[test]
    fn a_daily_reset_absent_from_the_footprint_is_detectable() {
        let asia = SessionCalendar {
            utc_offset_minutes: 0,
            open_windows: (0..7)
                .map(|day| WeeklyWindow {
                    start_minute: day * 1_440 + 1_200,
                    end_minute: day * 1_440 + 1_380,
                })
                .collect(),
            settlement_minute_of_day: None,
            envelope: None,
        };
        assert!(asia.contains_utc_minute_of_day(1_320));
        assert!(!asia.contains_utc_minute_of_day(1_020));
    }

    #[test]
    fn settlement_instants_returns_every_crossing_in_a_multi_day_span() {
        let value = calendar();
        value.validate().unwrap();
        let instants = value.settlement_instants(0, 3 * 86_400_000_000_000);
        assert_eq!(instants.len(), 3);
        assert!(
            instants
                .windows(2)
                .all(|pair| pair[1] - pair[0] == 86_400_000_000_000)
        );
    }

    #[test]
    fn settlement_day_step_respects_local_offset_and_open_filter() {
        let value = SessionCalendar {
            utc_offset_minutes: 90,
            open_windows: vec![WeeklyWindow {
                start_minute: 0,
                end_minute: 10_079,
            }],
            settlement_minute_of_day: Some(120),
            envelope: None,
        };
        value.validate().unwrap();
        let day_ns = 86_400_000_000_000;
        let expected_first = 30 * NS_PER_MINUTE;
        let (instants, candidates) = value.settlement_scan(0, 10 * day_ns);
        assert_eq!(
            instants,
            (0..10)
                .map(|day| expected_first + day * day_ns)
                .collect::<Vec<_>>()
        );
        // Ten days, ten candidates: the scan steps a day from the first
        // crossing rather than walking minutes and filtering. Without this the
        // instant list above is satisfied by either implementation.
        assert_eq!(candidates, 10);
    }
}
