#!/usr/bin/env python3
"""Pair protocol composition fixtures and apply the BBO budget policy."""

from __future__ import annotations

import argparse
import json
import math
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

# Two comparison contracts over one arithmetic. The budget policy and the
# required-reach rule are identical either way, which is exactly why they must
# not be duplicated: two copies of a sizing rule drift, and the drift shows up as
# a constant nobody can re-derive.
#
#   projection    versions 6 and 7, ONE traversal. Protocol 6 is a count
#                 projection of the protocol-7 stream, because quote placement
#                 draws no randomness. The two fixtures therefore share a
#                 `pairing_id`, and a mismatch means one is stale.
#
#   independent   versions 7 and 8, TWO traversals. The protocol-8 session
#                 profile divides the duration draw and scales the return, so its
#                 tape carries different timestamps and prices - it cannot be
#                 projected from protocol 7. The pairings MUST differ here, and
#                 equal pairings would mean the same run had been compared with
#                 itself.
MODES = {
    "projection": {
        "versions": (6, 7),
        "before": "analysis/tick-composition-protocol-6.json",
        "after": "analysis/tick-composition-protocol-7.json",
        "same_pairing": True,
    },
    "independent": {
        "versions": (7, 8),
        "before": "analysis/tick-composition-protocol-7.json",
        "after": "analysis/tick-composition-protocol-8.json",
        "same_pairing": False,
    },
}

# Presets with no calendar. Their normalizer is the literal 1.0, so protocol 8
# leaves their tape byte-identical and every measured field must match exactly.
# This is the acceptance gate, not a nicety: if one of these moves, the change
# did something beyond reshaping the two futures and no budget ratio derived
# from the run means anything.
CALENDAR_FREE = ("BTCUSDT", "ETHUSDT", "SOLUSDT")
CALENDAR_BEARING = ("MNQ", "MES")

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


def assert_unchanged_where_the_tape_did_not_move(old: dict, new: dict) -> None:
    """The independent mode's acceptance gate, run BEFORE any ratio.

    Three of the five presets carry no calendar, so protocol 8's normalizer is
    the literal 1.0 and their tape is byte-identical. Every measured field of
    theirs must match exactly - including `frames_per_wall_second`, which is
    binned from simulated seconds rather than measured CPU throughput and so is
    as deterministic as the rest.

    `ticks_per_parent` must match for ALL five. The session profile changes when
    events happen, never how many: child count comes from the arrival-state
    chain and the surge window, neither of which reads the profile, so the RNG
    draw sequence is identical across the change.
    """
    for k in sorted(old):
        preset = k[0]
        assert old[k]["parents"] == new[k]["parents"], f"{k}: parents moved"
        assert old[k]["ticks_per_parent"] == new[k]["ticks_per_parent"], (
            f"{k}: parent fanout moved, so the change did something beyond "
            "reshaping the session and no budget ratio from this run is valid"
        )
        if preset in CALENDAR_FREE:
            assert old[k] == new[k], (
                f"{k}: a calendar-free preset moved. Its normalizer is the "
                "literal 1.0 and its tape must be byte-identical, so this is "
                "an unintended tape change rather than a session reshape"
            )
        else:
            assert preset in CALENDAR_BEARING, f"{k}: unknown preset {preset}"


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mode", choices=sorted(MODES), default="projection")
    parser.add_argument("--before", type=Path)
    parser.add_argument("--after", type=Path)
    args = parser.parse_args()
    mode = MODES[args.mode]
    before_path = args.before or ROOT / mode["before"]
    after_path = args.after or ROOT / mode["after"]

    before = json.loads(before_path.read_text())
    after = json.loads(after_path.read_text())
    lo, hi = mode["versions"]
    assert before["tape_protocol_version"] == lo, (
        f"{before_path} is protocol {before['tape_protocol_version']}, "
        f"not {lo} as --mode {args.mode} requires"
    )
    assert after["tape_protocol_version"] == hi, (
        f"{after_path} is protocol {after['tape_protocol_version']}, "
        f"not {hi} as --mode {args.mode} requires"
    )
    assert (
        before["parent_events_per_combination"]
        == after["parent_events_per_combination"]
        == 2_000_000
    )
    # Matching row keys prove only that the two files describe the same
    # combinations, never that they describe the same tape. The pairing
    # identifier is what carries that, and which relation it must satisfy is the
    # whole difference between the two modes. Fixtures written before the
    # identifier existed carry no such evidence at all, which is a refusal with a
    # reason rather than a KeyError.
    pairing_before = before.get("pairing_id")
    pairing_after = after.get("pairing_id")
    assert pairing_before is not None and pairing_after is not None, (
        "these fixtures predate pairing identifiers, so nothing shows which "
        "traversals produced them; regenerate with `mogwai tick-composition`"
    )
    if mode["same_pairing"]:
        assert pairing_before == pairing_after, (
            "projection mode compares two counter sets from ONE traversal, and "
            "these carry different pairings; regenerate with a single "
            "`mogwai tick-composition` run"
        )
    else:
        assert pairing_before != pairing_after, (
            "independent mode compares two SEPARATELY measured tapes, and these "
            "carry the same pairing - the same run is being compared with itself"
        )
    key = lambda row: (row["preset"], row["seed"], row["configuration"])
    old = {key(row): row for row in before["entries"]}
    new = {key(row): row for row in after["entries"]}
    assert old.keys() == new.keys()
    if not mode["same_pairing"]:
        assert_unchanged_where_the_tape_did_not_move(old, new)

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
