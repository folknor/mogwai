#!/usr/bin/env python3
"""Fit a calendar-conditional SessionProfile for MNQ from the NQ 1-minute archive.

Offline and stdlib-only, like every script in this folder. Two modes:

    preflight   the executable gate: what the archive actually contains
    fit         the estimator and its report

The contract this implements is settled before the estimator runs, and every
threshold below is preregistered. Reading the residual matrix and then choosing a
tolerance is the failure this file exists to prevent.

Ownership model, decided separately and assumed here:

    SessionCalendar   owns hard closure - whether an event may exist at all
    SessionProfile    owns relative arrival and volatility WHILE open
    mean_event_dur    the unconditional baseline over open model time

So the fitted factors are conditional on the market being open, exposure is
calendar-open minutes, and closed minutes contribute neither activity nor
exposure. A near-zero factor remains legal - a genuinely thin open hour - but no
longer means "closed", which is the calendar's word alone.
"""

from __future__ import annotations

import argparse
import json
import tomllib
import zipfile
from collections import defaultdict
from dataclasses import dataclass, field
from datetime import datetime, timedelta
from pathlib import Path
from zoneinfo import ZoneInfo

ROOT = Path(__file__).resolve().parents[1]
ARCHIVE = ROOT / "research/market-data/nq-1m_bk.zip"
PRESET = ROOT / "crates/mogwai-server/presets/mnq.toml"

# ---------------------------------------------------------------------------
# Preregistered constants. Fixed before the fit was first run; changing one
# after reading a result invalidates the acceptance claim it supports.
# ---------------------------------------------------------------------------

# A cell is material when observed/fitted leaves [1/R, R]. Calibrated to the
# defect being corrected - a flat 1.78x curve against a real 20-50x intraday
# swing - not to brick 7's 1.05, which answered a different question about
# whether a budget constant needed resizing at all.
INTERACTION_RATIO_LIMIT = 1.25

# Separability is rejected when more than this share of open exposure sits in
# material cells. Allows a handful of genuine weekday-specific interactions
# without letting them reject an otherwise useful separable model.
MAX_MATERIAL_EXPOSURE_SHARE = 0.05

# Calendar-defined, never selected from results. MNQ and MES did not list until
# May 2019, so the instrument being modelled exists only in the third.
ERAS: tuple[tuple[str, int, int], ...] = (
    ("early", 2009, 2014),
    ("middle", 2015, 2019),
    ("recent", 2020, 2026),
)
DESIGNATED_FIT_ERA = "recent"

# A session whose last observed minute precedes its calendar close by more than
# this is an exchange-scheduled early close, not thin trading. Excluded from
# both numerator and exposure: the model deliberately carries no date-specific
# exceptions, so letting a half-session contribute would leak holiday behaviour
# into the ordinary weekday and hour factors.
EARLY_CLOSE_TOLERANCE_MINUTES = 60

# How a historical civil timestamp lands on the model's permanent UTC-05:00
# clock. Both readings are defensible and they disagree by an hour for roughly
# half the corpus, so the choice is named rather than implied.
#
#   "civil"    preserve the civil label: read every timestamp against the fixed
#              offset unchanged. Both CST and CDT observations land on the same
#              canonical session phase, so no season disproportionately supplies
#              the boundary buckets. Their true UTC instants then differ
#              seasonally, which is exactly the dimension the model declares
#              unmodelled.
#   "instant"  preserve the true instant: a CST civil time moves forward 60
#              minutes into model-local. Retained because it was measured, and
#              because it is the correct reading for a calendar that encodes one
#              DST regime's UTC instants rather than civil labels.
#
# Civil is the default because the preset now declares permanent-CDT CIVIL
# hours. Against the previous 18:00-17:00 table - the same schedule rendered an
# hour forward - instant discarded 175,394 rows and civil 247,289, which was
# evidence about the calendar rather than about the alignment: that table matched
# CST instants, so only the winter half could align with it. With the corrected
# civil table the phases coincide year-round and the comparison inverts.
MODEL_CLOCK_ALIGNMENT = "civil"

CHICAGO = ZoneInfo("America/Chicago")
MINUTES_PER_WEEK = 7 * 24 * 60
HOURS = 24
DAYS = 7


