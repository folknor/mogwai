#!/usr/bin/env python3
"""Pair protocol composition fixtures and apply the BBO budget policy."""

from __future__ import annotations

import json
import math
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
P6 = ROOT / "analysis/tick-composition-protocol-6.json"
P7 = ROOT / "analysis/tick-composition-protocol-7.json"

OLD = {
    "checkpoint_k": 262_144,
    "sweep_drain_budget": 5_000_000,
    "max_extend_ticks": 1 << 30,
    "fanout_depth": 65_536,
}


def power_of_two(value: float) -> int:
    return 1 << math.ceil(math.log2(value))


def million(value: float) -> int:
    return math.ceil(value / 1_000_000) * 1_000_000


def main() -> None:
    before = json.loads(P6.read_text())
    after = json.loads(P7.read_text())
    assert before["tape_protocol_version"] == 6
    assert after["tape_protocol_version"] == 7
    assert before["parent_events_per_combination"] == 2_000_000
    assert after["parent_events_per_combination"] == 2_000_000
    # Both fixtures are counted off one traversal and stamped with its
    # identifier. Matching row keys prove only that the two files describe the
    # same combinations - not that they describe the same tape. Fixtures written
    # before the identifier existed carry no such evidence at all, which is a
    # refusal with a reason rather than a KeyError.
    pairing_before = before.get("pairing_id")
    pairing_after = after.get("pairing_id")
    assert pairing_before is not None and pairing_after is not None, (
        "these fixtures predate pairing identifiers, so nothing shows they came "
        "from one traversal; regenerate both with a single "
        "`mogwai tick-composition` run"
    )
    assert pairing_before == pairing_after, (
        "fixtures come from different traversals; regenerate both with a single "
        "`mogwai tick-composition` run"
    )
    key = lambda row: (row["preset"], row["seed"], row["configuration"])
    old = {key(row): row for row in before["entries"]}
    new = {key(row): row for row in after["entries"]}
    assert old.keys() == new.keys()

    def ratio(field: str) -> float:
        return max(
            new[k][field]["p999"] / old[k][field]["p999"]
            for k in old
            if old[k][field]["p999"] > 0
        )

    ratios = {
        "checkpoint_k": ratio("ticks_per_sim_second"),
        "sweep_drain_budget": ratio("ticks_per_vol_window"),
        "warmup_materialization_ticks": ratio("ticks_per_warmup"),
        "fanout_depth": max(
            new[k]["frames_per_wall_second"][speed]["p999"]
            / old[k]["frames_per_wall_second"][speed]["p999"]
            for k in old
            for speed in ("1.0", "10.0")
            if old[k]["frames_per_wall_second"][speed]["p999"] > 0
        ),
    }
    # The required reach is the larger of two candidates, and which one wins is
    # the whole argument: a rate projected across the window, or the window count
    # actually observed. `reference/performance.md` quotes both and says the
    # observed ones lose, so both are printed - a figure a document cites and its
    # source does not emit is a figure that drifts.
    rate = max(row["ticks_per_sim_second"]["p999"] for row in new.values())
    observed = {
        "frames_per_sim_second": rate,
        "vol_window_count": max(
            row["ticks_per_vol_window"]["p999"] for row in new.values()
        ),
        "warmup_window_count": max(
            row["ticks_per_warmup"]["p999"] for row in new.values()
        ),
    }
    reach = {
        "sweep_drain_budget": max(observed["vol_window_count"], rate * 300),
        "warmup_materialization_ticks": max(
            observed["warmup_window_count"], rate * 86_400
        ),
    }
    proposed = {
        "checkpoint_k": power_of_two(OLD["checkpoint_k"] * ratios["checkpoint_k"] * 2),
        "sweep_drain_budget": million(
            max(
                OLD["sweep_drain_budget"] * ratios["sweep_drain_budget"] * 2,
                reach["sweep_drain_budget"],
            )
        ),
        "max_extend_ticks": OLD["max_extend_ticks"],
        "warmup_materialization_ticks": million(
            max(
                OLD["max_extend_ticks"] * ratios["warmup_materialization_ticks"] * 2,
                reach["warmup_materialization_ticks"],
            )
        ),
        "fanout_depth": power_of_two(OLD["fanout_depth"] * ratios["fanout_depth"] * 2),
    }
    horizons = {
        "fanout_old_wall_seconds": min(
            OLD["fanout_depth"] / old[k]["frames_per_wall_second"][speed]["p999"]
            for k in old
            for speed in ("1.0", "10.0")
            if old[k]["frames_per_wall_second"][speed]["p999"] > 0
        ),
        "fanout_new_wall_seconds": min(
            proposed["fanout_depth"]
            / new[k]["frames_per_wall_second"][speed]["p999"]
            for k in new
            for speed in ("1.0", "10.0")
            if new[k]["frames_per_wall_second"][speed]["p999"] > 0
        ),
    }
    print(
        json.dumps(
            {
                "ratios": ratios,
                "observed": observed,
                "required_reach": reach,
                "proposed": proposed,
                "horizons": horizons,
            },
            indent=2,
            sort_keys=True,
        )
    )


if __name__ == "__main__":
    main()
