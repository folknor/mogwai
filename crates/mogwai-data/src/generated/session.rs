// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Wall-clock derivation and the precomputed per-tick session multipliers.
//! [`utc_hour_dow`] turns the nanosecond clock into civil hour/day-of-week
//! fields with no chrono dependency; [`SessionModulator`] turns a
//! [`super::SessionProfile`] - or, when the calendar carries one, a
//! [`super::calendar::SessionEnvelope`] - into the two multipliers the walk
//! applies every tick (arrival-rate and volatility).

use super::calendar::SessionCalendar;
use super::fingerprint::SessionProfile;

const MINUTES_PER_WEEK: u64 = 7 * 24 * 60;
const NS_PER_MINUTE: u64 = 60_000_000_000;

// Civil UTC fields the session profile is keyed on. Derived purely from the
// nanosecond wall clock with no chrono dependency: the session curves only need
// hour-of-day and day-of-week, both of which fall out of integer division on the
// unix-epoch second. Day-of-week uses the (days_since_epoch + 4) % 7 convention
// that puts Sun=0 (1970-01-01 was a Thursday), matching the fingerprint.
pub(super) fn utc_hour_dow(clock_ns: u64) -> (usize, usize) {
    let secs = clock_ns / 1_000_000_000;
    let days = secs / 86_400;
    let hour = ((secs % 86_400) / 3_600) as usize;
    let dow = ((days + 4) % 7) as usize;
    (hour, dow)
}

// Hour-of-day only, for the sites that key on the hour and discard the
// day-of-week. Wraps utc_hour_dow so the civil-time derivation lives in exactly
// one place.
pub(super) fn utc_hour(clock_ns: u64) -> usize {
    utc_hour_dow(clock_ns).0
}

// The envelope, precomputed into the two per-minute tables the walk reads.
// Carries its calendar because minute of session is the calendar's notion of
// local time; re-deriving it here would be a second implementation of the
// local-week arithmetic that could disagree with the one gating events.
#[derive(Clone)]
struct EnvelopeTables {
    calendar: SessionCalendar,
    // volume[m] * weekday: the arrival-rate shape.
    arrival: Vec<f64>,
    // range[m] / sqrt(volume[m]): the per-parent volatility shape. A minute's
    // range scales with the square root of its arrivals when parents move
    // independently, so dividing that out leaves what each parent contributes.
    vol: Vec<f64>,
    weekday: [f64; 7],
}

impl EnvelopeTables {
    fn raw_arrival(&self, clock_ns: u64) -> f64 {
        match self.calendar.session_position(clock_ns) {
            Some((day, minute)) => self.arrival[minute] * self.weekday[day],
            // A closure minute; the calendar never lets one through, and the
            // normalizer loop below skips them, so this value is never applied.
            None => 1.0,
        }
    }

    fn raw_vol(&self, clock_ns: u64) -> f64 {
        match self.calendar.session_position(clock_ns) {
            Some((_, minute)) => self.vol[minute],
            None => 1.0,
        }
    }
}

// Precomputed session multipliers. Built once from the fingerprint's
// SessionProfile so the per-tick hot path is two array indexes and a multiply,
// not a re-normalization. The arrival multiplier centers each share on 1.0 by
// dividing out the uniform share (24 hours, 7 days); the vol multiplier is the
// fingerprint's per-mean ratio used as-is. When the calendar carries an
// envelope, the hourly curves are not consulted at all: the envelope's
// per-minute tables stand in for them, normalized the same way.
#[derive(Clone)]
pub(super) struct SessionModulator {
    // intensity_hour[h] * 24.0: arrival-rate multiplier from the hour share,
    // centered on 1.0 (uniform hour share is 1/24).
    arr_hour: [f64; 24],
    // dow_weight[d] * 7.0: arrival-rate multiplier from the day share, centered
    // on 1.0 (uniform day share is 1/7). Sun=0 .. Sat=6.
    arr_dow: [f64; 7],
    // vol_hour[h]: per-mean per-trade RMS-return multiplier.
    vol_hour: [f64; 24],
    // Exposure-weighted mean of the raw arrival shape over the calendar's open
    // minutes, or exactly 1.0 when there is no calendar. Dividing by it is what
    // makes the profile describe behaviour conditional on being open: the
    // calendar owns whether an event may exist, and this owns how intense it is
    // given that it may.
    arrival_normalizer: f64,
    // The same, for the raw volatility shape.
    vol_normalizer: f64,
    envelope: Option<EnvelopeTables>,
}

