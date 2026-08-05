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
#
# Each mode carries its OWN baseline constants, and that is not bookkeeping. A
# ratio is meaningless without the number it resizes, and the number it resizes
# is whatever was shipping at the mode's BEFORE version - not whatever ships
# today. Sharing one table resized the protocol-7 constants by the
# pre-protocol-7 baseline and under-proposed checkpoint and fanout by the factor
# protocol 7 had already absorbed, while every acceptance assertion still passed.
#
# So a baseline table is a historical record and is frozen once its mode's
# resize has landed. A protocol-9 comparison adds a mode whose baseline is the
# protocol-8 column, and does not edit the one below.
#
# `warmup_baseline` is separate from `max_extend_ticks` for the same reason.
# Before protocol 7 the warmup reach had no constant of its own and borrowed the
# extend ceiling; protocol 7 gave it `MAX_WARMUP_MATERIALIZATION_TICKS`, so that
# is what protocol 8 resizes.
MODES = {
    "projection": {
        "versions": (6, 7),
        "before": "analysis/tick-composition-protocol-6.json",
        "after": "analysis/tick-composition-protocol-7.json",
        "same_pairing": True,
        # Pre-protocol-7, as shipped when the 6-to-7 comparison was made.
        "baseline": {
            "checkpoint_k": 262_144,
            "sweep_drain_budget": 5_000_000,
            "max_extend_ticks": 1 << 30,
            "warmup_baseline": 1 << 30,
            "fanout_depth": 65_536,
        },
    },
    "independent": {
        "versions": (7, 8),
        "before": "analysis/tick-composition-protocol-7.json",
        "after": "analysis/tick-composition-protocol-8.json",
        "same_pairing": False,
        "acceptance": "session_reshape",
        # The protocol-7 constants, as shipped when this comparison was made.
        # Protocol 8 has since replaced four of the five with the values this
        # mode proposed, so do not read these as current. `max_extend_ticks` is
        # the exception and does still ship at 1 << 30: it is a per-lock runaway
        # backstop rather than a reach ceiling, and is deliberately not scaled.
        "baseline": {
            "checkpoint_k": 1_048_576,
            "sweep_drain_budget": 282_000_000,
            "max_extend_ticks": 1 << 30,
            "warmup_baseline": 81_124_000_000,
            "fanout_depth": 262_144,
        },
    },
    "independent_9_10": {
        "versions": (9, 10),
        "before": "analysis/tick-composition-protocol-9.json",
        "after": "analysis/tick-composition-protocol-10.json",
        "same_pairing": False,
        # Protocol 10 is the MNQ TBBO fit (notes/mnq-tbbo-fit-spec.md): the
        # two futures presets change their fitted generator scalars, so their
        # cadence and fanout MAY move - permitted, never required, since a
        # fitted value may legitimately equal a declared one. The three
        # crypto presets are untouched by the fit and must stay
        # byte-identical. `ticks_per_parent` is therefore NOT frozen for the
        # futures here, unlike the 7-to-8 session reshape, where child count
        # was structurally unreadable from the profile.
        "acceptance": "preset_fit",
        # The currently shipped protocol-9 constants: 5fc974d changed none of
        # them (byte-identical tape), so these are the protocol-8 resize
        # values, frozen here as this mode's historical baseline.
        "baseline": {
            "checkpoint_k": 4_194_304,
            "sweep_drain_budget": 1_434_000_000,
            "max_extend_ticks": 1 << 30,
            "warmup_baseline": 162_349_000_000,
            "fanout_depth": 1_048_576,
        },
    },
}

# The 8/9 identity contract: 5fc974d split parent advancement from wire
# materialization with an unchanged draw order, so the protocol-9 tape is
# byte-identical to protocol 8 and a freshly measured protocol-9 fixture must
# equal the historical protocol-8 fixture in every field EXCEPT these two.
IDENTITY_EXCLUDED_FIELDS = ("tape_protocol_version", "pairing_id")

# Presets with no calendar. Their normalizer is the literal 1.0, so protocol 8
# leaves their tape byte-identical and every measured field must match exactly.
# This is the acceptance gate, not a nicety: if one of these moves, the change
# did something beyond reshaping the two futures and no budget ratio derived
# from the run means anything.
CALENDAR_FREE = ("BTCUSDT", "ETHUSDT", "SOLUSDT")
CALENDAR_BEARING = ("MNQ", "MES")


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


