// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The `GeneratedSource` lineage: synthesizes market data from a committed
//! fingerprint fitted offline to Kraken trade history. Split by concern:
//!
//! - `consts` - the tuning constants every other submodule draws from.
//! - `fingerprint` - the fingerprint's `Deserialize` config schema and the
//!   validation that keeps caller-supplied scalars/session profiles inside
//!   the fitted bands.
//! - `session` - wall-clock derivation and the precomputed session
//!   multipliers.
//! - `regime` - the optional per-subscription market-regime overlay.
//! - `dynamics` - the ACD duration clock, GARCH latent vol and bounce/drift
//!   price process the walk composes each tick.
//! - `numeric` - small numeric helpers (range checks, saturating decimal
//!   conversion, round-lot snapping).
//! - `source` - `GeneratedSource` itself, the `TickSource` the running server
//!   drives.
//! - `checkpoint` - `CheckpointIndex`, the O(K) seek accelerator over a
//!   `GeneratedSource` realization.
//!
//! Behavior preservation across this split is byte-for-byte: the module
//! boundaries only regroup code, they never reorder an RNG draw or an
//! arithmetic expression relative to the original single-file generator.

mod checkpoint;
mod consts;
mod dynamics;
mod fingerprint;
mod numeric;
mod regime;
mod session;
mod source;

#[cfg(test)]
mod tests;

pub use checkpoint::CheckpointIndex;
pub use fingerprint::{
    AbsReturnAcf, AnchorRange, Fingerprint, GeneratedSourceError, GeneratorScalars, GoldenTargets,
    MinMedianMax, ScalarError, ScalarRanges, SessionProfile, SessionProfileError,
};
pub use source::GeneratedSource;
