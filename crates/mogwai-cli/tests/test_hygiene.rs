// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Two things a running test may never do to the tree, scanned out of source.
//!
//! Neither is decidable at runtime, which is why they are source scans rather
//! than assertions. A test that rewrites a committed pin has already rewritten
//! it by the time anything could notice, and the tree it rewrote is the tree the
//! next run reads. A test that declines to assert because its input is missing
//! reports the same green as one that checked.
//!
//! THIS FILE USED TO CARRY THREE MORE CHECKS, all of them about the build tool's
//! skip and only filters, and all of them now redundant. A skip entry catching a
//! live test is an ORPHANED PAIR in the coverage audit, resolved against
//! libtest's own enumeration; a filter matching no test at all is a DEAD FILTER,
//! reported with the config block that declared it. Both are the tool's job and
//! it does them without reconstructing test names from source text - which this
//! file did, with a hand-written parser that had already gone blind once, to
//! macro-generated tests. A parser-backed scanner that fails open is worse than
//! no scanner, because it reports green.
//!
//! The third was a self-check policing a lint exemption this file no longer
//! needs, since nothing here reads the build tool's config any more.
//!
//! WHAT THE REMAINING SCANS READ is the source text of `crates/*/src/**` and
//! `crates/*/tests/**`, plus examples and benches, stripped of comments in a
//! literal-aware way so a `//` inside a string cannot eat a closing brace and
//! shift every later span. An unterminated block comment or string panics naming
//! the file rather than guessing.

use std::path::{Path, PathBuf};

/// The crate's own manifest dir, walked up to the workspace root.
fn repo_root() -> PathBuf {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    dir.pop();
    dir.pop();
    dir
}

/// Every `.rs` file under `crates/*/src` and `crates/*/tests`.
fn source_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let crates = root.join("crates");
    let mut packages: Vec<PathBuf> = std::fs::read_dir(&crates)
        .expect("the crates directory")
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    packages.sort();
    for package in packages {
        walk(&package.join("src"), &mut files);
        walk(&package.join("tests"), &mut files);
    }
    files
}

/// Every `.rs` file under `crates/*/examples` and `crates/*/benches`.
///
/// KEPT OUT OF [`source_files`] DELIBERATELY. An example target is the
/// SANCTIONED home for a fixture generator - `no_test_binary_writes_a_committed
/// _fixture` names it as the fix - so folding these into the shared list would
/// make that gate refuse its own remedy. They carry no unit tests either, so
/// the name reconstruction has nothing to say about them. What they CAN carry
/// is a `#[test]` that declines to assert, which is a property of the code and
/// not of the target kind, so the missing-input gate reads them too.
fn example_and_bench_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let crates = root.join("crates");
    let mut packages: Vec<PathBuf> = std::fs::read_dir(&crates)
        .expect("the crates directory")
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    packages.sort();
    for package in packages {
        for kind in ["examples", "benches"] {
            let dir = package.join(kind);
            walk(&dir, &mut files);
        }
    }
    files
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            walk(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

fn strip_comments(path: &Path, text: &str) -> String {
    strip(path, text, true)
}

/// The same stripper, KEEPING literal contents.
///
/// The name-reconstructing scan wants literals blanked, because every counter it
/// runs reads punctuation. The fixture-write scan below wants the opposite: the
/// path it looks for lives INSIDE a string, and blanking it would make that
/// check pass for free - a scanner that cannot see the thing it forbids. Comments
/// are stripped either way, so prose discussing a fixture path cannot trip it.
fn strip_comments_keeping_literals(path: &Path, text: &str) -> String {
    strip(path, text, false)
}

fn strip(path: &Path, text: &str, blank_literals: bool) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut at = 0usize;
    while at < bytes.len() {
        let ch = bytes[at];
        // A line comment: blank to the newline, which is kept.
        if ch == b'/' && bytes.get(at + 1) == Some(&b'/') {
            while at < bytes.len() && bytes[at] != b'\n' {
                at += 1;
            }
            continue;
        }
        // A block comment, nesting, newlines preserved so line numbers hold.
        if ch == b'/' && bytes.get(at + 1) == Some(&b'*') {
            let mut depth = 0usize;
            while at < bytes.len() {
                if bytes[at] == b'/' && bytes.get(at + 1) == Some(&b'*') {
                    depth += 1;
                    at += 2;
                } else if bytes[at] == b'*' && bytes.get(at + 1) == Some(&b'/') {
                    depth -= 1;
                    at += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    if bytes[at] == b'\n' {
                        out.push('\n');
                    }
                    at += 1;
                }
            }
            assert!(
                depth == 0,
                "an unterminated block comment in {}; this scan cannot read the file and will not \
                 report a partial view of it as a complete one",
                path.display()
            );
            continue;
        }
        if let Some(end) = raw_string_end(bytes, at) {
            emit_literal(&mut out, text, at, end, blank_literals);
            at = end;
            continue;
        }
        if ch == b'"' {
            let end = quoted_end(bytes, at, b'"').unwrap_or_else(|| {
                panic!(
                    "an unterminated string literal in {}, at byte {at}",
                    path.display()
                )
            });
            emit_literal(&mut out, text, at, end, blank_literals);
            at = end;
            continue;
        }
        if ch == b'\'' && is_char_literal(bytes, at) {
            let end = quoted_end(bytes, at, b'\'').unwrap_or_else(|| {
                panic!(
                    "an unterminated character literal in {}, at byte {at}",
                    path.display()
                )
            });
            emit_literal(&mut out, text, at, end, blank_literals);
            at = end;
            continue;
        }
        out.push(text[at..].chars().next().expect("a char boundary"));
        at += text[at..]
            .chars()
            .next()
            .expect("a char boundary")
            .len_utf8();
    }
    out
}

