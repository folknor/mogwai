#!/usr/bin/env python3
"""Enforce the Document folders rules from AGENTS.md.

Two rules, over tracked files only:

1. Nothing durable cites `notes/`. AGENTS.md: "nothing durable may cite
   `notes/` - not a code comment, not `docs/`, not `reference/`", because
   `notes/` carries no truth guarantee and a code comment outlives the note.
   Durable here is every markdown file under `docs/` and `reference/`, every
   crate README, and the comment lines of every `.rs` and `.toml` file under
   `crates/`. Only comment lines of source are read: a string literal naming a
   path is data, not a citation.

2. A durable document's references resolve. In `docs/` and `reference/`, a
   relative markdown link must name a file or directory that exists, read
   against the citing file's own directory, and a backticked repository path
   must name a file or directory that exists, read from the repository root.

What counts as a citation. A citation is a path into the folder - `notes/` is
followed by at least one more path segment, as in `notes/todo.md`. The bare
folder name on its own is a description of the layout, and durable prose may
describe the layout: this repository's documents legitimately say that
transient work lives in `notes/` without sending a reader to any document in
it. A bare folder name cannot go stale the way a named document can, and a
named document is what the rule is about.

Root convention files are exempt from rule 1 outright. AGENTS.md says they are
found by convention at the root and governed by none of these folder rules, and
AGENTS.md itself names `notes/todo.md` as the backlog and records deleted
notes files by name. They are not scanned by rule 2 either, which AGENTS.md
scopes to `docs/` and `reference/`.

`research/` is vendored upstream and out of scope everywhere; a backticked
path into it is not claimed, since the vendored trees are not always present
in full.

What a backticked path is. A code span whose first segment is one of this
repository's own top-level directories and which carries at least one `/`, so
`lib.rs`, a crate name or a `::` path is never claimed. A span holding a glob,
a placeholder (`<...>`, `{...}`, `N`-style templates spelled with `*`) or
whitespace is not a path. Anything else that looks like a path in a
repository directory and is not there is reported; the marker
`doc-links-ok` on the line or one of the two above it exempts a path that is
deliberately hypothetical, so every exception is legible as one.

Prints `document conventions ok` as its last line only when both rules are
clean: brokkr's script_check matches the last line and ignores the exit code.
"""

import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
DURABLE_DOC_DIRS = ("docs", "reference")
OUT_OF_SCOPE = ("research/",)

# `notes/` followed by at least one more segment, not preceded by a path or
# word character, so `../notes/x` in another repository's spelling and a
# `mynotes/x` are both caught or skipped for the right reason.
NOTES_CITATION = re.compile(r"(?<![\w.-])notes/[\w.-][\w./-]*")
MARKDOWN_LINK = re.compile(r"\[[^\]]*\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")
CODE_SPAN = re.compile(r"`([^`\n]+)`")
PATH_SHAPE = re.compile(r"^[A-Za-z0-9_.-]+(?:/[A-Za-z0-9_.-]+)*/?$")
COMMENT_LINE = re.compile(r"^\s*(?://|#)")
EXEMPT_MARKER = "doc-links-ok"
EXEMPT_LOOKBACK = 2


