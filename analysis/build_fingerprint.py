#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 folknor
# SPDX-License-Identifier: AGPL-3.0-only

"""Synthesize the generator fingerprint from the per-pair characterizations.

Reads every char_<PAIR>.json under analysis/ and produces two artifacts:

  fingerprint.json - the contract the synthetic generator is built against:
    * golden stylized-fact targets with tolerances (the validation gate),
    * the pooled UTC session profile (24-hour intensity and volatility curves,
      7-day weights), normalized and instrument-agnostic,
    * scalar ranges that bound the per-instrument knobs (tick, price decimals,
      activity, size shape).

  findings.md - a human-readable summary of the same.

The model is symbol-agnostic, so curves are normalized per pair (each pair to its
own total / its own mean) before pooling: what survives is the shared shape, not
any one instrument's level. XBTUSD, as the deepest series, anchors the golden
ACF/dispersion targets; the cross-pair spread sets the tolerances.
"""

import glob
import json
import os
import statistics

HERE = os.path.dirname(__file__)
ANCHOR = "XBTUSD"

LEVEL_DOC = (
    "AT-TOUCH TRADED VOLUME per level visit, era-windowed, expressed as a "
    "multiple of the same era's median trade size. NOT book depth and NOT "
    "queue position: cancelled liquidity is invisible so the number is "
    "deflated, liquidity that joined the level mid-visit is counted so it is "
    "also inflated, and the corpus has no aggressor side so buy- and "
    "sell-initiated flow at one price are summed together."
)
# The four proceed conditions a trades-only queue-ahead fill model would have
# had to clear before it could claim its queue quantity was fitted rather than
# chosen. Each is a reading of the numbers in this same block, recomputed on
# every regeneration so the verdict cannot drift away from the measurement.
LEVEL_CONDITIONS = (
    ("single_print_frac <= 0.50", "a strict majority of level visits carry "
     "more than one print, so visit volume is a real quantity and not a "
     "restatement of the trade-size distribution"),
    ("vol_dispersion >= 3.0", "the volume distribution has genuine dispersion, "
     "so a draw from it is not a constant in disguise"),
    ("vol_dispersion >= 1.5 * size_dispersion", "that dispersion exceeds the "
     "trade-size dispersion it could have been inherited from"),
    ("anchor single_print_frac within 1.5x of the cross-pair median",
     "the anchor is not the outlier a model would have been fitted to"),
)


def level_verdict(level, single_fracs):
    """Evaluate the four proceed conditions against the anchor's measurement."""
    median = statistics.median(single_fracs)
    held = (
        level["single_print_frac"] <= 0.50,
        level["vol_dispersion"] >= 3.0,
        level["vol_dispersion"] >= 1.5 * level["size_dispersion"],
        median / 1.5 <= level["single_print_frac"] <= median * 1.5,
    )
    failed = [test for (test, _why), ok in zip(LEVEL_CONDITIONS, held) if not ok]
    return {
        "proceed": all(held),
        "conditions": [
            {"test": test, "why": why, "held": ok}
            for (test, why), ok in zip(LEVEL_CONDITIONS, held)
        ],
        "failed": failed,
        "single_print_frac_cross_pair_median": median,
    }


