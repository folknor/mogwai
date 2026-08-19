// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The gate profile's skip list, held against the tests it actually matches -
//! and, on the same scan, the one thing a running test may never do to the tree.
//!
//! `brokkr.toml`'s gate profile turns on `include_ignored`, which pulls in two
//! unrelated families: the socket-backed tests, which the gate WANTS, and a
//! couple of dozen measurement instruments that outlive the per-test watchdog by
//! design. The second family is excluded by a `skip` list, and skipping them is
//! free ONLY because each one is `#[ignore]`d at the source - the coverage audit
//! accepts a non-run pair that is ignored, because `include_ignored` is the sole
//! reason it was in scope at all.
//!
//! THAT IS AN INVARIANT WITH NO ENFORCER, and the file says so itself: the skip
//! entries are libtest substrings, so a bare prefix like `parity3b_` matches
//! every test that will ever share it, and "every test that shares it stays
//! ignored" is "a property of files this file cannot see". It has already been
//! violated once - `parity3a_` also caught
//! `parity3a_cadence_feasible_verdict_matches_the_committed_cadence`, which is
//! not ignored and needs nothing but a committed artifact, so the complete
//! profile silently stopped running a real test. The coverage audit caught that
//! one as an orphan, which is a downstream symptom in another tool's output; the
//! statement worth making is the direct one, here, naming both the pattern and
//! the test it swallowed.
//!
//! WHAT THIS SCAN CAN AND CANNOT SEE. It reads test declarations out of the
//! source text of `crates/*/src/**` and `crates/*/tests/**`, reconstructing the
//! name libtest would report - the file's module path for a unit test, the
//! enclosing inline `mod` chain, then the function. It also reads the tests a
//! `macro_rules!` generator emits, because ten of the envelope fidelity gates
//! are generated and a `fn`-only scan would have called their skip entry dead
//! while a non-ignored one among them stayed invisible - see
//! `test_generating_macros`. It does not read `examples/` or `benches/`. A
//! pattern is checked against the reconstructed name exactly as libtest checks
//! it: `contains`.
//!
//! A PARSER-BACKED SCANNER THAT FAILS OPEN IS WORSE THAN NO SCANNER, because it
//! reports green. Everywhere this one can lose track it REFUSES instead of
//! guessing, and the three places it could are named where they happen:
//!
//! - Comment stripping is literal-aware and understands nested block comments,
//!   so a `//` inside a string cannot eat a closing brace. That direction is the
//!   dangerous one: losing a CLOSE leaves `depth` too high, the enclosing `mod`
//!   is never popped, and every later test in the file gets a phantom prefix -
//!   which turns a live skip entry into a false DEAD one, and, where the prefix
//!   was all that stood above the match, a live violation into a false negative.
//!   An unterminated block comment or string panics naming the file.
//! - A test attribute that is not followed by a declaration this parser
//!   recognizes is collected and reported, rather than silently dropped. That is
//!   exactly how the first version failed - it could not see macro-generated
//!   tests - and a count threshold would not have caught it.
//! - A `macro_rules!` generator whose emitted tests are not UNIFORMLY ignored
//!   cannot have one ignore status attributed to all of them, so it panics
//!   rather than marking them all ignored. Marking them all ignored is the
//!   failure that satisfies this file's own invariant for free.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

/// The crate's own manifest dir, walked up to the workspace root.
fn repo_root() -> PathBuf {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    dir.pop();
    dir.pop();
    dir
}

/// One test declaration, named the way libtest names it.
#[derive(Debug)]
struct TestFn {
    name: String,
    ignored: bool,
    file: PathBuf,
    line: usize,
}

/// Everything the scan produced, including what it could NOT resolve.
#[derive(Default)]
struct Scan {
    tests: Vec<TestFn>,
    /// A `#[test]` attribute with no declaration this parser recognized. Any
    /// entry here means the scan has gone partly blind, which is the failure
    /// mode that must never be silent.
    unresolved: Vec<String>,
}

