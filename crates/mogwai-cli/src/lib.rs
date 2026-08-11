// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The library half of the `mogwai` binary: exists so its own integration
//! tests can call a subcommand's driver function directly rather than
//! shelling out to `target/release/mogwai`. Today that is exactly
//! [`measure::run_measure`], the golden-gate parity test's entrypoint
//! (the retired rewrite plan, slice 2c-ii) and [`fit::run`]'s config
//! resolution (phase 3b's cache-replay parity gate) - every other
//! subcommand still lives module-private under `src/main.rs`.

pub mod arrival_control;
pub mod arrival_envelope_diagnostic;
pub mod arrival_screen;
pub mod count_curve;
pub mod fit;
pub mod measure;
pub mod minute_range_envelope;
pub mod ordered_counts;
pub mod slow_geometry;
