// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The nautilus client pair for the `MOGWAI` venue: [`MogwaiDataClient`] and
//! [`MogwaiExecutionClient`]. Split along the natural data/exec/shared seam:
//!
//! - `data` - the `DataClient` half (subscriptions, the WS transport, live
//!   bar aggregation, historical request paging).
//! - `exec` - the `ExecutionClient` half (the order/fill/position mirror,
//!   order dispatch over the WS command channel,
//!   venue-message-to-nautilus-event translation).
//! - `shared` - plumbing both halves use: the havoc dispatch pipeline, the
//!   instrument cache, and the clock/url/lock glue.

mod data;
mod exec;
mod shared;

pub use data::MogwaiDataClient;
pub use exec::{MogwaiExecutionClient, RETRYABLE_REJECT_PREFIX};
pub(crate) use shared::join_url;