# ---------------------------------------------------------------------------
# The calendar, read from the shipped preset rather than restated here
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class Calendar:
    """The venue's own weekly grid, in local minutes from Sunday 00:00."""

    utc_offset_minutes: int
    windows: tuple[tuple[int, int], ...]

    @classmethod
    def from_preset(cls, path: Path) -> "Calendar":
        with path.open("rb") as handle:
            document = tomllib.load(handle)
        calendar = document["instrument"]["calendar"]
        windows = tuple(
            (entry["start_minute"], entry["end_minute"])
            for entry in calendar["open_windows"]
        )
        return cls(calendar["utc_offset_minutes"], windows)

    def open_mask(self) -> list[bool]:
        mask = [False] * MINUTES_PER_WEEK
        for start, end in self.windows:
            for minute in range(start, end):
                mask[minute] = True
        return mask

    def sessions(self) -> list[tuple[int, int]]:
        """Contiguous trading cycles: windows separated only by the 15-minute
        halt belong to one session; the 60-minute break starts a new one."""
        merged: list[list[int]] = []
        for start, end in sorted(self.windows):
            if merged and start - merged[-1][1] <= 15:
                merged[-1][1] = end
            else:
                merged.append([start, end])
        return [(start, end) for start, end in merged]

    def open_minutes(self) -> int:
        return sum(end - start for start, end in self.windows)

    def session_open_minutes(self) -> list[int]:
        """Open minutes per session, which is NOT the session's span: sessions
        are merged across the daily 15-minute halt, so the span overstates
        exposure by that much for every trading day."""
        mask = self.open_mask()
        return [
            sum(1 for minute in range(start, end) if mask[minute])
            for start, end in self.sessions()
        ]


def local_week_minute(stamp: datetime) -> int:
    """Minutes from local Sunday 00:00, matching the calendar's own convention."""
    # Python weekday(): Monday=0 .. Sunday=6. The calendar counts from Sunday.
    sunday_index = (stamp.weekday() + 1) % 7
    return sunday_index * 1440 + stamp.hour * 60 + stamp.minute


# ---------------------------------------------------------------------------
# The archive
# ---------------------------------------------------------------------------


@dataclass
class Row:
    """One archive minute, already placed on the model clock."""

    model_local: datetime
    close: float
    volume: float
    was_cst: bool


def read_archive(path: Path) -> list[Row]:
    """Stream the single member out of the zip without extracting it.

    Format, from `analysis/inspect_cme_bars.py`'s validation: no header,
    `DD/MM/YYYY;HH:MM;O;H;L;C;V`, semicolon-delimited, NQ uses HH:MM with LF
    endings. Timestamps are CME local civil time and observe DST.
    """
    rows: list[Row] = []
    with zipfile.ZipFile(path) as archive:
        member = archive.namelist()[0]
        with archive.open(member) as handle:
            for raw in handle:
                line = raw.decode("ascii").strip()
                if not line:
                    continue
                parts = line.split(";")
                if len(parts) != 7:
                    raise ValueError(f"unexpected field count in {member}: {line!r}")
                date_text, time_text = parts[0], parts[1]
                civil = datetime.strptime(
                    f"{date_text} {time_text}", "%d/%m/%Y %H:%M"
                )
                offset = civil.replace(tzinfo=CHICAGO).utcoffset()
                if offset is None:
                    raise ValueError(f"no offset for {civil}")
                was_cst = offset == timedelta(minutes=-360)
                if MODEL_CLOCK_ALIGNMENT == "instant" and was_cst:
                    model_local = civil + timedelta(minutes=60)
                else:
                    model_local = civil
                rows.append(Row(model_local, float(parts[5]), float(parts[6]), was_cst))
    return rows


# ---------------------------------------------------------------------------
# Session eligibility
# ---------------------------------------------------------------------------


@dataclass
class SessionKey:
    """A single trading cycle, identified by the model-local date its close
    falls on plus the session's own weekly slot."""

    close_date: str
    slot: int


@dataclass
class SessionState:
    minutes_present: int = 0
    volume: float = 0.0
    last_offset: int = -1
    span: int = 0