/// Blanks a literal's span, keeping newlines so line numbers hold.
///
/// THE CONTENTS ARE BLANKED, NOT COPIED, because every counter downstream reads
/// punctuation: a `{` or a `[` inside a string message would otherwise unbalance
/// the module-depth walk or an attribute's bracket count. Nothing this file does
/// needs the text of a literal - the names it reconstructs are all identifiers.
fn blank(out: &mut String, text: &str, from: usize, to: usize) {
    for ch in text[from..to].chars() {
        out.push(if ch == '\n' { '\n' } else { ' ' });
    }
}

/// A literal's span, blanked or kept verbatim.
fn emit_literal(out: &mut String, text: &str, from: usize, to: usize, blank_it: bool) {
    if blank_it {
        blank(out, text, from, to);
    } else {
        out.push_str(&text[from..to]);
    }
}

/// The end of `r"..."`, `r#"..."#`, `b"..."` or `br#"..."#` starting at `at`,
/// or `None` if no raw string starts there.
fn raw_string_end(bytes: &[u8], at: usize) -> Option<usize> {
    // A leading `r` is only a raw-string prefix at a token boundary; inside
    // `for` or an identifier it is just a letter.
    if at > 0 && (bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_') {
        return None;
    }
    let mut cursor = at;
    if bytes.get(cursor) == Some(&b'b') {
        cursor += 1;
    }
    if bytes.get(cursor) != Some(&b'r') {
        return None;
    }
    cursor += 1;
    let hash_start = cursor;
    while bytes.get(cursor) == Some(&b'#') {
        cursor += 1;
    }
    let hashes = cursor - hash_start;
    if bytes.get(cursor) != Some(&b'"') {
        return None;
    }
    cursor += 1;
    let terminator: Vec<u8> = std::iter::once(b'"')
        .chain(std::iter::repeat_n(b'#', hashes))
        .collect();
    while cursor < bytes.len() {
        if bytes[cursor..].starts_with(&terminator) {
            return Some(cursor + terminator.len());
        }
        cursor += 1;
    }
    None
}

/// The byte after the closing `quote` of the literal opening at `at`, honouring
/// backslash escapes.
fn quoted_end(bytes: &[u8], at: usize, quote: u8) -> Option<usize> {
    let mut cursor = at + 1;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'\\' => cursor += 2,
            byte if byte == quote => return Some(cursor + 1),
            _ => cursor += 1,
        }
    }
    None
}

/// Whether the `'` at `at` opens a character literal rather than a lifetime.
/// `'a'` and `'\n'` are literals; `'static` and `'a,` are not, and treating one
/// as a literal would swallow everything to the next apostrophe.
fn is_char_literal(bytes: &[u8], at: usize) -> bool {
    match bytes.get(at + 1) {
        Some(b'\\') => true,
        Some(&first) => {
            // One character - of any UTF-8 width - and then the closing quote.
            let width = match first {
                byte if byte < 0x80 => 1,
                byte if byte >> 5 == 0b110 => 2,
                byte if byte >> 4 == 0b1110 => 3,
                _ => 4,
            };
            bytes.get(at + 1 + width) == Some(&b'\'')
        }
        None => false,
    }
}

