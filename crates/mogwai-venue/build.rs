// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Stamp the binary's version string into a compile-time env var that
//! `main.rs` reads for `--version`.
//!
//! Composes semver (from `CARGO_PKG_VERSION`) with the short git hash and the
//! UTC commit time into `MOGWAI_LONG_VERSION`, e.g.
//! `0.1.0 (abc123def 2026-06-24 12:34:56 UTC)`. Kept dependency-light: shells
//! `git` directly rather than pulling crates, and falls back to `unknown`
//! outside a checkout (e.g. a `cargo install` tarball with no `.git`) so a
//! release never fails to build for lack of git metadata.
//!
//! Every stamped value is a function of the tree, never of the moment the
//! script ran, so two builds of one tree state print one version string.
//! A rerun still recompiles this crate and its dependents whatever it emits,
//! which is why the watched set is kept to the files that move the hash or
//! the dirty flag.

use std::process::Command;

/// Run `cmd args`, returning trimmed stdout, or `None` on failure / empty
/// output (so a missing `git` or a non-repo build degrades cleanly).
/// `TZ=UTC` makes `--date=format-local` render in UTC whatever the host zone.
fn capture(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd)
        .env("TZ", "UTC")
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if s.is_empty() { None } else { Some(s) }
}

fn main() {
    let hash =
        capture("git", &["rev-parse", "--short=9", "HEAD"]).unwrap_or_else(|| "unknown".to_owned());

    // A non-empty porcelain status means the tree carried uncommitted changes at
    // build time - flag it so a `-dirty` build is never mistaken for one off a
    // clean commit. `git status` is repo-wide regardless of this crate subdir.
    let hash = if capture("git", &["status", "--porcelain"]).is_some() {
        format!("{hash}-dirty")
    } else {
        hash
    };

    let commit_time = capture(
        "git",
        &[
            "log",
            "-1",
            "--date=format-local:%Y-%m-%d %H:%M:%S UTC",
            "--format=%cd",
        ],
    )
    .unwrap_or_else(|| "unknown".to_owned());

    let semver = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "unknown".to_owned());

    println!("cargo:rustc-env=MOGWAI_LONG_VERSION={semver} ({hash} {commit_time})");

    // Re-run when the checked-out commit or the index moves so the hash and the
    // dirty flag stay honest. The workspace `.git` is two levels up from this
    // crate. `.git/HEAD` alone is not enough: on a branch it holds
    // `ref: refs/heads/<name>` and only changes on a checkout, while a commit
    // moves the ref file it names, or `packed-refs` once refs are packed.
    watch("../../.git/HEAD");
    watch("../../.git/index");
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
