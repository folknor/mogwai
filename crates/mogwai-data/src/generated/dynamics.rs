// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The stochastic building blocks the core walk composes: [`ArrivalClock`]
//! (the two-state Markov-modulated duration clock), [`SweepShape`] and
//! [`SweepBurst`] (the parent/child sweep the tape publishes raw), [`GarchVol`]
//! (latent volatility and the walking mid) and [`BounceState`] (the high/low
//! bounce regime and its same-direction drift). Kept together since none is
//! meaningful on its own - each is a small piece of state plus the update the
//! walk drives it with.

use mogwai_protocol::AggressorSide;
use rand::RngExt;
use rand_chacha::ChaCha12Rng;

use super::consts::{
    BOUNCE_HIGH_FLIP_PROB, BOUNCE_HIGH_TO_LOW_PROB, BOUNCE_LOW_FLIP_PROB, BOUNCE_LOW_TO_HIGH_PROB,
    CHILD_CAP, DRIFT_DIR_FLIP_PROB, GARCH_ARCH, GARCH_GARCH, HIGH_REGIME_DRIFT_PROB,
    HOT_DRIFT_PROB,
};

/// The child-count mixture of one parent sweep: with probability `q` exactly
/// one raw fill, otherwise a Geometric on {1, 2, ...} with mean `m`. Neither is
/// a fitted knob - both are solved in closed form from the two MEASURED
/// statistics `children_mean` and `children_single_frac`.
#[derive(Clone)]
pub(super) struct SweepShape {
    pub(super) q: f64,
    pub(super) m: f64,
    pub(super) level_step_prob: f64,
    pub(super) truncated: u64,
    pub(super) drawn: u64,
}

impl SweepShape {
    /// Solves the mixture. The extra single-child mass the mixture adds can only
    /// be non-negative, so the mixture exists iff `single_frac >= 1 /
    /// children_mean`; below that the shape degenerates to a pure geometric and
    /// the measured single fraction becomes documentation rather than a target.
    ///
    /// The clamps are guards on a scaled mean, not on config. `GeneratorScalars`
    /// already refuses `children_mean <= 1`, but the caller scales that mean by
    /// the arrival state and by an armed `FlowSurge`, and a scaled mean at or
    /// below one would make `ln(1 - 1/m)` non-negative or NaN and silently pin
    /// every sweep at one child. Refusing to go below `1 + f64::EPSILON` keeps
    /// the inverse CDF well formed; at the committed scalars the quiet-state
    /// mean is 1.70, so the clamp never binds on the shipped tape.
    pub(super) fn new(children_mean: f64, single_frac: f64, levels_mean: f64) -> Self {
        let children_mean = children_mean.max(1.0 + f64::EPSILON);
        let (q, m) = if single_frac < 1.0 / children_mean {
            (0.0, children_mean)
        } else {
            let m = (children_mean - 1.0) / (1.0 - single_frac);
            (1.0 - (children_mean - 1.0) / (m - 1.0), m)
        };
        Self {
            q,
            m,
            level_step_prob: ((levels_mean - 1.0) / (children_mean - 1.0)).clamp(0.0, 1.0),
            truncated: 0,
            drawn: 0,
        }
    }

    /// The inverse CDF of the mixture, consuming exactly one uniform plus one
    /// more when the geometric branch is taken - the RNG contract two
    /// implementations must share for the byte-identical golden to mean
    /// anything. `1 - u` keeps the logarithm finite on `u` in `[0, 1)`.
    ///
    /// `truncated` counts draws clipped by `CHILD_CAP`, because a clipped draw
    /// is indistinguishable from one that legitimately landed on the cap once it
    /// has been clipped, and the realism gate asserts a truncation FRACTION.
    pub(super) fn next_count(&mut self, rng: &mut ChaCha12Rng) -> u32 {
        self.drawn += 1;
        if rng.random::<f64>() < self.q {
            return 1;
        }
        let raw = 1.0 + (1.0 - rng.random::<f64>()).ln() / (1.0 - 1.0 / self.m).ln();
        let count = raw.floor().max(1.0) as u64;
        if count > u64::from(CHILD_CAP) {
            self.truncated += 1;
            CHILD_CAP
        } else {
            count as u32
        }
    }
}

/// The children of the current parent still owed to `next_tick`. It lives in
/// `GeneratedSource` state, not in a local, so a `CheckpointIndex` snapshot
/// taken mid-sweep resumes mid-sweep and the chain stays byte-identical.
#[derive(Clone)]
pub(super) struct SweepBurst {
    pub(super) remaining: u32,
    pub(super) emitted: u32,
    pub(super) parent_ts_ns: u64,
    pub(super) side: AggressorSide,
    pub(super) price_ticks: f64,
}

