// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

use serde::{Deserialize, Serialize};

/// Saturating UNIX-nanoseconds clock reader: the single source of truth for
/// "now" on the wire's `ts_event` axis, shared by the venue (its `now_ns`) and
/// the adapter (its `now_unix_nanos`, which wraps the result in `UnixNanos`).
///
/// A backward clock step (NTP correction, leap second) yields an `Err` from
/// `duration_since`, which we saturate to `0` rather than panic on - the prior
/// duplicated readers `.expect("clock before epoch")`ed and would kill their
/// host task on any skew. The nanosecond count is a `u128`; we clamp it to
/// `u64::MAX` rather than truncate with `as u64`, which would silently wrap
/// past year 2554. `map_or` (not `map(...).unwrap_or(...)`) keeps clippy's
/// `map_unwrap_or` lint quiet.
#[must_use]
pub fn now_unix_nanos() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
}

/// Affine wall-to-simulated time map shared by the venue, adapter, and the
/// nautilus clock injected into the host's live node.
///
/// The scaling step (`offset as f64 * speed` / `offset as f64 / speed` in
/// `sim_ns`/`wall_ns`/`wall_span`) goes through `f64`, so it is only exact
/// while the elapsed nanosecond offset (`wall_ns - wall_anchor_ns` or
/// `sim_ns - sim_epoch_ns`) stays under 2^53 (~104 days of continuous span
/// from the anchor). Past that the `as f64` cast drops low-order nanoseconds
/// before scaling, and `sim_ns`/`wall_ns` stop being exact inverses of each
/// other. Harmless for any realistic session length; noted here because the
/// anchors themselves (`sim_epoch_ns`, `wall_anchor_ns`) stay `u64` end to end
/// specifically to dodge this loss, so only the *offset* from the anchor is
/// exposed to it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SimClock {
    pub sim_epoch_ns: u64,
    pub wall_anchor_ns: u64,
    pub speed: f64,
}

impl SimClock {
    /// The identity map: simulated time equals wall time.
    #[must_use]
    pub const fn identity() -> Self {
        Self {
            sim_epoch_ns: 0,
            wall_anchor_ns: 0,
            speed: 1.0,
        }
    }

    /// Map a wall-clock nanosecond read onto the simulated axis.
    #[must_use]
    pub fn sim_ns(&self, wall_ns: u64) -> u64 {
        if wall_ns <= self.wall_anchor_ns {
            return self.sim_epoch_ns;
        }
        if !self.speed.is_finite() || self.speed <= 0.0 {
            return self.sim_epoch_ns;
        }
        let offset = wall_ns - self.wall_anchor_ns;
        let scaled = scaled_f64_to_u64(offset as f64 * self.speed);
        self.sim_epoch_ns.saturating_add(scaled)
    }

    /// Return the wall instant at which the clock reaches `sim_ns`.
    #[must_use]
    pub fn wall_ns(&self, sim_ns: u64) -> u64 {
        if sim_ns <= self.sim_epoch_ns {
            return self.wall_anchor_ns;
        }
        if !self.speed.is_finite() || self.speed <= 0.0 {
            return u64::MAX;
        }
        let offset = sim_ns - self.sim_epoch_ns;
        let scaled = scaled_f64_to_u64(offset as f64 / self.speed);
        self.wall_anchor_ns.saturating_add(scaled)
    }

    /// Return the wall duration that realizes `sim_dur_ns` simulated nanos.
    #[must_use]
    pub fn wall_span(&self, sim_dur_ns: u64) -> u64 {
        if !self.speed.is_finite() || self.speed <= 0.0 {
            return u64::MAX;
        }
        scaled_f64_to_u64(sim_dur_ns as f64 / self.speed)
    }

    /// `wall_span` as a `Duration`, the form every caller actually sleeps or
    /// intervals on. The single place the wall floor is applied: a span that
    /// scales to zero nanos is clamped to 1ns so a `tokio::time::sleep` /
    /// `interval` derived from a configured (sim-intended) duration never
    /// degenerates to a zero-delay busy loop. This 1ns is only the code floor;
    /// the EFFECTIVE floor is the tokio timer granularity (~1ms), below which a
    /// scaled duration coalesces regardless. See `reference/clock.md`.
    #[must_use]
    pub fn wall_duration(&self, sim_dur_ns: u64) -> std::time::Duration {
        std::time::Duration::from_nanos(self.wall_span(sim_dur_ns).max(1))
    }
}

