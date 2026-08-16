//! Gate on the hand-maintained tape-version prose.
//!
//! The artifact binding blocks cannot go stale - `stage_a_batch.rs` refuses a
//! manifest whose version is not the live constant, and the screen artifact's
//! test asserts the binding equals it. The PROSE had no such gate, and three
//! separate bumps in 2026-08 each left durable statements naming a superseded
//! identity; one of them left `reference/architecture.md` claiming version 11
//! across two prior bumps, so the durable architecture reference was wrong
//! about tape identity and nobody caught it.
//!
//! English cannot be parsed for this, and most mentions of the constant are
//! deliberately HISTORICAL - a frozen spec recording which identity a past
//! landing consumed must not be rewritten when the constant moves. So the gate
//! keys on two CLAIM FORMS that assert a live fact, and any prose that means to
//! assert one writes it in that form:
//!
//! - ``TAPE_PROTOCOL_VERSION` is N`` - N is the live identity right now.
//! - ``TAPE_PROTOCOL_VERSION` next takes N`` - N is the next unspent identity,
//!   so it must be the live one plus one.
//!
//! Every other phrasing - "stays 11", "= 15 (AMENDED ...)", "13 went to the
//! fill-band decimal normalization" - is a record of what was true at a past
//! landing and is left alone.
//!
//! The document SET is discovered by walking the repository, never listed here.
//! The third occurrence of this defect was in `notes/`, read by a different
//! workstream and invisible to any gate that only checked the durable folders.

use std::fs;
use std::path::{Path, PathBuf};

use mogwai_data::TAPE_PROTOCOL_VERSION;

/// Directories that are not this repository's prose: build output, git's own
/// store, and the read-only vendored upstream copies under `research/`.
const SKIPPED_DIRS: [&str; 3] = ["target", ".git", "research"];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the crate sits two levels below the repository root")
        .to_path_buf()
}

fn markdown_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
    for entry in entries {
        let entry = entry.expect("directory entry");
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if !SKIPPED_DIRS.contains(&name.as_ref()) {
                markdown_files(&path, out);
            }
        } else if name.ends_with(".md") {
            out.push(path);
        }
    }
}

/// Collapse every whitespace run to a single space. The prose is hard-wrapped
/// at ~72 columns, so a claim routinely straddles a line break and a
/// line-oriented match would miss exactly the statements the gate exists for.
fn flatten(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Every number following `pattern` in `haystack`, paired with enough
/// surrounding text to locate the claim in the file by eye.
fn claims(haystack: &str, pattern: &str) -> Vec<(u32, String)> {
    let mut found = Vec::new();
    let mut rest = haystack;
    while let Some(at) = rest.find(pattern) {
        let tail = &rest[at + pattern.len()..];
        let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
        if let Ok(value) = digits.parse::<u32>() {
            let end = (at + pattern.len() + digits.len() + 40).min(rest.len());
            let start = at.saturating_sub(40);
            found.push((value, rest[start..end].to_string()));
        }
        rest = &rest[at + pattern.len()..];
    }
    found
}

#[test]
fn durable_prose_names_the_live_tape_version() {
    let root = repo_root();
    let mut files = Vec::new();
    markdown_files(&root, &mut files);
    files.sort();
    assert!(
        files.len() > 5,
        "the walk found only {} markdown files under {} - the gate is not \
         reading the repository it thinks it is",
        files.len(),
        root.display()
    );

    let mut wrong = Vec::new();
    let mut live_claims = 0usize;
    for path in &files {
        let text = flatten(&fs::read_to_string(path).expect("read markdown"));
        let shown = path.strip_prefix(&root).unwrap_or(path).display();

        for (value, context) in claims(&text, "`TAPE_PROTOCOL_VERSION` is ") {
            live_claims += 1;
            if value != TAPE_PROTOCOL_VERSION {
                wrong.push(format!(
                    "{shown}: claims the live identity is {value}, but it is \
                     {TAPE_PROTOCOL_VERSION} - ...{context}..."
                ));
            }
        }
        for (value, context) in claims(&text, "`TAPE_PROTOCOL_VERSION` next takes ") {
            live_claims += 1;
            if value != TAPE_PROTOCOL_VERSION + 1 {
                wrong.push(format!(
                    "{shown}: claims the next identity is {value}, but the live \
                     one is {TAPE_PROTOCOL_VERSION} so the next is {} - ...{context}...",
                    TAPE_PROTOCOL_VERSION + 1
                ));
            }
        }
    }

    assert!(
        wrong.is_empty(),
        "tape-version prose is stale:\n{}",
        wrong.join("\n")
    );

    // A gate that matches nothing passes forever. Both claim forms are in the
    // tree today (`reference/architecture.md` and `AGENTS.md`); if a rewrite
    // drops the last one, the gate has silently stopped gating.
    assert!(
        live_claims >= 2,
        "no live tape-version claim found in any markdown file - either the \
         claim forms were rewritten out of the prose or the gate's patterns no \
         longer match them"
    );
}
