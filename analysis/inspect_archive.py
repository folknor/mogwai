"""Streaming, read-only contract inspector for Binance daily ZIP archives.

Reports FACTS about a file. It does not interpret them: it will tell you a
column is a 13-digit integer in a plausible epoch range, and it will NOT tell
you that column is the transaction time. Adopting a schema is a decision for the
report, where it can be justified and argued with; a script that guesses reads
like a measurement.

Read-only and archive-aware: members are streamed from inside the ZIP and never
extracted, so an 88 MB archive costs no disk and bounded memory.

    python3 analysis/inspect_archive.py selftest
    python3 analysis/inspect_archive.py inspect <archive.zip> [more.zip ...]

Deliberate limitation, stated rather than hidden: duplicate detection is
ADJACENT-ONLY. In physical order that is complete for a sorted file, and the
monotonicity report says whether the file is sorted. Full-file duplicate
detection would need an unbounded set, and an inspector that quietly grows to
the size of its input is not one you can point at a large archive.
"""

import datetime as dt
import hashlib
import io
import json
import os
import sys
import zipfile

# Epoch magnitudes for a plausible 2015-2035 instant, by digit width.
_UNIT_BY_WIDTH = {10: "seconds", 13: "milliseconds", 16: "microseconds", 19: "nanoseconds"}
_DIVISOR = {"seconds": 1, "milliseconds": 10**3, "microseconds": 10**6, "nanoseconds": 10**9}
_PLAUSIBLE = (dt.datetime(2015, 1, 1, tzinfo=dt.timezone.utc).timestamp(),
              dt.datetime(2035, 1, 1, tzinfo=dt.timezone.utc).timestamp())


def _looks_like_epoch(value, width):
    unit = _UNIT_BY_WIDTH.get(width)
    if unit is None:
        return None
    seconds = value / _DIVISOR[unit]
    if _PLAUSIBLE[0] <= seconds <= _PLAUSIBLE[1]:
        return unit
    return None


def _is_int(token):
    return token.lstrip("-").isdigit() and token.lstrip("-") != ""


class ColumnFacts:
    """Per-column observations accumulated in one streaming pass."""

    def __init__(self, index):
        self.index = index
        self.all_int = True
        self.widths = set()
        self.minimum = None
        self.maximum = None
        self.monotonic_nondecreasing = True
        self.strictly_increasing = True
        self.first_violation = None
        self.prev = None
        self.adjacent_duplicates = 0
        self.max_tie_run = 1
        self._tie_run = 1
        self.regressions = 0
        self.gaps = 0

    def observe(self, token, row_index):
        if not _is_int(token):
            self.all_int = False
            return
        value = int(token)
        self.widths.add(len(token.lstrip("-")))
        self.minimum = value if self.minimum is None else min(self.minimum, value)
        self.maximum = value if self.maximum is None else max(self.maximum, value)
        if self.prev is not None:
            if value < self.prev:
                self.monotonic_nondecreasing = False
                self.strictly_increasing = False
                self.regressions += 1
                if self.first_violation is None:
                    self.first_violation = {
                        "row": row_index,
                        "previous": self.prev,
                        "value": value,
                    }
            elif value == self.prev:
                self.strictly_increasing = False
                self.adjacent_duplicates += 1
                self._tie_run += 1
                self.max_tie_run = max(self.max_tie_run, self._tie_run)
            else:
                self._tie_run = 1
                # A gap is a jump of more than one. Reported as its own fact:
                # ids are NOT assumed contiguous, and a gap is not a defect.
                if value - self.prev > 1:
                    self.gaps += 1
        self.prev = value

    def summary(self):
        out = {
            "index": self.index,
            "all_integer": self.all_int,
            "widths": sorted(self.widths),
            "min": self.minimum,
            "max": self.maximum,
            "monotonic_nondecreasing": self.monotonic_nondecreasing,
            "strictly_increasing": self.strictly_increasing,
            "first_violation": self.first_violation,
            "adjacent_duplicate_values": self.adjacent_duplicates,
            "max_tie_run": self.max_tie_run,
            "regressions": self.regressions,
            "gaps_over_one": self.gaps,
        }
        if self.all_int and len(self.widths) == 1 and self.minimum is not None:
            width = next(iter(self.widths))
            unit = _looks_like_epoch(self.minimum, width)
            if unit and _looks_like_epoch(self.maximum, width):
                out["epoch_candidate"] = {
                    "inferred_unit": unit,
                    "utc_start": dt.datetime.fromtimestamp(
                        self.minimum / _DIVISOR[unit], dt.timezone.utc
                    ).isoformat(),
                    "utc_end": dt.datetime.fromtimestamp(
                        self.maximum / _DIVISOR[unit], dt.timezone.utc
                    ).isoformat(),
                }
        return out


