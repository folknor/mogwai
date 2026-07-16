// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The three stochastic building blocks the core walk composes each tick:
//! [`AcdClock`] (duration clustering), [`GarchVol`] (latent volatility and the
//! walking mid) and [`BounceState`] (the high/low bounce regime and its
//! same-direction drift). Kept together since none is meaningful on its own -
//! each is a small piece of state plus the update the walk drives it with.

use mogwai_protocol::AggressorSide;
use rand::RngExt;
use rand_chacha::ChaCha12Rng;

use super::consts::{
    BOUNCE_HIGH_FLIP_PROB, BOUNCE_HIGH_TO_LOW_PROB, BOUNCE_LOW_FLIP_PROB, BOUNCE_LOW_TO_HIGH_PROB,
    DRIFT_DIR_FLIP_PROB, GARCH_ARCH, GARCH_GARCH, HIGH_REGIME_DRIFT_PROB, HOT_DRIFT_PROB,
};

#[derive(Clone)]
pub(super) struct AcdClock {
    pub(super) omega: f64,
    pub(super) alpha: f64,
    pub(super) beta: f64,
    pub(super) psi: f64,
    pub(super) prev_duration_s: f64,
    pub(super) eps_mean: f64,
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
    /// Accumulated same-direction on-grid drift, in ticks, applied on top of the
    /// latent mid in `next_price`. This is an UNBOUNDED, never-reset random walk
    /// (it only advances inside the high regime), so over a long run the printed
    /// price = mid + drift can wander arbitrarily far from `start_price` even
    /// though the mid itself is clamped to [tick, MID_CEILING]. Left unbounded
    /// deliberately: the diffusion is slow, the downside is fenced by the
    /// `price_ticks.max(1.0)` floor in `next_price` (a quote can never undercut
    /// one tick), and the only untethered direction is upward and cosmetic.
    /// Bounding or mean-reverting the drift would change the on-grid walk and
    /// break the committed fingerprint's byte-identical golden stream, so the
    /// long-run price un-tethering is documented rather than fixed.
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
