//! Wall-clock derivation and the precomputed per-tick session multipliers.
//! [`utc_hour_dow`] turns the nanosecond clock into civil hour/day-of-week
//! fields with no chrono dependency; [`SessionModulator`] turns a
//! [`super::SessionProfile`] into the two multipliers the walk applies every
//! tick (arrival-rate and volatility).

use super::fingerprint::SessionProfile;

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
    // vol_hour[h]: per-mean per-trade RMS-return multiplier, used directly.
    vol_hour: [f64; 24],
}

impl SessionModulator {
    pub(super) fn new(profile: &SessionProfile) -> Self {
        let mut arr_hour = [0.0; 24];
        for (h, mult) in arr_hour.iter_mut().enumerate() {
            *mult = profile.intensity_hour[h] * 24.0;
        }
        let mut arr_dow = [0.0; 7];
        for (d, mult) in arr_dow.iter_mut().enumerate() {
            *mult = profile.dow_weight[d] * 7.0;
        }
        Self {
            arr_hour,
            arr_dow,
            vol_hour: profile.vol_hour,
        }
    }

    // Arrival-rate multiplier at this wall-clock instant: hour-of-day times
    // day-of-week, both centered on 1.0. A duration is divided by this so a
    // high-activity instant produces shorter inter-arrivals.
    pub(super) fn arrival_mult(&self, clock_ns: u64) -> f64 {
        let (hour, dow) = utc_hour_dow(clock_ns);
        self.arr_hour[hour] * self.arr_dow[dow]
    }

    // Volatility multiplier at this wall-clock instant: the fingerprint's
    // per-mean hour ratio. A formed return is multiplied by this, scaling the
    // innovation standard deviation rather than the variance.
    pub(super) fn vol_mult(&self, clock_ns: u64) -> f64 {
        self.vol_hour[utc_hour(clock_ns)]
    }
}