def level_queue(anchor, reports):
    """Promote the at-touch level-visit measurement into a golden target.

    Refuses rather than degrades. A report without a `level` block was written
    by a characterization pass older than the measurement, and quietly dropping
    it - or dropping the whole target - would put a cross-pair band on the
    record that some pairs never contributed to, or leave the measurement out
    of the fingerprint entirely with nothing said about it. Both hide a stale
    `char_<PAIR>.json`, which is the one failure this measurement cannot
    survive: the proceed/close verdict is read off the anchor's numbers.
    """
    stale = sorted(pair for pair, report in reports.items() if "level" not in report)
    if stale:
        raise ValueError(
            "char_<PAIR>.json predates the level measurement, re-run "
            f"run_corpus.py for: {', '.join(stale)}")
    levels = [report["level"] for report in reports.values()]
    binning = {key: anchor["level"][key] for key in ("bin_lo", "bin_hi", "bins_per_decade")}
    if any({key: level[key] for key in binning} != binning for level in levels):
        raise ValueError("level histogram binning differs across pairs")
    level = anchor["level"]
    total = sum(level["vol_hist"])
    if not total or not level["size_median"]:
        raise ValueError("anchor has no usable level visits")
    support = [
        binning["bin_lo"] * 10 ** ((index + 0.5) / binning["bins_per_decade"])
        / level["size_median"]
        for index in range(len(level["vol_hist"]))
    ]
    verdict = level_verdict(level, [x["single_print_frac"] for x in levels])
    if verdict["proceed"]:
        reading = (
            " All four proceed conditions hold, so the queue-ahead fill model "
            "may sample support_norm/pmf as an inverse CDF; single_print_frac "
            "is the credibility reading its landing was judged against.")
    else:
        reading = (
            " VERDICT: DECLINED. RFC 4631 phase B is refused in full - "
            f"{', '.join(verdict['failed'])} does not hold on the anchor "
            f"(single_print_frac {level['single_print_frac']:.6f}, "
            f"vol_dispersion {level['vol_dispersion']:.6f}, "
            f"size_dispersion {level['size_dispersion']:.6f}), so a "
            "trades-only queue-ahead quantity would be a free parameter "
            "dressed in a histogram. Nothing samples support_norm/pmf; they "
            "are kept as the durable corpus fact the refusal was read off.")
    return {
        "era_start_ts": level["era_start_ts"],
        "_doc": LEVEL_DOC + reading,
        "single_print_frac": {"anchor": level["single_print_frac"], "range": rng([x["single_print_frac"] for x in levels])},
        "vol_p50_norm": {"anchor": level["vol_p50_norm"], "range": rng([x["vol_p50_norm"] for x in levels])},
        "vol_p90_norm": {"anchor": level["vol_p90_norm"], "range": rng([x["vol_p90_norm"] for x in levels])},
        "vol_dispersion": {"anchor": level["vol_dispersion"], "range": rng([x["vol_dispersion"] for x in levels])},
        "size_dispersion": {"anchor": level["size_dispersion"], "range": rng([x["size_dispersion"] for x in levels])},
        "verdict": verdict,
        "binning": binning,
        "support_norm": support,
        "pmf": [count / total for count in level["vol_hist"]],
    }


def load_reports():
    reps = {}
    for path in sorted(glob.glob(os.path.join(HERE, "char_*.json"))):
        with open(path) as f:
            r = json.load(f)
        reps[r["pair"]] = r
    return reps


def hour_shares(rep):
    c = rep["session"]["count_hour_dow"]
    hour = [sum(c[h]) for h in range(24)]
    tot = sum(hour) or 1
    return [x / tot for x in hour]


def hour_vol(rep):
    c = rep["session"]["count_hour_dow"]
    s = rep["session"]["sumsq_ret_hour_dow"]
    out = []
    for h in range(24):
        cnt = sum(c[h])
        out.append((sum(s[h]) / cnt) ** 0.5 if cnt else 0.0)
    mean = (statistics.fmean([v for v in out if v > 0]) or 1.0) if any(out) else 1.0
    return [v / mean for v in out]  # normalized to per-pair mean


def dow_weights(rep):
    c = rep["session"]["count_hour_dow"]
    dow = [sum(c[h][d] for h in range(24)) for d in range(7)]  # Sun=0..Sat=6
    tot = sum(dow) or 1
    return [x / tot for x in dow]


def avg_curves(curves):
    n = len(curves)
    k = len(curves[0])
    return [sum(c[i] for c in curves) / n for i in range(k)]


def rng(values):
    vals = [v for v in values if v is not None]
    if not vals:
        return None
    return {
        "min": min(vals),
        "median": statistics.median(vals),
        "max": max(vals),
    }