/// Every `.rs` file under `crates/*/src` and `crates/*/tests`, with the module
/// prefix its declarations inherit.
fn source_files(root: &Path) -> Vec<(PathBuf, Vec<String>)> {
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
        // `src/` carries unit tests, whose libtest name is prefixed by the
        // file's module path.
        walk(&package.join("src"), &package.join("src"), true, &mut files);
        // `tests/` carries integration binaries. Only a TOP-LEVEL file is a
        // binary, and there the binary is the root and the file contributes no
        // prefix; a nested file is a MODULE of one, so `tests/foo/bar.rs`
        // reports as `foo::bar::<test>`. Prefixing every nested file with
        // nothing under-reconstructed those names and showed up as false dead
        // entries.
        walk(
            &package.join("tests"),
            &package.join("tests"),
            true,
            &mut files,
        );
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
            let mut found = Vec::new();
            walk(&dir, &dir, false, &mut found);
            files.extend(found.into_iter().map(|(path, _)| path));
        }
    }
    files
}

fn walk(dir: &Path, base: &Path, path_is_module: bool, out: &mut Vec<(PathBuf, Vec<String>)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            walk(&path, base, path_is_module, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let prefix = if path_is_module {
                module_prefix(&path, base)
            } else {
                Vec::new()
            };
            out.push((path, prefix));
        }
    }
}

/// Every test declaration under `crates/`.
///
/// TWO PASSES, because a `macro_rules!` generator and its invocations need not
/// share a file: the first pass collects every generator in the tree, the second
/// scans with the whole set in hand. A single pass saw only generators declared
/// beside their invocation, so a generator exported from a shared module was
/// invisible to both the dead-entry check and the violation check. The cost is
/// an over-approximation - a generator's name matched in a file that never
/// imported it would invent a test - which is the safe direction for the
/// violation check and merely keeps a dead entry looking live for the other.
fn collect_tests(root: &Path) -> Scan {
    let files = source_files(root);
    let mut generators: BTreeMap<String, bool> = BTreeMap::new();
    for (path, _) in &files {
        let text = stripped_source(path);
        for (name, ignored) in test_generating_macros(path, &text) {
            generators.insert(name, ignored);
        }
    }

    let mut scan = Scan::default();
    for (path, prefix) in &files {
        let text = stripped_source(path);
        scan_file(path, &text, prefix, &generators, &mut scan);
    }
    scan
}

fn stripped_source(path: &Path) -> String {
    let text = std::fs::read_to_string(path).expect("a scanned source file reads");
    strip_comments(path, &text)
}

/// The module path a file contributes: `src/a/b.rs` is `a::b`, `src/a/mod.rs`
/// is `a`, and `src/lib.rs` / `src/main.rs` contribute nothing.
fn module_prefix(path: &Path, base: &Path) -> Vec<String> {
    let relative = path.strip_prefix(base).expect("scanned under its own base");
    let mut parts: Vec<String> = relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let last = parts.pop().expect("a file has a name");
    let stem = last.trim_end_matches(".rs");
    if !matches!(stem, "mod" | "lib" | "main") {
        parts.push(stem.to_string());
    }
    parts
}