def assert_crypto_frozen_and_futures_finite(old: dict, new: dict) -> None:
    """The preset_fit mode's acceptance gate, run BEFORE any ratio.

    The fit touches only the two futures presets, so the three crypto tapes
    must be byte-identical - every measured field exactly equal. The futures
    MAY move (cadence and fanout both, since the fitted scalars change what a
    parent looks like), but every measurement entering a ratio must be finite
    and positive on BOTH sides: a NaN or zero would poison the max() silently
    or divide by zero loudly, and neither is a verdict.
    """
    def assert_numeric_leaves_finite_positive(key: tuple, path: str, node) -> None:
        # EVERY numeric measurement leaf, not merely the ratio inputs: a NaN
        # in a field the ratios skip today would silently survive into a
        # fixture the next comparison reads.
        if isinstance(node, bool):
            return
        if isinstance(node, (int, float)):
            assert math.isfinite(node) and node > 0, (
                f"{key}: {path} is {node}, not finite and positive"
            )
            return
        if isinstance(node, dict):
            for name, child in node.items():
                assert_numeric_leaves_finite_positive(key, f"{path}.{name}", child)

    for k in sorted(old):
        preset = k[0]
        assert old[k]["parents"] == new[k]["parents"], f"{k}: parents moved"
        if preset in CALENDAR_FREE:
            assert old[k] == new[k], (
                f"{k}: a calendar-free preset moved. The fit changes only the "
                "futures presets, so this is an unintended tape change and no "
                "budget ratio from this run is valid"
            )
            continue
        assert preset in CALENDAR_BEARING, f"{k}: unknown preset {preset}"
        for row in (old[k], new[k]):
            for name, node in row.items():
                if name in ("preset", "configuration", "seed"):
                    continue
                assert_numeric_leaves_finite_positive(k, name, node)


def verify_8_9_identity(eight_path: Path, nine_path: Path) -> None:
    """The Brick B0 identity gate: a freshly measured protocol-9 fixture must
    equal the historical protocol-8 fixture everywhere but the version and
    pairing fields, verifying 5fc974d's byte-identity claim on the current
    implementation. Any other differing field is a hard failure."""
    eight = json.loads(eight_path.read_text())
    nine = json.loads(nine_path.read_text())
    assert eight["tape_protocol_version"] == 8, (
        f"{eight_path} is protocol {eight['tape_protocol_version']}, not 8"
    )
    assert nine["tape_protocol_version"] == 9, (
        f"{nine_path} is protocol {nine['tape_protocol_version']}, not 9"
    )
    pairing_eight = eight.get("pairing_id")
    pairing_nine = nine.get("pairing_id")
    assert pairing_eight is not None and pairing_nine is not None, (
        "a fixture carries no pairing id, so nothing shows which traversal "
        "produced it; regenerate with `mogwai tick-composition`"
    )
    assert pairing_eight != pairing_nine, (
        "the two fixtures carry the same pairing id, so the same traversal "
        "is being compared with itself rather than remeasured"
    )
    stripped_eight = {k: v for k, v in eight.items() if k not in IDENTITY_EXCLUDED_FIELDS}
    stripped_nine = {k: v for k, v in nine.items() if k not in IDENTITY_EXCLUDED_FIELDS}
    for field in sorted(set(stripped_eight) | set(stripped_nine)):
        assert field in stripped_eight and field in stripped_nine, (
            f"identity check: field {field} exists in only one fixture"
        )
        assert stripped_eight[field] == stripped_nine[field], (
            f"identity check: field {field} differs between protocol 8 and "
            "the remeasured protocol 9 - the byte-identity claim of 5fc974d "
            "does not hold on the current implementation"
        )
    print("8/9 identity: PASS - every field equal outside "
          f"{', '.join(IDENTITY_EXCLUDED_FIELDS)}")


