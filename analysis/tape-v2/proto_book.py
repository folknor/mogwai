#!/usr/bin/env python3
"""Prototype of the discrete book: spread state, effective depletion,
minimal projection and permanent-only impact, jointly, on the splitting
sign model. The mechanism is `notes/book-dynamics-spec.md`; the fit
targets are the book statistics `micro-stats` now measures on the real
year (`data/micro/MNQ-real-targets.json`, pulled from the run host).

The simulation is parent-indexed: each step is one parent observation,
so the prototype's transition statistics are between-parent effective
transitions by construction, the same convention the real extraction is
limited to. Elapsed-time conditioning has no analogue here; the real
`lt_100ms` bucket is the comparison column, because at the pooled parent
rate most consecutive parents sit inside it.

State per step, all in ticks: the latent efficient price x, the book
(bid b, ask b + s), the touch quantities on both sides, and one exposed
residual quantity behind each touch. Per parent, in the causal order the
spec fixes:

1. external diffusion moves x, then minimal projection re-centres the
   book only if the mid left the admissible band (s/2 + slack around x);
2. the sign comes from the splitting model, the parent quantity from the
   real size pmf;
3. the quantity walks the struck side: consuming a level exhausts it and
   exposes the level behind with its residual quantity; the walk is
   capped by the declared ladder depth;
4. effective depletion: even a walk that consumed the whole touch leaves
   the touch in place when replenishment beats the observation (the
   witness at `gt` is 0.6, not 1.0), and a touch the parent did not
   consume can still be gone at the next observation (witness at `lt` is
   0.17) - both are one fitted probability per trichotomy arm, the
   effective-model reading the spec discloses;
5. the spread relaxes: a book wider than the drawn target narrows by
   quoting inside on the side that brings the mid toward x, with a fresh
   replenishment quantity; a book at one tick can widen by a fitted
   cancellation probability;
6. the permanent impact g * sign moves x, and one more projection
   reconciles the book with the moved anchor.

    uv --directory analysis/tape-v2 run python proto_book.py --phase all
    uv --directory analysis/tape-v2 run python proto_book.py --grid
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

from proto_micro import splitting_signs  # noqa: E402

TARGETS = HERE / "data" / "micro" / "MNQ-real-targets.json"
LAGS = (1, 10, 100)
SIZE_EDGES = [1, 2, 3, 4, 5, 10, 20, 50]
SIZE_REP = [1, 2, 3, 4, 5, 7, 15, 35, 70]


# ------------------------------------------------------------- real side


def real_medians(phase: str, month: str | None = None) -> dict:
    """Median across the month blocks of every statistic the score uses,
    or one month's own values when `month` is given (the holdout view)."""
    blocks = json.loads(TARGETS.read_text())["blocks"]
    if month is not None:
        blocks = {month: blocks[month]}
    rows = [b[phase] for b in blocks.values() if phase in b]

    def med(path: list[str]) -> float:
        values = []
        for r in rows:
            node = r
            for key in path:
                node = node.get(key, {})
            if isinstance(node, (int, float)):
                values.append(float(node))
        return float(np.median(values)) if values else float("nan")

    out = {
        "spread_pmf": {k: med(["price", "spread_pmf", k]) for k in ("1", "2", "3", "4+")},
        # The fast-bucket pmfs: the prototype is parent-indexed with no
        # elapsed time, so consecutive parents inside 100 ms are what it
        # honestly models.
        "dmid_pmf": {
            k: med(["book", "mid_change_pmf_fast", k])
            for k in ("0", "0.5", "1", "1.5", "2", "2.5+")
        },
        "dprice_pmf": {k: med(["price", "abs_change_pmf_fast", k]) for k in ("0", "1", "2", "3+")},
        "impact": {str(lag): med(["price", "impact", str(lag), "mean_ticks"]) for lag in LAGS},
        "impact_spread_1": med(["book", "impact_by_spread", "1", "1"]),
        "impact_spread_2": med(["book", "impact_by_spread", "2", "1"]),
        "witness": {
            k: med(["book", "depletion_witness", "lt_100ms", k]) for k in ("lt", "eq", "gt")
        },
        "spr_1_stay": med(["book", "spread_transition", "lt_100ms", "1", "1"]),
        "spr_2_stay": med(["book", "spread_transition", "lt_100ms", "2", "2"]),
        "touch_p50_1": med(["book", "touch_size_spread_1", "p50"]),
        "touch_p50_2": med(["book", "touch_size_spread_2", "p50"]),
        "size_ge_touch": med(["sweep", "touch", "size_ge_touch"]),
        "size_gt_touch": med(["sweep", "touch", "size_gt_touch"]),
        "size_pmf": {
            k: med(["sweep", "parent_size_pmf", k])
            for k in ("1", "2", "3", "4", "5", "6-10", "11-20", "21-50", "51+")
        },
    }
    return out


