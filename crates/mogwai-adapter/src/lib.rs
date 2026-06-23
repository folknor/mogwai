//! Nautilus client adapter for a running mogwai-server.
//!
//! The rest of mogwai remains independent of nautilus and speaks the native
//! JSON-over-WebSocket vocabulary from `mogwai-protocol`. This crate is the
//! deliberate boundary where broadarrow can construct nautilus data and
//! execution clients for the synthetic MOGWAI venue.

use std::sync::LazyLock;

use nautilus_model::identifiers::Venue;

mod client;
mod config;
mod convert;
mod factories;
mod lifecycle;

pub use client::{MogwaiDataClient, MogwaiExecutionClient};
pub use config::{MogwaiDataClientConfig, MogwaiExecClientConfig};
pub use factories::{MogwaiDataClientFactory, MogwaiExecutionClientFactory};

/// The canonical MOGWAI venue identifier, single-sourced so a future rename
/// propagates to the `Venue`, the factory `name()` impls, and any test that
/// names the venue.
pub const MOGWAI_VENUE_STR: &str = "MOGWAI";

pub static MOGWAI_VENUE: LazyLock<Venue> = LazyLock::new(|| Venue::from(MOGWAI_VENUE_STR));