def group_sessions(
    rows: list[Row], calendar: Calendar
) -> tuple[dict[tuple[str, int], SessionState], int]:
    """Bucket rows into trading cycles, returning the state per session and the
    count of rows falling outside every declared open window."""
    sessions_by_slot = calendar.sessions()
    mask = calendar.open_mask()
    states: dict[tuple[str, int], SessionState] = defaultdict(SessionState)
    outside = 0
    for row in rows:
        minute = local_week_minute(row.model_local)
        if not mask[minute]:
            outside += 1
            continue
        slot = next(
            index
            for index, (start, end) in enumerate(sessions_by_slot)
            if start <= minute < end
        )
        start, end = sessions_by_slot[slot]
        # A cycle can straddle midnight and the week boundary, so it is keyed on
        # the date its CLOSE falls on rather than on the row's own date.
        close_date = (
            row.model_local + timedelta(minutes=end - minute)
        ).date().isoformat()
        state = states[(close_date, slot)]
        state.minutes_present += 1
        state.volume += row.volume
        offset = minute - start
        state.last_offset = max(state.last_offset, offset)
        state.span = end - start
    return states, outside


def eligible_sessions(
    states: dict[tuple[str, int], SessionState],
) -> tuple[set[tuple[str, int]], dict[str, int]]:
    """Ordinary full sessions only. A session with no rows never appears here at
    all - a full holiday is invisible rather than excluded - so the count of
    absent sessions is reported separately by the caller."""
    eligible: set[tuple[str, int]] = set()
    excluded_early = 0
    for key, state in states.items():
        shortfall = state.span - 1 - state.last_offset
        if shortfall > EARLY_CLOSE_TOLERANCE_MINUTES:
            excluded_early += 1
            continue
        eligible.add(key)
    return eligible, {"early_close_excluded": excluded_early}


# ---------------------------------------------------------------------------
# The estimator
# ---------------------------------------------------------------------------


@dataclass
class Grid:
    """Volume and exposure on the (UTC hour, UTC day-of-week) grid the runtime
    profile is keyed on. Exposure is calendar-open minutes, never row presence:
    a vendor archive that omits zero-volume minutes would otherwise shrink a
    quiet hour's denominator in exact proportion to its own quietness."""

    volume: list[list[float]] = field(
        default_factory=lambda: [[0.0] * DAYS for _ in range(HOURS)]
    )
    exposure: list[list[float]] = field(
        default_factory=lambda: [[0.0] * DAYS for _ in range(HOURS)]
    )


def utc_cell(model_local: datetime, calendar: Calendar) -> tuple[int, int]:
    utc = model_local - timedelta(minutes=calendar.utc_offset_minutes)
    return utc.hour, (utc.weekday() + 1) % 7


def build_grid(
    rows: list[Row],
    calendar: Calendar,
    eligible: set[tuple[str, int]],
    era: tuple[int, int] | None,
) -> Grid:
    """Numerator from observed rows, denominator from the calendar.

    Every eligible session contributes its FULL open-minute grid as exposure,
    whether or not the archive carried a row for each minute. A missing minute
    inside an ordinary session is a genuinely quiet minute and is exposed with
    zero volume; only ineligible sessions contribute nothing at all.
    """
    grid = Grid()
    sessions_by_slot = calendar.sessions()
    mask = calendar.open_mask()
    seen_sessions: set[tuple[str, int]] = set()
    for row in rows:
        if era is not None and not era[0] <= row.model_local.year <= era[1]:
            continue
        minute = local_week_minute(row.model_local)
        if not mask[minute]:
            continue
        slot = next(
            index
            for index, (start, end) in enumerate(sessions_by_slot)
            if start <= minute < end
        )
        start, end = sessions_by_slot[slot]
        close_date = (
            row.model_local + timedelta(minutes=end - minute)
        ).date().isoformat()
        key = (close_date, slot)
        if key not in eligible:
            continue
        hour, day = utc_cell(row.model_local, calendar)
        grid.volume[hour][day] += row.volume
        if key not in seen_sessions:
            seen_sessions.add(key)
            anchor = row.model_local - timedelta(minutes=minute - start)
            for offset in range(end - start):
                stamp = anchor + timedelta(minutes=offset)
                if not mask[local_week_minute(stamp)]:
                    continue
                exposed_hour, exposed_day = utc_cell(stamp, calendar)
                grid.exposure[exposed_hour][exposed_day] += 1.0
    return grid


@dataclass
class Fit:
    hour: list[float]
    day: list[float]
    alpha: float
    sweeps: int


