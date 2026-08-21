// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Nautilus client adapter for a running mogwai venue.
//!
//! The rest of mogwai remains independent of nautilus and speaks the native
//! JSON-over-WebSocket vocabulary from `mogwai-protocol`. This crate is the
//! deliberate boundary where a nautilus host can construct data and
//! execution clients for the synthetic MOGWAI venue.

use std::sync::LazyLock;

use nautilus_model::identifiers::Venue;

mod client;
mod clock;
mod config;
mod convert;
mod factories;
mod lifecycle;

pub use client::{MogwaiDataClient, MogwaiExecutionClient, RETRYABLE_REJECT_PREFIX};
pub use clock::{MogwaiClock, mogwai_clock_factory};
pub use config::{DEFAULT_ACCOUNT_ID, MogwaiDataClientConfig, MogwaiExecClientConfig};
pub use factories::{MogwaiDataClientFactory, MogwaiExecutionClientFactory};

/// Launching a venue and learning its endpoint.
///
/// Re-exported from `mogwai-protocol`, where it lives so that mogwai's OWN gates
/// can drive the venue through the same launcher a consumer uses - the venue's
/// test binaries cannot depend on this crate, and a launcher shipped from here
/// would leave the contract hand-rolled on both sides of it. A consumer that
/// already depends on this crate needs no second dependency to launch.
pub use mogwai_protocol::launch;

/// The canonical MOGWAI venue identifier, single-sourced so a future rename
/// propagates to the `Venue`, the factory `name()` impls, and any test that
/// names the venue.
pub const MOGWAI_VENUE_STR: &str = "MOGWAI";

pub static MOGWAI_VENUE: LazyLock<Venue> = LazyLock::new(|| Venue::from(MOGWAI_VENUE_STR));
