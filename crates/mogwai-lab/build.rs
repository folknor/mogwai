// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Folds the git sha of the tree this crate was built from into the
//! provenance token (storage policy, notes/rust-rewrite-phases.md phase 1).
//! A `cargo install`/crates.io build has no `.git` directory, so this is
//! best-effort: absent git, the token falls back to the crate-version +
//! `TAPE_PROTOCOL_VERSION` + fingerprint-hash + command components alone,
//! which is still a real invalidation key - just weaker than a repo dev's,
//! matching phase 1's stated tradeoff ("repo dev keeps current invalidation
//! strength").

use std::process::Command;

fn main() {
    let sha = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    println!("cargo:rustc-env=MOGWAI_LAB_GIT_SHA={sha}");
    // Re-run only when HEAD moves, not on every touch of the tree - a build
    // script that reruns per file edit would make every `cargo check`
    // shell out to git.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
}
