// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Wall-clock derivation and the precomputed per-tick session multipliers.
//! [`utc_hour_dow`] turns the nanosecond clock into civil hour/day-of-week
//! fields with no chrono dependency; [`SessionModulator`] turns a
//! [`super::SessionProfile`] into the two multipliers the walk applies every
//! tick (arrival-rate and volatility).

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

// Precomputed session multipliers. Built once from the fingerprint's
// SessionProfile so the per-tick hot path is two array indexes and a multiply,
// not a re-normalization. The arrival multiplier centers each share on 1.0 by
// dividing out the uniform share (24 hours, 7 days); the vol multiplier is the
// fingerprint's per-mean ratio used as-is.
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
    // Exposure-weighted mean of `arr_hour * arr_dow` over the calendar's open
    // minutes, or exactly 1.0 when there is no calendar. Dividing by it is what
    // makes the profile describe behaviour conditional on being open: the
    // calendar owns whether an event may exist, and this owns how intense it is
    // given that it may.
    arrival_normalizer: f64,
    // The same, for `vol_hour` alone - it carries no day dimension.
    vol_normalizer: f64,
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
                    let (hour, dow) = utc_hour_dow(clock_ns);
                    arrival += arr_hour[hour] * arr_dow[dow];
                    vol += profile.vol_hour[hour];
                    open += 1.0;
                }
                if open == 0.0 {
                    (1.0, 1.0)
                } else {
                    (arrival / open, vol / open)
                }
            }
        };
        Self {
            arr_hour,
            arr_dow,
            vol_hour: profile.vol_hour,
            arrival_normalizer,
            vol_normalizer,
        }
    }

    // Arrival-rate multiplier at this wall-clock instant: hour-of-day times
    // day-of-week, divided by the exposure-weighted mean of that product over
    // open minutes. A duration is divided by this so a high-activity instant
    // produces shorter inter-arrivals. The division is what makes the stored
    // factors scale-invariant: a profile whose factors are all doubled, or all
    // ones, is exactly neutral rather than a 168x arrival multiplier.
    pub(super) fn arrival_mult(&self, clock_ns: u64) -> f64 {
        let (hour, dow) = utc_hour_dow(clock_ns);
        self.arr_hour[hour] * self.arr_dow[dow] / self.arrival_normalizer
    }

    // Volatility multiplier at this wall-clock instant. Normalized the same way
    // and for the same reason: this value is applied unmodified to a formed return, so
    // without a normalizer nothing at all constrains its scale once the sum
    // guard is relaxed for calendar-bearing profiles, and a curve normalized
    // over 168 hours instead of the 113.75 a CME week actually trades would
    // scale every return by roughly 1.5 with no error anywhere.
    pub(super) fn vol_mult(&self, clock_ns: u64) -> f64 {
        self.vol_hour[utc_hour(clock_ns)] / self.vol_normalizer
    }
}