def draw_sizes(rng: np.random.Generator, count: int, size_pmf: dict) -> np.ndarray:
    probs = np.array(list(size_pmf.values()))
    probs = probs / probs.sum()
    return rng.choice(np.array(SIZE_REP), size=count, p=probs)


# ------------------------------------------------------------- the model


class BookConfig(argparse.Namespace):
    pass


def simulate(rng: np.random.Generator, sign: np.ndarray, sizes: np.ndarray, cfg) -> dict:
    n = sign.size
    # Recorded per parent: the pre-trade book and the last print.
    pre_bid = np.empty(n)
    pre_ask = np.empty(n)
    pre_touch = np.empty(n)
    last_print = np.empty(n)
    witnessed = np.zeros(n, dtype=bool)
    tri = np.empty(n, dtype="U2")

    def replenish() -> int:
        # Geometric on 1.. with the fitted mean: the law behind a fresh
        # queue, which is thinner than the observed pre-trade touch
        # because large queues survive longer.
        return 1 + rng.geometric(1.0 / cfg.replenish_mean) - 1 or 1

    def fresh(spread: int) -> int:
        # A fresh touch quantity, wider books quoting deeper.
        mean = cfg.replenish_mean * (cfg.depth_by_spread ** (spread - 1))
        return max(1, int(rng.geometric(1.0 / mean)))


    x = 1000.0
    bid = int(round(x)) - 1
    ask = bid + 2
    q = {1: fresh(2), -1: fresh(2)}  # +1 keys the ask (buys strike it)
    register = 0.0
    sig_level = 0.0

    def project() -> None:
        nonlocal bid, ask
        s = ask - bid
        mid = bid + s / 2.0
        band = s / 2.0 + cfg.slack
        if abs(mid - x) <= band:
            return
        # Minimal translation of the whole book onto the nearest
        # admissible placement.
        shift = x - mid
        move = int(np.ceil(abs(shift) - band)) * (1 if shift > 0 else -1)
        bid += move
        ask += move
        bid = max(bid, 1)
        ask = bid + s

    p_widen = cfg.p_widen
    for k in range(n):
        # 1. External diffusion, then projection. Student-t with a
        # persistent lognormal sigma level: the real dmid pmf carries
        # five percent of moves past 2.5 ticks, which a constant sigma
        # never produces - in the engine this modulation is the cascade's
        # own second_sigma, supplied for free.
        sig_level = cfg.rho_sig * sig_level + np.sqrt(1.0 - cfg.rho_sig**2) * rng.standard_normal()
        sigma = cfg.sigma_e * np.exp(cfg.sig_ln * sig_level - 0.5 * cfg.sig_ln**2)
        x += sigma * rng.standard_t(cfg.df) / np.sqrt(cfg.df / (cfg.df - 2.0))
        project()
        s = ask - bid
        pre_bid[k] = bid
        pre_ask[k] = ask
        side = int(sign[k])
        touch_q = q[side]
        pre_touch[k] = touch_q
        # 2 and 3. The walk on the struck side, against the frozen
        # per-parent ladder: level i behind the touch carries the
        # declared multiply-and-floor quantity, constructed once from
        # the pre-trade snapshot and never rebuilt mid-parent. Only the
        # touch quantity is stochastic state; the deeper book is the
        # ladder both the generator and the venue crossing derive.
        # Parent size is endogenous to the touch nearly half the time:
        # the real eq arm of the trichotomy is 0.46 pooled, because a
        # taker sizes the order to the displayed quantity. With the
        # match probability the size is exactly the touch; otherwise it
        # is the independent draw from the fitted size law.
        v = int(touch_q) if rng.random() < cfg.p_size_match else int(sizes[k])
        struck_price = ask if side > 0 else bid
        ladder = [touch_q]
        for _level in range(1, cfg.max_levels):
            ladder.append(max(1, int(ladder[-1] * cfg.depth_growth)))
        remaining = v
        levels = 0
        price = struck_price
        residual = touch_q
        while levels < cfg.max_levels and remaining >= ladder[levels]:
            remaining -= ladder[levels]
            levels += 1
            if remaining == 0 or levels == cfg.max_levels:
                break
            price = struck_price + levels * side
        if levels < cfg.max_levels:
            residual = ladder[levels] - remaining
        else:
            # Declared depth exhausted: the parent partially fills and
            # the post-trade replenishment creates the next touch.
            residual = 0
        consumed_touch = levels > 0
        last_print[k] = price
        tri[k] = "lt" if v < touch_q else ("eq" if v == touch_q else "gt")
        # 4. Effective depletion: does the touch actually recede by the
        # next observation?
        p = {"lt": cfg.p_dep_lt, "eq": cfg.p_dep_eq, "gt": cfg.p_dep_gt}[str(tri[k])]
        if rng.random() < p:
            witnessed[k] = True
            recede = max(1, levels)
            if side > 0:
                ask += recede
            else:
                bid -= recede
            # The exposed touch is the surviving level's exact residual
            # where the walk reached it; a receded touch the walk never
            # touched, or an exhausted ladder, takes a replenishment
            # draw.
            q[side] = residual if levels > 0 and residual > 0 else fresh(ask - bid)
            # The un-struck side follows a depletion in with a fitted
            # probability: quotes step up behind a lifted ask (and down
            # behind a hit bid), which is where most of the immediate
            # mid response lives - the real lag-1 impact is twice what
            # the struck-side recession alone can produce.
            if rng.random() < cfg.p_follow:
                if side > 0:
                    bid += recede
                else:
                    ask -= recede
                q[-side] = fresh(ask - bid)
        else:
            # Replenished before observed: the touch price stands; its
            # quantity is a fresh draw when the walk consumed it.
            if consumed_touch:
                q[side] = fresh(s)
        # 5. The impact moves the anchor before the spread relaxes, so
        # narrowing is directional: after a buy the inside quote tends to
        # arrive on the bid. The transient term is the residual impact
        # channel the fit protocol admits after the emergent route
        # measurably undershot lags 1 and 10 with lag 100 on target: the
        # book is anchored to x through projection, so a flat response
        # curve needs x itself to carry a decaying component. Projection
        # credits mechanical movement against it, so nothing is counted
        # twice. A witnessed depletion that no quote followed keeps its
        # widened book through this parent - narrowing it back in the
        # same step would cancel the very move the witness measures.
        x += cfg.g * side + cfg.a_t * (side - (1.0 - cfg.rho) * register)
        register = cfg.rho * register + side
        s = ask - bid
        # The stationary spread the relaxation aims at: three states, the
        # third carrying the wide-book mass Asia needs (29 percent of its
        # real books are 3 ticks or wider).
        u = rng.random()
        target = 1 if u < cfg.p_target_1 else (2 if u < cfg.p_target_1 + cfg.p_target_2 else 3)
        while s > target and not witnessed[k] and rng.random() < cfg.p_narrow:
            # Quote inside on the side that brings the mid toward x when
            # the displacement is meaningful, else symmetrically.
            mid = bid + s / 2.0
            toward_ask = x < mid - 0.25 or (abs(x - mid) <= 0.25 and rng.random() < 0.5)
            if toward_ask:
                ask -= 1
                q[1] = fresh(s - 1)
            else:
                bid += 1
                q[-1] = fresh(s - 1)
            s = ask - bid
        # Widening is a quote pull at any spread below the drawn target,
        # not only at one tick: a two-tick book in a thin phase loses its
        # touch to cancellation as readily as a locked one.
        if s < target and rng.random() < p_widen:
            if rng.random() < 0.5:
                ask += 1
                q[1] = max(1, int(q[1] * cfg.depth_growth))
            else:
                bid -= 1
                q[-1] = max(1, int(q[-1] * cfg.depth_growth))
        # 6. The post-trade projection against the moved anchor.
        project()

    return {
        "pre_bid": pre_bid,
        "pre_ask": pre_ask,
        "pre_touch": pre_touch,
        "last_print": last_print,
        "witnessed": witnessed,
        "tri": tri,
        "sign": sign.astype(float),
        "sizes": sizes,
    }