/// Strips the visibility and qualifier keywords that may precede `fn` or `mod`.
/// THE COMPANION INVARIANT, and the one that is genuinely SILENT: no code the
/// test binaries compile may write a committed fixture.
///
/// WHY THIS IS THE CHECK AND "EVERY IGNORED TEST OWES A SKIP ENTRY" IS NOT. The
/// gate sets `include_ignored` ON PURPOSE, so an `#[ignore]`d test with no skip
/// entry RUNS - and for most of them that is the intent, not an omission. The
/// socket-backed adapter binaries are the whole reason the flag is on; the two
/// `/trades` sizing instruments in `mogwai-server`'s `http` module are ignored,
/// unskipped and finish in milliseconds. A check demanding an entry for every
/// ignored test would refuse both families and would have to grow an exception
/// list, which is the shape this arc keeps paying for. And where a missing entry
/// IS a defect - a walk past the 20-second watchdog, a corpus no clone carries -
/// the gate says so LOUDLY, red, naming the test. That direction needs no
/// scanner.
///
/// What the gate cannot see is a test that runs, passes, and REWRITES A
/// COMMITTED PIN on its way through, because the tree it rewrites is the tree
/// the next run reads. That is not hypothetical: it was
/// `regenerate_arrival_transcripts_amendment_only`, an `#[ignore]`d test in
/// `mogwai-data` that wrote `tests/fixtures/arrival-transcript-shot_noise.json`,
/// the file `arrival_transcripts_replay_bit_exact` pins through `include_str!`.
/// A kernel change would have failed the pin and rewritten the fixture in the
/// SAME run; the re-run would have read the new fixture and reported green. It
/// ran on every full gate for as long as it existed and cost nothing only
/// because its output happened not to have moved yet. It is an example target
/// now - compiled by every lane, run by none.
///
/// THE LINE IS `tests/fixtures/`, and it is drawn by what the directory MEANS
/// rather than around any one file. A fixture is a committed input: the suite
/// reads it and a deliberate tool produces it. A GOLDEN (`tests/golden/`) is a
/// different contract - `mogwai-server`'s `fill_distribution_matches_the_golden`
/// writes one, but only when the file is ABSENT, and it panics after writing so
/// the run can never be green on a fresh bless. That shape is safe and stays
/// legal here. If a golden ever grows an unguarded writer, the rule it needs is
/// its own, not a widening of this one.
///
/// THE SCANNER IS EXEMPT FROM ITSELF, and structurally so rather than by
/// convenience: a scanner has to name every construct it forbids and has to
/// name the directory it protects, so its own source matches its own rule by
/// construction, and so does the fixture block that proves it bites. The
/// exemption is one file, matched by name, and it is the reason the pure
/// `fixture_write_offenders` below is tested on synthetic samples in
/// `mod parser` instead of resting on what the tree happens to contain.
#[test]
fn no_test_binary_writes_a_committed_fixture() {
    let root = repo_root();
    let mut offenders = Vec::new();
    let files = source_files(&root);
    // A SCAN THAT READS NOTHING FORBIDS NOTHING, and it reports green while
    // doing it. Same floor the name scan carries, for the same reason.
    assert!(
        files.len() > 100,
        "this scan found only {} source files under crates/, which is too few for this workspace - \
         it is reading the wrong tree, and a scan that sees nothing accepts every writer there is",
        files.len()
    );
    // One path, not one file NAME - see the note in the missing-input gate.
    let this_file = root.join(file!());
    for path in files {
        if path == this_file {
            continue;
        }
        let raw = std::fs::read_to_string(&path).expect("a scanned source file reads");
        // Comments stripped so the paragraph above - which necessarily names the
        // path - cannot trip a check that is about CODE; literals KEPT, because
        // the path being looked for only ever appears inside one.
        let code = strip_comments_keeping_literals(&path, &raw);
        let shown = path.strip_prefix(&root).unwrap_or(&path).display();
        offenders.extend(fixture_write_offenders(&format!("{shown}"), &code));
    }

    assert!(
        offenders.is_empty(),
        "code compiled into a test binary writes a committed fixture:\n{}\n\nA fixture is a \
         committed INPUT - the suite reads it, a deliberate tool produces it. A writer living in \
         the suite re-blesses the pin it is supposed to be held against, and it does so on the \
         very run where the pin failed, which is the one run that must not rewrite anything. \
         `#[ignore]` does not keep it out: the gate profile sets `include_ignored` deliberately. \
         Move the generator to an example target or a CLI subcommand, the way \
         `mogwai-data/examples/regenerate_arrival_transcript.rs` was moved.",
        offenders.join("\n")
    );
}