def _hash_stream(reader, chunk=1 << 20):
    digest = hashlib.sha256()
    total = 0
    while True:
        block = reader.read(chunk)
        if not block:
            break
        digest.update(block)
        total += len(block)
    return digest.hexdigest(), total


def _file_sha256(path):
    with open(path, "rb") as fh:
        return _hash_stream(fh)[0]


def inspect_member(zf, info):
    """One streaming pass over one member."""
    with zf.open(info) as raw:
        member_sha, member_bytes = _hash_stream(raw)

    columns = {}
    first_row = None
    last_row = None
    rows = 0
    field_counts = {}
    malformed = 0
    adjacent_full_duplicates = 0
    previous_line = None

    header_present = False
    with zf.open(info) as raw:
        stream = io.TextIOWrapper(raw, encoding="utf-8", errors="replace", newline="")
        for line in stream:
            line = line.rstrip("\r\n")
            if not line:
                continue
            fields = line.split(",")
            field_counts[len(fields)] = field_counts.get(len(fields), 0) + 1
            if first_row is None:
                # Raw STRINGS are retained. Converting here would hide leading
                # zeros, trailing precision and scientific notation - exactly the
                # formatting a parser has to get right.
                first_row = fields
                header_present = any(not _is_number(token) for token in fields)
                if header_present:
                    # A header's labels are not observations. Feeding them to
                    # the column accumulators makes every column non-integer and
                    # silently suppresses every epoch inference - which presents
                    # as "this file has no timestamps".
                    rows += 1
                    last_row = fields
                    previous_line = line
                    continue
            if line == previous_line:
                adjacent_full_duplicates += 1
            previous_line = line
            for i, token in enumerate(fields):
                columns.setdefault(i, ColumnFacts(i)).observe(token, rows)
            last_row = fields
            rows += 1

    return {
        "member": info.filename,
        "compressed_bytes": info.compress_size,
        "uncompressed_bytes": info.file_size,
        "member_sha256": member_sha,
        "member_stream_bytes": member_bytes,
        "rows_including_header": rows,
        "field_count_histogram": {str(k): v for k, v in sorted(field_counts.items())},
        "field_count_varies": len(field_counts) > 1,
        "malformed_rows": malformed,
        "header_present": header_present,
        "first_row_raw": first_row,
        "last_row_raw": last_row,
        "adjacent_full_duplicate_rows": adjacent_full_duplicates,
        "columns": [columns[i].summary() for i in sorted(columns)],
    }


def _is_number(token):
    try:
        float(token)
    except ValueError:
        return False
    return True


def inspect_archive(path):
    record = {
        "path": os.path.relpath(path, os.path.dirname(os.path.dirname(os.path.abspath(__file__)))),
        "zip_sha256": _file_sha256(path),
        "zip_bytes": os.path.getsize(path),
    }
    with zipfile.ZipFile(path) as zf:
        infos = zf.infolist()
        record["member_count"] = len(infos)
        record["members"] = [inspect_member(zf, info) for info in infos]
    return record


def coverage_overlap(records):
    """Overlap between archives, using EPOCH CANDIDATE columns only.

    Which candidate is transaction time is NOT decided here - if a file exposes
    several, all are reported and the report chooses. If it exposes only one,
    that limitation is stated rather than papered over by assuming the single
    column must be the transaction time.
    """
    spans = []
    for record in records:
        for member in record["members"]:
            candidates = [c for c in member["columns"] if "epoch_candidate" in c]
            spans.append(
                {
                    "member": member["member"],
                    "epoch_candidate_count": len(candidates),
                    "single_timestamp_only": len(candidates) == 1,
                    "candidates": [
                        {
                            "column_index": c["index"],
                            "unit": c["epoch_candidate"]["inferred_unit"],
                            "utc_start": c["epoch_candidate"]["utc_start"],
                            "utc_end": c["epoch_candidate"]["utc_end"],
                        }
                        for c in candidates
                    ],
                }
            )
    return spans


def fail_closed(records):
    """Refuse to proceed on evidence a parser cannot safely be built against."""
    problems = []
    for record in records:
        for member in record["members"]:
            if member["field_count_varies"]:
                problems.append(
                    f"{member['member']}: field count varies "
                    f"{member['field_count_histogram']}"
                )
            if member["malformed_rows"]:
                problems.append(f"{member['member']}: {member['malformed_rows']} malformed rows")
    return problems


def inspect_paths(paths):
    records = [inspect_archive(p) for p in paths]
    out = {
        "archives": records,
        "coverage": coverage_overlap(records),
        "fail_closed": fail_closed(records),
    }
    print(json.dumps(out, indent=1))
    if out["fail_closed"]:
        raise SystemExit("FAIL CLOSED: " + "; ".join(out["fail_closed"]))
    return out


