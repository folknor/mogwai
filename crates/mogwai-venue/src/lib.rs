// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The mogwai venue venue: config loading, instrument profiles, the tape
//! source seam, the HTTP/WS surface and the one foreground `serve` run.
//!
//! A library, not a binary. The `mogwai` command lives in `mogwai-cli`, which
//! calls [`serve`] for the serve subcommand and reuses `config` and `source`
//! for the offline generator subcommands.

#[cfg(not(unix))]
compile_error!("mogwai-venue requires a Unix target");

mod admission;
mod boatyard;
pub mod config;
mod extremes;
mod fills;
mod history;
mod http;
mod registry;
mod risk;
mod run;
mod serve;
pub mod source;
mod sweeper;
mod tape;
mod vol_window;
mod ws;

/// The fill-timing distribution certification. Test-only, and here rather than
/// in `tests/` because this is the only place that can see
/// `fills::scan_triggers`, `Engine::apply_scans` and `InstrumentProfiles`
/// at once - precisely the seam it certifies.
#[cfg(test)]
mod fill_golden;

mod source_stamp;

pub use serve::serve;
pub use source_stamp::SourceStamp;
