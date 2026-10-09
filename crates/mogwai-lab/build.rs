// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Folds the git sha of the tree this crate was built from into the
//! provenance token (storage policy, the retired rewrite plan, phase 1).
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
        .map_or_default(|s| s.trim().to_string());
    println!("cargo:rustc-env=MOGWAI_LAB_GIT_SHA={sha}");
    // Re-run only when the commit moves, not on every touch of the tree - a
    // build script that reruns per file edit would make every `cargo check`
    // shell out to git. `.git/HEAD` alone is not enough: on a branch it holds
    // `ref: refs/heads/<name>` and only changes on a checkout, while a commit
    // moves the ref file it names, or `packed-refs` once refs are packed.
    // Watching HEAD alone left the sha stale after every commit.
    watch("../../.git/HEAD");
    watch("../../.git/packed-refs");
    if let Some(r) = std::fs::read_to_string("../../.git/HEAD")
        .ok()
        .and_then(|h| h.strip_prefix("ref: ").map(|r| r.trim().to_owned()))
    {
        watch(&format!("../../.git/{r}"));
    }
}

/// Watch `path` only if it exists. Cargo treats a missing rerun-if-changed
/// path as always stale, so watching an absent `packed-refs` reran this script
/// and rebuilt every dependent on every invocation. Nothing is lost: packing
/// refs deletes the loose ref file being watched, and a deletion triggers.
fn watch(path: &str) {
    if std::path::Path::new(path).exists() {
        println!("cargo:rerun-if-changed={path}");
    }
}
