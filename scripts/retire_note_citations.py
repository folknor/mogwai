#!/usr/bin/env python3
"""One-shot: repoint prose citations whose source artifacts have retired.

Git history is the archive, so citations become plain descriptions. The scope
is the Rust/config tree plus the live CLI and notes prose named by the sweep.

Three disciplines, all learned the hard way. A round of this sweep ran plain
substring replacement and corrupted a dozen durable sites, one of them a
preset's provenance table: `select_windows.py` was rewritten inside
`scripts/bless_select_windows.py`; a phrase carrying its own article was
substituted into sentences that already had one; a filename that opened a
sentence, or wore a line-number suffix, or named a subcommand after itself,
became ungrammatical the moment it turned into a noun phrase. So:

- Every substitution matches a complete citation. A name preceded or followed
  by path or word characters is not a citation and is left alone, and the
  compound filenames and command invocations that embed a retired name carry
  explicit replacements of their own, applied first.
- Four mechanical corruptions are gated before anything is written: a doubled
  article, a replacement phrase welded into a path, a line-number suffix left
  dangling off a prose phrase, and a lowercase phrase opening a sentence. Any
  of them aborts the run with the offending lines named, and the gate has no
  fixer - prose is repaired by reading.
- The gate is not sufficient and cannot be made sufficient. "under the frozen
  harness analysis/mnq_fit.py" becomes "under the frozen harness the retired
  Python fit implementation", which no pattern distinguishes from correct
  prose; that exact line shipped corrupt into a preset's claim ledger. So this
  script does not write by default. It prints every line it would change, a
  human reads them, and only `--write` applies the result. Substituting a noun
  phrase for a filename is a prose edit wearing a script's clothes, and the
  reviewer is the gate.
"""

from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parent.parent

# Live scripts that merely contain a retired name, or that are not retired at
# all. Replaced by a placeholder before the sweep and restored after it, so no
# rule can reach inside them.
PROTECTED = [
    "analysis/test_characterize.py",
    "analysis/probe_binance_trades.py",
    "analysis/probe_binance_aggtrades.py",
    "analysis/plot_tape.py",
    "analysis/asia_jump_probe.py",
    "pair_harness.py",
]

# Compound citations - a retired name inside a longer filename, or a retired
# script named with the subcommand it was invoked as. Applied before the plain
# rules, because the plain rules cannot see the surrounding token.
COMPOUND = [
    ("`scripts/bless_select_windows.py`", "the retired blessing script"),
    ("scripts/bless_select_windows.py", "the retired blessing script"),
    ("`select_windows.py features`",
     "the retired Python window-selection implementation's `features` mode"),
    ("select_windows.py features",
     "the retired Python window-selection implementation's `features` mode"),
    ("python3 analysis/check_cadence_feasible.py",
     "the retired Python cadence-feasibility implementation"),
    ("frozen harness analysis/mnq_fit.py",
     "frozen harness of the retired Python fit implementation"),
]

# Plain citations. Each is matched only as a whole path-or-filename token: the
# lookaround refuses a match glued to a word, a path separator, an underscore
# or a further extension.
PLAIN = [
    ("notes/rust-rewrite-phases.md phase", "the retired rewrite plan, phase"),
    ("notes/rust-rewrite-phases.md slice", "the retired rewrite plan, slice"),
    ("notes/rust-rewrite-phases.md", "the retired rewrite plan"),
    ("notes/mnq-tbbo-fit-spec.md", "the retired protocol-10 fit spec"),
    ("notes/mnq-generator-successor-spec.md",
     "the retired protocol-10 successor spec"),
    ("check_cadence_feasible.py",
     "the retired Python cadence-feasibility implementation"),
    ("tick_composition_ratios.py",
     "the retired Python tick-composition implementation"),
    ("fit_session_profile.py",
     "the retired Python session-profile implementation"),
    ("build_fingerprint.py", "the retired Python fingerprint implementation"),
    ("build_cadence.py", "the retired Python cadence implementation"),
    ("select_windows.py",
     "the retired Python window-selection implementation"),
    ("characterize.py", "the retired Python characterization implementation"),
    ("run_corpus.py", "the retired Python corpus driver"),
    ("mnq_fit.py", "the retired Python fit implementation"),
]

