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

#[derive(Debug, Clone, PartialEq, Eq)]
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
        let Some(target) = self.settlement_minute_of_day else {
            return Vec::new();
        };
        if to_ns <= from_ns {
            return Vec::new();
        }
        let mut result = Vec::new();
        let first_minute = from_ns / NS_PER_MINUTE + 1;
        let last_minute = to_ns / NS_PER_MINUTE;
        for utc_minute in first_minute..=last_minute {
            let local = (i128::from(utc_minute)
                + i128::from(self.utc_offset_minutes)
                + UNIX_EPOCH_LOCAL_WEEK_MINUTE)
                .rem_euclid(MINUTES_PER_WEEK);
            if local.rem_euclid(MINUTES_PER_DAY) == i128::from(target) {
                let instant = utc_minute.saturating_mul(NS_PER_MINUTE);
                if instant > from_ns && instant <= to_ns && self.is_open(instant) {
                    result.push(instant);
                }
            }
        }
        result
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
}
