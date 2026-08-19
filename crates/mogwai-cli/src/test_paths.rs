// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! THE TWO PATHS A UNIT TEST IN THIS CRATE MAY RESOLVE, in one place because
//! hand-rolling either one is a defect this crate has shipped twice.
//!
//! A cargo unit test runs with its working directory set to the PACKAGE root,
//! `crates/mogwai-cli`, never the repository root. Two consequences, and both
//! have bitten:
//!
//! - A test reading a repo-relative input (`analysis/mnq-arrival-screen.json`)
//!   resolves `crates/mogwai-cli/analysis/...`, which does not exist. Where the
//!   read was guarded by an existence check with an early return, the test
//!   asserted NOTHING on every run it ever made, while reporting green.
//!   [`repo_root`] is what such a test joins against, and
//!   `gate_skip_list.rs`'s `no_test_declines_to_assert_on_a_missing_input`
//!   holds the class shut.
//! - A test WRITING to a relative `target/` lands in
//!   `crates/mogwai-cli/target/`, which the root `.gitignore` hides - its bare
//!   `target` pattern matches a directory of that name at any depth - and
//!   which `cargo clean` never touches, because cargo's target directory is at
//!   the workspace root. The result is an untracked, unswept, permanently
//!   growing scratch area wearing a build directory's name. [`scratch_dir`] is
//!   the real one.
//!
//! `CARGO_TARGET_TMPDIR` would serve the second case and is what the
//! INTEGRATION tests use; cargo does not set it for unit tests, which is why
//! this module resolves the workspace target directory from
//! `CARGO_MANIFEST_DIR` instead.
//!
//! A PATH IS PER-PROCESS OR IT IS A SHARED RESOURCE, and the first cut of
//! [`scratch_dir`] got that wrong: it resolved `target/<name>` - one FIXED path
//! per name - and opened with `remove_dir_all`. The full gate profile runs the
//! workspace and instrumented sweeps CONCURRENTLY, so two processes running the
//! same test race the wipe against the other's writes, and the directories
//! survived the run besides. The leaf is unique per process now, and the
//! `ScratchDir` guard removes it on drop, which is the same shape
//! `mogwai_lab::storage::unit_test_scratch` settled on for the lab's own unit
//! tests - a second test under one `name` is unrepresentable rather than
//! merely unlikely.

#![allow(dead_code, reason = "each test target uses whichever half it needs")]

use std::path::PathBuf;

use mogwai_lab::storage::ScratchDir;

/// The workspace root, from this crate's manifest directory.
pub(crate) fn repo_root() -> PathBuf {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    dir.pop();
    dir.pop();
    dir
}

/// A fresh, empty, PROCESS-PRIVATE scratch directory under
/// `target/cli-unit-scratch/<name>/`, removed when the returned guard drops.
///
/// The guard is the return value on purpose: `scratch_dir("x").path()
/// .to_path_buf()` deletes the directory before the caller's first write, which
/// is the guard-scope defect in the one line that looks tidiest. Bind it.
pub(crate) fn scratch_dir(name: &str) -> ScratchDir {
    let base = repo_root()
        .join("target")
        .join("cli-unit-scratch")
        .join(name);
    ScratchDir::new(&base).expect("creating the test scratch directory")
}
