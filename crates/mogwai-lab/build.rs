// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Folds the git sha of the tree this crate was built from into the
//! provenance token (storage policy, the retired rewrite plan, phase 1).
//! Absent a sha, the token falls back to the crate-version +
//! `TAPE_PROTOCOL_VERSION` + fingerprint-hash + command components alone,
//! which is still a real invalidation key - just weaker.
//!
//! The stamp is the 12-character short `HEAD` sha and nothing else: no dirty
//! flag, so a dirty tree shares its `HEAD`'s token. It is empty, not
//! `unknown`, when no sha can be read.
//!
//! The sha arrives in `MOGWAI_SOURCE_IDENTITY`, which the workspace's build
//! runner computes with `scripts/source_identity.py` before every cargo
//! invocation; its first field is the full `HEAD` sha, and the script reruns
//! only when that value changes. This workspace is only ever built through
//! that runner, so there is no git fallback. A rerun also
//! follows a dirty-flag or commit-time change the sha ignores; the stamp comes
//! out identical, but the crate still recompiles. The flag flips on the first
//! edit after a commit, not on every edit.

const IDENTITY_VAR: &str = "MOGWAI_SOURCE_IDENTITY";

fn main() {
    println!("cargo:rerun-if-env-changed={IDENTITY_VAR}");

    // `<sha> <clean|dirty> <commit time>`. The script's own `unknown` is not a
    // sha and reads as empty, as an unset variable does.
    let sha = std::env::var(IDENTITY_VAR)
        .ok()
        .and_then(|identity| {
            identity
                .split_once(' ')
                .and_then(|(sha, _)| sha.get(..12))
                .map(str::to_owned)
        })
        .unwrap_or_default();
    println!("cargo:rustc-env=MOGWAI_LAB_GIT_SHA={sha}");
}
