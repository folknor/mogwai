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
//! The Python reference for everything here is `analysis/mnq_fit.py`; it
//! stays authoritative through phase 4 (notes/rust-rewrite-phases.md) - where
//! this port and the Python disagree, the Python wins and the port changes.

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
