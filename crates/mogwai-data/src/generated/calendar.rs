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

/// One UTC minute of the week, as the calendar answers it.
#[derive(Debug, Clone, Copy)]
struct MinuteCell {
    open: bool,
    /// Whole minutes from this one to the next open minute start, or
    /// `NEVER_OPENS`. Read only while closed.
    until_open: u16,
    /// The session position as `(weekday, minute of session)`, or `None`.
    position: Option<(u8, u32)>,
}

const NEVER_OPENS: u16 = u16::MAX;

/// A [`SessionCalendar`] frozen into one row per UTC minute of the
/// week, so the walk's calendar questions are a table lookup rather than a
/// scan over windows (`is_open`) or over up to a week of minutes
/// (`next_open_ns`).
///
/// Every answer the calendar gives depends on the instant only through
/// `local_week_minute`, which floors to the minute before shifting by a
/// whole-minute offset, so it is a function of the UTC minute modulo a week.
/// The table is therefore filled by asking the calendar's own methods at
/// each minute start - never a second implementation of the local-week
/// arithmetic - and a lookup returns what the calendar would have answered
/// at any instant, bit for bit
/// (`a_compiled_calendar_answers_every_instant_as_the_calendar_does` pins
/// it). Openness and session position are independent columns because the
/// calendar keeps them independent: a closed minute can carry a position, and
/// an open one past the envelope's length carries none.
///
/// `SessionCalendar` stays the public, mutable input value; this is a copy
/// compiled from it once, so no later edit to a calendar's public fields can
/// leave a cache stale.
#[derive(Debug)]
pub(crate) struct CompiledCalendar {
    calendar: SessionCalendar,
    minutes: Box<[MinuteCell]>,
}

impl CompiledCalendar {
    /// Compile a calendar the caller has validated. The table answers what
    /// the calendar's methods answer, so compiling adds no refusal of its own
    /// and validation stays where each caller already performs it. An
    /// unvalidated calendar is no safer here than in its own methods: an
    /// envelope opening past the end of the day underflows
    /// `session_position`, which compilation calls.
    pub(crate) fn new(calendar: &SessionCalendar) -> Self {
        let week = MINUTES_PER_WEEK as usize;
        let mut minutes: Vec<MinuteCell> = (0..week as u64)
            .map(|minute| {
                let clock_ns = minute * NS_PER_MINUTE;
                MinuteCell {
                    open: calendar.is_open(clock_ns),
                    until_open: NEVER_OPENS,
                    position: calendar
                        .session_position(clock_ns)
                        .map(|(day, minute)| (day as u8, minute as u32)),
                }
            })
            .collect();
        // Minutes to the next open minute start, strictly after each minute
        // and at most a week on (the scan's own `1..=MINUTES_PER_WEEK`): two
        // backward passes, so the second sees, from late in the week, the
        // earliest opening of the next. An index at or below the minute being
        // filled is one the walk reaches by wrapping.
        let mut next_open: Option<usize> = None;
        for _ in 0..2 {
            for index in (0..week).rev() {
                if let Some(open_at) = next_open {
                    let distance = if open_at > index {
                        open_at - index
                    } else {
                        open_at + week - index
                    };
                    minutes[index].until_open = distance as u16;
                }
                if minutes[index].open {
                    next_open = Some(index);
                }
            }
        }
        Self {
            calendar: calendar.clone(),
            minutes: minutes.into_boxed_slice(),
        }
    }

    fn cell(&self, clock_ns: u64) -> MinuteCell {
        self.minutes[((clock_ns / NS_PER_MINUTE) % MINUTES_PER_WEEK as u64) as usize]
    }

    /// The calendar this was compiled from.
    #[must_use]
    pub(crate) fn calendar(&self) -> &SessionCalendar {
        &self.calendar
    }

    /// [`SessionCalendar::is_open`], by lookup.
    #[must_use]
    pub(crate) fn is_open(&self, clock_ns: u64) -> bool {
        self.cell(clock_ns).open
    }

