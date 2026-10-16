// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The single integration test binary for mogwai-lab. Each module below was
//! once its own test target; `autotests = false` in Cargo.toml keeps cargo
//! from building them separately again, so a new test file is declared here
//! rather than added as a target of its own.

mod exact_pvariance;
mod parity12a_i;
mod parity3a;
mod parity3b_session_profile;
mod parity_select_windows;
mod parity_tick_composition_ratios;
mod stream_refusals;