def fit_grid(grid: Grid, sweeps: int = 200, tolerance: float = 1e-12) -> Fit:
    """Maximum likelihood for a multiplicative Poisson rate with an exposure
    offset, by alternating closed-form updates.

    The factors are scale-indeterminate, so each sweep re-normalizes the
    COMPOSITE to an exposure-weighted mean of one over open minutes. That fixes
    the identification and leaves `alpha` carrying the corpus-wide contracts per
    open minute, matching the runtime semantics where hour and day are purely
    relative.

    Saturday is pinned to 1.0 and never updated: with the shipped calendar it
    has zero exposure in every cell, so its update is 0/0 and its value is a
    declared convention rather than an estimate.
    """
    hour = [1.0] * HOURS
    day = [1.0] * DAYS
    exposed_days = [
        d for d in range(DAYS) if any(grid.exposure[h][d] > 0.0 for h in range(HOURS))
    ]
    exposed_hours = [
        h for h in range(HOURS) if any(grid.exposure[h][d] > 0.0 for d in range(DAYS))
    ]
    total_exposure = sum(sum(row) for row in grid.exposure)
    total_volume = sum(sum(row) for row in grid.volume)
    if total_exposure <= 0.0:
        return Fit(hour, day, 0.0, 0)

    # `alpha` is constant under the chosen identification: once the composite is
    # normalized to an exposure-weighted mean of one, the rate scale is exactly
    # total volume over total exposure. It must appear in BOTH denominators. Omit
    # it and each sweep fits absolute volume into hour x day, the normalization
    # divides that scale back out, and the next sweep reintroduces it - so the
    # two arrays drift reciprocally, their product stays right while each is
    # individually meaningless, and the convergence test never fires.
    alpha = total_volume / total_exposure

    completed = 0
    for sweep in range(sweeps):
        previous = list(hour) + list(day)
        for h in exposed_hours:
            numerator = sum(grid.volume[h][d] for d in exposed_days)
            denominator = alpha * sum(
                grid.exposure[h][d] * day[d] for d in exposed_days
            )
            if denominator > 0.0:
                hour[h] = numerator / denominator
        for d in exposed_days:
            numerator = sum(grid.volume[h][d] for h in exposed_hours)
            denominator = alpha * sum(
                grid.exposure[h][d] * hour[h] for h in exposed_hours
            )
            if denominator > 0.0:
                day[d] = numerator / denominator
        composite = sum(
            grid.exposure[h][d] * hour[h] * day[d]
            for h in range(HOURS)
            for d in range(DAYS)
        )
        scale = composite / total_exposure
        if scale > 0.0:
            root = scale ** 0.5
            for h in exposed_hours:
                hour[h] /= root
            for d in exposed_days:
                day[d] /= root
        completed = sweep + 1
        moved = max(
            abs(a - b) for a, b in zip(list(hour) + list(day), previous, strict=True)
        )
        if moved < tolerance:
            break
    return Fit(hour, day, alpha, completed)


@dataclass
class VolFit:
    """`vol_hour`, the per-mean RMS return ratio, conditional on being open."""

    ratio: list[float]
    returns: list[int]
    trimmed_sessions: int
    excluded_non_adjacent: int