    /// [`SessionCalendar::session_position`], by lookup.
    #[must_use]
    pub(crate) fn session_position(&self, clock_ns: u64) -> Option<(usize, usize)> {
        self.cell(clock_ns)
            .position
            .map(|(day, minute)| (usize::from(day), minute as usize))
    }

    /// [`SessionCalendar::next_open_ns`], by lookup. An open instant is
    /// returned unchanged; a closed one floors to its minute before adding
    /// the distance, exactly as the scan built its candidates, so the
    /// subminute remainder is dropped the same way and saturation lands on
    /// the same `u64::MAX` sentinel.
    #[must_use]
    pub(crate) fn next_open_ns(&self, clock_ns: u64) -> u64 {
        let cell = self.cell(clock_ns);
        if cell.open {
            return clock_ns;
        }
        if cell.until_open == NEVER_OPENS {
            return u64::MAX;
        }
        (clock_ns / NS_PER_MINUTE)
            .saturating_add(u64::from(cell.until_open))
            .saturating_mul(NS_PER_MINUTE)
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

    /// The compiled table against the calendar's own scans, at every minute
    /// start of a week and at both subminute edges of each, under offsets at
    /// both validation extremes, a wrapping window, a single open minute, an
    /// envelope that disagrees with the windows about which minutes exist,
    /// and instants far enough out that `next_open_ns` saturates.
    #[test]
    fn a_compiled_calendar_answers_every_instant_as_the_calendar_does() {
        let window = |start_minute, end_minute| WeeklyWindow {
            start_minute,
            end_minute,
        };
        let mut wrapping = enveloped(900);
        wrapping.utc_offset_minutes = -720;
        wrapping.open_windows = vec![window(9_000, 1_200), window(3_000, 4_500)];
        let mut single = calendar();
        single.utc_offset_minutes = 840;
        single.open_windows = vec![window(5_000, 5_001)];
        single.settlement_minute_of_day = None;
        let mut disagreeing = enveloped(1_440);
        disagreeing.utc_offset_minutes = -300;
        disagreeing.open_windows = vec![window(1_000, 2_000), window(6_000, 8_000)];
        for value in [calendar(), enveloped(1_380), wrapping, single, disagreeing] {
            value.validate().unwrap();
            let compiled = CompiledCalendar::new(&value);
            let week_ns = MINUTES_PER_WEEK as u64 * NS_PER_MINUTE;
            let far = u64::MAX - 2 * week_ns;
            for base in [0, 53 * week_ns, far, u64::MAX - week_ns] {
                for minute in 0..MINUTES_PER_WEEK as u64 {
                    let start = base / NS_PER_MINUTE * NS_PER_MINUTE + minute * NS_PER_MINUTE;
                    for clock_ns in [start, start + 1, start + NS_PER_MINUTE - 1] {
                        assert_eq!(
                            compiled.is_open(clock_ns),
                            value.is_open(clock_ns),
                            "is_open at {clock_ns}"
                        );
                        assert_eq!(
                            compiled.session_position(clock_ns),
                            value.session_position(clock_ns),
                            "session_position at {clock_ns}"
                        );
                    }
                    // The reference scan is up to a week of minutes per
                    // closed instant, so it is asked on a stride plus every
                    // minute beside an open/closed edge, where an off-by-one
                    // in the distance would show.
                    let near_edge = [start.saturating_sub(NS_PER_MINUTE), start + NS_PER_MINUTE]
                        .iter()
                        .any(|&other| value.is_open(other) != value.is_open(start));
                    if minute % 37 == 0 || near_edge {
                        for clock_ns in [start, start + NS_PER_MINUTE - 1] {
                            assert_eq!(
                                compiled.next_open_ns(clock_ns),
                                value.next_open_ns(clock_ns),
                                "next_open_ns at {clock_ns}"
                            );
                        }
                    }
                }
            }
            for clock_ns in [u64::MAX, u64::MAX - 1, u64::MAX - NS_PER_MINUTE] {
                assert_eq!(compiled.is_open(clock_ns), value.is_open(clock_ns));
                assert_eq!(
                    compiled.next_open_ns(clock_ns),
                    value.next_open_ns(clock_ns)
                );
            }
        }
    }
}