# ------------------------------------------------------------ statistics


def pmf_counts(values: np.ndarray, edges: list[tuple[str, float, float | None]]) -> dict:
    out = {}
    for label, lo, hi in edges:
        mask = values >= lo if hi is None else (values >= lo) & (values < hi)
        out[label] = float(mask.mean())
    return out


def measure(sim: dict) -> dict:
    spread = sim["pre_ask"] - sim["pre_bid"]
    mid = (sim["pre_ask"] + sim["pre_bid"]) / 2.0
    sign = sim["sign"]
    out: dict = {}
    out["spread_pmf"] = pmf_counts(
        spread, [("1", 1, 2), ("2", 2, 3), ("3", 3, 4), ("4+", 4, None)]
    )
    dmid = np.abs(np.diff(mid))
    out["dmid_pmf"] = pmf_counts(
        dmid,
        [
            ("0", 0, 0.25),
            ("0.5", 0.25, 0.75),
            ("1", 0.75, 1.25),
            ("1.5", 1.25, 1.75),
            ("2", 1.75, 2.25),
            ("2.5+", 2.25, None),
        ],
    )
    dprice = np.abs(np.diff(sim["last_print"]))
    out["dprice_pmf"] = pmf_counts(
        dprice, [("0", 0, 0.5), ("1", 0.5, 1.5), ("2", 1.5, 2.5), ("3+", 2.5, None)]
    )
    impact = {}
    for lag in LAGS:
        move = (mid[lag:] - mid[:-lag]) * sign[:-lag]
        impact[str(lag)] = float(move.mean())
    out["impact"] = impact
    for state, want in (("1", 1), ("2", 2)):
        anchors = spread[:-1] == want
        move = (mid[1:][anchors] - mid[:-1][anchors]) * sign[:-1][anchors]
        out[f"impact_spread_{state}"] = float(move.mean()) if anchors.sum() else float("nan")
    witness = {}
    for arm in ("lt", "eq", "gt"):
        mask = sim["tri"] == arm
        witness[arm] = float(sim["witnessed"][mask].mean()) if mask.sum() else float("nan")
    out["witness"] = witness
    stay = {}
    for state, want in (("1", 1), ("2", 2)):
        cur = spread[:-1] == want
        stay[state] = float((spread[1:][cur] == want).mean()) if cur.sum() else float("nan")
    out["spr_1_stay"] = stay["1"]
    out["spr_2_stay"] = stay["2"]
    for state, want in (("1", 1), ("2", 2)):
        mask = spread == want
        out[f"touch_p50_{state}"] = (
            float(np.median(sim["pre_touch"][mask])) if mask.sum() else float("nan")
        )
    out["size_ge_touch"] = float((sim["tri"] != "lt").mean())
    out["size_gt_touch"] = float((sim["tri"] == "gt").mean())
    return out


