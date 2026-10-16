// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Stamp the binary's version string into a compile-time env var that
//! `main.rs` reads for `--version`.
//!
//! Composes semver (from `CARGO_PKG_VERSION`) with the short git hash and the
//! UTC commit time into `MOGWAI_LONG_VERSION`, e.g.
//! `0.1.0 (abc123def 2026-06-24 12:34:56 UTC)`. The hash carries `-dirty`
//! when the repository had any uncommitted change, untracked files included.
//!
//! Every value arrives ready-made in `MOGWAI_SOURCE_IDENTITY`, which the
//! workspace's build runner computes with `scripts/source_identity.py` before
//! every cargo invocation, and the script reruns only when that value changes.
//! This workspace is only ever built through that runner, so there is no git
//! fallback: a build without the variable stamps `unknown`.
//!
//! Every stamped value is a function of the tree, never of the moment the
//! script ran, so two builds of one tree state print one version string.

const IDENTITY_VAR: &str = "MOGWAI_SOURCE_IDENTITY";

/// The short hash (with `-dirty`) and commit time from the identity line,
/// `<sha> <clean|dirty> <commit time>`. Anything else, including the script's
/// own `unknown`, reads as `None` and stamps `unknown`.
fn from_identity(identity: &str) -> Option<(String, String)> {
    let (sha, rest) = identity.split_once(' ')?;
    let (flag, commit_time) = rest.split_once(' ')?;
    let short = sha.get(..9)?;
    let hash = match flag {
        "clean" => short.to_owned(),
        "dirty" => format!("{short}-dirty"),
        _ => return None,
    };
    Some((hash, commit_time.to_owned()))
}

fn main() {
    println!("cargo:rerun-if-env-changed={IDENTITY_VAR}");

    let (hash, commit_time) = std::env::var(IDENTITY_VAR)
        .ok()
        .and_then(|identity| from_identity(&identity))
        .unwrap_or_else(|| ("unknown".to_owned(), "unknown".to_owned()));

    let semver = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "unknown".to_owned());

    println!("cargo:rustc-env=MOGWAI_LONG_VERSION={semver} ({hash} {commit_time})");
}
