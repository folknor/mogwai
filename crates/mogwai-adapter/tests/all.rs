// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The single integration test binary for mogwai-adapter. Each module below
//! was once its own test target; `autotests = false` in Cargo.toml keeps cargo
//! from building them separately again, so a new test file is declared here
//! rather than added as a target of its own. The socket stub lives in
//! `common`, declared here once and imported by the modules that use it.

mod common;

mod adapter_smoke;
mod data_client_transport;
mod havoc;
mod reconciliation;