SCORE_KEYS = [
    ("spread_pmf", "1"),
    ("spread_pmf", "2"),
    ("dmid_pmf", "0"),
    ("dmid_pmf", "0.5"),
    ("dmid_pmf", "1"),
    ("dmid_pmf", "2.5+"),
    ("dprice_pmf", "0"),
    ("dprice_pmf", "1"),
    ("dprice_pmf", "3+"),
    ("impact", "1"),
    ("impact", "10"),
    ("impact", "100"),
    ("impact_spread_1", None),
    ("impact_spread_2", None),
    ("witness", "lt"),
    ("witness", "gt"),
    ("spr_1_stay", None),
    ("spr_2_stay", None),
    ("touch_p50_1", None),
    ("touch_p50_2", None),
    ("size_ge_touch", None),
    ("size_gt_touch", None),
]


def score(candidate: dict, real: dict) -> float:
    misses = []
    for key, sub in SCORE_KEYS:
        c = candidate[key] if sub is None else candidate[key][sub]
        r = real[key] if sub is None else real[key][sub]
        if np.isnan(c) or np.isnan(r) or r <= 0 or c <= 0:
            misses.append(1.0)
        else:
            misses.append(abs(np.log(c / r)))
    return float(np.mean(misses))


def report(candidate: dict, real: dict) -> None:
    print(f"{'statistic':<22}{'proto':>9}{'real':>9}")
    for key, sub in SCORE_KEYS:
        c = candidate[key] if sub is None else candidate[key][sub]
        r = real[key] if sub is None else real[key][sub]
        name = key if sub is None else f"{key} {sub}"
        print(f"{name:<22}{c:>9.3f}{r:>9.3f}")
    print(f"score {score(candidate, real):.3f}")