/// Every construct in `code` that could put bytes on disk with a fixture path
/// in the same scope, reported as one line each.
///
/// SCOPE IS THE ENCLOSING FUNCTION, not a byte window. The first cut of this
/// check searched a fixed 1200 bytes BEFORE the construct, which was wrong in
/// both directions at once: a path built after the write (or through a helper
/// whose definition sits further up) was invisible, and an `include_str!` block
/// a few hundred bytes above an unrelated `File::create` was a false positive
/// pointing at the wrong line. A function body is the unit the defect actually
/// lives in, so that is the unit searched, and there is no tunable left.
///
/// A path declared OUTSIDE every function - the idiomatic
/// `const FIXTURE: &str = ...` at module top - puts the file itself in scope,
/// because such a constant is reachable from every function in it.
///
/// WHAT THIS DOES NOT CATCH, stated rather than implied. The list below is
/// literal constructs; a `write!`/`writeln!` into a handle some helper opened
/// is not among them, because those two macros are overwhelmingly used to
/// format into a `String` and a rule flagging them would be an exception list
/// on day one. The list DOES cover the tokio spellings for free -
/// `tokio::fs::write(` contains `fs::write(`, `tokio::fs::File::create(`
/// contains `File::create(` - and it covers `Command::new(`, because a test
/// spawning the shipped binary with a fixture path on its argv is the one
/// non-`fs` way this workspace could write one.
fn fixture_write_offenders(shown: &str, code: &str) -> Vec<String> {
    const WRITES: [&str; 9] = [
        "fs::write(",
        "File::create(",
        "File::options(",
        "OpenOptions",
        "fs::copy(",
        "fs::rename(",
        "fs::remove_file(",
        "fs::remove_dir_all(",
        "Command::new(",
    ];
    const FIXTURES: &str = "tests/fixtures";

    let spans = fn_body_spans(code);
    // A fixture path that sits in no function body at all is in scope for the
    // whole file.
    let module_level = code
        .match_indices(FIXTURES)
        .any(|(at, _)| !spans.iter().any(|&(from, to)| from <= at && at < to));

    let mut offenders = Vec::new();
    for construct in WRITES {
        for (at, _) in code.match_indices(construct) {
            let in_scope = module_level
                || spans
                    .iter()
                    .filter(|&&(from, to)| from <= at && at < to)
                    .any(|&(from, to)| code[from..to].contains(FIXTURES));
            if in_scope {
                // `lines()` yields one FEWER than the construct's line whenever
                // the slice ends on the newline before it; counting the
                // newlines is unconditionally right.
                let line = code[..at].matches('\n').count() + 1;
                let where_ = if module_level {
                    "a module-level fixture path in scope"
                } else {
                    "a fixture path in the same function"
                };
                offenders.push(format!(
                    "  {shown}:{line} reaches {construct} with {where_}"
                ));
            }
        }
    }
    offenders.sort();
    offenders
}

/// The byte span of every `fn` body in comment-stripped, literal-KEEPING code.
///
/// Literals are kept, so the brace walk has to skip them itself or a lone `{`
/// inside a string unbalances it. Spans nest, and a closure body following an
/// `fn` POINTER type is recorded as if it were a function - both are harmless
/// here, because every enclosing span is consulted and a spurious inner span is
/// a subset of the real one.
fn fn_body_spans(code: &str) -> Vec<(usize, usize)> {
    let bytes = code.as_bytes();
    let mut spans = Vec::new();
    let mut open: Vec<(usize, bool)> = Vec::new();
    let mut pending_fn = false;
    let mut at = 0usize;
    while at < bytes.len() {
        if let Some(end) = raw_string_end(bytes, at) {
            at = end;
            continue;
        }
        if bytes[at] == b'"' {
            match quoted_end(bytes, at, b'"') {
                Some(end) => at = end,
                None => break,
            }
            continue;
        }
        if bytes[at] == b'\'' && is_char_literal(bytes, at) {
            match quoted_end(bytes, at, b'\'') {
                Some(end) => at = end,
                None => break,
            }
            continue;
        }
        match bytes[at] {
            b'{' => {
                open.push((at, pending_fn));
                pending_fn = false;
            }
            b'}' => {
                if let Some((from, was_fn)) = open.pop()
                    && was_fn
                {
                    spans.push((from, at + 1));
                }
            }
            b';' => pending_fn = false,
            b'f' if bytes[at..].starts_with(b"fn")
                && !at.checked_sub(1).is_some_and(|before| {
                    bytes[before].is_ascii_alphanumeric() || bytes[before] == b'_'
                })
                && !bytes
                    .get(at + 2)
                    .is_some_and(|next| next.is_ascii_alphanumeric() || *next == b'_') =>
            {
                pending_fn = true;
                at += 2;
                continue;
            }
            _ => {}
        }
        at += 1;
    }
    spans
}