# ---------------------------------------------------------------------------
# Fixture. A tiny synthetic ZIP pinning every behaviour the inspector claims,
# so the inspector itself is not the unaudited part of the pipeline.
# ---------------------------------------------------------------------------

_GOOD = "\n".join(
    [
        "id,txn_time,event_time,price,qty",
        # ids 100, 101, 103, 102, 104, 104. Two GAPS (101->103 and 102->104),
        # one REGRESSION (103->102), one adjacent duplicate (104->104). Gaps and
        # regressions are counted as SEPARATE facts because a gap is not a
        # defect - update ids are not required to be contiguous - while a
        # regression means the file is not in id order.
        # txn_time carries two ties, with ids ordered within each tie.
        "100,1711756800000,1711756800500,70000.10,0.001",
        "101,1711756800000,1711756800600,70000.20,0.002",
        "103,1711756800001,1711756800700,70000.30,0.003",
        "102,1711756800002,1711756800800,70000.40,0.004",
        "104,1711756800002,1711756800900,70000.40,0.004",
        "104,1711756800002,1711756800900,70000.40,0.004",
    ]
)

_MALFORMED = "\n".join(
    [
        "id,txn_time,event_time,price,qty",
        "200,1711756800000,1711756800500,70000.10,0.001",
        "201,1711756800001,1711756800600,70000.20",
    ]
)


def _write_fixture(directory, name, body):
    path = os.path.join(directory, name)
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as zf:
        zf.writestr(name.replace(".zip", ".csv"), body + "\n")
    return path


def _check(name, actual, expected):
    if actual != expected:
        raise AssertionError(f"{name}: expected {expected!r}, got {actual!r}")
    print(f"  ok  {name}")


def selftest():
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    out_dir = os.path.join(root, "analysis", "out")
    os.makedirs(out_dir, exist_ok=True)
    good = _write_fixture(out_dir, "fixture-good.zip", _GOOD)
    bad = _write_fixture(out_dir, "fixture-malformed.zip", _MALFORMED)

    print("archive inspector contract")
    record = inspect_archive(good)
    _check("member count", record["member_count"], 1)
    member = record["members"][0]
    _check("header detected", member["header_present"], True)
    _check("rows include header", member["rows_including_header"], 7)
    _check("field count stable", member["field_count_varies"], False)
    # Raw strings preserved: trailing zeros in the price would be lost by float.
    _check("first row raw strings", member["first_row_raw"][0], "id")
    _check("last row raw price kept as string", member["last_row_raw"][3], "70000.40")
    _check("adjacent full duplicate row", member["adjacent_full_duplicate_rows"], 1)
    _check("zip hash present", len(record["zip_sha256"]), 64)
    _check("member hash present", len(member["member_sha256"]), 64)

    # Column 0 is the id: header makes it non-integer overall, so integer facts
    # are reported per observed token rather than assumed.
    ids = member["columns"][0]
    _check("id regressions", ids["regressions"], 1)
    _check("id first violation value", ids["first_violation"]["value"], 102)
    _check("id gaps over one", ids["gaps_over_one"], 2)
    _check("id not strictly increasing", ids["strictly_increasing"], False)

    txn = member["columns"][1]
    _check("txn ties detected", txn["adjacent_duplicate_values"], 3)
    _check("txn max tie run", txn["max_tie_run"], 3)
    _check("txn non-decreasing", txn["monotonic_nondecreasing"], True)
    _check("txn unit inferred", txn["epoch_candidate"]["inferred_unit"], "milliseconds")

    event = member["columns"][2]
    _check("event time is a distinct candidate", "epoch_candidate" in event, True)
    _check(
        "event time differs from txn",
        event["epoch_candidate"]["utc_start"] != txn["epoch_candidate"]["utc_start"],
        True,
    )

    spans = coverage_overlap([record])
    _check("two epoch candidates found", spans[0]["epoch_candidate_count"], 2)
    _check("single-timestamp limitation not claimed", spans[0]["single_timestamp_only"], False)

    bad_record = inspect_archive(bad)
    problems = fail_closed([bad_record])
    _check("malformed width fails closed", len(problems), 1)

    os.remove(good)
    os.remove(bad)
    print("archive inspector contract: all checks passed")


def main():
    if len(sys.argv) < 2 or sys.argv[1] == "selftest":
        selftest()
        return
    if sys.argv[1] != "inspect" or len(sys.argv) < 3:
        raise SystemExit("usage: inspect_archive.py selftest | inspect <zip> [zip ...]")
    inspect_paths(sys.argv[2:])


if __name__ == "__main__":
    main()
