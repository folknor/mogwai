// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The mogwai venue venue: config loading, instrument profiles, the tape
//! source seam, the HTTP/WS surface and the one foreground `serve` run.
//!
//! A library, not a binary. The `mogwai` command lives in `mogwai-cli`, which
//! calls [`serve`] for the serve subcommand and reuses `config` and `source`
//! for the offline generator subcommands.

#[cfg(not(unix))]
compile_error!("mogwai-server requires a Unix target");

mod admission;
mod boatyard;
pub mod config;
mod extremes;
mod fills;
mod http;
mod risk;
mod run;
mod serve;
pub mod source;
mod sweeper;
mod tape;
mod ws;

/// The fill-timing distribution certification. Test-only, and here rather than
/// in `tests/` because this is the only place that can see
/// `fills::scan_triggers`, `Engine::apply_scans` and `InstrumentProfiles`
/// at once - precisely the seam it certifies.
#[cfg(test)]
mod fill_golden;

pub use serve::serve;

const LONG_VERSION: &str = env!("MOGWAI_LONG_VERSION");

/// The `--version` string: semver, git hash and build time from `build.rs`,
/// composed with the tape protocol version the binary was built against.
///
/// Lives here rather than in the CLI because the readiness record carries it
/// too, and one run must not be able to announce a different version than the
/// one it prints.
pub fn long_version() -> String {
    format!("{LONG_VERSION} tape {}", mogwai_data::TAPE_PROTOCOL_VERSION)
}