/// THE SCANNERS' OWN GATES, on synthetic samples rather than on the tree.
///
/// Both scans above are exempt from themselves, because a scanner has to name
/// every construct it forbids and so its own source matches its own rule by
/// construction. Neither can therefore be proven to BITE by running it over the
/// repository. These fixtures are that proof, and each one is a shape that was
/// wrong at some point rather than an invented case.
mod parser {
    use super::{fixture_write_offenders, strip_comments, strip_comments_keeping_literals};
    use std::path::Path;

    fn offenders(text: &str) -> Vec<String> {
        let path = Path::new("fixture.rs");
        fixture_write_offenders("fixture.rs", &strip_comments_keeping_literals(path, text))
    }

    /// THE TWIN'S REASON FOR EXISTING, pinned so nobody collapses it back.
    ///
    /// `strip_comments` and `strip_comments_keeping_literals` differ by one
    /// bool, and merging them is the obvious simplification. It would also be
    /// silent: the fixture-write scan looks for a path that only ever appears
    /// INSIDE a string, so a blanking stripper makes that scan pass
    /// unconditionally forever, and the source-file floor stays green because it
    /// counts files rather than matches. This is the whole guard, in two lines.
    #[test]
    fn the_literal_keeping_stripper_keeps_literals_and_still_drops_comments() {
        let path = Path::new("fixture.rs");
        let kept = strip_comments_keeping_literals(path, "let x = \"tests/fixtures/a.json\";\n");
        assert!(
            kept.contains("tests/fixtures"),
            "a stripper that blanks literals blinds the fixture-write scan completely: {kept:?}"
        );
        let blanked = strip_comments(path, "let x = \"tests/fixtures/a.json\";\n");
        assert!(
            !blanked.contains("tests/fixtures"),
            "the name scan still wants literals gone: {blanked:?}"
        );
        let commented = strip_comments_keeping_literals(path, "// tests/fixtures/a.json\n");
        assert!(
            !commented.contains("tests/fixtures"),
            "prose about a fixture path must never trip a check about code: {commented:?}"
        );
    }

    /// THE SCAN BITES. Until this existed the only evidence was a manual text
    /// edit recorded in a note, which is evidence that expires.
    #[test]
    fn a_formatted_fixture_path_reaching_a_write_is_flagged() {
        let found = offenders(concat!(
            "mod inner {\n",
            "    #[test]\n",
            "    fn regenerate() {\n",
            "        let path = format!(\"{root}/crates/x/tests/fixtures/a.json\");\n",
            "        std::fs::write(&path, bytes).expect(\"written\");\n",
            "    }\n",
            "}\n",
        ));
        assert_eq!(
            found.len(),
            1,
            "the shape the rule was written for must be flagged exactly once: {found:?}"
        );
        assert!(found[0].contains("fixture.rs:5"), "{found:?}");
    }

    /// THE DIRECTION THE BYTE-WINDOW FIRST CUT COULD NOT SEE: a path declared
    /// once at module top, which is how anyone would actually write this.
    #[test]
    fn a_module_level_fixture_constant_puts_the_whole_file_in_scope() {
        let found = offenders(concat!(
            "const FIXTURE: &str = \"tests/fixtures/a.json\";\n",
            "fn writer() {\n",
            "fs::write(FIXTURE, b\"x\").unwrap();\n",
            "}\n",
        ));
        assert_eq!(found.len(), 1, "{found:?}");
        // Column zero: `lines()` would have reported 2 here.
        assert!(found[0].contains("fixture.rs:3"), "{found:?}");
    }