def tracked():
    out = subprocess.run(
        ["git", "ls-files", "-z"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=True,
    )
    names = sorted(set(out.stdout.split("\0")) - {""})
    # A deletion not yet staged is still in the index.
    return [n for n in names if (ROOT / n).is_file()]


def exempted(lines, index):
    start = max(0, index - EXEMPT_LOOKBACK)
    return any(EXEMPT_MARKER in line for line in lines[start : index + 1])


def strip_fences(lines):
    """Blank fenced code blocks: an example config or shell transcript is not
    prose, and its paths are illustrative."""
    out = []
    fenced = False
    for line in lines:
        if line.lstrip().startswith("```"):
            fenced = not fenced
            out.append("")
            continue
        out.append("" if fenced else line)
    return out


def notes_citations(name, lines, comments_only):
    found = []
    for number, line in enumerate(lines, 1):
        if comments_only and not COMMENT_LINE.match(line):
            continue
        if exempted(lines, number - 1):
            continue
        for match in NOTES_CITATION.finditer(line):
            found.append(f"{name}:{number}: cites {match.group(0).rstrip('.,;:)')}")
    return found


def roots(names):
    return {n.split("/", 1)[0] for n in names if "/" in n}


def dangling(name, lines, top_dirs, present):
    found = []
    base = pathlib.PurePosixPath(name).parent
    for number, line in enumerate(lines, 1):
        if exempted(lines, number - 1):
            continue
        for target in MARKDOWN_LINK.findall(line):
            if ":" in target or target.startswith("#"):
                continue
            target = target.split("#", 1)[0]
            if not target:
                continue
            resolved = (ROOT / base / target).resolve()
            try:
                rel = resolved.relative_to(ROOT).as_posix()
            except ValueError:
                continue
            if rel.startswith(OUT_OF_SCOPE):
                continue
            if not resolved.exists():
                found.append((None, f"{name}:{number}: link to {target} does not exist"))
        for span in CODE_SPAN.findall(line):
            span = span.strip()
            if "/" not in span or not PATH_SHAPE.match(span):
                continue
            if span.split("/", 1)[0] not in top_dirs or span.startswith(OUT_OF_SCOPE):
                continue
            if span.rstrip("/") in present:
                continue
            found.append((span, f"{name}:{number}: backticked path {span} is not in the tree"))
    return found


def drop_ignored(candidates):
    """Drop backticked paths git ignores. `analysis/out/` is where the offline
    subcommands write by default, and a document naming a default output path
    is describing where a file will appear, not citing one that should exist."""
    links = [row for span, row in candidates if span is None]
    spans = [(span, row) for span, row in candidates if span is not None]
    if not spans:
        return links
    out = subprocess.run(
        ["git", "check-ignore", "--no-index", "--stdin"],
        cwd=ROOT,
        input="\n".join(span for span, _ in spans) + "\n",
        capture_output=True,
        text=True,
    )
    ignored = set(out.stdout.split("\n")) - {""}
    return links + [row for span, row in spans if span not in ignored]


def main():
    names = tracked()
    files = set(names)
    # Every tracked file and every directory holding one, so a backticked
    # directory such as `crates/mogwai-data/src/` resolves too.
    present = set(files)
    for n in names:
        parts = n.split("/")
        for i in range(1, len(parts)):
            present.add("/".join(parts[:i]))
    top_dirs = roots(names)

    docs = [
        n
        for n in names
        if n.split("/", 1)[0] in DURABLE_DOC_DIRS and n.endswith(".md")
    ]
    for directory in DURABLE_DOC_DIRS:
        if not any(n.startswith(directory + "/") for n in docs):
            print(f"no tracked markdown under {directory}/ - a pass over nothing is not a pass")
            return 1
    crate_docs = [n for n in names if n.startswith("crates/") and n.endswith(".md")]
    crate_sources = [
        n for n in names if n.startswith("crates/") and n.endswith((".rs", ".toml"))
    ]
    if not crate_sources:
        print("no tracked sources under crates/ - the code-comment leg would be blind")
        return 1

    cites = []
    broken = []
    for name in docs + crate_docs:
        lines = (ROOT / name).read_text(encoding="utf-8").splitlines()
        cites += notes_citations(name, lines, comments_only=False)
        if name in docs:
            broken += dangling(name, strip_fences(lines), top_dirs, present)
    broken = drop_ignored(broken)
    for name in crate_sources:
        lines = (ROOT / name).read_text(encoding="utf-8", errors="replace").splitlines()
        cites += notes_citations(name, lines, comments_only=True)

    for label, rows in (("durable-cites-notes", cites), ("dangling", broken)):
        print(f"== {label}: {len(rows)}")
        for row in rows:
            print(f"   {row}")
    if cites or broken:
        print(f"{len(cites) + len(broken)} document-convention violation(s)")
        return 1
    print(
        f"checked {len(docs) + len(crate_docs)} document(s) "
        f"and {len(crate_sources)} source file(s)"
    )
    print("document conventions ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
