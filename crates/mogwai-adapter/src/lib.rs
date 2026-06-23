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

pub static MOGWAI_VENUE: LazyLock<Venue> = LazyLock::new(|| Venue::from("MOGWAI"));
