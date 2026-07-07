//! The optional per-subscription market regime overlay: [`RegimeState`]
//! derives the walk's arrival/vol/clamp multipliers from a
//! [`mogwai_protocol::MarketRegime`] at construction time, and exposes the
//! per-tick hooks ([`RegimeState::vol_mult`], [`RegimeState::realized_clamp_mult`],
//! [`RegimeState::take_reopen_crossed`]) the core walk calls into.

use mogwai_protocol::MarketRegime;

use super::session::utc_hour;

#[derive(Debug, Clone)]
pub(super) struct RegimeState {
    pub(super) arrival_thin: f64,
    vol_mult: f64,
    pub(super) clamp_mult: f64,
    edge: Option<EdgeSpike>,
    reopen: Option<Reopen>,
}

impl RegimeState {
    pub(super) fn new(
        regime: Option<MarketRegime>,
        clamp_override: Option<f64>,
        start_ts: u64,
        symbol: &str,
    ) -> Self {
        let mut state = Self {
            arrival_thin: 1.0,
            vol_mult: 1.0,
            clamp_mult: 1.0,
            edge: None,
            reopen: None,
        };

        // LOCKSTEP with mogwai_protocol::validate_market_regime: these arms
        // destructure the same MarketRegime variants that validator range-checks,
        // one crate away, with no compiler link forcing the two to agree. A new
        // variant or a renamed field must be mirrored in BOTH - add the arm here
        // and the matching guard there. Notably SessionEdgeSpike's
        // `start_hour < end_hour` invariant is enforced only in the validator and
        // trusted at runtime by EdgeSpike's window comparison below.
        match regime {
            Some(MarketRegime::VolStorm { vol_mult }) => {
                state.vol_mult = vol_mult;
                // Default the clamp lift to the storm multiplier; the test-only
                // clamp_override below uniformly replaces it when present (it is
                // how the pricing instrument pins the clamp to disprove the bare
                // multiply), so there is no need to fold the override in here.
                state.clamp_mult = vol_mult;
            }
            Some(MarketRegime::LiquidityDrought { thin_factor }) => {
                state.arrival_thin = thin_factor;
            }
            Some(MarketRegime::SessionEdgeSpike {
                start_hour,
                end_hour,
                extra_vol_mult,
            }) => {
                // `start_hour < end_hour` is enforced by
                // mogwai_protocol::validate_market_regime, not here - this
                // lockstep match trusts a validated regime reached it. A
                // regime that bypasses the validator (a test fixture, or a
                // future construction path) with an inverted window would
                // silently produce a zero-width spike in vol_mult below; catch
                // that in debug builds instead of staying silent.
                debug_assert!(
                    start_hour < end_hour,
                    "SessionEdgeSpike window must satisfy start_hour < end_hour \
                     (start_hour={start_hour}, end_hour={end_hour})"
                );
                state.edge = Some(EdgeSpike {
                    start_hour,
                    end_hour,
                    extra_vol_mult,
                });
            }
            Some(MarketRegime::ReopenGap {
                at_ts,
                halt_secs,
                gap_frac,
            }) => {
                // Crossing detection (`take_reopen_crossed`) fires when a
                // tick's gap straddles the instant:
                // `old_clock < at_ts && at_ts <= new_clock`. The very first
                // gap opens at the tape anchor (`start_ts`), so an `at_ts` at
                // or before the anchor can NEVER satisfy the first conjunct -
                // the halt would sit armed forever, silently inert.
                // `validate_market_regime` rejects `at_ts == 0` for exactly
                // that failure mode, but any other already-elapsed instant
                // reproduces it one layer down, so treat it the way the server
                // treats out-of-band divergences: consume it at construction
                // (fail closed) and say so, rather than fabricate a halt at
                // whatever tick the anchor happens to make first. No RNG is
                // drawn either way, so an elapsed ReopenGap leaves a stream
                // byte-identical to no regime at all
                // (`reopen_gap_at_or_before_anchor_is_consumed_and_matches_clean`
                // pins this). No tracing dep in this crate, so stderr is the
                // visible channel - same convention as KrakenCsvSource.
                if at_ts <= start_ts {
                    eprintln!(
                        "GeneratedSource({symbol}): ReopenGap at_ts {at_ts} is at or before \
                         the tape anchor {start_ts}; the halt has already elapsed and is \
                         dropped (it would never fire)"
                    );
                } else {
                    state.reopen = Some(Reopen {
                        at_ts,
                        halt_ns: halt_secs.saturating_mul(1_000_000_000),
                        gap_frac,
                    });
                }
            }
            None => {}
        }

        if let Some(clamp_mult) = clamp_override {
            state.clamp_mult = clamp_mult;
        }

        state
    }

    // `extra_vol_mult` inside a SessionEdgeSpike's UTC hour window, 0.0 outside
    // it (and 0.0 when no edge spike is armed). Shared by `vol_mult` and
    // `realized_clamp_mult` so the RMS amplification and the clamp lift track
    // the exact same window, by construction, and cannot drift apart.
    fn edge_extra(&self, clock_ns: u64) -> f64 {
        self.edge.as_ref().map_or(0.0, |edge| {
            let hour = utc_hour(clock_ns);
            if usize::from(edge.start_hour) <= hour && hour < usize::from(edge.end_hour) {
                edge.extra_vol_mult
            } else {
                0.0
            }
        })
    }

    pub(super) fn vol_mult(&self, clock_ns: u64) -> f64 {
        // ADDITIVE within the regime envelope: the neutral value is 1.0 (set in
        // `new`), and a SessionEdgeSpike layers its extra_vol_mult on top by
        // addition (out of window edge_extra is 0.0, leaving the baseline). The
        // RESULT is then composed MULTIPLICATIVELY with the session envelope in
        // next_latent_mid - see the convention note there. The mix is deliberate
        // and load-bearing on both neutral values being 1.0.
        self.vol_mult + self.edge_extra(clock_ns)
    }

    // Clamp multiplier for the REALIZED return (the one that moves the mid), as
    // opposed to the GARCH feedback which uses `self.clamp_mult` raw. Mirrors
    // `vol_mult`'s additive structure so the realized clamp is lifted exactly
    // where and by exactly how much the realized RMS is amplified: the base
    // clamp (vol_mult for a storm, the test override, or 1.0) plus a
    // SessionEdgeSpike's in-window `extra_vol_mult`. Keeping this SEPARATE from
    // the feedback clamp is what lets the edge spike lift its in-window ceiling
    // while leaving the recursion state - and therefore every out-of-window
    // tick - byte-identical to a clean run. For VolStorm and the
    // clean/drought/reopen regimes `edge_extra` is 0.0, so this equals
    // `self.clamp_mult` and their streams are unchanged.
    pub(super) fn realized_clamp_mult(&self, clock_ns: u64) -> f64 {
        self.clamp_mult + self.edge_extra(clock_ns)
    }

    pub(super) fn take_reopen_crossed(
        &mut self,
        old_clock_ns: u64,
        new_clock_ns: u64,
    ) -> Option<Reopen> {
        if self
            .reopen
            .as_ref()
            .is_some_and(|reopen| old_clock_ns < reopen.at_ts && reopen.at_ts <= new_clock_ns)
        {
            self.reopen.take()
        } else {
            None
        }
    }
}

#[derive(Debug, Clone)]
struct EdgeSpike {
    start_hour: u8,
    end_hour: u8,
    extra_vol_mult: f64,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Reopen {
    pub(super) at_ts: u64,
    pub(super) halt_ns: u64,
    pub(super) gap_frac: f64,
}
