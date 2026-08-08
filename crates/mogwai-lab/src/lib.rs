// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

// The fit's artifact blocks are wide `json!` literals mirroring the Python's
// own dict literals field for field; `json_internal!` recurses once per
// field, so the default 128 does not reach the end of `observe`'s record.
#![recursion_limit = "512"]

//! `mogwai-lab`: the corpus and measurement layer of the Python-to-Rust
//! rewrite (notes/rust-rewrite-phases.md phases 1 and 2) - the TBBO stream
//! contract, the session/segment math, ledger-bound input verification,
//! preflight, the storage policy (artifact/cache/scratch), the deterministic
//! kernel ([`kernel`]) and the unified protocol-12a block engine
//! ([`measure12a`]), and from phase 3b the protocol-11 fit ([`fit`]) with the
//! `gen --type summary` accumulator ([`summary`]) it walks in-process.
//! Depends on `mogwai-data`/`mogwai-protocol` READ-ONLY for the generated
//! measurement side, and on `mogwai-server` for exactly one thing: the
//! `InstrumentProfile` a fit walk runs against, resolved through the
//! server's own `Config::load` the way `mnq_fit.py`'s scratch configs were.
//! It never depends on `mogwai-engine`.
//!
//! THE PYTHON REFERENCE HAS RETIRED. `analysis/mnq_fit.py` and the seven other
//! absorbed scripts were authoritative through phase 4 - where this port and
//! the Python disagreed, the Python won and the port changed - and phase 4b
//! item 7 moved them to the gitignored `research/dead/` after a program-level
//! review signed the retirement.
//!
//! So THIS is the reference now, and the difference is worth stating rather
//! than discovering. What survives of the old oracle is committed artifacts and
//! git history: the parity gates still run, but against frozen files
//! (`analysis/mnq-fit.json`, `analysis/fingerprint.json`,
//! `analysis/select-windows-blessed.json`,
//! `analysis/select-windows-python-cache.json`, the tick-composition fixtures)
//! rather than against a program that can be re-run. A disagreement with one of
//! those is still a defect here; a question they cannot answer no longer has an
//! oracle, and needs its own argument.
//!
//! Three deviations from the Python are deliberate and documented at their
//! sites: `sqrt` over `x ** 0.5` in the fingerprint synthesis,
//! `select_windows::squared` over `** 2`, and the exact
//! [`exact::population_variance`] over the ill-conditioned float variance the
//! cadence path first used. All three replace a libm-dependent or
//! ill-conditioned CPython result with a correctly rounded one.

pub mod aggregate;
pub mod cadence;
pub mod cadence_feasible;
pub mod characterize;
pub mod error;
pub mod exact;
pub mod fingerprint;
pub mod fit;
pub mod kernel;
pub mod ledger;
pub mod measure12a;
pub mod preflight;
pub mod sampler;
pub mod select_windows;
pub mod session;
pub mod session_profile;
pub mod storage;
pub mod stream;
pub mod subcontract;
pub mod summary;
pub mod tick_composition_ratios;

pub use error::{LabError, LabResult};
