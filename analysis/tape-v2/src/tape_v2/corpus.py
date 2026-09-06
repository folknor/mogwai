"""Index the Databento batch corpus on disk.

Every batch job lands as one directory `GLBX-<date>-<id>/` holding a
`metadata.json` (the query as submitted), a `manifest.json`, a
`condition.json`, and one `glbx-mdp3-<yyyymmdd>.<schema>.dbn.zst` per day
carrying every instrument in the job. Nothing on disk is organised by
schema or date, so every experiment would otherwise rescan five hundred
directories to find its inputs. This module scans once and writes a
file-level index.

The backfill fetched at week, month and year granularity and the top-up
daemon refetches a trailing window at day granularity, so the same day can
be present in two jobs. The index keeps both rows and `coverage()` reports
the overlap; a reader picks one by a rule it states, never by directory
order.

The date in a file name is the vendor's split day. Whether that boundary
is midnight UTC or something else is not asserted here; a consumer that
needs the boundary reads the first and last record.
"""

from __future__ import annotations

import json
import os
import re
import sys
from datetime import UTC, datetime
from pathlib import Path

import polars as pl

CORPUS_ROOT = Path(os.environ.get("TAPE_CORPUS", "/speilelg/databento"))
# The project root's `data/` (gitignored), on whichever host runs this: the
# run host keeps the corpus-derived artifacts there, and the tree's copy
# holds what is pulled back for charting.
DEFAULT_DATA = Path(__file__).resolve().parents[2] / "data"
DATA_DIR = Path(os.environ.get("TAPE_DATA", str(DEFAULT_DATA)))
INDEX_PATH = DATA_DIR / "corpus-index.parquet"

JOB_RE = re.compile(r"^GLBX-\d{8}-[A-Z0-9]+$")
FILE_RE = re.compile(
    r"^glbx-mdp3-(\d{8})(?:-(\d{8}))?\.([a-z0-9-]+)\.dbn\.zst$"
)
SIDE_FILES = {
    "metadata.json",
    "manifest.json",
    "condition.json",
    ".dbnget-lock",
}


def _ns_to_date(ns: int) -> str:
    return datetime.fromtimestamp(ns / 1e9, tz=UTC).strftime("%Y-%m-%d")


def scan(root: Path = CORPUS_ROOT) -> tuple[pl.DataFrame, list[str]]:
    """Walk the corpus root.

    Returns the file index and a list of debris paths: anything that is not
    a batch job directory or not a data file inside one.
    """
    rows: list[dict] = []
    debris: list[str] = []
    for job_dir in sorted(root.iterdir()):
        if not job_dir.is_dir() or not JOB_RE.match(job_dir.name):
            debris.append(str(job_dir))
            continue
        meta_path = job_dir / "metadata.json"
        if not meta_path.exists():
            debris.append(str(job_dir) + " (no metadata.json)")
            continue
        meta = json.loads(meta_path.read_text())
        query = meta["query"]
        custom = meta.get("customizations", {})
        symbols = query.get("symbols") or []
        for entry in sorted(job_dir.iterdir()):
            name = entry.name
            if name in SIDE_FILES:
                continue
            match = FILE_RE.match(name)
            if not match:
                debris.append(str(entry))
                continue
            day_start, day_end, file_schema = match.groups()
            rows.append(
                {
                    "job_id": meta["job_id"],
                    "schema": query["schema"],
                    "file_schema": file_schema,
                    "day_start": day_start,
                    "day_end": day_end or day_start,
                    "path": str(entry),
                    "bytes": entry.stat().st_size,
                    "query_start": _ns_to_date(query["start"]),
                    "query_end": _ns_to_date(query["end"]),
                    "split_duration": custom.get("split_duration"),
                    "stype_in": query.get("stype_in"),
                    "n_symbols": len(symbols),
                    "symbols": ",".join(symbols),
                }
            )
    frame = pl.DataFrame(rows)
    return frame, debris


def coverage(frame: pl.DataFrame) -> pl.DataFrame:
    """Per-schema summary: day span, file and byte totals, duplicate days."""
    per_day = (
        frame.group_by("schema", "day_start")
        .len()
        .rename({"len": "copies"})
    )
    dups = (
        per_day.filter(pl.col("copies") > 1)
        .group_by("schema")
        .len()
        .rename({"len": "days_duplicated"})
    )
    return (
        frame.group_by("schema")
        .agg(
            pl.col("day_start").min().alias("first_day"),
            pl.col("day_end").max().alias("last_day"),
            pl.col("day_start").n_unique().alias("days"),
            pl.len().alias("files"),
            (pl.col("bytes").sum() / 1e9).round(1).alias("gb"),
        )
        .join(dups, on="schema", how="left")
        .with_columns(pl.col("days_duplicated").fill_null(0))
        .sort("gb", descending=True)
    )


def build_index() -> None:
    frame, debris = scan()
    DATA_DIR.mkdir(parents=True, exist_ok=True)
    frame.write_parquet(INDEX_PATH)
    summary = coverage(frame)
    with pl.Config(tbl_rows=-1, tbl_cols=-1, tbl_width_chars=200):
        print(summary)
    mismatch = frame.filter(pl.col("schema") != pl.col("file_schema"))
    if mismatch.height:
        print(
            "schema mismatch between query and file name in "
            f"{mismatch.height} files",
            file=sys.stderr,
        )
    if debris:
        print(
            f"{len(debris)} debris entries outside the batch layout:",
            file=sys.stderr,
        )
        for item in debris[:20]:
            print("  " + item, file=sys.stderr)
    jobs = frame["job_id"].n_unique()
    print(f"wrote {INDEX_PATH}: {frame.height} files in {jobs} jobs")


def load_index() -> pl.DataFrame:
    if not INDEX_PATH.exists():
        raise SystemExit(f"{INDEX_PATH} missing; run `tape-v2 index` first")
    return pl.read_parquet(INDEX_PATH)
