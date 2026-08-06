// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! `mogwai-lab`: the observed-corpus layer of the Python-to-Rust rewrite
//! (notes/rust-rewrite-phases.md phase 1) - the TBBO stream contract, the
//! session/segment math, ledger-bound input verification, preflight, and the
//! storage policy (artifact/cache/scratch). Depends on none of
//! `mogwai-data`, `mogwai-protocol`, `mogwai-engine` or `mogwai-adapter`.
//!
//! The Python reference for everything here is `analysis/mnq_fit.py`; it
//! stays authoritative through phase 4 (notes/rust-rewrite-phases.md) - where
//! this port and the Python disagree, the Python wins and the port changes.

pub mod error;
pub mod ledger;
pub mod preflight;
pub mod session;
pub mod storage;
pub mod stream;
pub mod subcontract;

pub use error::{LabError, LabResult};
