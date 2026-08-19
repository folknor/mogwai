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

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionCalendar {
    pub utc_offset_minutes: i16,
    pub open_windows: Vec<WeeklyWindow>,
    pub settlement_minute_of_day: Option<u16>,
}

impl SessionCalendar {
    pub fn validate(&self) -> Result<(), CalendarError> {
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

    /// The scan behind `settlement_instants`, returning the crossings AND how
    /// many day-step candidates it considered to find them.
    ///
    /// The count is what separates "steps a day at a time from the first
    /// crossing" from "steps a minute at a time and filters", and only a test
    /// wants it - but it is RETURNED rather than recorded in a counter. The
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
        }
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
        };
        assert_eq!(
            value.validate(),
            Err(CalendarError("settlement_minute_of_day"))
        );
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
        // Ten days, ten candidates: the scan steps a DAY from the first
        // crossing rather than walking minutes and filtering. Without this the
        // instant list above is satisfied by either implementation.
        assert_eq!(candidates, 10);
    }
}
