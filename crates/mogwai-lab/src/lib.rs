// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! `mogwai-lab`: the corpus and measurement layer of the Python-to-Rust
//! rewrite (notes/rust-rewrite-phases.md phases 1 and 2) - the TBBO stream
//! contract, the session/segment math, ledger-bound input verification,
//! preflight, the storage policy (artifact/cache/scratch), the deterministic
//! kernel ([`kernel`]) and the unified protocol-12a block engine
//! ([`measure12a`]). Depends on `mogwai-data`/`mogwai-protocol` READ-ONLY for
//! the generated measurement side, and on neither `mogwai-server` nor
//! `mogwai-engine`.
//!
//! The Python reference for everything here is `analysis/mnq_fit.py`; it
//! stays authoritative through phase 4 (notes/rust-rewrite-phases.md) - where
//! this port and the Python disagree, the Python wins and the port changes.

pub mod aggregate;
pub mod error;
pub mod kernel;
pub mod ledger;
pub mod measure12a;
pub mod preflight;
pub mod session;
pub mod storage;
pub mod stream;
pub mod subcontract;

pub use error::{LabError, LabResult};