def compare(mode_name: str, before: dict, after: dict,
            before_path: Path, after_path: Path) -> dict:
    mode = MODES[mode_name]
    old_consts = mode["baseline"]
    lo, hi = mode["versions"]
    assert before["tape_protocol_version"] == lo, (
        f"{before_path} is protocol {before['tape_protocol_version']}, "
        f"not {lo} as --mode {mode_name} requires"
    )
    assert after["tape_protocol_version"] == hi, (
        f"{after_path} is protocol {after['tape_protocol_version']}, "
        f"not {hi} as --mode {mode_name} requires"
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
        if mode.get("acceptance") == "preset_fit":
            assert_crypto_frozen_and_futures_finite(old, new)
        else:
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
        "checkpoint_k": power_of_two(
            old_consts["checkpoint_k"] * ratios["checkpoint_k"] * 2
        ),
        "sweep_drain_budget": million(
            max(
                old_consts["sweep_drain_budget"] * ratios["sweep_drain_budget"] * 2,
                reach["sweep_drain_budget"],
            )
        ),
        "max_extend_ticks": old_consts["max_extend_ticks"],
        "warmup_materialization_ticks": million(
            max(
                old_consts["warmup_baseline"]
                * ratios["warmup_materialization_ticks"]
                * 2,
                reach["warmup_materialization_ticks"],
            )
        ),
        "fanout_depth": power_of_two(
            old_consts["fanout_depth"] * ratios["fanout_depth"] * 2
        ),
    }
    horizons = {
        "fanout_old_wall_seconds": min(
            old_consts["fanout_depth"]
            / old[k]["frames_per_wall_second"][speed]["p999"]
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
    return {
        "ratios": ratios,
        "observed": observed,
        "required_reach": reach,
        "proposed": proposed,
        "horizons": horizons,
    }


def selftest() -> None:
    """Fixture-driven checks of the NEW comparison logic (the preset_fit
    acceptance, the ratio arithmetic through it, and the 8/9 identity
    verifier), against synthetic fixtures written FOR them. Version rejection
    alone proves nothing about the arithmetic, so the arithmetic is pinned on
    known numbers."""
    checks = 0

    def check(label: str, ok: bool) -> None:
        nonlocal checks
        checks += 1
        assert ok, label
        print(f"  ok   {label}")

    def refuses(fn) -> bool:
        try:
            fn()
        except AssertionError:
            return True
        return False

    def stat(p999: float) -> dict:
        return {"p999": p999}

    def row(preset: str, seed: int, tps: float, tvw: float, tw: float,
            fws: float) -> dict:
        return {
            "preset": preset,
            "seed": seed,
            "configuration": "clean",
            "parents": 2_000_000,
            "ticks_per_parent": stat(10.0),
            "ticks_per_sim_second": stat(tps),
            "ticks_per_vol_window": stat(tvw),
            "ticks_per_warmup": stat(tw),
            "frames_per_wall_second": {"1.0": stat(fws), "10.0": stat(fws)},
        }

    def fixture(version: int, pairing: str, rows: list) -> dict:
        return {
            "tape_protocol_version": version,
            "pairing_id": pairing,
            "parent_events_per_combination": 2_000_000,
            "entries": rows,
        }

    crypto = [row("BTCUSDT", 1, 100.0, 1_000.0, 10_000.0, 50.0)]
    futures_before = [row("MNQ", 1, 200.0, 2_000.0, 20_000.0, 100.0)]
    # MNQ doubles every measured rate; crypto is byte-identical.
    futures_after = [row("MNQ", 1, 400.0, 4_000.0, 40_000.0, 200.0)]
    before = fixture(9, "pair-a", crypto + futures_before)
    after = fixture(10, "pair-b", crypto + futures_after)

    print("preset_fit arithmetic on known numbers")
    result = compare("independent_9_10", before, after, Path("b"), Path("a"))
    check("doubled MNQ rates ratio to exactly 2.0",
          all(abs(result["ratios"][k] - 2.0) < 1e-12 for k in result["ratios"]))
    base = MODES["independent_9_10"]["baseline"]
    check("checkpoint proposal is the doubled baseline rounded up, times two",
          result["proposed"]["checkpoint_k"] == power_of_two(base["checkpoint_k"] * 2.0 * 2))
    check("max_extend_ticks is carried, never scaled",
          result["proposed"]["max_extend_ticks"] == base["max_extend_ticks"])

    print("a fitted value may equal a declared one: no change also passes")
    unchanged_after = fixture(10, "pair-c", crypto + futures_before)
    result = compare("independent_9_10", before, unchanged_after, Path("b"), Path("a"))
    check("identical futures ratio to exactly 1.0",
          all(abs(result["ratios"][k] - 1.0) < 1e-12 for k in result["ratios"]))

    print("the crypto-equality gate, both ways")
    moved_crypto = [row("BTCUSDT", 1, 101.0, 1_000.0, 10_000.0, 50.0)]
    bad_after = fixture(10, "pair-d", moved_crypto + futures_after)
    check("a moved crypto preset refuses",
          refuses(lambda: compare("independent_9_10", before, bad_after,
                                  Path("b"), Path("a"))))
    check("an intact crypto preset passes (checked above)", True)

    print("finite and positive, refused before any ratio")
    nan_after = fixture(10, "pair-e",
                        crypto + [row("MNQ", 1, float("nan"), 4_000.0,
                                      40_000.0, 200.0)])
    check("a NaN futures measurement refuses",
          refuses(lambda: compare("independent_9_10", before, nan_after,
                                  Path("b"), Path("a"))))
    zero_after = fixture(10, "pair-f",
                         crypto + [row("MNQ", 1, 0.0, 4_000.0, 40_000.0,
                                       200.0)])
    check("a zero futures measurement refuses",
          refuses(lambda: compare("independent_9_10", before, zero_after,
                                  Path("b"), Path("a"))))
    bad_leaf = row("MNQ", 1, 400.0, 4_000.0, 40_000.0, 200.0)
    bad_leaf["ticks_per_parent"] = stat(float("nan"))
    outside_ratio_after = fixture(10, "pair-g", crypto + [bad_leaf])
    check("a NaN leaf OUTSIDE the ratio inputs still refuses",
          refuses(lambda: compare("independent_9_10", before,
                                  outside_ratio_after, Path("b"), Path("a"))))

    print("wrong versions refuse")
    check("a protocol-8 before refuses in independent_9_10",
          refuses(lambda: compare("independent_9_10",
                                  fixture(8, "pair-a", crypto + futures_before),
                                  after, Path("b"), Path("a"))))
    check("equal pairings refuse in an independent comparison",
          refuses(lambda: compare("independent_9_10", before,
                                  fixture(10, "pair-a", crypto + futures_after),
                                  Path("b"), Path("a"))))

    print("the 8/9 identity verifier, both ways")
    import tempfile

    scratch = Path(tempfile.mkdtemp(prefix="ratio-selftest-",
                                    dir=ROOT / "analysis"))
    try:
        eight = fixture(8, "pair-x", crypto + futures_before)
        nine_same = fixture(9, "pair-y", crypto + futures_before)
        nine_diff = fixture(9, "pair-y", crypto + futures_after)
        nine_same_pairing = fixture(9, "pair-x", crypto + futures_before)
        p8 = scratch / "p8.json"
        p9 = scratch / "p9.json"
        p8.write_text(json.dumps(eight))
        p9.write_text(json.dumps(nine_same))
        verify_8_9_identity(p8, p9)
        check("identical-but-for-version-and-pairing passes", True)
        p9.write_text(json.dumps(nine_diff))
        check("any other differing field refuses",
              refuses(lambda: verify_8_9_identity(p8, p9)))
        p9.write_text(json.dumps(nine_same_pairing))
        check("a shared pairing id refuses: identity must be REMEASURED",
              refuses(lambda: verify_8_9_identity(p8, p9)))
        nine_unpaired = fixture(9, "pair-y", crypto + futures_before)
        del nine_unpaired["pairing_id"]
        p9.write_text(json.dumps(nine_unpaired))
        check("a missing pairing id refuses rather than passing as different",
              refuses(lambda: verify_8_9_identity(p8, p9)))
    finally:
        for child in scratch.iterdir():
            child.unlink()
        scratch.rmdir()

    print(f"{checks} check(s), 0 failed")
    print("selftest PASS")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mode", choices=sorted(MODES), default="projection")
    parser.add_argument("--before", type=Path)
    parser.add_argument("--after", type=Path)
    parser.add_argument(
        "--selftest", action="store_true",
        help="run the synthetic-fixture checks of the comparison logic",
    )
    parser.add_argument(
        "--verify-8-9-identity", action="store_true", dest="verify_identity",
        help="assert the remeasured protocol-9 fixture equals protocol 8 "
             "outside tape_protocol_version and pairing_id",
    )
    args = parser.parse_args()
    if args.selftest:
        selftest()
        return
    if args.verify_identity:
        verify_8_9_identity(
            ROOT / "analysis/tick-composition-protocol-8.json",
            ROOT / "analysis/tick-composition-protocol-9.json",
        )
        return
    mode = MODES[args.mode]
    before_path = args.before or ROOT / mode["before"]
    after_path = args.after or ROOT / mode["after"]
    before = json.loads(before_path.read_text())
    after = json.loads(after_path.read_text())
    result = compare(args.mode, before, after, before_path, after_path)
    print(json.dumps(result, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