/// The `/clock` payload: the affine `SimClock` plus the tape boundary the
/// venue derived at boot. `SimClock` stays the pure wall-to-sim map; this
/// richer envelope publishes where the synthetic tape begins so a consumer can
/// guard its own warmup window instead of issuing a doomed off-tape fetch.
///
/// `venue_now_ns` is `sim.sim_ns(wall)` sampled when the request is served, so
/// a consumer gets sim-now and the tape floor from one round trip without having
/// to read its own (possibly skewed) wall clock. `data_origin_ns` is the
/// earliest `ts_event` any source can serve (`run_start_ns - warmup_ns`); a
/// request for a `start` below it is refused. The span is echoed so the consumer
/// can report the floor in its own terms.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct VenueClock {
    /// The affine wall-to-sim map the adapter feeds to the nautilus node.
    pub sim: SimClock,
    /// `sim.sim_ns(wall)` at the instant the `/clock` request was served.
    pub venue_now_ns: u64,
    /// Earliest `ts_event` the tape can serve; a `start` below it is off-tape.
    pub data_origin_ns: u64,
    /// Uniform servable history span before `run_start_ns`, in nanoseconds.
    /// The boot river is materialized before readiness; other rivers
    /// materialize it on first read.
    pub warmup_ns: u64,
    /// True when this answer belongs to a placed boat, false when it is the
    /// venue clock served as a fallback because the named river carries no
    /// boat (or because no river was named). The two are not interchangeable -
    /// a boat's clock is anchored at ITS placement - so the fallback is
    /// labelled rather than left to look like a boat's answer. Defaulted on
    /// decode so a payload predating the boatyard still parses.
    #[serde(default)]
    pub boat_clock: bool,
}

/// API-boundary guard for `SimClock`, mirroring `validate_conn_havoc` /
/// `validate_market_regime` / `validate_inbound_havoc` in style. `speed` must
/// be finite and strictly positive.
///
/// `sim_ns`/`wall_ns`/`wall_span` all tolerate a non-finite or non-positive
/// `speed` in memory rather than panic: `sim_ns` pins to the sim anchor
/// (`sim_epoch_ns` - sim time never leaves the epoch), while `wall_ns` and
/// `wall_span` saturate to `u64::MAX` (at zero speed a sim instant beyond the
/// epoch is never reached, so the wall answer is "the end of time", not the
/// anchor). So nothing in this type stops a degenerate `SimClock` from
/// existing - but it cannot round-trip over the wire: serde_json serializes a
/// non-finite `f64` as JSON `null`, and `null` fails to decode back into the
/// bare `f64` field, wedging whichever end tries to parse it.
/// `mogwai-venue`'s own config-time check (`build_run_clock`) already guards
/// the configured speed before a `SimClock` is ever constructed there; this
/// validator exists so any other sender of a `SimClock` - present or future -
/// has the same one-line gate to call before serializing one, instead of
/// reproducing the check ad hoc.
pub fn validate_sim_clock(clock: &SimClock) -> Result<(), &'static str> {
    if !clock.speed.is_finite() || clock.speed <= 0.0 {
        return Err("speed must be finite and > 0.0");
    }
    Ok(())
}