/// Pulls test functions out of one file's COMMENT-STRIPPED text, tracking the
/// inline `mod` nesting so the reconstructed name matches what libtest reports.
fn scan_file(
    path: &Path,
    text: &str,
    prefix: &[String],
    generators: &BTreeMap<String, bool>,
    out: &mut Scan,
) {
    let lines: Vec<&str> = text.lines().collect();
    let mut mods: Vec<(String, usize)> = Vec::new();
    let mut depth = 0usize;
    let mut attrs: Vec<String> = Vec::new();
    // AN ATTRIBUTE MAY SPAN LINES. `#[ignore = "<a long reason>"]` wraps, and a
    // line-at-a-time reader took the continuation for the declaration under the
    // attribute - which the refusal above caught on the first run, in
    // `parity12a_ii.rs`. Brackets are counted over a text whose string interiors
    // are blanked, so a bracket inside the reason cannot unbalance the count.
    let mut open_attr: Option<(String, isize)> = None;
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();

        if let Some((mut text, mut balance)) = open_attr.take() {
            text.push(' ');
            text.push_str(trimmed);
            balance += bracket_balance(trimmed);
            if balance > 0 {
                open_attr = Some((text, balance));
            } else {
                attrs.push(text);
            }
            continue;
        }

        if trimmed.starts_with("#[") || trimmed.starts_with("#![") {
            let balance = bracket_balance(trimmed);
            if balance > 0 {
                open_attr = Some((trimmed.to_string(), balance));
            } else {
                attrs.push(trimmed.to_string());
            }
        } else if !trimmed.is_empty() {
            let is_test = attrs.iter().any(|attr| {
                attr.starts_with("#[test]")
                    || attr.starts_with("#[tokio::test")
                    || attr.starts_with("#[test_case")
            });
            match fn_name(trimmed) {
                Some(name) if is_test => {
                    let mut full = prefix.to_vec();
                    full.extend(mods.iter().map(|(name, _)| name.clone()));
                    full.push(name);
                    out.tests.push(TestFn {
                        name: full.join("::"),
                        ignored: attrs.iter().any(|attr| attr.starts_with("#[ignore")),
                        file: path.to_path_buf(),
                        line: index + 1,
                    });
                }
                // A TEST ATTRIBUTE WITH NO DECLARATION UNDER IT. Inside a
                // `macro_rules!` body the name is a metavariable (`fn $name`),
                // which is expected and handled by the generator pass; anything
                // else means this parser has stopped recognizing a way of
                // declaring a test, and it must say so rather than drop it.
                None if is_test && !strip_qualifiers(trimmed).starts_with("fn $") => {
                    out.unresolved.push(format!(
                        "  {}:{} carries {:?} but the next declaration is {:?}, which this parser \
                         does not recognize as a test",
                        path.display(),
                        index + 1,
                        attrs.join(" "),
                        trimmed
                    ));
                }
                _ => {}
            }
            if let Some(name) = mod_name(trimmed) {
                mods.push((name, depth));
            }
            for (macro_name, ignored) in generators {
                let Some(generated) = macro_argument(&lines, index, macro_name) else {
                    continue;
                };
                let mut full = prefix.to_vec();
                full.extend(mods.iter().map(|(name, _)| name.clone()));
                full.push(generated);
                out.tests.push(TestFn {
                    name: full.join("::"),
                    ignored: *ignored,
                    file: path.to_path_buf(),
                    line: index + 1,
                });
            }
            attrs.clear();
        }

        depth = (depth + count(line, '{')).saturating_sub(count(line, '}'));
        while mods.last().is_some_and(|(_, at)| depth <= *at) {
            mods.pop();
        }
    }
}

/// The `macro_rules!` macros in this file that EMIT tests, and whether what
/// they emit is `#[ignore]`d.
///
/// A SCANNER THAT ONLY READS `fn` DECLARATIONS IS BLIND TO A WHOLE FAMILY, and
/// the blindness is exactly as dangerous as the hole this file closes: ten of
/// the envelope fidelity gates are generated by one `fidelity_gate!` macro, so
/// a skip pattern naming them would have looked DEAD to a `fn`-only scan and a
/// non-ignored one would have looked absent. The convention this recognizes is
/// the one the tree uses - the generated test's name is the macro's first
/// argument - and it is deliberately narrow: a macro whose body carries no
/// `#[test]` is not a generator, and an invocation whose first argument is not
/// a bare identifier yields nothing.
///
/// ONE IGNORE STATUS IS ATTRIBUTED TO EVERY TEST A GENERATOR EMITS, which is
/// only sound while the body is UNIFORM. A macro carrying `#[ignore]` on one arm
/// and not another, or taking the attribute per invocation, would otherwise mark
/// all of its tests ignored - and "all ignored" is precisely the answer that
/// satisfies this file's invariant without checking anything, so the hole this
/// file exists to close would reappear one level down. The uniformity is
/// therefore MEASURED, by counting `#[test]` against `#[ignore` in the body, and
/// a body that mixes them PANICS. When that fires, the fix is to split the
/// generator or to name the generated tests in the skip list in full, never to
/// widen this.
fn test_generating_macros(path: &Path, text: &str) -> Vec<(String, bool)> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("macro_rules!") {
        let after = &rest[at + "macro_rules!".len()..];
        let name: String = after
            .trim_start()
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        let body = after.find('{').map(|open| balanced(&after[open..]));
        if let (false, Some(body)) = (name.is_empty(), body) {
            let tests = body.matches("#[test]").count() + body.matches("#[tokio::test").count();
            let ignores = body.matches("#[ignore").count();
            if tests > 0 {
                assert!(
                    ignores == 0 || ignores >= tests,
                    "the macro {name}! in {} emits {tests} test(s) but carries {ignores} \
                     #[ignore] attribute(s), so no single ignore status describes what it \
                     generates. This scan would have to mark them all ignored, which silently \
                     satisfies the skip-list invariant for whichever of them is NOT ignored - the \
                     exact hole this file exists to close. Split the generator, or drop the prefix \
                     from the skip list and name the generated tests in full.",
                    path.display()
                );
                // A per-invocation ignore attribute is the other way this goes
                // wrong, and it is visible as a metavariable in the body.
                assert!(
                    !body.contains("#[ignore = $") && !body.contains("$ignore"),
                    "the macro {name}! in {} takes its #[ignore] per invocation, so its generated \
                     tests do not share one ignore status and this scan cannot attribute one",
                    path.display()
                );
                found.push((name, ignores > 0));
            }
        }
        rest = after;
    }
    found
}

