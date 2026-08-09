#!/usr/bin/env python3
"""One-shot: repoint code/config comments that cite notes documents retired
on 2026-08-09 (the rewrite phase records and the protocol-10 spec pair).
Git history is the archive, so the citations become plain descriptions.

Scope: crates/ and brokkr.toml only. notes/, DATA-PURCHASE-REPORT.md and the
frozen 12a spec are deliberately untouched. Delete this script after review
or keep it as the record of the sweep.
"""

from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# Order matters: backticked and suffixed forms first.
SUBSTITUTIONS = [
    ("`notes/rust-rewrite-phases.md`", "the retired rewrite plan"),
    ("notes/rust-rewrite-phases.md phase", "the retired rewrite plan, phase"),
    ("notes/rust-rewrite-phases.md slice", "the retired rewrite plan, slice"),
    ("notes/rust-rewrite-phases.md", "the retired rewrite plan"),
    ("notes/mnq-tbbo-fit-spec.md", "the retired protocol-10 fit spec"),
    ("notes/mnq-generator-successor-spec.md",
     "the retired protocol-10 successor spec"),
]

TARGETS = [ROOT / "brokkr.toml"] + sorted(
    p
    for suffix in ("*.rs", "*.toml")
    for p in (ROOT / "crates").rglob(suffix)
)


def main() -> None:
    changed = 0
    for path in TARGETS:
        text = path.read_text()
        original = text
        for old, new in SUBSTITUTIONS:
            text = text.replace(old, new)
        if text != original:
            path.write_text(text)
            changed += 1
            print(f"rewrote {path.relative_to(ROOT)}")
    print(f"{changed} files changed")


if __name__ == "__main__":
    main()
