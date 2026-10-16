#!/usr/bin/env python3
"""Assertion-free `#[test]` functions, as a source-text gate.

A test with no assertion is the degenerate guard: it cannot fail, it counts
toward the suite total, and a green summary looks identical with it present or
absent. Nothing else in the toolchain flags one - not dead-code analysis, not
clippy, not the coverage audit, which certifies that every test that EXISTS
runs rather than that every test asserts anything.

It arrives specifically when a deletion guts a test instead of removing it: the
assertions go with the code they covered and an empty `fn name() {}` is left
behind, passing forever as coverage of something that no longer exists. Adapted
from piners, where exactly that happened.

What this guarantees, exactly. The scan is textual and its domain is every
tracked `.rs` file under `crates/`, read from `git ls-files` so a test in a new
file is seen once it is added. It flags a `#[test]` (or `#[tokio::test]` and
any other `path::test` attribute, with or without arguments) whose body neither
asserts nor calls: no `assert*`, no `panic!`/`unreachable!`/`todo!`/
`unimplemented!`, no unwrap/expect, and no call expression of any kind.

The second half of that conjunction is what makes the gate usable. Delegating
the assertion to a helper - a fixture runner, a lockstep driver, a socket
harness - is legitimate and pervasive, and an assertion-only rule would demand
an escape marker on every such test, which teaches people to add the marker
reflexively. A body that calls nothing cannot be delegating to anything, so the
conjunction costs no real coverage.

Not covered, stated so nobody reads more into a green run:

- A test that calls something and asserts nothing, where the callee does not
  assert either. It does work, so it is not the gutted-body shape, and telling
  an asserting helper from a non-asserting one needs call-graph resolution a
  text scan does not have.
- A test that asserts something vacuously true (`assert!(true)`, or an
  `assert_eq!` over two constants). Shape 1 territory, and a different problem:
  this gate answers "is there an assertion at all", not "does it constrain".
- A macro that expands to both the `#[test]` and its body. The scan sees the
  macro invocation, not the generated function.
- `#[should_panic]` tests are exempt by construction: the panic IS the
  assertion, and an empty body fails such a test rather than passing it, so the
  shape cannot occur there.

`assert-free-ok` on the fn or its attributes exempts a test the rule reaches
wrongly. It is expected to stay unused; if it starts accumulating, the rule is
mis-scoped rather than the tests being unusual.

The scanners are plain functions over text and self-test on every run before any
real file is read. A guard of this kind is worth having only if it is itself
tested - the vacuous-gate family in `reference/test-doctrine.md`. If a
self-test fails the gate fails, and no result about the real tree is reported.

Everything reads files checked into the repository, sorted by path."""

import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent

MARKER = "assert-free-ok"

# Panic-capable constructs. Generous by design: the target is the empty body.
ASSERTION = re.compile(
    r"""
    \b(?:
        assert | assert_eq | assert_ne | assert_matches
      | debug_assert | debug_assert_eq | debug_assert_ne
      | panic | unreachable | todo | unimplemented
    )\s*!
    |
    \.\s*(?:unwrap|expect|unwrap_err|expect_err|unwrap_none)\s*\(
    """,
    re.VERBOSE,
)

# Any call expression: `name(`, `Type::assoc(`, `.method(`, or a macro in any of
# its three delimiter forms. A body that spells one is doing work, so it is not
# the gutted-body shape even when it asserts nothing itself - the assertion may
# be inside the callee.
CALL = re.compile(
    r"""
      [A-Za-z0-9_\]\)]\s*!\s*[\(\[\{]   # macro!(..) macro![..] macro!{..}
    | [A-Za-z0-9_\]\)>]\s*\(            # name(..) Type::assoc(..) f::<T>(..)
    | \.\s*[A-Za-z_][A-Za-z0-9_]*\s*\(  # .method(..)
    """,
    re.VERBOSE,
)

# `#[test]`, `#[tokio::test]`, `#[tokio::test(flavor = "multi_thread")]`. A
# trailing comment is allowed so the marker can sit on the attribute line.
TEST_ATTR = re.compile(r"^\s*#\[(?:\w+::)*test(?:\(.*\))?\]\s*(?://.*)?$")
SHOULD_PANIC = re.compile(r"^\s*#\[should_panic")
FN_DECL = re.compile(r"^\s*(?:pub\s+)?(?:async\s+)?fn\s+([A-Za-z0-9_]+)")


RAW_STRING = re.compile(r'(?:b|c)?r(#*)"')
CHAR_LIT = re.compile(r"'(?:\\.|[^\\'])'")