# Each entry is compiled to match the bare name and its `analysis/` form,
# optionally backticked, and never as a fragment of a longer token.
_BOUNDARY = [
    (re.compile(
        r"(?<![\w/._-])`?(?:analysis/)?" + re.escape(old) + r"`?(?![\w.])"),
     new)
    for old, new in PLAIN
]

GATES = [
    (re.compile(r"\b(?:[Tt]he|[Aa]n?)\s+the retired\b"),
     "doubled article before a replacement phrase"),
    (re.compile(r"[\w/._-]the retired\b"),
     "replacement phrase welded into a path or word"),
    (re.compile(r"the retired[^`\n]{0,80}?(?:implementation|driver|plan|spec):\d"),
     "line-number suffix left dangling off a prose phrase"),
    (re.compile(r"(?<=\w)[.!?]\s+the retired\b"),
     "lowercase replacement phrase opening a sentence"),
]

# Leading comment markers are not prose. They are blanked - not removed, so
# every offset still maps back to its line - before the gates run, both so that
# a `//!` is not read as a sentence-ending `!` and so that a sentence broken
# across two comment lines is still read as one sentence.
MARKER = re.compile(r"^(\s*)(//!|///|//|#)( ?)", re.MULTILINE)

TARGETS = [ROOT / "brokkr.toml", ROOT / "Cargo.toml", ROOT / "docs/cli.md"] + sorted(
    p
    for suffix in ("*.rs", "*.toml")
    for p in (ROOT / "crates").rglob(suffix)
) + sorted((ROOT / "notes").rglob("*.md"))


def rewrite(text: str) -> str:
    """Apply every substitution to one file's text, protections first."""
    for index, name in enumerate(PROTECTED):
        text = text.replace(name, f"\x00PROTECTED{index}\x00")
    for old, new in COMPOUND:
        text = text.replace(old, new)
    for pattern, new in _BOUNDARY:
        text = pattern.sub(new, text)
    # A backticked replacement is prose, not code: drop the ticks the citation
    # wore.
    text = re.sub(r"`(the retired [^`]+)`", r"\1", text)
    for index, name in enumerate(PROTECTED):
        text = text.replace(f"\x00PROTECTED{index}\x00", name)
    return text


def gate(path: Path, text: str) -> list[str]:
    """Every gate violation in one file's rewritten text, as reportable lines."""
    prose = MARKER.sub(lambda m: m.group(1) + " " * (len(m.group(2)) + len(m.group(3))), text)
    lines = text.splitlines()
    findings = []
    for pattern, reason in GATES:
        for match in pattern.finditer(prose):
            number = prose.count("\n", 0, match.start()) + 1
            excerpt = lines[number - 1].strip() if number <= len(lines) else ""
            findings.append(f"{path.relative_to(ROOT)}:{number}: {reason}\n    {excerpt}")
    return sorted(findings)


def changed_lines(path: Path, before: str, after: str) -> list[str]:
    """Every line the rewrite would change, old above new, for a human to read."""
    old_lines = before.splitlines()
    new_lines = after.splitlines()
    report = []
    for number, (old, new) in enumerate(zip(old_lines, new_lines), start=1):
        if old != new:
            report.append(f"{path.relative_to(ROOT)}:{number}\n  - {old}\n  + {new}")
    if len(old_lines) != len(new_lines):
        report.append(
            f"{path.relative_to(ROOT)}: line count would change "
            f"{len(old_lines)} to {len(new_lines)}; read the whole file"
        )
    return report


def main(argv: list[str]) -> int:
    write = "--write" in argv[1:]
    pending = []
    findings = []
    report = []
    for path in TARGETS:
        original = path.read_text()
        text = rewrite(original)
        findings.extend(gate(path, text))
        if text != original:
            pending.append((path, text))
            report.extend(changed_lines(path, original, text))

    if findings:
        print("refusing to write - the rewrite would leave corrupt prose:\n")
        for finding in findings:
            print(finding)
        print(f"\n{len(findings)} violations; nothing written")
        return 1

    if not pending:
        print("no citations left to repoint")
        return 0

    for line in report:
        print(line)

    if not write:
        print(
            f"\n{len(pending)} files would change, {len(report)} lines. "
            "The gates catch four mechanical corruptions and cannot catch a "
            "phrase that is merely ungrammatical where it landed - read the "
            "lines above, then re-run with --write."
        )
        return 0

    for path, text in pending:
        path.write_text(text)
        print(f"\nrewrote {path.relative_to(ROOT)}")
    print(f"{len(pending)} files changed")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
