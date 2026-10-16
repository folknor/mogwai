// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The single integration test binary for mogwai-cli. Each module below was
//! once its own test target; `autotests = false` in Cargo.toml keeps cargo
//! from building them separately again, so a new test file is declared here
//! rather than added as a target of its own. The venue harness lives in
//! `common`, declared here once and imported by the modules that use it.

mod common;

mod arrival_control_exposure;
mod characterize_cli;
mod completion;
mod gen_trace_cli;
mod lifecycle;
mod parity12a;
mod parity12a_aggregate;
mod parity12a_ii;
mod parity3b;
mod presets_cli;
mod serving;
mod test_hygiene;
mod unconfigured_symbol;