# ---------------------------------------------------------------- driver


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--phase", default="all")
    ap.add_argument("--parents", type=int, default=400_000)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--sigma-e", type=float, default=0.35)
    ap.add_argument("--df", type=float, default=4.0)
    ap.add_argument("--sig-ln", type=float, default=0.8)
    ap.add_argument("--rho-sig", type=float, default=0.995)
    ap.add_argument("--g", type=float, default=0.28)
    ap.add_argument("--a-t", type=float, default=0.22)
    ap.add_argument("--rho", type=float, default=0.98)
    ap.add_argument("--slack", type=float, default=1.0)
    ap.add_argument("--p-dep-lt", type=float, default=0.10)
    ap.add_argument("--p-dep-eq", type=float, default=0.45)
    ap.add_argument("--p-dep-gt", type=float, default=0.60)
    ap.add_argument("--p-narrow", type=float, default=0.35)
    ap.add_argument("--p-target-1", type=float, default=0.5)
    ap.add_argument("--p-target-2", type=float, default=0.45)
    ap.add_argument("--p-follow", type=float, default=0.6)
    ap.add_argument("--p-size-match", type=float, default=0.4)
    ap.add_argument("--p-widen", type=float, default=0.12)
    ap.add_argument("--replenish-mean", type=float, default=2.0)
    ap.add_argument("--depth-by-spread", type=float, default=1.6)
    ap.add_argument("--depth-growth", type=float, default=2.0)
    ap.add_argument("--max-levels", type=int, default=8)
    ap.add_argument("--slots", type=int, default=5)
    ap.add_argument("--alpha", type=float, default=2.2)
    ap.add_argument("--mix", type=float, default=0.08)
    ap.add_argument("--grid", action="store_true")
    ap.add_argument(
        "--holdout",
        action="store_true",
        help="score the configuration against every real month block "
        "separately instead of the cross-month median",
    )
    args = ap.parse_args()
    real = real_medians(args.phase)
    rng = np.random.default_rng(args.seed)
    sign = splitting_signs(rng, args.parents, args.slots, args.alpha, args.mix)
    sizes = draw_sizes(rng, args.parents, real["size_pmf"])
    if args.grid:
        import itertools

        sweep = {
            "p_narrow": (0.55, 0.62, 0.7),
            "p_target_1": (0.45, 0.55, 0.65),
            "p_widen": (0.14, 0.18, 0.24),
            "replenish_mean": (1.3, 1.6, 2.0),
            "sig_ln": (1.1, 1.3, 1.5),
            "a_t": (0.28, 0.32, 0.38),
        }
        names = list(sweep)
        best: tuple = (1e9, None)
        for values in itertools.product(*sweep.values()):
            cfg = argparse.Namespace(**vars(args))
            for name, value in zip(names, values):
                setattr(cfg, name, value)
            sim = simulate(np.random.default_rng(args.seed + 1), sign, sizes, cfg)
            s = score(measure(sim), real)
            print("  ".join(f"{n}={v}" for n, v in zip(names, values)) + f"  score {s:.3f}")
            if s < best[0]:
                best = (s, values)
        print("best " + f"{best[0]:.3f} at " + "  ".join(f"{n}={v}" for n, v in zip(names, best[1])))
        return
    sim = simulate(np.random.default_rng(args.seed + 1), sign, sizes, args)
    measured = measure(sim)
    if args.holdout:
        months = sorted(json.loads(TARGETS.read_text())["blocks"])
        for month in months:
            month_real = real_medians(args.phase, month)
            print(f"{month}  score {score(measured, month_real):.3f}")
        return
    report(measured, real)


if __name__ == "__main__":
    main()