    /// AND THE FALSE POSITIVE THE WINDOW PRODUCED: a test that READS fixtures
    /// through `include_str!` sits a few hundred bytes above any number of
    /// unrelated scratch writes. Scope is the function, so they do not meet.
    #[test]
    fn a_fixture_read_does_not_convict_the_next_function() {
        let found = offenders(concat!(
            "mod inner {\n",
            "    #[test]\n",
            "    fn reads() {\n",
            "        const V: &str = include_str!(\"../tests/fixtures/a.json\");\n",
            "    }\n",
            "    #[test]\n",
            "    fn stages_a_scratch_file() {\n",
            "        let f = std::fs::File::create(\"target/scratch.json\").unwrap();\n",
            "    }\n",
            "}\n",
        ));
        assert!(found.is_empty(), "{found:?}");
    }

    fn skips(code: &str) -> Vec<String> {
        super::missing_input_skip_offenders("sample.rs", code)
    }

    /// The two spellings the tree shipped.
    #[test]
    fn both_shipped_skip_spellings_convict() {
        let existence = skips(concat!(
            "    #[test]\n",
            "    fn pins_the_artifact() {\n",
            "        let path = Path::new(\"analysis/a.json\");\n",
            "        if !path.exists() {\n",
            "            return;\n",
            "        }\n",
            "        assert!(true);\n",
            "    }\n",
        ));
        assert_eq!(existence.len(), 1, "{existence:?}");
        let fallible = skips(concat!(
            "    #[test]\n",
            "    fn pins_the_artifact() {\n",
            "        let Ok(bytes) = std::fs::read(\"analysis/a.json\") else {\n",
            "            return;\n",
            "        };\n",
            "        assert!(!bytes.is_empty());\n",
            "    }\n",
        ));
        assert_eq!(fallible.len(), 1, "{fallible:?}");
    }

    /// THE WIDENING, PINNED. `#[test]` is not a substring of `#[tokio::test]`,
    /// so the first cut of this gate could not see a single async test - and
    /// "found zero offenders" from a scan that reads none of the socket suites
    /// is not a finding. This is the whole guard against that regressing.
    #[test]
    fn an_async_test_that_skips_is_convicted_too() {
        let found = skips(concat!(
            "    #[tokio::test]\n",
            "    async fn pins_the_artifact() {\n",
            "        let Ok(bytes) = std::fs::read(\"analysis/a.json\") else {\n",
            "            return;\n",
            "        };\n",
            "        assert!(!bytes.is_empty());\n",
            "    }\n",
        ));
        assert_eq!(
            found.len(),
            1,
            "an async test declining to assert is the same defect: {found:?}"
        );
    }

    /// THE FALSE POSITIVE THE WIDENING EXPOSED, and the reason the probe is the
    /// let-else form rather than the `let Ok(` substring. A socket drain binds
    /// `while let Ok(..)` and RETURNS ON SUCCESS, panicking if the loop runs
    /// out - nothing is skipped, and ten of these in `serving.rs` were the
    /// entire yield of the widening. A gate that convicts them is as useless as
    /// one that sees nothing.
    #[test]
    fn a_socket_drain_loop_is_not_a_missing_input_skip() {
        assert!(
            skips(concat!(
                "    #[tokio::test]\n",
                "    async fn reads_a_named_frame() {\n",
                "        while let Ok(Some(Ok(message))) = next(deadline).await {\n",
                "            if matches!(message, Message::Text(_)) {\n",
                "                return;\n",
                "            }\n",
                "        }\n",
                "        panic!(\"produced no frames\");\n",
                "    }\n",
            ))
            .is_empty()
        );
        // The let-chain spelling reduces to the same thing.
        assert!(
            skips(concat!(
                "    #[tokio::test]\n",
                "    async fn reads_a_trade() {\n",
                "        if let Message::Text(text) = message\n",
                "            && let Ok(trade) = serde_json::from_str(&text)\n",
                "        {\n",
                "            return;\n",
                "        }\n",
                "        panic!(\"no trade\");\n",
                "    }\n",
            ))
            .is_empty()
        );
    }