/// The text from an opening brace through its matching close.
fn balanced(from: &str) -> &str {
    let mut depth = 0usize;
    for (at, ch) in from.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &from[..=at];
                }
            }
            _ => {}
        }
    }
    from
}

/// The first identifier argument of `name!(...)` invoked at `index`, which may
/// sit on a following line when rustfmt has wrapped the call.
fn macro_argument(lines: &[&str], index: usize, name: &str) -> Option<String> {
    let call = format!("{name}!(");
    let at = lines[index].find(&call)?;
    let mut tail = lines[index][at + call.len()..].trim_start().to_string();
    let mut cursor = index;
    while tail.is_empty() && cursor + 1 < lines.len() {
        cursor += 1;
        tail = lines[cursor].trim().to_string();
    }
    let ident: String = tail
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!ident.is_empty()).then_some(ident)
}

/// How many `[` an attribute leaves unclosed on this line.
fn bracket_balance(line: &str) -> isize {
    isize::try_from(count(line, '[')).expect("a line has few brackets")
        - isize::try_from(count(line, ']')).expect("a line has few brackets")
}

fn count(line: &str, ch: char) -> usize {
    line.chars().filter(|c| *c == ch).count()
}

/// Blanks every comment in a file, preserving line structure, WITHOUT cutting
/// inside a string or character literal.
///
/// A NAIVE `line.find("//")` IS WRONG IN THE DANGEROUS DIRECTION, which is why
/// this exists. `assert!(ok, "see ws://host/x"); }` truncates at the `//` and
/// takes the closing brace with it: `depth` then stays one too high, the
/// enclosing `mod` is never popped, and every later test in the file is
/// reconstructed under a prefix it does not have. That produces FALSE DEAD
/// ENTRIES - a live skip pattern reported as matching nothing, so the check
/// fails for a parser reason - and, where the phantom prefix was the only thing
/// standing above a match, a FALSE NEGATIVE on the violation check. Block
/// comments were not handled at all, so a commented-out `#[test] fn foo()`
/// counted as a live test.
///
/// The literals recognized are the ones Rust has: `"..."` with backslash
/// escapes, raw strings with any hash count, byte and byte-raw strings, and
/// character literals - distinguished from a lifetime, which is not a literal
/// and must not open one. Block comments NEST, as they do in Rust. An
/// unterminated block comment or string is a file this parser cannot read, so it
/// panics rather than returning a truncated view of it.
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
fn strip_qualifiers(trimmed: &str) -> &str {
    let mut rest = trimmed;
    loop {
        let stripped = [
            "pub(crate) ",
            "pub(super) ",
            "pub ",
            "async ",
            "unsafe ",
            "const ",
        ]
        .iter()
        .find_map(|keyword| rest.strip_prefix(keyword));
        match stripped {
            Some(next) => rest = next.trim_start(),
            None => return rest,
        }
    }
}