def strip_noise(text):
    """Blank out string/char literals and comments so brace matching is honest.

    Replaces their contents with spaces rather than deleting, so byte offsets
    and line numbers are preserved for the caller.

    Raw strings are handled explicitly. Without that, `r#"...("..."#` reads as a
    plain string opening at the inner quote, every later offset is wrong, and
    `body_span` returns a truncated body that appears to call nothing - which is
    a false positive on exactly the tests that embed Pine source. Found by
    running the scan against this tree, not by review.

    The scan jumps from one token start to the next with a regex rather than
    stepping a character at a time, which was most of this gate's run time.
    A raw string opens only where the preceding character is not part of an
    identifier, so `br"` after a letter is a plain string at its quote."""
    out = []
    i = 0
    n = len(text)
    while i < n:
        token = TOKEN_START.search(text, i)
        if token is None:
            out.append(text[i:])
            break
        start = token.start()
        out.append(text[i:start])
        kind = token.lastgroup
        if kind == "raw":
            closing = '"' + token.group("hashes")
            end = text.find(closing, token.end())
            end = n if end < 0 else end + len(closing)
            out.append(blank(text[start:end]))
        elif kind == "char":
            end = token.end()
            out.append(" " * (end - start))
        elif kind == "string":
            body = STRING_BODY.match(text, token.end())
            end = body.end()
            # An escape blanks both of its characters, a newline included,
            # as a plain character does not.
            out.append(" " + STRING_PIECE.sub(blank_piece, body.group(0)))
        elif kind == "line":
            end = text.find("\n", start)
            end = n if end < 0 else end
            out.append(" " * (end - start))
        else:
            depth = 1
            end = token.end()
            while depth:
                mark = COMMENT_MARK.search(text, end)
                if mark is None:
                    end = n
                    break
                depth += 1 if mark.group(0) == "/*" else -1
                end = mark.end()
            out.append(blank(text[start:end]))
        i = end
    return "".join(out)


TOKEN_START = re.compile(
    r"""
      (?P<raw>(?<![A-Za-z0-9_])(?:b|c)?r(?P<hashes>\#*)")
    | (?P<char>'(?:\\.|[^\\'])')
    | (?P<string>")
    | (?P<line>//)
    | (?P<block>/\*)
    """,
    re.VERBOSE,
)
# A plain string's body through its closing quote, or to the end of the text
# when it never closes.
STRING_BODY = re.compile(r'(?:\\.|[^"\\])*(?:"|\\?\Z)', re.DOTALL)
STRING_PIECE = re.compile(r"\\.|\\\Z|[^\n]", re.DOTALL)
COMMENT_MARK = re.compile(r"/\*|\*/")
NOT_NEWLINE = re.compile(r"[^\n]")


def blank(segment):
    return NOT_NEWLINE.sub(" ", segment)


def blank_piece(piece):
    return "  " if piece.group(0).startswith("\\") and len(piece.group(0)) == 2 else " "


def body_span(clean, start):
    """Offsets of the balanced `{...}` beginning at or after `start`, or None."""
    i = clean.find("{", start)
    if i < 0:
        return None
    depth = 0
    j = i
    while j < len(clean):
        if clean[j] == "{":
            depth += 1
        elif clean[j] == "}":
            depth -= 1
            if depth == 0:
                return (i, j + 1)
        j += 1
    return None


def find_assertion_free_tests(text):
    """Names and lines of `#[test]` fns whose body neither asserts nor calls."""
    clean = strip_noise(text)
    lines = text.splitlines()
    starts = [0]
    for line in lines:
        starts.append(starts[-1] + len(line) + 1)
    found = []
    for idx, line in enumerate(lines):
        if not TEST_ATTR.match(line):
            continue
        # Walk the attribute block plus any doc comments to the `fn`.
        exempt = False
        j = idx
        while j < len(lines) and not FN_DECL.match(lines[j]):
            if SHOULD_PANIC.match(lines[j]) or MARKER in lines[j]:
                exempt = True
            j += 1
        if j >= len(lines) or exempt:
            continue
        if MARKER in lines[j]:
            continue
        name = FN_DECL.match(lines[j]).group(1)
        offset = starts[j]
        span = body_span(clean, offset)
        if span is None:
            continue
        body = clean[span[0] : span[1]]
        if not ASSERTION.search(body) and not CALL.search(body):
            found.append((name, j + 1))
    return found


