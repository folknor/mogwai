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

/// Walk `dir` for markdown, REFUSING rather than skipping wherever it cannot
/// see. An unreadable directory is a failure on purpose: this gate's whole
/// claim is that it read every markdown file in the repository, and a walk that
/// quietly steps over a directory it could not open has stopped making that
/// claim while still reporting green.
///
/// Symlinks are NOT followed. `entry.file_type()` reports the link itself,
/// where `path.is_dir()` reports its target, so a symlink pointing at an
/// ancestor used to be descended into. MEASURED RATHER THAN ASSUMED, because
/// the obvious statement of this hazard is wrong: it does NOT recurse forever.
/// Linux caps symlink resolution per pathname at 40, so `is_dir` starts
/// returning false there and the walk terminates on its own. What it actually
/// cost was a 40-fold walk - 0.27 s to 1.02 s with one `notes/x -> ..` link on
/// this tree - every markdown file counted up to forty times, and a `read_dir`
/// on the ELOOP boundary able to fail the gate with a symlink error where a
/// stale-prose verdict belongs.
///
/// A symlinked `.md` is likewise left alone: its target is either inside the
/// repository and already walked, or outside it and not this repository's
/// prose.
fn markdown_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir).unwrap_or_else(|e| {
        panic!(
            "read {}: {e} - this gate certifies that it read the whole \
             repository, so an unreadable directory fails it rather than \
             being skipped",
            dir.display()
        )
    });
    for entry in entries {
        let entry = entry.expect("directory entry");
        let file_type = entry
            .file_type()
            .unwrap_or_else(|e| panic!("file type of {}: {e}", entry.path().display()));
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if file_type.is_dir() {
            if !SKIPPED_DIRS.contains(&name.as_ref()) {
                markdown_files(&path, out);
            }
        } else if file_type.is_file() && name.ends_with(".md") {
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

/// Bytes of context carried either side of a match, for the human reading a
/// failure. It bounds the QUOTE only - never what the gate matches.
const CONTEXT_BYTES: usize = 40;

/// Largest char boundary at or below `at`. `CONTEXT_BYTES` is a byte count and
/// prose is UTF-8, so a window edge lands mid-character as soon as any
/// non-ASCII byte sits within that distance of a claim - and slicing there
/// panics with "byte index is not a char boundary", a message that looks
/// nothing like the stale-prose failure this gate exists to report. The
/// repository's no-gremlins rule makes it unlikely rather than impossible: a
/// name, a currency symbol or a pasted quote in `notes/`, which is deliberately
/// in scope, all suffice.
fn floor_boundary(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// Smallest char boundary at or above `at`, clamped to the end of `text`.
fn ceil_boundary(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at += 1;
    }
    at
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
            // The match itself is byte-exact and unaffected; only the quoted
            // window is snapped outward to the nearest char boundaries.
            let end = ceil_boundary(rest, at + pattern.len() + digits.len() + CONTEXT_BYTES);
            let start = floor_boundary(rest, at.saturating_sub(CONTEXT_BYTES));
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

/// The multi-byte character is BUILT rather than written, because this
/// repository forbids one in its own source and prose - which is also why the
/// case cannot be pinned by planting a file in the tree. A synthetic haystack
/// is the only place the hazard can be represented at all.
///
/// Placement is exact rather than incidental: the character straddles the
/// window edge on each side, which is the only arrangement that panics. A
/// non-ASCII character merely NEAR a claim is harmless, so a test that puts one
/// somewhere in the neighbourhood and passes proves nothing.
#[test]
fn a_claim_flanked_by_multi_byte_characters_is_quoted_rather_than_panicking() {
    const PATTERN: &str = "`TAPE_PROTOCOL_VERSION` is ";
    // U+20AC EURO SIGN, three bytes in UTF-8.
    let wide = char::from_u32(0x20AC)
        .expect("a valid scalar value")
        .to_string();
    assert_eq!(wide.len(), 3, "the case needs a multi-byte character");

    // Leading: the character occupies bytes 0..3 and the claim starts at byte
    // 41, so the lookback edge at 41 - 40 = 1 lands inside it.
    let lead = format!("{wide}{}", "x".repeat(38));
    // Trailing: 39 ASCII bytes follow the digits, so the forward edge at 40
    // lands inside the character that begins at byte 39.
    let trail = format!("{}{wide}", "x".repeat(39));
    let text = format!("{lead}{PATTERN}21{trail}");
    assert_eq!(
        lead.len(),
        41,
        "the lookback edge must fall inside the character"
    );
    assert!(
        !text.is_char_boundary(1) && !text.is_char_boundary(lead.len() + PATTERN.len() + 2 + 40),
        "both window edges must land mid-character, or this pins nothing"
    );

    let found = claims(&text, PATTERN);
    assert_eq!(
        found.len(),
        1,
        "exactly the one claim must be found, and no more: the repair snaps the \
         QUOTED window to char boundaries and must not touch what is matched"
    );
    assert_eq!(found[0].0, 21, "the claimed value must survive the quoting");
    assert!(
        found[0].1.contains(PATTERN),
        "the quoted context must still show the claim it is quoting, got {:?}",
        found[0].1
    );
}

/// The walk is exercised against a SCRATCH TREE, never against the repository:
/// planting a symlink cycle in the tree the suite is being judged against is
/// the defect `gate_skip_list::no_test_binary_writes_a_committed_fixture`
/// forbids, and the walk takes a path precisely so it can be driven somewhere
/// else. The name carries the pid, so two sweeps of this binary running at once
/// cannot collide.
#[test]
#[cfg(unix)]
fn a_symlink_cycle_is_not_descended_into() {
    use std::os::unix::fs::symlink;

    let scratch = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("prose-symlink-cycle-{}", std::process::id()));
    // A leftover from an aborted earlier run, if any. Absent is the normal
    // case, so the error is discarded rather than reported. This prologue,
    // rather than a scope guard, is deliberately the cleanup story: the removal
    // below runs after the walk, so a PANIC INSIDE `markdown_files` - its
    // `read_dir` arm, reachable here on the ELOOP boundary if the fix ever
    // regresses - leaves the tree behind. It is bounded, it lives under
    // `CARGO_TARGET_TMPDIR` rather than in the repository the suite judges, and
    // it is inert - a directory nothing reads. `cargo clean` and a recurring
    // pid are what collect it; that is the accepted cost of not wrapping the
    // walk in `catch_unwind` for a directory in the build tree.
    drop(fs::remove_dir_all(&scratch));
    let inner = scratch.join("inner");
    fs::create_dir_all(&inner).expect("create the scratch tree");
    fs::write(scratch.join("one.md"), "no claim here").expect("write a markdown file");
    fs::write(inner.join("two.md"), "nor here").expect("write a nested markdown file");
    // The cycle: `inner/up` resolves to the scratch root that contains it.
    symlink("..", inner.join("up")).expect("plant the cycle");

    let mut found = Vec::new();
    markdown_files(&scratch, &mut found);
    fs::remove_dir_all(&scratch).expect("remove the scratch tree");

    // TWO files, each ONCE. The count is the assertion rather than mere
    // termination, because following the link terminates too - Linux caps
    // symlink resolution at 40 levels - and reports the same two files forty
    // times over. A test asserting only that the walk returned would pass on
    // the defect.
    //
    // The failure quotes ONE deepest path rather than the whole vector: the
    // defect produces roughly eighty entries whose names are the cycle repeated
    // forty times, which is twenty kilobytes of panic that says nothing the
    // count and one sample do not.
    let deepest = found
        .iter()
        .max_by_key(|p| p.components().count())
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    assert_eq!(
        found.len(),
        2,
        "a symlinked directory must not be descended into: expected the two \
         real files once each, got {} entries, deepest {deepest}",
        found.len()
    );
}