    /// A PRODUCTION function that probes and returns is not a test, and most
    /// of this workspace's early returns are exactly that. The bound is the
    /// test body, so an ordinary function beside one is untouched - and so is
    /// a test that probes without returning.
    #[test]
    fn production_probes_and_assertive_tests_are_left_alone() {
        assert!(
            skips(concat!(
                "fn load(path: &Path) {\n",
                "    if !path.exists() {\n",
                "        return;\n",
                "    }\n",
                "}\n",
            ))
            .is_empty()
        );
        assert!(
            skips(concat!(
                "    #[test]\n",
                "    fn asserts_the_file_is_there() {\n",
                "        assert!(Path::new(\"a.json\").exists());\n",
                "    }\n",
            ))
            .is_empty()
        );
        // `returned` is not a `return`.
        assert!(
            skips(concat!(
                "    #[test]\n",
                "    fn names_a_variable_returned() {\n",
                "        let returned = Path::new(\"a\").exists();\n",
                "        assert!(returned);\n",
                "    }\n",
            ))
            .is_empty()
        );
    }
}

/// THE THIRD SOURCE GATE IN THIS FILE, and the class it holds shut has cost
/// this workspace three tests that reported green while asserting nothing.
///
/// A cargo test - unit or integration - runs with its working directory set to
/// the PACKAGE root, never the repository root. A test that reads a
/// repo-relative input therefore reads `crates/<pkg>/analysis/...`, which does
/// not exist; where the read was guarded by an existence check or a fallible
/// binding with an early return, the guard fired on EVERY run and the body
/// never executed. `mogwai-cli`'s `the_control_artifact_carries_no_b8_field`
/// and `the_screen_artifact_carries_every_evaluated_cell_and_its_verdict` were
/// both this, and the second was ALSO wrong on its merits underneath - the
/// skip had hidden a false invariant for as long as it had existed. That is
/// the general shape: a test that can decline to assert does not merely lose
/// coverage, it preserves whatever is wrong inside it.
///
/// THE RULE IS ON THE SKIP, NOT ON THE PATH, because "repo-relative" is not
/// decidable from source and the working directory is not the only way to make
/// an input absent. A test whose input may legitimately be missing states that
/// with `#[ignore]`, which the gate profile can then include deliberately;
/// declining at runtime is invisible to every count libtest reports.
///
/// The scan is over `crates/*/src` and `crates/*/tests`, exempt from itself
/// for the same structural reason the fixture-write gate is: it has to name
/// the constructs it forbids, and `mod parser` below proves it bites on
/// synthetic samples rather than on what the tree happens to hold.
#[test]
fn no_test_declines_to_assert_on_a_missing_input() {
    let root = repo_root();
    let files = source_files(&root);
    assert!(
        files.len() > 100,
        "this scan found only {} source files under crates/, which is too few for this workspace",
        files.len()
    );
    // THE EXEMPTION IS THIS FILE, not every file wearing its name. `file!()` is
    // workspace-relative, so joining it against the root names exactly one
    // path; matching on the bare file name would exempt any other
    // `test_hygiene.rs` anywhere under `crates/`, which is an exemption nobody
    // wrote and nothing would report.
    let this_file = root.join(file!());
    let mut offenders = Vec::new();
    for path in files.into_iter().chain(example_and_bench_files(&root)) {
        if path == this_file {
            continue;
        }
        let raw = std::fs::read_to_string(&path).expect("a scanned source file reads");
        let code = strip_comments_keeping_literals(&path, &raw);
        let shown = path.strip_prefix(&root).unwrap_or(&path).display();
        offenders.extend(missing_input_skip_offenders(&format!("{shown}"), &code));
    }
    assert!(
        offenders.is_empty(),
        "these tests return early when an input is absent, so on a run where it is absent they \
         assert nothing and still report green:\n{}\n\nA cargo test's working directory is its \
         PACKAGE root, so a repo-relative path is absent on every run - join it against the \
         workspace root instead. Where the input may genuinely be missing, say so with \
         `#[ignore]`, which is countable; a runtime skip is not.",
        offenders.join("\n")
    );
}