impl SweepBurst {
    pub(super) fn empty() -> Self {
        Self {
            remaining: 0,
            emitted: 0,
            parent_ts_ns: 0,
            side: AggressorSide::Buyer,
            price_ticks: 0.0,
        }
    }
}

/// The two-state Markov-modulated Weibull arrival clock that replaced the ACD
/// block (see the head of `consts.rs` for the evidence that forced the swap).
/// Each parent gap is an exponential-family draw scaled by the CURRENT state's
/// mean; the state then switches with the stored probabilities, whose
/// stationary quiet occupancy is `ARRIVAL_QUIET_FRACTION`.
#[derive(Clone)]
pub(super) struct ArrivalClock {
    pub(super) active_mean_s: f64,
    pub(super) quiet_mean_s: f64,
    pub(super) quiet_to_active: f64,
    pub(super) active_to_quiet: f64,
    /// The state the NEXT gap will be drawn from.
    pub(super) quiet: bool,
    /// The state the gap just drawn came from. Sweep size is conditioned on
    /// this, not on `quiet`, so the fat sweeps land on the short gaps rather
    /// than one event later.
    pub(super) last_quiet: bool,
}

#[derive(Clone)]
pub(super) struct GarchVol {
    pub(super) a0: f64,
    pub(super) a1: f64,
    pub(super) b1: f64,
    pub(super) sigma2: f64,
    pub(super) prev_return: f64,
    pub(super) mid: f64,
}

impl GarchVol {
    pub(super) fn new(mid: f64, vol_scalar: f64) -> Self {
        let unconditional_var = vol_scalar.powi(2);
        let persistence = GARCH_ARCH + GARCH_GARCH;
        Self {
            a0: unconditional_var * (1.0 - persistence),
            a1: GARCH_ARCH,
            b1: GARCH_GARCH,
            sigma2: unconditional_var,
            prev_return: 0.0,
            mid,
        }
    }
}

#[derive(Clone)]
pub(super) struct BounceState {
    pub(super) prev_side: AggressorSide,
    pub(super) high_regime: bool,
    /// Same-direction on-grid drift, in ticks, applied on top of the latent mid
    /// in `next_price`. `next_drift` steps it by one tick inside the high
    /// regime, exactly as before the raw-fill cadence rewrite, but it is NO
    /// LONGER an unbounded accumulation: `GeneratedSource::next_child` RE-SETS
    /// it at the end of every parent sweep to `DRIFT_RECENTER_FRAC` of the
    /// residual between the sweep's last printed level and the latent mid. The
    /// long-run un-tethering the print-layer generator documented here is
    /// therefore gone - a sweep can walk several ticks off the mid inside one
    /// event, and the re-centring is what pulls the next event's quote back
    /// toward it instead of letting the excursion become permanent. The
    /// `price_ticks.max(1.0)` floor in `next_price` still fences the downside.
    pub(super) drift_ticks: i64,
    pub(super) drift_dir: i64,
    pub(super) drift_hot: bool,
    pub(super) half_spread_ticks: f64,
}

impl BounceState {
    pub(super) fn next_drift(&mut self, rng: &mut ChaCha12Rng) {
        if !self.high_regime {
            return;
        }
        if rng.random_bool(DRIFT_DIR_FLIP_PROB) {
            self.drift_dir *= -1;
        }
        let p_drift = if self.drift_hot {
            HOT_DRIFT_PROB
        } else {
            HIGH_REGIME_DRIFT_PROB
        };
        if rng.random_bool(p_drift) {
            self.drift_ticks += self.drift_dir;
            self.drift_hot = true;
        } else {
            self.drift_hot = false;
        }
    }

    pub(super) fn next_side(&mut self, rng: &mut ChaCha12Rng) -> AggressorSide {
        if self.high_regime {
            if rng.random_bool(BOUNCE_HIGH_TO_LOW_PROB) {
                self.high_regime = false;
            }
        } else if rng.random_bool(BOUNCE_LOW_TO_HIGH_PROB) {
            self.high_regime = true;
        }
        let p_flip = if self.high_regime {
            BOUNCE_HIGH_FLIP_PROB
        } else {
            BOUNCE_LOW_FLIP_PROB
        };
        if rng.random_bool(p_flip) {
            self.prev_side = match self.prev_side {
                AggressorSide::Buyer => AggressorSide::Seller,
                AggressorSide::Seller | AggressorSide::NoAggressor => AggressorSide::Buyer,
            };
        }
        self.prev_side
    }
}