def build_vol_hour(
    rows: list[Row],
    calendar: Calendar,
    eligible: set[tuple[str, int]],
    era: tuple[int, int] | None,
) -> VolFit:
    """RMS of close-to-close minute returns per UTC hour, normalized to an
    observation-weighted mean of one.

    A return is formed ONLY between two adjacent present open minutes inside one
    eligible session. That single rule excludes closure crossings, the daily
    halt, the overnight break and every missing minute at once, because each of
    them breaks adjacency. Returns are admissible from this archive even though
    prices are not: ratio back-adjustment destroys price level and the tick grid
    while preserving returns exactly, which is why `nq-1m_bk.zip` can source
    `vol_hour` and could never source `modal_tick`.

    The roll trim follows the existing convention in `select_windows.py`: drop
    the single largest squared return per session. `notes` on the archive record
    that this handles realized volatility while gap features still see
    adjustment boundaries, so it is a mitigation rather than a cure.
    """
    mask = calendar.open_mask()
    sessions_by_slot = calendar.sessions()
    sum_sq = [0.0] * HOURS
    counts = [0] * HOURS
    trimmed = 0
    non_adjacent = 0

    current_key: tuple[str, int] | None = None
    session_returns: list[tuple[int, float]] = []
    previous: Row | None = None

    def flush() -> None:
        nonlocal trimmed
        if not session_returns:
            return
        largest = max(range(len(session_returns)), key=lambda i: abs(session_returns[i][1]))
        trimmed += 1
        for index, (hour, value) in enumerate(session_returns):
            if index == largest:
                continue
            sum_sq[hour] += value * value
            counts[hour] += 1
        session_returns.clear()

    for row in sorted(rows, key=lambda item: item.model_local):
        if era is not None and not era[0] <= row.model_local.year <= era[1]:
            previous = None
            continue
        minute = local_week_minute(row.model_local)
        if not mask[minute]:
            previous = None
            continue
        slot = next(
            index
            for index, (start, end) in enumerate(sessions_by_slot)
            if start <= minute < end
        )
        start, end = sessions_by_slot[slot]
        close_date = (
            row.model_local + timedelta(minutes=end - minute)
        ).date().isoformat()
        key = (close_date, slot)
        if key not in eligible:
            previous = None
            continue
        if key != current_key:
            flush()
            current_key = key
            previous = None
        if previous is not None:
            gap = row.model_local - previous.model_local
            if gap == timedelta(minutes=1) and previous.close > 0.0:
                hour, _ = utc_cell(row.model_local, calendar)
                session_returns.append(
                    (hour, (row.close - previous.close) / previous.close)
                )
            else:
                non_adjacent += 1
        previous = row
    flush()

    total = sum(counts)
    if total == 0:
        return VolFit([1.0] * HOURS, counts, trimmed, non_adjacent)
    rms = [
        (sum_sq[h] / counts[h]) ** 0.5 if counts[h] > 0 else 0.0 for h in range(HOURS)
    ]
    mean = sum(rms[h] * counts[h] for h in range(HOURS)) / total
    ratio = [rms[h] / mean if mean > 0.0 else 1.0 for h in range(HOURS)]
    # An unexposed hour carries no evidence. One is the neutral convention, the
    # same treatment Saturday's day factor gets, and it is recorded as such.
    for h in range(HOURS):
        if counts[h] == 0:
            ratio[h] = 1.0
    return VolFit(ratio, counts, trimmed, non_adjacent)


@dataclass
class Separability:
    material_share: float
    material_cells: int
    residuals: list[list[float | None]]
    passes: bool


def assess(grid: Grid, fit: Fit) -> Separability:
    """Materiality, not statistical rejection. With seventeen years of minutes
    any deviance test rejects separability regardless of whether the departure
    matters operationally."""
    material_exposure = 0.0
    total_exposure = 0.0
    material_cells = 0
    residuals: list[list[float | None]] = [[None] * DAYS for _ in range(HOURS)]
    for h in range(HOURS):
        for d in range(DAYS):
            exposure = grid.exposure[h][d]
            if exposure <= 0.0:
                continue
            total_exposure += exposure
            observed = grid.volume[h][d] / exposure
            fitted = fit.alpha * fit.hour[h] * fit.day[d]
            if fitted <= 0.0:
                continue
            ratio = observed / fitted
            residuals[h][d] = ratio
            if ratio > INTERACTION_RATIO_LIMIT or ratio < 1.0 / INTERACTION_RATIO_LIMIT:
                material_exposure += exposure
                material_cells += 1
    share = material_exposure / total_exposure if total_exposure > 0.0 else 0.0
    return Separability(
        share, material_cells, residuals, share <= MAX_MATERIAL_EXPOSURE_SHARE
    )


@dataclass
class EraStability:
    divergent_share: float
    divergent_cells: int
    passes: bool


def era_stability(
    full: Fit, designated: Fit, designated_grid: Grid
) -> EraStability:
    """Does the full-corpus curve represent the designated era?

    Deliberately the SAME rule and the same two constants as the separability
    test, so no new threshold enters after results exist. Each cell's runtime
    multiplier is compared between the two fits - both already normalized to an
    exposure-weighted mean of one, so they are directly comparable - and cells
    are weighted by the DESIGNATED era's exposure, because the question is
    whether the full corpus misrepresents the era we would actually ship.
    """
    divergent_exposure = 0.0
    total_exposure = 0.0
    divergent_cells = 0
    for h in range(HOURS):
        for d in range(DAYS):
            exposure = designated_grid.exposure[h][d]
            if exposure <= 0.0:
                continue
            total_exposure += exposure
            reference = designated.hour[h] * designated.day[d]
            candidate = full.hour[h] * full.day[d]
            if reference <= 0.0:
                continue
            ratio = candidate / reference
            if (
                ratio > INTERACTION_RATIO_LIMIT
                or ratio < 1.0 / INTERACTION_RATIO_LIMIT
            ):
                divergent_exposure += exposure
                divergent_cells += 1
    share = divergent_exposure / total_exposure if total_exposure > 0.0 else 0.0
    return EraStability(
        share, divergent_cells, share <= MAX_MATERIAL_EXPOSURE_SHARE
    )