impl SessionModulator {
    /// The calendar is a constructor input, not a later attachment, because the
    /// normalizers below cannot be computed without it. A modulator built from
    /// the profile alone and corrected afterwards would be wrong for the window
    /// between the two, which is precisely the state this signature makes
    /// unrepresentable.
    pub(super) fn new(profile: &SessionProfile, calendar: Option<&SessionCalendar>) -> Self {
        let mut arr_hour = [0.0; 24];
        for (h, mult) in arr_hour.iter_mut().enumerate() {
            *mult = profile.intensity_hour[h] * 24.0;
        }
        let mut arr_dow = [0.0; 7];
        for (d, mult) in arr_dow.iter_mut().enumerate() {
            *mult = profile.dow_weight[d] * 7.0;
        }
        let envelope = calendar.and_then(|calendar| {
            calendar.envelope.as_ref().map(|envelope| EnvelopeTables {
                calendar: calendar.clone(),
                arrival: envelope.volume.clone(),
                vol: envelope
                    .volume
                    .iter()
                    .zip(&envelope.range)
                    .map(|(volume, range)| range / volume.sqrt())
                    .collect(),
                weekday: envelope.weekday_weight,
            })
        });
        let mut this = Self {
            arr_hour,
            arr_dow,
            vol_hour: profile.vol_hour,
            arrival_normalizer: 1.0,
            vol_normalizer: 1.0,
            envelope,
        };
        let (arrival_normalizer, vol_normalizer) = match calendar {
            // Legacy exact branch. Without a calendar every minute is exposed,
            // and the schema's sum-to-one contract makes the week-mean of the
            // composite exactly 1.0 - so a literal 1.0 is not an approximation
            // of a computed value, it is the value itself, and dividing by it is exact
            // in IEEE 754. Recomputing it through a floating-point sum could
            // land a few ulps away and perturb every gap in every existing
            // operator profile, which no protocol migration should do silently.
            None => (1.0, 1.0),
            Some(calendar) => {
                let mut arrival = 0.0;
                let mut vol = 0.0;
                let mut open = 0.0;
                // One week at minute resolution, using the runtime's own
                // `is_open` and civil derivation. Re-deriving the local-to-UTC
                // mapping here would be a second implementation that could
                // disagree with the one that actually gates events. Any 10,080
                // consecutive minutes span a full week, so every hour/day cell
                // is visited exactly sixty times regardless of the anchor.
                for minute in 0..MINUTES_PER_WEEK {
                    let clock_ns = minute * NS_PER_MINUTE;
                    if !calendar.is_open(clock_ns) {
                        continue;
                    }
                    arrival += this.raw_arrival(clock_ns);
                    vol += this.raw_vol(clock_ns);
                    open += 1.0;
                }
                if open == 0.0 {
                    (1.0, 1.0)
                } else {
                    (arrival / open, vol / open)
                }
            }
        };
        this.arrival_normalizer = arrival_normalizer;
        this.vol_normalizer = vol_normalizer;
        this
    }

    // The unnormalized arrival shape at an instant: the envelope's minute of
    // session times its weekday weight when there is one, else the hour share
    // times the day share.
    fn raw_arrival(&self, clock_ns: u64) -> f64 {
        match &self.envelope {
            Some(envelope) => envelope.raw_arrival(clock_ns),
            None => {
                let (hour, dow) = utc_hour_dow(clock_ns);
                self.arr_hour[hour] * self.arr_dow[dow]
            }
        }
    }

    fn raw_vol(&self, clock_ns: u64) -> f64 {
        match &self.envelope {
            Some(envelope) => envelope.raw_vol(clock_ns),
            None => self.vol_hour[utc_hour(clock_ns)],
        }
    }

    // Arrival-rate multiplier at this wall-clock instant: the raw shape divided
    // by its exposure-weighted mean over open minutes. A duration is divided by
    // this so a high-activity instant produces shorter inter-arrivals. The
    // division is what makes the stored factors scale-invariant: a profile
    // whose factors are all doubled, or all ones, is exactly neutral rather
    // than a 168x arrival multiplier.
    pub(super) fn arrival_mult(&self, clock_ns: u64) -> f64 {
        self.raw_arrival(clock_ns) / self.arrival_normalizer
    }