def main():
    reps = load_reports()
    if not reps:
        print("no char_*.json found; run run_corpus.py first")
        return 1
    pairs = list(reps)
    anchor = reps.get(ANCHOR) or reps[pairs[0]]

    intensity = avg_curves([hour_shares(r) for r in reps.values()])
    vol = avg_curves([hour_vol(r) for r in reps.values()])
    dow = avg_curves([dow_weights(r) for r in reps.values()])

    # Duration-side targets (dispersion band, duration ACF anchor) are
    # era-windowed like the dwell block: the full-span values are dominated
    # by infancy/outage deserts the default profile does not claim (the
    # full-span anchor dispersion 4608.9 collapses to ~36 in the modern
    # window), so a gate fitted full-span polices exactly the behavior the
    # drought elimination evicts. Return/size/session targets stay full-span:
    # they are per-tick shape statistics, not gap statistics.
    disp = [r["duration"]["dwell"]["dispersion_index"] for r in reps.values()]
    ret1 = [r["returns"]["acf"][0] for r in reps.values()]
    abs1 = [r["returns"]["abs_acf"][0] for r in reps.values()]
    abs10 = [r["returns"]["abs_acf"][9] for r in reps.values()]
    abs50 = [r["returns"]["abs_acf"][49] for r in reps.values()]
    zchg = [r["returns"]["zero_change_frac"] for r in reps.values()]
    dwell = [r["duration"]["dwell"] for r in reps.values()]
    queue = level_queue(anchor, reps)

    fingerprint = {
        "source": {
            "pairs": pairs,
            "total_trades": sum(r["n_trades"] for r in reps.values()),
            "anchor": anchor["pair"],
        },
        "golden_targets": {
            "_doc": "the generator's synthetic stream must reproduce these; "
                    "tolerances are the cross-pair spread, anchored on the "
                    "deepest series; duration dispersion and duration ACF are "
                    "era-windowed like dwell, everything else full-span",
            "duration_dispersion_index": {
                "anchor": anchor["duration"]["dwell"]["dispersion_index"],
                "range": rng(disp),
            },
            "return_acf_lag1": {"anchor": anchor["returns"]["acf"][0],
                                "range": rng(ret1)},
            "abs_return_acf": {
                "lag1": {"anchor": anchor["returns"]["abs_acf"][0],
                         "range": rng(abs1)},
                "lag10": {"anchor": anchor["returns"]["abs_acf"][9],
                          "range": rng(abs10)},
                "lag50": {"anchor": anchor["returns"]["abs_acf"][49],
                          "range": rng(abs50)},
            },
            "zero_change_frac": {"anchor": anchor["returns"]["zero_change_frac"],
                                 "range": rng(zchg)},
            "duration_acf_anchor": anchor["duration"]["dwell"]["acf"][:10],
            "return_acf_anchor": anchor["returns"]["acf"][:10],
            "abs_return_acf_anchor": anchor["returns"]["abs_acf"],
            "dwell": {
                "era_start_ts": anchor["duration"]["dwell"]["era_start_ts"],
                "mean_s": {"anchor": anchor["duration"]["dwell"]["mean_s"],
                           "range": rng([d["mean_s"] for d in dwell])},
                "max_gap_s": {"anchor": anchor["duration"]["dwell"]["max_gap_s"],
                              "range": rng([d["max_gap_s"] for d in dwell])},
                "gap_p999_s": {"anchor": anchor["duration"]["dwell"]["gap_p999_s"],
                               "range": rng([d["gap_p999_s"] for d in dwell])},
                "empty_hour_frac": {"anchor": anchor["duration"]["dwell"]["empty_hour_frac"],
                                    "range": rng([d["empty_hour_frac"] for d in dwell])},
                "max_empty_hour_run_h": {"anchor": anchor["duration"]["dwell"]["max_empty_hour_run_h"],
                                         "range": rng([d["max_empty_hour_run_h"] for d in dwell])},
                "_doc": "era-windowed; gate reads the anchor p999, empty-hour fraction, and run, with p999 cadence-scaled against mean_s; max_gap_s is documentation and the range records the dying-symbol spread LiquidityDrought imitates",
            },
        },
        "session_profile": {
            "_doc": "UTC, instrument-agnostic. intensity[h] and vol[h] index "
                    "hour-of-day 0..23; dow[d] indexes Sun=0..Sat=6",
            "intensity_hour": intensity,
            "vol_hour": vol,
            "dow_weight": dow,
        },
        "scalar_ranges": {
            "_doc": "bound the per-instrument knobs the generator is called with",
            "modal_tick": rng([r["returns"]["modal_tick"] for r in reps.values()]),
            "price_decimals": rng(
                [r["returns"]["price_decimals_mode"] for r in reps.values()]),
            "mean_duration_s": rng(
                [r["duration"]["mean_s"] for r in reps.values()]),
            "size_round_frac": rng(
                [r["size"]["round_frac"] for r in reps.values()]),
        },
    }
    fingerprint["golden_targets"]["level_queue"] = queue

    out = os.path.join(HERE, "fingerprint.json")
    with open(out, "w") as f:
        json.dump(fingerprint, f, indent=2)

    # findings.md
    md = []
    md.append("# Phase 0 findings: Kraken tick-stream characterization\n")
    md.append(f"Pairs analyzed: {', '.join(pairs)}  ")
    md.append(f"Total trades: {fingerprint['source']['total_trades']:,}  ")
    md.append(f"Anchor (golden targets): {anchor['pair']}\n")
    md.append("## Stylized facts (cross-pair; disp is era-windowed)\n")
    md.append("| pair | trades | disp | ret1 | |ret|1 | zchg | tick | pdec |")
    md.append("|---|--:|--:|--:|--:|--:|--:|--:|")
    for p in sorted(reps, key=lambda x: -reps[x]["n_trades"]):
        r = reps[p]
        md.append(
            f"| {p} | {r['n_trades']:,} | "
            f"{r['duration']['dwell']['dispersion_index']:.0f} | "
            f"{r['returns']['acf'][0]:.2f} | {r['returns']['abs_acf'][0]:.2f} | "
            f"{r['returns']['zero_change_frac']:.2f} | "
            f"{r['returns']['modal_tick']} | {r['returns']['price_decimals_mode']} |"
        )
    md.append("\n## UTC session intensity (pooled, normalized share)\n")
    peak = max(intensity)
    for h in range(24):
        bar = "#" * int(round(40 * intensity[h] / peak))
        md.append(f"    {h:02d}:00  {100*intensity[h]:5.2f}%  {bar}")
    anchor_dwell = anchor["duration"]["dwell"]
    md.append("\n## Modern-era dwell (anchor)\n")
    md.append(
        f"Window starts at {anchor_dwell['era_start_ts']}; mean gap "
        f"{anchor_dwell['mean_s']:.3f}s, p999 {anchor_dwell['gap_p999_s']:.3f}s, "
        f"max gap {anchor_dwell['max_gap_s']:.3f}s, empty-hour fraction "
        f"{anchor_dwell['empty_hour_frac']:.6f}, longest empty run "
        f"{anchor_dwell['max_empty_hour_run_h']}h.\n"
    )
    md.append("\nintensity peaks at the London-NY overlap, troughs in the "
              "Asian small hours; weekends lighter (see dow_weight).\n")
    with open(os.path.join(HERE, "findings.md"), "w") as f:
        f.write("\n".join(md) + "\n")

    print(f"fingerprint: {out}")
    print(f"pairs: {pairs}")
    print(f"golden disp anchor={fingerprint['golden_targets']['duration_dispersion_index']['anchor']:.0f} "
          f"ret1 anchor={fingerprint['golden_targets']['return_acf_lag1']['anchor']:.3f}")
    print("dwell anchor: "
          f"p999={anchor_dwell['gap_p999_s']:.3f}s "
          f"empty_hours={anchor_dwell['empty_hour_frac']:.6f} "
          f"max_empty_run={anchor_dwell['max_empty_hour_run_h']}h")
    print("findings.md written")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