def peak_to_trough(fit: Fit, grid: Grid) -> dict[str, float]:
    """The quantity most directly connected to the visible defect: a flat 1.78x
    crypto curve standing in for a real index future's intraday swing."""
    exposed = [
        h for h in range(HOURS) if any(grid.exposure[h][d] > 0.0 for d in range(DAYS))
    ]
    fitted = [fit.hour[h] for h in exposed]
    observed = [
        sum(grid.volume[h][d] for d in range(DAYS))
        / sum(grid.exposure[h][d] for d in range(DAYS))
        for h in exposed
    ]
    return {
        "fitted": max(fitted) / min(fitted) if min(fitted) > 0.0 else float("inf"),
        "observed": max(observed) / min(observed)
        if min(observed) > 0.0
        else float("inf"),
    }


# ---------------------------------------------------------------------------
# Modes
# ---------------------------------------------------------------------------


def preflight() -> None:
    calendar = Calendar.from_preset(PRESET)
    rows = read_archive(ARCHIVE)
    states, outside = group_sessions(rows, calendar)
    eligible, exclusions = eligible_sessions(states)
    zero_volume_rows = sum(1 for row in rows if row.volume == 0.0)
    cst_rows = sum(1 for row in rows if row.was_cst)
    # Open minutes, not span: the span of a merged session includes the daily
    # 15-minute halt, which is closed and carries no exposure.
    slot_open = calendar.session_open_minutes()
    present = sum(
        state.minutes_present for key, state in states.items() if key in eligible
    )
    exposed = sum(slot_open[key[1]] for key in states if key in eligible)
    report = {
        "alignment": MODEL_CLOCK_ALIGNMENT,
        "rows": len(rows),
        "present_zero_volume_rows": zero_volume_rows,
        "rows_outside_declared_calendar": outside,
        "cst_rows_remapped": cst_rows,
        "cdt_rows": len(rows) - cst_rows,
        "sessions_observed": len(states),
        "sessions_eligible": len(eligible),
        "early_close_excluded": exclusions["early_close_excluded"],
        "calendar_open_minutes_per_week": calendar.open_minutes(),
        "eligible_open_minutes_expected": exposed,
        "eligible_minutes_present": present,
        "missing_minutes_inside_eligible_sessions": exposed - present,
    }
    print(json.dumps(report, indent=2, sort_keys=True))
    if zero_volume_rows == 0:
        print(
            "\nNOTE: the archive carries no zero-volume rows, so exposure MUST come\n"
            "from the calendar. Deriving it from row presence would shrink each\n"
            "quiet hour's denominator in proportion to its own quietness and\n"
            "compress the peak-to-trough ratio this fit exists to measure."
        )