    // Volatility multiplier at this wall-clock instant. Normalized the same way
    // and for the same reason: this value is applied unmodified to a formed return, so
    // without a normalizer nothing at all constrains its scale once the sum
    // guard is relaxed for calendar-bearing profiles, and a curve normalized
    // over 168 hours instead of the 113.75 a CME week actually trades would
    // scale every return by roughly 1.5 with no error anywhere.
    pub(super) fn vol_mult(&self, clock_ns: u64) -> f64 {
        self.raw_vol(clock_ns) / self.vol_normalizer
    }
}

#[cfg(test)]
mod tests {
    use super::super::calendar::{SessionEnvelope, WeeklyWindow};
    use super::*;

    fn profile() -> SessionProfile {
        SessionProfile {
            intensity_hour: [1.0 / 24.0; 24],
            vol_hour: [1.0; 24],
            dow_weight: [1.0 / 7.0; 7],
        }
    }

    fn always_open() -> SessionCalendar {
        SessionCalendar {
            utc_offset_minutes: 0,
            open_windows: vec![WeeklyWindow {
                start_minute: 0,
                end_minute: 10_079,
            }],
            settlement_minute_of_day: None,
            envelope: None,
        }
    }

    #[test]
    fn an_envelope_shapes_arrivals_by_minute_of_session_and_vol_by_its_root() {
        let mut calendar = always_open();
        let mut volume = vec![1.0; 1_440];
        volume[5] = 4.0;
        let mut range = vec![1.0; 1_440];
        range[7] = 3.0;
        let mut weekday = [1.0; 7];
        weekday[5] = 2.0;
        calendar.envelope = Some(SessionEnvelope {
            session_open_minute_of_day: 1_020,
            weekday_weight: weekday,
            volume,
            range,
        });
        calendar.validate().unwrap();
        let modulator = SessionModulator::new(&profile(), Some(&calendar));
        // 17:00 local on Thursday 1970-01-08 opens Friday's session (weekday
        // 5, weighted 2.0). Minute 5 carries four times the arrivals of minute
        // 6 and, with the same range, half the per-parent volatility. A week
        // in, so the same minute of Thursday's session is still on the clock.
        let open = (7 * 1_440 + 1_020) * NS_PER_MINUTE;
        let at = |minute: u64| open + minute * NS_PER_MINUTE;
        let ratio = modulator.arrival_mult(at(5)) / modulator.arrival_mult(at(6));
        assert!((ratio - 4.0).abs() < 1e-12, "arrival ratio {ratio}");
        let ratio = modulator.vol_mult(at(5)) / modulator.vol_mult(at(6));
        assert!((ratio - 0.5).abs() < 1e-12, "vol ratio {ratio}");
        // Minute 7 has the same arrivals as minute 6 and three times the
        // range, so three times the per-parent volatility.
        let ratio = modulator.vol_mult(at(7)) / modulator.vol_mult(at(6));
        assert!((ratio - 3.0).abs() < 1e-12, "range ratio {ratio}");
        // Friday's session is weighted twice Thursday's at the same minute.
        let ratio =
            modulator.arrival_mult(at(6)) / modulator.arrival_mult(at(6) - 1_440 * NS_PER_MINUTE);
        assert!((ratio - 2.0).abs() < 1e-12, "weekday ratio {ratio}");
        // The hourly curves are flat here, so without the envelope every
        // instant would read exactly 1.0; the envelope is what moves it.
        let flat = SessionModulator::new(&profile(), Some(&always_open()));
        assert_eq!(flat.arrival_mult(at(5)), flat.arrival_mult(at(6)));
    }

    #[test]
    fn an_enveloped_modulator_is_neutral_on_average_over_open_minutes() {
        let mut calendar = always_open();
        let volume: Vec<f64> = (0..1_440).map(|m| 0.5 + (m % 7) as f64).collect();
        calendar.envelope = Some(SessionEnvelope {
            session_open_minute_of_day: 0,
            weekday_weight: [1.0; 7],
            range: vec![1.0; 1_440],
            volume,
        });
        let modulator = SessionModulator::new(&profile(), Some(&calendar));
        // Over the calendar's open minutes only: the window above leaves the
        // week's last minute closed, and a closed minute is never applied.
        let open: Vec<f64> = (0..MINUTES_PER_WEEK)
            .map(|minute| minute * NS_PER_MINUTE)
            .filter(|clock_ns| calendar.is_open(*clock_ns))
            .map(|clock_ns| modulator.arrival_mult(clock_ns))
            .collect();
        let mean = open.iter().sum::<f64>() / open.len() as f64;
        assert!((mean - 1.0).abs() < 1e-9, "mean {mean}");
    }
}
