#!/usr/bin/env python3
"""Splice a generated envelope block into a venue preset.

The block `tape-v2 envelope-toml` writes has two parts: an
`[instrument.calendar.envelope]` table and a `[provenance]` table holding
that table's entries. This puts the first in place of any existing
envelope section (or before `[provenance]` when there is none) and
replaces the preset's existing `calendar.envelope.*` provenance lines
with the new ones. Everything else in the preset is left byte for byte.

    python3 analysis/tape-v2/splice_envelope.py PRESET.toml BLOCK.toml
"""

from __future__ import annotations

import sys
from pathlib import Path

ENVELOPE_HEADER = "[instrument.calendar.envelope]"
PROVENANCE_HEADER = "[provenance]"
PROVENANCE_PREFIX = '"calendar.envelope.'


def split_block(block: str) -> tuple[list[str], list[str]]:
    lines = block.splitlines()
    cut = lines.index(PROVENANCE_HEADER)
    return lines[:cut], [ln for ln in lines[cut + 1 :] if ln.strip()]


def strip_envelope(lines: list[str]) -> list[str]:
    """Remove an existing envelope section and its leading comment run."""
    if ENVELOPE_HEADER not in lines:
        return lines
    start = lines.index(ENVELOPE_HEADER)
    while start > 0 and lines[start - 1].startswith("#"):
        start -= 1
    end = start + 1
    while end < len(lines) and not lines[end].startswith("["):
        end += 1
    return lines[:start] + lines[end:]


def main(argv: list[str]) -> None:
    if len(argv) != 2:
        sys.exit(__doc__)
    preset_path = Path(argv[0])
    block_lines, provenance_lines = split_block(Path(argv[1]).read_text())
    lines = strip_envelope(preset_path.read_text().splitlines())
    lines = [ln for ln in lines if not ln.startswith(PROVENANCE_PREFIX)]
    if PROVENANCE_HEADER not in lines:
        sys.exit(f"{preset_path} has no {PROVENANCE_HEADER} table")
    at = lines.index(PROVENANCE_HEADER)
    while at > 0 and lines[at - 1].strip() == "":
        at -= 1
    lines = lines[:at] + block_lines + lines[at:] + provenance_lines
    preset_path.write_text("\n".join(lines) + "\n")
    print(
        f"spliced {len(block_lines)} envelope lines and "
        f"{len(provenance_lines)} provenance lines into {preset_path}"
    )


if __name__ == "__main__":
    main(sys.argv[1:])