def report_fit() -> None:
    calendar = Calendar.from_preset(PRESET)
    rows = read_archive(ARCHIVE)
    states, _ = group_sessions(rows, calendar)
    eligible, _ = eligible_sessions(states)

    scopes: list[tuple[str, tuple[int, int] | None]] = [("full", None)]
    scopes.extend((name, (start, end)) for name, start, end in ERAS)

    print("# MNQ session profile fit")
    print()
    print(f"Preregistered: R={INTERACTION_RATIO_LIMIT}, "
          f"max material exposure share={MAX_MATERIAL_EXPOSURE_SHARE}, "
          f"designated era={DESIGNATED_FIT_ERA}, alignment={MODEL_CLOCK_ALIGNMENT}")
    print()

    outcomes = {}
    for name, era in scopes:
        grid = build_grid(rows, calendar, eligible, era)
        fit = fit_grid(grid)
        verdict = assess(grid, fit)
        swing = peak_to_trough(fit, grid)
        vol = build_vol_hour(rows, calendar, eligible, era)
        outcomes[name] = (fit, verdict, swing, grid, vol)
        print(f"## {name}")
        print()
        print(f"alpha (contracts per open minute): {fit.alpha:.4f}   sweeps: {fit.sweeps}")
        print(f"separable: {'PASS' if verdict.passes else 'FAIL'}   "
              f"material exposure share: {verdict.material_share:.4f}   "
              f"material cells: {verdict.material_cells}")
        print(f"peak-to-trough  fitted {swing['fitted']:.2f}x   "
              f"observed {swing['observed']:.2f}x")
        print()
        print("hour factors and open-minute exposure:")
        for h in range(HOURS):
            exposure = sum(grid.exposure[h][d] for d in range(DAYS))
            if exposure <= 0.0:
                print(f"  {h:02d}  {'unexposed':>12}")
                continue
            print(f"  {h:02d}  {fit.hour[h]:12.4f}  exposure {exposure:12.0f}")
        print()
        print("day factors and open-minute exposure:")
        labels = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"]
        for d in range(DAYS):
            exposure = sum(grid.exposure[h][d] for h in range(HOURS))
            if exposure <= 0.0:
                print(f"  {labels[d]}  {fit.day[d]:12.4f}  CONVENTIONAL, zero exposure")
                continue
            print(f"  {labels[d]}  {fit.day[d]:12.4f}  exposure {exposure:12.0f}")
        print()
        print("hour x day residual matrix, observed/fitted "
              "(dot = unexposed, star = material):")
        print("      " + "".join(f"{label:>10}" for label in labels))
        for h in range(HOURS):
            cells = []
            for d in range(DAYS):
                ratio = verdict.residuals[h][d]
                if ratio is None:
                    cells.append(f"{'.':>10}")
                    continue
                material = (
                    ratio > INTERACTION_RATIO_LIMIT
                    or ratio < 1.0 / INTERACTION_RATIO_LIMIT
                )
                cells.append(f"{ratio:>9.3f}{'*' if material else ' '}")
            print(f"  {h:02d}  " + "".join(cells))
        print()
        print("vol_hour, per-mean RMS return ratio, and return count:")
        print(f"  roll-trimmed sessions {vol.trimmed_sessions}   "
              f"non-adjacent pairs skipped {vol.excluded_non_adjacent}")
        for h in range(HOURS):
            if vol.returns[h] == 0:
                print(f"  {h:02d}  {vol.ratio[h]:12.4f}  CONVENTIONAL, no returns")
                continue
            print(f"  {h:02d}  {vol.ratio[h]:12.4f}  returns {vol.returns[h]:12d}")
        print()

    print("## outcome")
    print()
    full_fit, full_verdict = outcomes["full"][0], outcomes["full"][1]
    era_fit, era_verdict, _, era_grid, _ = outcomes[DESIGNATED_FIT_ERA]
    stability = era_stability(full_fit, era_fit, era_grid)
    print(f"era stability: full vs {DESIGNATED_FIT_ERA}, "
          f"divergent exposure share {stability.divergent_share:.4f} "
          f"over {stability.divergent_cells} cells -> "
          f"{'STABLE' if stability.passes else 'ERA-DEPENDENT'}")
    print()
    if not era_verdict.passes:
        print("Outcome 3: material hour-by-day interaction in the designated era.")
        print("Stop before changing Rust and reconsider the [24] x [7] schema.")
    elif full_verdict.passes and stability.passes:
        print("Outcome 1: separable and era-stable. Fit the full archive.")
    else:
        reason = (
            "the full corpus is not separable"
            if not full_verdict.passes
            else "the full corpus misrepresents the designated era"
        )
        print(f"Outcome 2: {reason}. Fit {DESIGNATED_FIT_ERA} only.")


def main() -> None:
    global MODEL_CLOCK_ALIGNMENT
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("preflight", "fit"))
    parser.add_argument(
        "--alignment",
        choices=("instant", "civil"),
        default=MODEL_CLOCK_ALIGNMENT,
        help="how a historical civil timestamp lands on the fixed UTC-05:00 "
        "model clock; see MODEL_CLOCK_ALIGNMENT",
    )
    args = parser.parse_args()
    MODEL_CLOCK_ALIGNMENT = args.alignment
    if args.mode == "preflight":
        preflight()
    else:
        report_fit()


if __name__ == "__main__":
    main()