fn scaled_f64_to_u64(value: f64) -> u64 {
    if value.is_nan() || value <= 0.0 {
        0
    } else if value >= u64::MAX as f64 {
        u64::MAX
    } else {
        value as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sim_clock_identity_maps_wall_unchanged() {
        let clock = SimClock::identity();

        assert_eq!(clock.sim_ns(123), 123);
        assert_eq!(clock.wall_ns(456), 456);
        assert_eq!(clock.wall_span(789), 789);
        assert_eq!(clock, SimClock::identity());
    }

    #[test]
    fn sim_clock_preserves_epoch_scale_nanosecond_precision() {
        let clock = SimClock {
            sim_epoch_ns: 1_900_000_000_000_000_001,
            wall_anchor_ns: 10_000_000,
            speed: 1.0,
        };

        assert_eq!(
            clock.sim_ns(clock.wall_anchor_ns + 17),
            clock.sim_epoch_ns + 17
        );
        assert_eq!(
            clock.wall_ns(clock.sim_epoch_ns + 17),
            clock.wall_anchor_ns + 17
        );
    }

    #[test]
    fn sim_clock_wall_duration_scales_and_floors_at_one_nano() {
        use std::time::Duration;

        // Identity: a sim duration is its own wall duration.
        assert_eq!(
            SimClock::identity().wall_duration(5_000_000),
            Duration::from_nanos(5_000_000)
        );

        // Accelerated: a 100 ms sim duration realizes as 1 ms wall at 100x.
        let fast = SimClock {
            sim_epoch_ns: 1,
            wall_anchor_ns: 1,
            speed: 100.0,
        };
        assert_eq!(fast.wall_duration(100_000_000), Duration::from_millis(1));

        // A span that scales below one nanosecond is clamped to the 1ns code
        // floor, never zero (the tokio granularity floor is the real bound).
        assert_eq!(fast.wall_duration(0), Duration::from_nanos(1));
        assert_eq!(fast.wall_duration(50), Duration::from_nanos(1));
    }

    #[test]
    fn sim_clock_scales_and_inverts_with_rounding_bound() {
        let clock = SimClock {
            sim_epoch_ns: 86_400_000_000_000,
            wall_anchor_ns: 1_000_000_000,
            speed: 3.5,
        };
        let wall = clock.wall_anchor_ns + 20_000;
        let sim = clock.sim_ns(wall);

        assert_eq!(sim, clock.sim_epoch_ns + 70_000);
        assert_eq!(clock.wall_ns(sim), wall);
        assert_eq!(clock.wall_span(35_000), 10_000);
    }

    #[test]
    fn sim_clock_saturates_underflow_and_overflow() {
        let clock = SimClock {
            sim_epoch_ns: 1_000,
            wall_anchor_ns: 2_000,
            speed: 2.0,
        };
        assert_eq!(clock.sim_ns(1_999), 1_000);
        assert_eq!(clock.wall_ns(999), 2_000);

        let overflow_sim = SimClock {
            sim_epoch_ns: u64::MAX - 10,
            wall_anchor_ns: 0,
            speed: 100.0,
        };
        assert_eq!(overflow_sim.sim_ns(1), u64::MAX);

        let overflow_wall = SimClock {
            sim_epoch_ns: 0,
            wall_anchor_ns: u64::MAX - 10,
            speed: 1.0,
        };
        assert_eq!(overflow_wall.wall_ns(20), u64::MAX);
    }

    #[test]
    fn sim_clock_round_trips_over_json() {
        let clock = SimClock {
            sim_epoch_ns: 123,
            wall_anchor_ns: 456,
            speed: 7.5,
        };

        let json = serde_json::to_string(&clock).unwrap();
        let decoded: SimClock = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded, clock);
    }

    /// The snapshot `/clock` serves and the adapter reads. Its BYTE form is
    /// pinned, not just its round trip: `backfill_horizon_ns` became
    /// `warmup_ns` when warmup stopped being a permission and became something
    /// the venue materializes, and the adapter's transport test pins this exact
    /// text - so the two move together or the rename is caught here first.
    #[test]
    fn clock_snapshot_round_trips() {
        let clock = VenueClock {
            sim: SimClock {
                sim_epoch_ns: 1_900_000_000_000_000_000,
                wall_anchor_ns: 1_782_000_000_000_000_000,
                speed: 120.0,
            },
            venue_now_ns: 1_900_000_799_000_000_000,
            data_origin_ns: 1_899_913_600_000_000_000,
            warmup_ns: 86_400_000_000_000,
            boat_clock: true,
        };

        let json = serde_json::to_string(&clock).unwrap();
        assert_eq!(
            json,
            r#"{"sim":{"sim_epoch_ns":1900000000000000000,"wall_anchor_ns":1782000000000000000,"speed":120.0},"venue_now_ns":1900000799000000000,"data_origin_ns":1899913600000000000,"warmup_ns":86400000000000,"boat_clock":true}"#,
            "the /clock snapshot's byte form is a wire contract"
        );
        let decoded: VenueClock = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, clock);
    }

    #[test]
    fn now_unix_nanos_reads_a_post_epoch_clock() {
        // A real post-epoch wall clock is well past zero. Deliberately NO
        // ordering assertion across two reads: `now_unix_nanos` wraps
        // `SystemTime`, which is not monotonic - an NTP step between two reads
        // can legitimately go backward, and the helper's contract is exactly
        // to saturate such a step (to 0 at worst) rather than promise
        // monotonicity, so asserting `b >= a` would pin the host's clock
        // discipline, not this function.
        assert!(now_unix_nanos() > 0);
    }

    #[test]
    fn validate_sim_clock_rejects_non_finite_and_non_positive_speed() {
        let mut clock = SimClock::identity();
        assert_eq!(validate_sim_clock(&clock), Ok(()));

        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.0, -1.0] {
            clock.speed = bad;
            assert_eq!(
                validate_sim_clock(&clock),
                Err("speed must be finite and > 0.0")
            );
        }
    }
}