fn identifier_after(trimmed: &str, keyword: &str) -> Option<String> {
    let rest = strip_qualifiers(trimmed).strip_prefix(keyword)?;
    let name: String = rest
        .trim_start()
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

fn fn_name(trimmed: &str) -> Option<String> {
    identifier_after(trimmed, "fn ")
}

fn mod_name(trimmed: &str) -> Option<String> {
    let name = identifier_after(trimmed, "mod ")?;
    // `mod foo;` is a declaration, not a scope: the file it names is scanned in
    // its own right and would be double-counted if this pushed.
    trimmed.contains('{').then_some(name)
}

/// The tool's config, as data. Never invoked - this reads a list out of it.
fn gate_skip_patterns(config: &toml::Table) -> Vec<String> {
    let gate = config
        .get("test")
        .and_then(|t| t.get("gate_profile"))
        .and_then(toml::Value::as_str)
        .expect("the config names a gate profile")
        .to_string();
    let profile = config
        .get("test")
        .and_then(|t| t.get("profiles"))
        .and_then(|p| p.get(&gate))
        .unwrap_or_else(|| panic!("the gate profile {gate} is declared"));
    profile
        .get("skip")
        .and_then(toml::Value::as_array)
        .expect("the gate profile carries a skip list")
        .iter()
        .map(|entry| {
            // Package-qualified entries are tables; this workspace uses none,
            // and one landing here silently unchecked is the same hole again.
            let text = entry.as_str().unwrap_or_else(|| {
                panic!("this check only models bare-substring skip entries, got {entry:?}")
            });
            // A DEGENERATE ENTRY MAKES EVERY CHECK BELOW VACUOUS. The empty
            // string is a substring of every test name and of every `only`
            // filter, so it would satisfy the release-sweep agreement for free
            // while excluding the entire suite from the gate.
            assert!(
                text.len() >= 4,
                "the skip entry {text:?} is too short to name anything deliberately; a substring \
                 filter that broad excludes tests nobody chose and satisfies every check here \
                 vacuously"
            );
            text.to_string()
        })
        .collect()
}

fn config_table(root: &Path) -> toml::Table {
    let path = root.join("brokkr.toml");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("reading {}: {err}", path.display()))
        .parse()
        .expect("the project config parses as TOML")
}

/// THE INVARIANT, enforced: a skip pattern may match only `#[ignore]`d tests.
#[test]
fn every_gate_skip_pattern_matches_only_ignored_tests() {
    let root = repo_root();
    let scan = collect_tests(&root);
    // THE SCANNER REFUSES BEFORE IT REPORTS. A parser that has stopped
    // recognizing a declaration form agrees with every skip list there is, and
    // the count threshold below would not notice one file's worth going missing
    // - which is how the first version of this shipped blind to macro-generated
    // tests. Every test attribute must land on something.
    assert!(
        scan.unresolved.is_empty(),
        "this scan found test attributes it could not attach to a declaration:\n{}\n\nUntil that \
         is understood the scan is partly blind, and a blind scan reports the skip list green for \
         free. Teach the parser the declaration form, do not relax this.",
        scan.unresolved.join("\n")
    );
    let tests = scan.tests;
    assert!(
        tests.len() > 500,
        "the scan found only {} test declarations, which is too few for this workspace - it is \
         reading the wrong tree or its parser has stopped recognizing declarations, and a scan \
         that sees nothing agrees with every skip list there is",
        tests.len()
    );

    let mut violations = Vec::new();
    let mut unmatched = Vec::new();
    for pattern in gate_skip_patterns(&config_table(&root)) {
        let matched: Vec<&TestFn> = tests
            .iter()
            .filter(|test| test.name.contains(&pattern))
            .collect();
        if matched.is_empty() {
            unmatched.push(pattern.clone());
            continue;
        }
        for test in matched.iter().filter(|test| !test.ignored) {
            violations.push(format!(
                "  skip pattern {pattern:?} matches {} ({}:{}), which is NOT #[ignore]d",
                test.name,
                test.file
                    .strip_prefix(&root)
                    .unwrap_or(&test.file)
                    .display(),
                test.line
            ));
        }
    }

    assert!(
        violations.is_empty(),
        "the gate profile's skip list silently stops real tests from running:\n{}\n\nA skip entry \
         is free only while every test it matches is #[ignore]d at the source, because that is \
         what lets the coverage audit accept the pair as justified. Either #[ignore] the test at \
         its declaration with the reason, or replace the prefix with the full names it was meant \
         to catch.",
        violations.join("\n")
    );

    assert!(
        unmatched.is_empty(),
        "the gate profile's skip list names patterns that match no test at all: {unmatched:?}. A \
         dead entry is not harmless - it is a name that drifted, so whatever it was excluding is \
         being run again under a name nobody wrote down, or it will silently start catching an \
         unrelated test that grows into it.",
    );
}