/// Every `#[test]` body that both probes for an input's absence and returns,
/// reported as one line each.
///
/// TWO PROBE SPELLINGS, and only these two, because they are the two the tree
/// has actually shipped: `path.exists()` as a condition, and a fallible read
/// bound with `let Ok(..) = .. else`. A `return` ANYWHERE in the same test body
/// convicts - a closure's `return` inside such a body is a false positive in
/// principle, and no instance exists, so the tighter rule would be the more
/// complicated one for no gain. The bound is the test body: a production
/// function in the same file that legitimately probes and returns is out of
/// scope, which is most of the `return`s in this workspace.
///
/// THE `let Ok(` PROBE IS THE LET-ELSE FORM ONLY, and the distinction is what
/// makes this gate usable once it can see `#[tokio::test]`. A bare `let Ok(`
/// substring also matches `while let Ok(Some(Ok(message))) = ..` and the
/// let-chain `.. && let Ok(msg) = serde_json::from_str(&text)`, which are the
/// socket suites' DRAIN loops: there the `return` is the SUCCESS exit and the
/// loop falling through panics, so nothing is being skipped. Ten such loops in
/// `serving.rs` were the entire yield of widening the attribute match, and
/// suppressing them by exemption would have left a gate that convicts the
/// wrong shape. A let-else is recognized structurally - `else` reached before
/// the statement's `;` - so `while let`, `if let` and let-chains are excluded
/// by what they are rather than by where they live.
fn missing_input_skip_offenders(shown: &str, code: &str) -> Vec<String> {
    let mut offenders = Vec::new();
    for (from, to) in test_fn_body_spans(code) {
        let body = &code[from..to];
        if !body.contains(".exists()") && !has_fallible_let_else(body) {
            continue;
        }
        let Some(at) = body.find("return") else {
            continue;
        };
        // `returned`, `return_value` and friends are not returns.
        let tail = body[at + "return".len()..].trim_start();
        if !tail.starts_with(';') && !tail.starts_with('}') {
            continue;
        }
        let line = code[..from + at].matches('\n').count() + 1;
        offenders.push(format!(
            "  {shown}:{line} returns early from a test that probes for a missing input"
        ));
    }
    offenders.sort();
    offenders
}

/// Whether `body` binds a fallible value with a `let Ok(..) = .. else` -
/// the divergence-free spelling of "the input might not be there".
///
/// A `let` statement runs to its `;`; a LET-ELSE reaches its `else` first.
/// `while let` and `if let` are excluded by the keyword before the `let`,
/// which is also what a let-chain (`cond && let Ok(..) = ..`) reduces to once
/// the leading `if`/`while` is found - so the check is on the statement
/// position, not on the payload.
fn has_fallible_let_else(body: &str) -> bool {
    body.match_indices("let Ok(").any(|(at, _)| {
        let before = body[..at].trim_end();
        if before.ends_with("while") || before.ends_with("if") || before.ends_with("&&") {
            return false;
        }
        // The statement ends at its `;`; a let-else opens a block first. Take
        // whichever comes first and ask which one it was.
        let rest = &body[at..];
        let semicolon = rest.find(';').unwrap_or(rest.len());
        let brace = rest.find('{').unwrap_or(rest.len());
        brace < semicolon && rest[..brace].contains("else")
    })
}

/// The body span of every function carrying a `#[test]` attribute, in
/// comment-stripped, literal-KEEPING code.
///
/// A `#[test]` is followed by the attributes it stacks with (`#[ignore]`,
/// `#[should_panic]`) and then the declaration, so the body wanted is the
/// FIRST `fn` body beginning after the attribute. Nested spans are subsets of
/// it and start later, so taking the earliest start is right.
///
/// BOTH SPELLINGS, and the omission of the second is why the class gate found
/// nothing on the run that installed it. `#[test]` is not a substring of
/// `#[tokio::test]`, so a scan keyed on the former alone is blind to every
/// async test in the workspace - roughly sixty in `mogwai-cli`'s `serving.rs`
/// alone, which is where the socket suites live. The name scan above already
/// handled both spellings, so this was an inconsistency inside one file rather
/// than a workspace with no async tests: a scanner that sees nothing passes
/// trivially, and a full green gate is no evidence against it.
fn test_fn_body_spans(code: &str) -> Vec<(usize, usize)> {
    let mut spans = fn_body_spans(code);
    spans.sort();
    let mut out = Vec::new();
    let mut starts: Vec<usize> = code
        .match_indices("#[test]")
        .chain(code.match_indices("#[tokio::test"))
        .map(|(at, _)| at)
        .collect();
    starts.sort_unstable();
    for at in starts {
        if let Some(&span) = spans.iter().find(|&&(from, _)| from > at) {
            out.push(span);
        }
    }
    out.dedup();
    out
}
