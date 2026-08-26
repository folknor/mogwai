// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The serving path may not reach the session-segment composer.
//!
//! `SegmentSource` composes a river by sampling real session slices, and both
//! its running level and its sampling draw are path-dependent, so no segment
//! can be skipped without composing a different river. It therefore has no
//! checkpoint chain and `seek_to` is linear in ticks from wherever the source
//! stands: placing a boat, or a named window, far from the composed origin
//! would walk every tick in between while holding whatever lock the placement
//! took. The generated serving source does not have that shape, because its
//! river owns a `CheckpointIndex` and resumes from the snapshot before the
//! target.
//!
//! Today the venue cannot reach the composer at all: `CheckpointIndex` holds a
//! `GeneratedSource` by type, and the composer's only constructor outside its
//! own tests is the offline `mogwai segments compose` subcommand. That is a
//! fact about which code exists rather than a rule anything enforces, and it is
//! the kind of fact that changes silently - a `CheckpointIndex` generalized to
//! a boxed `TickSource`, or a river factory taught to select a composed source,
//! would remove the type-level guarantee without removing anything that reads
//! as a guarantee. This test is the rule: the day a serving path names the
//! composer, the build fails here by name instead of a placement hanging in
//! production.
//!
//! Lifting it is legitimate, and what it costs is stated rather than implied:
//! give the composer a checkpoint chain of its own, or bound the placement, and
//! then delete this file in the same change.

use std::fs;
use std::path::{Path, PathBuf};

/// The spellings by which venue code could name the composer: the type itself,
/// and the module it lives in, which covers an aliased or re-exported import.
const FORBIDDEN: [&str; 2] = ["SegmentSource", "mogwai_data::segment"];

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("the venue's own source tree is readable") {
        let path = entry.expect("a readable directory entry").path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn the_serving_crate_never_names_the_session_segment_composer() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_sources(&src, &mut files);
    assert!(
        !files.is_empty(),
        "found no Rust sources under {} - the guard would pass vacuously",
        src.display()
    );

    let mut offenders = Vec::new();
    for file in &files {
        let text = fs::read_to_string(file).expect("a readable Rust source");
        for (i, line) in text.lines().enumerate() {
            for needle in FORBIDDEN {
                if line.contains(needle) {
                    offenders.push(format!("{}:{}: {needle}", file.display(), i + 1));
                }
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "mogwai-venue names the session-segment composer, which has no \
         checkpoint chain: a placement far from the composed origin walks every \
         tick to get there, holding the placement's lock. Give the composer a \
         checkpoint chain, or bound the placement, before wiring it into the \
         serving path - and delete this guard in the same change. Sites:\n{}",
        offenders.join("\n")
    );
}