/// The other invariant `brokkr.toml` states in prose and nothing checked: the
/// release-profile `only` list and the gate's `skip` list have to agree.
///
/// They are two halves of one decision - a wall-clock contract is excluded from
/// the debug gate BECAUSE it is evaluated in the release sweep - and they are
/// written a hundred lines apart in different syntaxes. A test named in one and
/// not the other either runs in a lane that cannot judge it or runs in no lane
/// at all.
///
/// IT IS RESOLVED AGAINST THE TESTS, not against the two strings. Both lists are
/// libtest substring filters, and comparing them as substrings is either slack
/// or wrong: `skip.contains(filter)` is satisfied by ANY unrelated long skip
/// entry, and the sound converse `filter.contains(skip)` refuses the config as
/// it stands, where the `only` filter is the shorter of the two. Neither
/// statement is the invariant. The invariant is that EVERY TEST THE RELEASE
/// SWEEP RUNS IS SKIPPED BY THE GATE, and this file already reconstructs every
/// test name, so it is checked directly - which also catches the case the
/// substring form cannot see at all: a new test growing into the `only` filter
/// without growing into any skip entry.
#[test]
fn every_release_only_filter_is_skipped_by_the_gate() {
    let root = repo_root();
    let config = config_table(&root);
    let skips: BTreeSet<String> = gate_skip_patterns(&config).into_iter().collect();
    let scan = collect_tests(&root);

    let sweeps = config
        .get("check")
        .and_then(toml::Value::as_array)
        .expect("the config declares check sweeps");
    for sweep in sweeps {
        if sweep.get("profile").and_then(toml::Value::as_str) != Some("release") {
            continue;
        }
        let name = sweep
            .get("name")
            .and_then(toml::Value::as_str)
            .unwrap_or("<unnamed>");
        for filter in sweep
            .get("only")
            .and_then(toml::Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            let filter = filter.as_str().expect("an `only` filter is a string");
            let run: Vec<&TestFn> = scan
                .tests
                .iter()
                .filter(|test| test.name.contains(filter))
                .collect();
            assert!(
                !run.is_empty(),
                "the release sweep {name:?} names {filter:?}, which matches no test in the tree - \
                 so that lane evaluates nothing at all and the contract it was written for is \
                 running in no lane"
            );
            let unskipped: Vec<&str> = run
                .iter()
                .filter(|test| !skips.iter().any(|skip| test.name.contains(skip)))
                .map(|test| test.name.as_str())
                .collect();
            assert!(
                unskipped.is_empty(),
                "the release sweep {name:?} runs {filter:?}, which catches {unskipped:?} - and no \
                 gate skip entry excludes those, so the debug gate is ALSO evaluating a \
                 wall-clock contract it cannot validly judge. The two lists state one decision \
                 and have to agree. Skip entries: {skips:?}"
            );
        }
    }
}

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
    for (path, _) in files {
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

/// THE PARSER'S OWN GATES. Everything above rests on the scan seeing the tree
/// as libtest does, and the two ways it silently stopped doing that are pinned
/// here as fixtures rather than left to be rediscovered on the tree.
mod parser {
    use super::{
        Scan, TestFn, fixture_write_offenders, scan_file, strip_comments,
        strip_comments_keeping_literals,
    };
    use std::{collections::BTreeMap, path::Path};

    fn scan(text: &str) -> Scan {
        let path = Path::new("fixture.rs");
        let stripped = strip_comments(path, text);
        let mut out = Scan::default();
        scan_file(path, &stripped, &[], &BTreeMap::new(), &mut out);
        out
    }

    fn names(scan: &Scan) -> Vec<String> {
        scan.tests
            .iter()
            .map(|test: &TestFn| test.name.clone())
            .collect()
    }

    /// The dangerous direction of a naive `line.find("//")`: it takes the
    /// closing brace with the comment, `depth` stays too high, the `mod` is
    /// never popped, and every later test is reconstructed under a prefix it
    /// does not have - a live skip pattern then looks DEAD.
    #[test]
    fn a_double_slash_inside_a_string_does_not_eat_the_brace_beside_it() {
        let scan = scan(concat!(
            "mod inner {\n",
            "    #[test]\n",
            "    fn first() { let _ = \"see ws://host/x\"; }\n",
            "}\n",
            "#[test]\n",
            "fn second() {}\n",
        ));
        assert_eq!(
            names(&scan),
            vec!["inner::first".to_string(), "second".to_string()],
            "`second` sits outside `inner`; a stripper that cut the string's `//` would lose the \
             brace closing `first` and report it as `inner::second`"
        );
    }

    /// A commented-out test is not a test, and a block comment is how one gets
    /// commented out. The first parser did not handle `/* */` at all.
    #[test]
    fn a_block_commented_test_is_not_counted() {
        let scan = scan(concat!(
            "/*\n",
            "#[test]\n",
            "fn commented_out() {}\n",
            "*/\n",
            "#[test]\n",
            "fn live() {}\n",
        ));
        assert_eq!(names(&scan), vec!["live".to_string()]);
    }

    /// The refusal that caught the real blind spot on its first run: a wrapped
    /// `#[ignore = "..."]` whose continuation line was read as the declaration.
    #[test]
    fn a_wrapped_attribute_is_one_attribute() {
        let scan = scan(concat!(
            "#[test]\n",
            "#[ignore = \"a reason long enough that rustfmt wraps it onto \\\n",
            "     a second line entirely\"]\n",
            "fn wrapped() {}\n",
        ));
        assert!(
            scan.unresolved.is_empty(),
            "a wrapped attribute must not be read as a declaration: {:?}",
            scan.unresolved
        );
        assert_eq!(names(&scan), vec!["wrapped".to_string()]);
        assert!(scan.tests[0].ignored, "the wrapped attribute is the ignore");
    }

    /// The refusal itself, which is the whole reason this scanner may be
    /// trusted: an unrecognized declaration form is reported, never dropped.
    #[test]
    fn an_unrecognized_declaration_under_a_test_attribute_is_reported() {
        let scan = scan("#[test]\nstruct NotAFunction;\n");
        assert!(
            names(&scan).is_empty() && scan.unresolved.len() == 1,
            "expected one unresolved report, got {scan:?} tests {:?}",
            names(&scan)
        );
    }

    /// A brace inside a string message must not move the module depth either -
    /// the same failure as the `//` one, reached through a different character.
    #[test]
    fn a_brace_inside_a_string_does_not_move_the_module_depth() {
        let scan = scan(concat!(
            "mod inner {\n",
            "    #[test]\n",
            "    fn first() { assert!(ok, \"a }} shaped message\"); }\n",
            "}\n",
            "#[test]\n",
            "fn second() {}\n",
        ));
        assert_eq!(
            names(&scan),
            vec!["inner::first".to_string(), "second".to_string()]
        );
    }

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
    // `gate_skip_list.rs` anywhere under `crates/`, which is an exemption
    // nobody wrote and nothing would report.
    let this_file = root.join(file!());
    let mut offenders = Vec::new();
    for path in files
        .into_iter()
        .map(|(path, _)| path)
        .chain(example_and_bench_files(&root))
    {
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

impl std::fmt::Debug for Scan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scan")
            .field("tests", &self.tests.len())
            .field("unresolved", &self.unresolved)
            .finish()
    }
}

/// THE TEXTLINT EXEMPTION, BOUNDED. This file is the single named exclusion in
/// the `no-brokkr-in-rust-source` rule, and the justification is a property of
/// what it CONTAINS - it reads the project config as data and spawns nothing -
/// not of where it sits. An exemption nothing re-checks is an exemption that
/// decays: a subprocess landing here later is the exact deadlock the rule was
/// written for, now unguarded, and the rule can no longer see it.
///
/// So the exemption checks itself. The file reads its own source and refuses any
/// construct that could start a process. If this fails, the fix is to move the
/// spawning code out of this file, never to widen the list below.
#[test]
fn the_excluded_file_spawns_no_subprocess() {
    let root = repo_root();
    let path = root.join(file!());
    let source = std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "this test reads its own source at {}: {err}",
            path.display()
        )
    });
    // Stripped first, so the prose above - which necessarily discusses spawning
    // - cannot trip a check that is about the CODE.
    let code = strip_comments(&path, &source);
    for construct in [
        "process::Command",
        "Command::new",
        "std::process::",
        "tokio::process",
    ] {
        assert!(
            !code.contains(construct),
            "this file names {construct:?}, and it is the single file excluded from the textlint \
             rule that keeps the build tool out of Rust source. That exclusion is justified by \
             this file spawning nothing - the deadlock the rule prevents needs a subprocess - so a \
             spawn here is unguarded by construction. Move it to a file the rule still covers."
        );
    }
}