def self_test():
    """Every known evasion, fed to the scanner, required to fire or stay quiet."""
    must_fire = {
        "empty body": "#[test]\nfn a() {}\n",
        "whitespace body": "#[test]\nfn a() {\n\n}\n",
        "let of a literal only": "#[test]\nfn a() {\n    let x = 1;\n}\n",
        "assert in a comment": "#[test]\nfn a() {\n    // assert_eq!(1, 1);\n}\n",
        "assert in a string": '#[test]\nfn a() {\n    let s = "assert!(x)";\n}\n',
        "call in a comment": "#[test]\nfn a() {\n    // run_case(1);\n}\n",
        "qualified attr": "#[tokio::test]\nfn a() {}\n",
        "qualified attr with args": (
            '#[tokio::test(flavor = "multi_thread")]\nasync fn a() {}\n'
        ),
        "assert only in a sibling": (
            "#[test]\nfn a() {}\n\n#[test]\nfn b() {\n    assert!(x);\n}\n"
        ),
        "nested braces, no call": "#[test]\nfn a() {\n    if x {\n        y;\n    }\n}\n",
        "raw string hiding nothing": '#[test]\nfn a() {\n    let s = r#"run_case(1)"#;\n}\n',
    }
    must_stay_quiet = {
        "assert_eq": "#[test]\nfn a() {\n    assert_eq!(1, 1);\n}\n",
        "assert": "#[test]\nfn a() {\n    assert!(x);\n}\n",
        "panic": "#[test]\nfn a() {\n    if x { panic!(\"no\") }\n}\n",
        "unwrap": "#[test]\nfn a() {\n    f().unwrap();\n}\n",
        "expect": '#[test]\nfn a() {\n    f().expect("m");\n}\n',
        "unreachable": "#[test]\nfn a() {\n    unreachable!();\n}\n",
        "debug_assert": "#[test]\nfn a() {\n    debug_assert!(x);\n}\n",
        # The delegating shape the conjunction exists to clear.
        "bare call, assertion in the helper": "#[test]\nfn a() {\n    run_case(1);\n}\n",
        "method call": "#[test]\nfn a() {\n    thing.run();\n}\n",
        "associated call": "#[test]\nfn a() {\n    Harness::drive(2);\n}\n",
        "let bound to a call": "#[test]\nfn a() {\n    let x = f();\n}\n",
        "macro call": "#[test]\nfn a() {\n    check_all![x];\n}\n",
        "call nested in a block": (
            "#[test]\nfn a() {\n    if x {\n        run_case(1);\n    }\n}\n"
        ),
        "should_panic exempt": "#[test]\n#[should_panic]\nfn a() {\n    let x = 1;\n}\n",
        "marker on fn": f"#[test]\nfn a() {{ // {MARKER}\n    let x = 1;\n}}\n",
        "marker on attr": f"#[test] // {MARKER}\nfn a() {{\n    let x = 1;\n}}\n",
        "assert nested deep": (
            "#[test]\nfn a() {\n    for i in 0..2 {\n        assert!(i < 2);\n    }\n}\n"
        ),
        "not a test fn": "fn a() {}\n",
        "ignored non-test": "#[cfg(test)]\nfn helper() {}\n",
        # The two shapes the tree itself found after the first narrowing.
        "turbofish call": "#[test]\nfn a() {\n    assert_send::<State>();\n}\n",
        "call after a raw string with quotes and braces": (
            '#[test]\nfn a() {\n    let src = r#"strategy("x", y={1})"#;\n'
            "    run_case(src);\n}\n"
        ),
        "raw string with inner hashes": (
            '#[test]\nfn a() {\n    let s = r##"a "# b"##;\n    run_case(s);\n}\n'
        ),
        "char literal holding a quote": (
            "#[test]\nfn a() {\n    let q = '\"';\n    run_case(q);\n}\n"
        ),
    }
    failures = []
    for label, src in must_fire.items():
        if not find_assertion_free_tests(src):
            failures.append(f"self-test: scanner missed [{label}]")
    for label, src in must_stay_quiet.items():
        if find_assertion_free_tests(src):
            failures.append(f"self-test: scanner false-fired on [{label}]")
    return failures


def main():
    failures = self_test()
    if failures:
        for f in failures:
            print(f)
        print("test-assertion scan self-test FAILED; no tree result reported")
        return 1

    violations = []
    out = subprocess.run(
        ["git", "ls-files", "-z", "--", "crates"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=True,
    )
    files = [
        ROOT / name
        for name in sorted(set(out.stdout.split("\0")) - {""})
        if name.endswith(".rs") and (ROOT / name).is_file()
    ]
    if not files:
        print("no tracked .rs files found under crates/")
        return 1
    for path in files:
        text = path.read_text(encoding="utf-8", errors="replace")
        if "#[test]" not in text and "::test]" not in text:
            continue
        for name, line in find_assertion_free_tests(text):
            rel = path.relative_to(ROOT)
            violations.append(f"{rel}:{line}: {name} asserts nothing")

    if violations:
        for v in violations:
            print(v)
        print(
            f"{len(violations)} assertion-free test(s); "
            f"add an assertion or mark `{MARKER}` if the assertion is in a helper"
        )
        return 1

    print(f"scanned {len(files)} file(s)")
    print("test assertions ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
