"""Survey scanner for the all-caps prose rule.

Throwaway cleanup tooling: finds shouting emphasis in markdown prose so it
can be rewritten by hand. The exemption patterns below are the draft of
what eventually lands as a textlint in brokkr.toml, at which point this
script retires.

Usage: python3 scripts/caps_scan.py [--counts] [paths...]
Default paths: every tracked .md file.
"""

import re
import subprocess
import sys

# A "shout" is a run of two or more consecutive all-caps words (3+ letters
# each), or a single all-caps word of 5+ letters. Two-to-four-letter single
# words are overwhelmingly acronyms (CSV, JSON, HTTP, WS, PID) and free.
SHOUT = re.compile(
    r"\b[A-Z]{3,}(?:[ ,]+[A-Z]{2,})+\b"  # runs: THE VENUE DOES NOT ...
    r"|\b[A-Z]{5,}\b"                     # lone long word: IMPORTANT
)

# The draft brokkr textlint's pattern: lone 5+ words only, no run rule.
GATE = re.compile(r"\b[A-Z]{5,}\b")

EXEMPT = [
    # -- Machine text, matched by shape rather than by name. --------------
    #
    # Env vars, sentinels and constants are legitimately capitalised and no
    # allowlist could ever be finished, so exempt them by the context they
    # appear in:
    #
    #   `RUSTFLAGS`   an inline code span - the single best signal, and the
    #                 one to reach for when something is wrongly flagged
    r"`[^`]*[A-Z]{5,}",
    #   $RUSTFLAGS / ${RUSTFLAGS} / %PATH%
    r"[$%]\{?[A-Z][A-Z0-9_]*",
    #   RUSTFLAGS=... or RUSTFLAGS: ... (an assignment or a table cell)
    r"\b[A-Z][A-Z0-9_]{3,}\s*[:=]",
    #   env!("RUSTFLAGS"), std::env::var("EDITOR"), getenv, os.environ
    r"\b(?:env!|env::var(?:_os)?|getenv|environ)",
    #   URLs, and a bare HTTP request line.
    r"https?://",
    r"^\s*(?://[/!]?|\*|#+)?\s*(?:GET|PUT|POST|PATCH|DELETE|HEAD|OPTIONS)\s+/",
    #
    # SCREAMING_SNAKE_CASE needs no entry at all: \b does not break at _,
    # so no run of 5+ caps in REQUEST_TIMEOUT_SECS is ever whole-word and
    # the pattern never fires on it. Only single-word names are exposed,
    # and the three rules above cover how those are actually written.

    # -- Idiomatic section markers, not emphasis. --------------------------
    r"^\s*(?://[/!]?|\*|#+)?\s*(?:SAFETY|PANICS|ERRORS|INVARIANT|WARNING|DEPRECATED)\b",

    # -- Acronyms and proper nouns that are legitimately 5+ letters. -------
    # Short by design: prose acronyms are mostly 2-4 letters and clear the
    # threshold for free. Bare env-var mentions that escape the shape rules
    # above (a sentence naming RUSTFLAGS with no backticks) belong here too.
    r"\b(?:ASCII|HTTPS|README|LICENSE|SQLITE|POSIX|JSONL|MSRV|NOTICE"
    r"|RUSTFLAGS|EDITOR|PAGER|VISUAL|LOGNAME|HOSTNAME|RUSTUP"
    r"|MOGWAI|BTCUSDT|ETHUSDT|SOLUSDT|GARCH|CPYTHON|XXH128|ETXTBSY"
    r"|AGENTS|CLAUDE|TBBO|CARGO_BIN_EXE_mogwai)\b",
]
EXEMPT_RE = [re.compile(p) for p in EXEMPT]


def spans(pattern, line):
    return [m.span() for m in pattern.finditer(line)]


def scan_line(line):
    """Return shout matches on the line not covered by any exemption."""
    exempt_spans = []
    for rx in EXEMPT_RE:
        exempt_spans.extend(spans(rx, line))
    hits = []
    for m in SHOUT.finditer(line):
        a, b = m.span()
        if any(ea <= a and b <= eb or (ea <= a < eb) for ea, eb in exempt_spans):
            continue
        hits.append(m.group(0))
    return hits


def tracked_markdown():
    out = subprocess.run(
        ["git", "ls-files", "*.md"], capture_output=True, text=True, check=True,
        cwd=None,
    )
    return out.stdout.split()


def main():
    args = sys.argv[1:]
    counts_only = "--counts" in args
    if "--gate" in args:
        global SHOUT
        SHOUT = GATE
    paths = [a for a in args if not a.startswith("--")] or tracked_markdown()

    total = 0
    per_file = {}
    in_fence = False
    for path in paths:
        n = 0
        in_fence = False
        try:
            text = open(path, encoding="utf-8").read()
        except OSError as e:
            print(f"{path}: {e}", file=sys.stderr)
            continue
        for i, line in enumerate(text.splitlines(), 1):
            if line.lstrip().startswith("```"):
                in_fence = not in_fence
                continue
            if in_fence:
                continue
            hits = scan_line(line)
            if hits:
                n += len(hits)
                if not counts_only:
                    joined = ", ".join(hits)
                    print(f"{path}:{i}: {joined}")
        if n:
            per_file[path] = n
            total += n

    if counts_only:
        for path, n in sorted(per_file.items(), key=lambda kv: -kv[1]):
            print(f"{n:5d}  {path}")
    print(f"total: {total} hits in {len(per_file)} files", file=sys.stderr)


if __name__ == "__main__":
    main()
