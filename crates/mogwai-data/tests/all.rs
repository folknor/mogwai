// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The single integration test binary for mogwai-data. Each module below was
//! once its own test target; `autotests = false` in Cargo.toml keeps cargo
//! from building them separately again, so a new test file is declared here
//! rather than added as a target of its own.

mod glossary_vocabulary_prose;
mod tape_version_prose;
