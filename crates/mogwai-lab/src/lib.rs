// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

// The fit's artifact blocks are wide `json!` literals mirroring the Python's
// own dict literals field for field; `json_internal!` recurses once per
// field, so the default 128 does not reach the end of `observe`'s record.
#![recursion_limit = "512"]

//! `mogwai-lab`: the corpus and measurement layer of the Python-to-Rust
//! rewrite (the retired rewrite plan, phases 1 and 2) - the TBBO stream
//! contract, the session/segment math, manifest-bound input verification,
//! preflight, the storage policy (artifact/cache/scratch), the deterministic
//! kernel ([`kernel`]) and the unified protocol-12a block engine
//! ([`measure12a`]), and from phase 3b the protocol-11 fit ([`fit`]) with the
//! `gen --type summary` accumulator ([`summary`]) it walks in-process.
//! Depends on `mogwai-data`/`mogwai-protocol` read-only for the generated
//! measurement side, and on `mogwai-venue` for exactly one thing: the
//! `InstrumentProfile` a fit walk runs against, resolved through the
//! venue's own `Config::load` the way the retired Python fit implementation's scratch configs were.
//! It never depends on `mogwai-engine`.
//!
//! The Python reference has retired. The fit implementation and the seven
//! other
//! absorbed scripts were authoritative through phase 4 - where this port and
//! the Python disagreed, the Python won and the port changed - and phase 4b
//! item 7 moved them to the gitignored `research/dead/` after a program-level
//! review signed the retirement.
//!
//! So this is the reference now, and the difference is worth stating rather
//! than discovering. What survives of the old oracle is committed artifacts and
//! git history: the parity gates still run, but against frozen files
//! (`analysis/mnq-fit.json`, `analysis/fingerprint.json`,
//! `analysis/select-windows-blessed.json`,
//! `analysis/select-windows-python-cache.json`, the tick-composition fixtures)
//! rather than against a program that can be re-run. A disagreement with one of
//! those is still a defect here; a question they cannot answer no longer has an
//! oracle, and needs its own argument.
//!
//! Two deviations from the Python are deliberate, and both are documented at
//! their sites: `sqrt` over `x ** 0.5` in the fingerprint synthesis, and
//! `select_windows::squared` over `** 2`. Each replaces a libm-dependent
//! CPython result - `pow` is not correctly rounded - with a correctly rounded
//! one, on the ground that matching bug-for-bug would make output a function of
//! whichever libm the machine carries.
//!
//! [`exact::population_variance`] is not one of them, and the distinction
//! matters enough to state: it exists to achieve parity, not to depart from it.
//! The float variance the cadence path first used was ill-conditioned and could
//! be wrong by a factor of three, so it was replaced by exact integer
//! arithmetic that reproduces `statistics.pvariance` bit for bit. Every field of
//! the density report matches CPython exactly, and `cadence_feasible.rs` records
//! that no deviation remains there.

pub mod aggregate;
pub mod arrival_control;
pub mod arrival_envelope;
pub mod arrival_screen;
pub mod cadence;
pub mod cadence_feasible;
pub mod characterize;
pub mod delivery;
pub mod error;
pub mod exact;
pub mod fingerprint;
pub mod fit;
pub mod kernel;
pub mod measure12a;
pub mod preflight;
pub mod sampler;
pub mod segments;
pub mod select_windows;
pub mod session;
pub mod session_profile;
pub mod sidecar;
pub mod stage_a_batch;
pub mod storage;
pub mod stream;
pub mod subcontract;
pub mod summary;
pub mod tick_composition_ratios;

pub use error::{LabError, LabResult};
