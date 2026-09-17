#!/usr/bin/env python3
"""Prototype of the discrete book: spread state, effective depletion,
minimal projection and permanent-plus-transient impact, jointly, on the
splitting sign model. The mechanism is `notes/book-dynamics-spec.md`; the
fit targets are the book statistics `micro-stats` measures on the real
year (`data/micro/MNQ-real-targets.json`, pulled from the run host).

This is the refit revision after the first engine transcription measured
a heavy spread tail and inflated parent sizes. Three mechanism changes,
sparred to consensus 2026-09-16:

- Replenishment is a per-spread-state table (states 1, 2, 3+), not the
  exponential `touch_by_spread ** (spread - 1)`: the real conditional
  touch is non-monotone in spread (p50 2, 5, 3), which falsifies any
  monotone law and removes the width-to-quantity amplification at the
  root. A draw uses the state immediately after the transition creating
  the queue; an existing queue is never redrawn because a later
  transition changed the spread.
- The recession is a saturated transition: the visible recede is capped
  by the room below `max_spread`, the follow-in moves by the recede
  actually applied, the exact residual survives only when the visible
  touch lands on the level it belongs to, and only a positive applied
  recession suppresses same-parent relaxation.
- The deeper ladder is the fitted depth profile (`depth-profile` on the
  mbp-10 corpus): level i behind the touch carries
  `max(1, round(touch * ratio_i))`, frozen per parent. Cancellation
  widening exposes the ladder level behind the pulled touch; it makes no
  replenishment draw.

The simulation is parent-indexed: each step is one parent observation,
so transition statistics are between-parent effective transitions by
construction, the convention the real extraction is limited to. The
`lt_100ms` bucket is the comparison column.

The measured witness is the observed one - the next parent's pre-trade
touch moved through the struck side, the exact rule the real extractor
applies - never the internal depletion coin, which is an unobserved
model parameter calibrated so the observed witness matches. The
trichotomy in measurement uses executed quantity, as the real side must.

    ssh speilegg uv --directory Claude/tape-v2 run python proto_book.py --phase all
    ssh speilegg uv --directory Claude/tape-v2 run python proto_book.py --grid
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
BOOK_CONFIG = HERE / "book-config.json"
# The three frozen tier-1 exceptions (see book-config.json): allowed to
# sit outside the real-month band at their frozen magnitudes, one-sided
# at the engine gate, while every other tier-1 cell takes its band.
EXCEPTIONS = {("size_gt_touch", None), ("levels_mean", None), ("dprice_pmf", "1")}
# Tier 2: statistics the parent-indexed prototype does not identify
# (elapsed-time-conditioned dmid and dwell); engine-mandatory through
# the gen-quotes micro-stats convention, excluded from the prototype's
# tier-1 verdict.
TIER2 = {("dmid_pmf", sub) for sub in ("0", "0.5", "1", "1.5", "2", "2.5+")} | {
    ("spr_1_stay", None),
    ("spr_2_stay", None),
    ("spr_3_stay", None),
}
# Declared one-sided contracts for the two diagnostics without a
# same-convention real statistic: the spread tail beyond four ticks is
# bounded a fortiori by the real 4+ band's upper edge, and the
# unexecuted share is a venue design bound (partial-fill cancellation
# must stay rare on a fitted preset).
UNEXECUTED_BOUND = 0.005


def book_config(phase: str = "all") -> dict:
    """The fitted row for one phase: the pooled base with the phase's
    overrides, from the single source of truth."""
    cfg = json.loads(BOOK_CONFIG.read_text())
    row = dict(cfg["base"])
    row.update(cfg["phases"].get(phase, {}))
    return row


def adopted_size_channel(row: dict) -> tuple[dict, dict]:
    """The committed size channel from a fitted row: the seven-bucket
    match table and the deconvolved independent law, in the simulator's
    keyed forms. Every runner consumes these adopted arrays (fallback
    included) rather than re-deriving - a fresh derivation could
    silently validate a different law than the one the preset ships."""
    return (
        dict(zip(MATCH_BUCKETS, row["p_match"])),
        dict(zip(SIZE_SUPPORT, row["size_law"])),
    )
LAGS = (1, 10, 100)
SIZE_EDGES = [1, 2, 3, 4, 5, 10, 20, 50]
SIZE_REP = [1, 2, 3, 4, 5, 7, 15, 35, 70]

# Levels 1..9 behind the touch, relative to it: the struck-side ratio
# p50s from the 48-day mbp-10 depth profile (`depth-profile`, 41.6M
# parents). The shape is jump-then-plateau, which no single multiplier
# carries; the ratio vector is stable across months and phases while
# absolute depth varies twofold, so the touch carries the variation.
# The ADOPTED vector is the seven-entry exercised prefix in
# book-config.json (the walk slices `[: max_levels - 1]` at
# max_levels 8); this nine-entry constant is the CLI fallback and the
# record of the two unexercised tail values for the 8-versus-10
# display-bound replay.
DEFAULT_RATIOS = "2.0,2.4,2.5,2.83,3.0,3.0,3.33,3.33,3.33"


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
        "spread_pmf": {
            k: med(["price", "spread_pmf", k]) for k in ("1", "2", "3", "4+")
        },
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
        "spr_3_stay": med(["book", "spread_transition", "lt_100ms", "3+", "3+"]),
        "touch_p50_1": med(["book", "touch_size_spread_1", "p50"]),
        "touch_p50_2": med(["book", "touch_size_spread_2", "p50"]),
        "touch_p50_3": med(["book", "touch_size_spread_3+", "p50"]),
        "touch_p90_1": med(["book", "touch_size_spread_1", "p90"]),
        "touch_p90_2": med(["book", "touch_size_spread_2", "p90"]),
        "size_ge_touch": med(["sweep", "touch", "size_ge_touch"]),
        "size_gt_touch": med(["sweep", "touch", "size_gt_touch"]),
        "size_p99": med(["sweep", "parent_size", "p99"]),
        "size_pmf_1": med(["sweep", "parent_size_pmf", "1"]),
        "size_pmf_11_20": med(["sweep", "parent_size_pmf", "11-20"]),
        "levels_mean": med(["sweep", "levels_mean"]),
        "size_pmf": {
            k: med(["sweep", "parent_size_pmf", k])
            for k in ("1", "2", "3", "4", "5", "6-10", "11-20", "21-50", "51+")
        },
        "touch_pmf": {
            k: med(["sweep", "touch", "touch_pmf", k])
            for k in ("1", "2", "3", "4", "5", "6-10", "11-20", "21-50", "51+")
        },
        "match_by_touch": {
            k: med(["sweep", "touch", "match_by_touch", k])
            for k in ("1", "2", "3", "4", "5", "6-10", "11+")
        },
        "size_pmf_full": {
            k: med(["sweep", "parent_size_pmf_full", k]) for k in SIZE_SUPPORT
        },
        "touch_pmf_full": {
            k: med(["sweep", "touch", "touch_pmf_full", k]) for k in SIZE_SUPPORT
        },
    }
    return out


def draw_sizes(rng: np.random.Generator, count: int, size_pmf: dict) -> np.ndarray:
    """Integer draws from an integer-support pmf (the deconvolved F):
    each mass sits at its actual integer, the pooled tail at its
    declared representative."""
    values = np.array(
        [TAIL_REP if k.startswith(">") else int(k) for k in size_pmf], dtype=np.int64
    )
    probs = np.array(list(size_pmf.values()))
    probs = probs / probs.sum()
    return rng.choice(values, size=count, p=probs)


MATCH_BUCKETS = ["1", "2", "3", "4", "5", "6-10", "11+"]
SIZE_CAP = 30
# The integer support the mixture is solved and simulated on: masses at
# 1..30 with the tail pooled, matching `parent_size_pmf_full` and
# `touch_pmf_full` from the real extraction. The tail atom's declared
# representative in simulation is 40 (its mass is under a thousandth).
SIZE_SUPPORT = [str(v) for v in range(1, SIZE_CAP + 1)] + [f">{SIZE_CAP}"]
TAIL_REP = 40


def match_bucket_of(t: int) -> str:
    return str(t) if t <= 5 else ("6-10" if t <= 10 else "11+")


def deconvolve_sizes(real: dict, fixed_p: dict | None = None) -> tuple[dict, dict]:
    """Jointly solve the touch-conditional match probability p(t) and the
    independent size law F from the observed integer-support marginal,
    the integer-support touch pmf and the exact-match share by touch.

    The observed marginal is the mixture M_V = sum_t P(T=t) [p(t) 1(s=t)
    + (1-p(t)) F(s)]: using it directly as the independent draw
    double-counts matched orders and shrinks the fitted channel to an
    effective coefficient. The mixture is solved on integers, so
    within-bucket coincidence is exact rather than a width heuristic: a
    match bucket's coincidence is the touch-mass-weighted F at each
    integer touch it contains, p is shared across the bucket's touches,
    and the matched mass is subtracted at each integer size. Fixed-point
    iteration with a convergence check; genuinely negative F mass
    rejects the mixture. The tail atom's coincidence is declared zero
    (its mass is under a thousandth).

    `fixed_p` is the thin-bucket rule's fixture: those buckets keep the
    given behavioral probability inside the joint solve - contributing
    matched mass every round while staying out of the coincidence
    update - and F is solved under that constraint. Patching a bucket's
    p after solving would change the total matched mass and silently
    break the mixture everywhere else.
    """
    m_v = dict(real["size_pmf_full"])
    m_t = dict(real["touch_pmf_full"])
    match = dict(real["match_by_touch"])
    tail = f">{SIZE_CAP}"
    bucket_touches: dict = {b: [] for b in MATCH_BUCKETS}
    for t in range(1, SIZE_CAP + 1):
        bucket_touches[match_bucket_of(t)].append(str(t))
    bucket_touches["11+"].append(tail)
    f = dict(m_v)
    p: dict = dict(fixed_p or {})
    for _round in range(80):
        previous = dict(f)
        for bucket in MATCH_BUCKETS:
            if fixed_p and bucket in fixed_p:
                continue
            if bucket not in match:
                continue
            t_masses = [(key, m_t.get(key, 0.0)) for key in bucket_touches[bucket]]
            bucket_mass = sum(w for _k, w in t_masses)
            if bucket_mass <= 0.0:
                continue
            coincidence = (
                sum(w * (0.0 if key == tail else f.get(key, 0.0)) for key, w in t_masses)
                / bucket_mass
            )
            coincidence = min(0.99, coincidence)
            p[bucket] = max(0.0, (match[bucket] - coincidence) / (1.0 - coincidence))
        matched_at = {
            key: m_t.get(key, 0.0) * p.get(match_bucket_of(int(key)) if key != tail else "11+", 0.0)
            for key in SIZE_SUPPORT
        }
        matched_total = sum(matched_at.values())
        if matched_total >= 0.95:
            raise SystemExit("deconvolution: matched mass approaching one")
        for key in SIZE_SUPPORT:
            value = (m_v.get(key, 0.0) - matched_at[key]) / (1.0 - matched_total)
            if value < -1e-9:
                raise SystemExit(f"deconvolution: negative mass at {key}")
            f[key] = max(0.0, value)
        total = sum(f.values())
        f = {k: v / total for k, v in f.items()}
        if max(abs(f[k] - previous[k]) for k in f) < 1e-10:
            break
    else:
        raise SystemExit("deconvolution: no fixed point in 80 rounds")
    return p, f


# ------------------------------------------------------------- the model


def spread_state(spread: int) -> int:
    return 1 if spread <= 1 else (2 if spread == 2 else 3)


def simulate(rng: np.random.Generator, sign: np.ndarray, sizes: np.ndarray, cfg) -> dict:
    n = sign.size
    ratios = (
        [float(r) for r in cfg.depth_ratios.split(",")]
        if isinstance(cfg.depth_ratios, str)
        else list(cfg.depth_ratios)
    )
    replenish_by_state = {1: cfg.replenish_1, 2: cfg.replenish_2, 3: cfg.replenish_3}
    # Recorded per parent: the pre-trade book, the execution and the last
    # print. The depletion coin is kept only as a diagnostic; the scored
    # witness is observed from consecutive pre-trade books.
    pre_bid = np.empty(n)
    pre_ask = np.empty(n)
    pre_touch = np.empty(n)
    last_print = np.empty(n)
    requested = np.empty(n)
    executed = np.empty(n)
    levels_x = np.empty(n)
    coin = np.zeros(n, dtype=bool)
    capped = np.zeros(n, dtype=bool)
    origin = np.empty(n, dtype="U18")
    joined = np.zeros(n, dtype=bool)

    def fresh(spread: int) -> int:
        # The replenishment law behind a fresh queue, per spread state:
        # thinner than the observed pre-trade touch because large queues
        # survive longer. Keyed by the spread right after the transition
        # that creates the queue, collapsed to the three model states.
        # `replenish_exp` is the ablation arm: nonzero restores the
        # falsified exponential law of the first transcription, keyed by
        # the raw spread, so the refit can state what the table fixes
        # and what the cap merely bounds.
        if cfg.replenish_exp > 0.0:
            mean = max(1.0, cfg.replenish_1 * cfg.replenish_exp ** (max(1, spread) - 1))
        else:
            mean = max(1.0, replenish_by_state[spread_state(spread)])
        return max(1, int(rng.geometric(1.0 / mean)))

    # The origin sits at the real instrument's tick scale (MNQ trades
    # near 113,000 ticks): a 400k-parent walk at sigma 0.9 wanders
    # hundreds of ticks, and an origin near the bid floor turns the
    # clamp into an absorbing boundary that reads as a non-tracking
    # regime - the fifth spar's "seed-reachable attractor" was exactly
    # this artifact.
    x = float(cfg.x0)
    bid = int(round(x)) - 1
    ask = bid + 2
    q = {1: fresh(2), -1: fresh(2)}  # +1 keys the ask (buys strike it)
    # The creation path of each side's standing queue, for the queue
    # attribution diagnostic: which mechanism produced the touch a
    # parent observes.
    q_origin = {1: "init", -1: "init"}
    q_joined = {1: False, -1: False}
    register = 0.0
    sig_level = 0.0

    def project() -> None:
        # The band's slack is a per-spread-state table (fifth spar): a
        # one-tick book cannot narrow directionally, so projection is
        # the only anchor correction it has, and a flat slack leaves a
        # self-reinforcing non-tracking regime seed-reachable. The
        # current spread selects the band before the projection acts -
        # that ordering is the stated invariant.
        nonlocal bid, ask
        s = ask - bid
        slack = cfg.slack_1 if s <= 1 else (cfg.slack if s == 2 else cfg.slack_3)
        mid = bid + s / 2.0
        band = s / 2.0 + slack
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

    for k in range(n):
        # 1. External diffusion, then projection. Student-t with a
        # persistent lognormal sigma level; in the engine this modulation
        # is the cascade's own second_sigma, supplied for free.
        sig_level = cfg.rho_sig * sig_level + np.sqrt(1.0 - cfg.rho_sig**2) * rng.standard_normal()
        sigma = cfg.sigma_e * np.exp(cfg.sig_ln * sig_level - 0.5 * cfg.sig_ln**2)
        x += sigma * rng.standard_t(cfg.df) / np.sqrt(cfg.df / (cfg.df - 2.0))
        project()
        # The replacement-queue renewal (seventh spar): limit orders
        # arriving at a standing price level between transactions join
        # the queue before the next parent observes it. Both standing
        # sides in fixed order (bid then ask), keyed by the spread at
        # the observation boundary; exact surviving residuals at the
        # fill instant are untouched, and the join is recorded as
        # secondary provenance so the creation-path attribution that
        # justified the channel is not erased by it.
        if cfg.p_join > 0.0:
            # Eligibility is bounded: only a queue below the state's
            # standing ceiling attracts joins, because real queues are
            # stationary - arrivals balance cancellations - and an
            # unbounded join compounds on long-lived queues into an
            # artificial touch tail (the failure the full-bucket gate
            # caught). The ceiling ties to the state's replenish mean.
            s_now = ask - bid
            ceiling = cfg.join_cap_factor * replenish_by_state[spread_state(s_now)]
            for join_side in (-1, 1):
                if q[join_side] < ceiling and rng.random() < cfg.p_join:
                    q[join_side] += max(
                        1, int(rng.geometric(1.0 / max(1.0, cfg.join_mean)))
                    )
                    q_joined[join_side] = True
        s = ask - bid
        pre_bid[k] = bid
        pre_ask[k] = ask
        side = int(sign[k])
        touch_q = int(q[side])
        pre_touch[k] = touch_q
        origin[k] = q_origin[side]
        joined[k] = q_joined[side]
        # 2. Parent size is endogenous to the touch, with a strength
        # that depends on the touch itself: the deconvolved match
        # probability is 0.77 at a one-lot touch falling toward 0.08 at
        # a large one (matched total near 0.38), the profile of takers
        # sizing to displayed quantity plus marketable-limit truncation
        # at the touch. The full seven-bucket deconvolved table is
        # carried; it is measured from the real side, not fitted. The
        # non-match arm draws from the deconvolved independent law,
        # never the observed marginal, which contains the matched
        # orders.
        v = touch_q if rng.random() < cfg.p_match[match_bucket_of(touch_q)] else int(sizes[k])
        requested[k] = v
        # 3. The walk on the struck side, against the frozen per-parent
        # ladder: level i behind the touch carries the fitted
        # ratio-to-touch quantity, constructed once from the pre-trade
        # snapshot and never rebuilt mid-parent.
        struck_price = ask if side > 0 else bid
        ladder = [touch_q] + [
            max(1, int(round(touch_q * r))) for r in ratios[: cfg.max_levels - 1]
        ]
        remaining = v
        levels = 0
        price = struck_price
        residual = touch_q
        while levels < len(ladder) and remaining >= ladder[levels]:
            remaining -= ladder[levels]
            levels += 1
            if remaining == 0 or levels == len(ladder):
                break
            price = struck_price + levels * side
        if levels < len(ladder):
            # A partial stop consumes the remainder at the stopping
            # level; only a fully exhausted ladder leaves it unexecuted.
            partial = remaining > 0
            residual = ladder[levels] - remaining
            remaining = 0
        else:
            partial = False
            residual = 0
        executed[k] = v - remaining
        # The OBSERVED level count matches the real extractor: distinct
        # executed prices, a partially consumed stopping level included.
        # The mechanism below keeps `levels` (fully exhausted levels) -
        # recession distance is exhaustion, not execution breadth. The
        # old recording scored exhausted levels and sat below the real
        # convention by construction (found in the tenth spar's review:
        # mean observed levels >= 1 + P(size > touch) is an arithmetic
        # identity of a contiguous ladder, and the recorded numbers
        # violated it).
        levels_x[k] = levels + 1 if partial else max(levels, 1)
        last_print[k] = price
        tri = "lt" if v < touch_q else ("eq" if v == touch_q else "gt")
        # 4. Effective depletion as a saturated transition. The coin is
        # the unobserved model parameter; what it produces on the wire is
        # bounded by the room below the spread ceiling.
        p = {"lt": cfg.p_dep_lt, "eq": cfg.p_dep_eq, "gt": cfg.p_dep_gt}[tri]
        suppress_relax = False
        if rng.random() < p:
            coin[k] = True
            room = cfg.max_spread - (ask - bid)
            visible = min(max(1, levels), max(0, room))
            capped[k] = visible < max(1, levels)
            if visible > 0:
                suppress_relax = True
                if side > 0:
                    ask += visible
                else:
                    bid -= visible
                bid = max(bid, 1)
                # The exact residual survives only when the visible
                # touch lands on the level it belongs to; a capped
                # recession publishes a different level and takes a
                # replenishment draw at the post-recession state.
                if visible == levels and levels > 0 and residual > 0:
                    q[side] = residual
                    q_origin[side] = "recede_residual"
                    q_joined[side] = False
                else:
                    q[side] = fresh(ask - bid)
                    q_origin[side] = "recede_replenish"
                    q_joined[side] = False
                # The un-struck side follows the recession actually
                # applied, never the uncapped level count.
                if rng.random() < cfg.p_follow:
                    if side > 0:
                        bid += visible
                        bid = min(bid, ask - 1)
                    else:
                        ask -= visible
                        ask = max(ask, bid + 1)
                    q[-side] = fresh(ask - bid)
                    q_origin[-side] = "follow"
                    q_joined[-side] = False
        elif levels > 0:
            # Replenished before observed: the touch price stands; its
            # quantity is a fresh draw at the unchanged state.
            q[side] = fresh(s)
            q_origin[side] = "consumed_replenish"
            q_joined[side] = False
        elif executed[k] > 0:
            # Partial consumption of a standing touch leaves the exact
            # residual.
            q[side] = residual
            q_origin[side] = "partial_residual"
            q_joined[side] = False
        # 5. The impact moves the anchor before the spread relaxes, so
        # narrowing is directional. The transient term is the residual
        # impact channel; projection credits mechanical movement against
        # it, so nothing is counted twice.
        x += cfg.g * side + cfg.a_t * (side - (1.0 - cfg.rho) * register)
        register = cfg.rho * register + side
        s = ask - bid
        # The stationary spread the relaxation aims at: three states, the
        # third carrying the wide-book mass Asia needs.
        u = rng.random()
        target = 1 if u < cfg.p_target_1 else (2 if u < cfg.p_target_1 + cfg.p_target_2 else 3)
        while s > target and not suppress_relax and rng.random() < cfg.p_narrow:
            # Quote inside on the side that brings the mid toward x when
            # the displacement is meaningful, else symmetrically. The new
            # queue draws at the state it joins.
            mid = bid + s / 2.0
            toward_ask = x < mid - 0.25 or (abs(x - mid) <= 0.25 and rng.random() < 0.5)
            if toward_ask:
                ask -= 1
                q[1] = fresh(s - 1)
                q_origin[1] = "narrow"
                q_joined[1] = False
            else:
                bid += 1
                q[-1] = fresh(s - 1)
                q_origin[-1] = "narrow"
                q_joined[-1] = False
            s = ask - bid
        # Widening is a quote pull at any spread below the drawn target:
        # the pulled touch exposes the ladder level behind it, per the
        # fitted profile - no replenishment draw.
        if s < target and s < cfg.max_spread and rng.random() < cfg.p_widen:
            if rng.random() < 0.5:
                ask += 1
                q[1] = max(1, int(round(q[1] * ratios[0])))
                q_origin[1] = "widen_expose"
                q_joined[1] = False
            else:
                bid = max(1, bid - 1)
                q[-1] = max(1, int(round(q[-1] * ratios[0])))
                q_origin[-1] = "widen_expose"
                q_joined[-1] = False
        # 6. The post-trade projection against the moved anchor.
        project()

    return {
        "pre_bid": pre_bid,
        "pre_ask": pre_ask,
        "pre_touch": pre_touch,
        "last_print": last_print,
        "requested": requested,
        "executed": executed,
        "levels": levels_x,
        "coin": coin,
        "capped": capped,
        "origin": origin,
        "joined": joined,
        "sign": sign.astype(float),
    }


# ------------------------------------------------------------ statistics


def pmf_counts(values: np.ndarray, edges: list[tuple[str, float, float | None]]) -> dict:
    out = {}
    for label, lo, hi in edges:
        mask = values >= lo if hi is None else (values >= lo) & (values < hi)
        out[label] = float(mask.mean())
    return out


def measure(sim: dict, real: dict | None = None) -> dict:
    spread = sim["pre_ask"] - sim["pre_bid"]
    mid = (sim["pre_ask"] + sim["pre_bid"]) / 2.0
    sign = sim["sign"]
    executed = sim["executed"]
    touch = sim["pre_touch"]
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
    # The observed witness, the real extractor's own rule: the next
    # parent's pre-trade touch moved through the struck side. The
    # trichotomy conditions on executed quantity, as the real side must.
    buys = sign[:-1] > 0
    moved = np.where(
        buys, sim["pre_ask"][1:] > sim["pre_ask"][:-1], sim["pre_bid"][1:] < sim["pre_bid"][:-1]
    )
    tri = np.where(
        executed[:-1] < touch[:-1], "lt", np.where(executed[:-1] == touch[:-1], "eq", "gt")
    )
    witness = {}
    for arm in ("lt", "eq", "gt"):
        mask = tri == arm
        witness[arm] = float(moved[mask].mean()) if mask.sum() else float("nan")
    out["witness"] = witness
    for state, lo, hi in (("1", 1, 2), ("2", 2, 3), ("3", 3, None)):
        cur = (spread[:-1] >= lo) if hi is None else (spread[:-1] == lo)
        nxt = (spread[1:] >= lo) if hi is None else (spread[1:] == lo)
        out[f"spr_{state}_stay"] = float(nxt[cur].mean()) if cur.sum() else float("nan")
    for state, lo, hi in (("1", 1, 2), ("2", 2, 3), ("3", 3, None)):
        mask = (spread >= lo) if hi is None else (spread == lo)
        values = touch[mask]
        out[f"touch_p50_{state}"] = float(np.median(values)) if mask.sum() else float("nan")
        out[f"touch_p90_{state}"] = (
            float(np.quantile(values, 0.9)) if mask.sum() else float("nan")
        )
    out["size_ge_touch"] = float((executed >= touch).mean())
    out["size_gt_touch"] = float((executed > touch).mean())
    # The exact-match share by touch bucket, the identification the
    # deconvolution rests on: all seven cells, measured on the sim
    # exactly as the real extractor measures them, so every inferred
    # probability is gated rather than assumed to have transferred.
    match_by_touch = {}
    for label, lo, hi in (
        ("1", 1, 2),
        ("2", 2, 3),
        ("3", 3, 4),
        ("4", 4, 5),
        ("5", 5, 6),
        ("6-10", 6, 11),
        ("11+", 11, None),
    ):
        mask = (touch >= lo) if hi is None else ((touch >= lo) & (touch < hi))
        match_by_touch[label] = (
            float((executed[mask] == touch[mask]).mean()) if mask.sum() else float("nan")
        )
    out["match_by_touch"] = match_by_touch
    out["size_p99"] = float(np.quantile(executed, 0.99))
    out["size_pmf_1"] = float((executed == 1).mean())
    out["size_pmf_11_20"] = float(((executed >= 11) & (executed <= 20)).mean())
    out["levels_mean"] = float(np.maximum(sim["levels"], 1).mean())
    # Diagnostics outside the score: the tail the first transcription
    # grew, the coin against its observed consequence, the witness
    # attribution the fourth spar asked for (projection-produced against
    # depletion-produced), the cap-hit rate, and the reconstructed
    # executed-size marginal against the real integer pmf.
    out["spread_p99"] = float(np.quantile(spread, 0.99))
    out["spread_5plus"] = float((spread >= 5).mean())
    out["executed_mean"] = float(executed.mean())
    out["unexecuted_share"] = float((sim["requested"] > executed).mean())
    out["coin_rate"] = float(sim["coin"].mean())
    out["cap_hit_rate"] = float(sim["capped"].mean())
    coin = sim["coin"][:-1]
    if moved.sum():
        out["witness_from_coin"] = float((moved & coin).sum() / moved.sum())
    else:
        out["witness_from_coin"] = float("nan")
    out["witness_rate_nocoin"] = (
        float(moved[~coin].mean()) if (~coin).sum() else float("nan")
    )
    if real is not None:
        sim_pmf = np.array(
            [
                (executed > SIZE_CAP).mean() if k.startswith(">") else (executed == int(k)).mean()
                for k in SIZE_SUPPORT
            ]
        )
        real_pmf = np.array([real["size_pmf_full"].get(k, 0.0) for k in SIZE_SUPPORT])
        out["size_tv"] = float(np.abs(sim_pmf - real_pmf).sum() / 2.0)
    return out


SCORE_KEYS = [
    ("spread_pmf", "1"),
    ("spread_pmf", "2"),
    ("spread_pmf", "3"),
    ("spread_pmf", "4+"),
    ("dmid_pmf", "0"),
    ("dmid_pmf", "0.5"),
    ("dmid_pmf", "1"),
    ("dmid_pmf", "1.5"),
    ("dmid_pmf", "2"),
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
    ("witness", "eq"),
    ("witness", "gt"),
    ("spr_1_stay", None),
    ("spr_2_stay", None),
    ("spr_3_stay", None),
    ("touch_p50_1", None),
    ("touch_p50_2", None),
    ("touch_p50_3", None),
    ("touch_p90_1", None),
    ("touch_p90_2", None),
    ("size_ge_touch", None),
    ("size_gt_touch", None),
    ("size_p99", None),
    ("size_pmf_1", None),
    ("size_pmf_11_20", None),
    ("levels_mean", None),
    ("match_by_touch", "1"),
    ("match_by_touch", "2"),
    ("match_by_touch", "3"),
    ("match_by_touch", "4"),
    ("match_by_touch", "5"),
    ("match_by_touch", "6-10"),
    ("match_by_touch", "11+"),
]

DIAGNOSTICS = [
    "spread_p99",
    "spread_5plus",
    "executed_mean",
    "unexecuted_share",
    "coin_rate",
    "cap_hit_rate",
    "witness_from_coin",
    "witness_rate_nocoin",
    "size_tv",
]


def key_path(key: str, sub: str | None) -> list[str]:
    """The real-targets JSON path behind one score key."""
    if key == "spread_pmf":
        return ["price", "spread_pmf", sub]
    if key == "dmid_pmf":
        return ["book", "mid_change_pmf_fast", sub]
    if key == "dprice_pmf":
        return ["price", "abs_change_pmf_fast", sub]
    if key == "impact":
        return ["price", "impact", sub, "mean_ticks"]
    if key in ("impact_spread_1", "impact_spread_2"):
        return ["book", "impact_by_spread", key[-1], "1"]
    if key == "witness":
        return ["book", "depletion_witness", "lt_100ms", sub]
    if key.startswith("spr_"):
        state = {"1": "1", "2": "2", "3": "3+"}[key[4]]
        return ["book", "spread_transition", "lt_100ms", state, state]
    if key.startswith("touch_p"):
        quantile, state = key.split("_")[1], key.split("_")[2]
        state = {"1": "1", "2": "2", "3": "3+"}[state]
        return ["book", f"touch_size_spread_{state}", quantile]
    if key in ("size_ge_touch", "size_gt_touch"):
        return ["sweep", "touch", key]
    if key == "size_p99":
        return ["sweep", "parent_size", "p99"]
    if key == "size_pmf_1":
        return ["sweep", "parent_size_pmf", "1"]
    if key == "size_pmf_11_20":
        return ["sweep", "parent_size_pmf", "11-20"]
    if key == "levels_mean":
        return ["sweep", "levels_mean"]
    if key == "match_by_touch":
        return ["sweep", "touch", "match_by_touch", sub]
    raise KeyError(key)


def real_band(phase: str) -> dict:
    """Each score key's real cross-month p10 to p90: the hard per-target
    tolerance. A candidate inside the months' own variation is
    indistinguishable from a real month on that statistic, so the band
    is the gate and no tolerance is invented."""
    blocks = json.loads(TARGETS.read_text())["blocks"]
    rows = [b[phase] for b in blocks.values() if phase in b]
    band: dict = {}
    for key, sub in SCORE_KEYS:
        values = []
        for r in rows:
            node = r
            for part in key_path(key, sub):
                node = node.get(part, {})
            if isinstance(node, (int, float)):
                values.append(float(node))
        if len(values) >= 3:
            band[(key, sub)] = (
                float(np.quantile(values, 0.1)),
                float(np.quantile(values, 0.9)),
            )
    return band


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


def exception_name(key: str, sub: str | None) -> str:
    """The exceptions table's flattened name for a score cell."""
    return key if sub is None else f"{key}_{sub}"


def load_gate() -> dict:
    """The gate configuration from the single source of truth: the
    pooled frozen exceptions with their declared allowance, and the
    ninth-spar deferral ledger."""
    cfg = json.loads(BOOK_CONFIG.read_text())
    return {"exceptions": cfg["exceptions"], "ledger": cfg.get("deferrals", {})}


def grade(measured: dict, phase: str, band: dict, gate: dict) -> dict:
    """The one grading function - the CLI report, the robustness
    battery and the refit grid all call it, so they cannot answer
    different questions. Classification per non-tier-2 score cell:

    - a nonfinite measurement is unexpected before any lookup;
    - on the pooled row, the three frozen exceptions bind one-sided at
      their frozen value plus the declared allowance;
    - every other cell takes its phase band (a missing band is a hard
      error, never a silent skip); a miss is a deferral only when the
      cell is in this phase's ledger, else unexpected.

    Returns the unexpected misses, the deferred cells with their
    values (movement evidence against the ledger's record), and the
    one-sided contract violations (spread tail, unexecuted share).
    """
    unexpected: list = []
    deferred: list = []
    contracts: list = []
    ledger = {
        (entry["key"], entry.get("sub")) for entry in gate["ledger"].get(phase, [])
    }
    for key, sub in SCORE_KEYS:
        if (key, sub) in TIER2:
            continue
        c = measured[key] if sub is None else measured[key][sub]
        if not np.isfinite(c):
            unexpected.append((key, sub, c, float("nan"), float("nan")))
            continue
        if phase == "all" and (key, sub) in EXCEPTIONS:
            spec = gate["exceptions"][exception_name(key, sub)]
            # A retired exception has no gate effect: the cell falls
            # through to its ordinary band like any other.
            if "retired" not in spec:
                frozen, allowance = spec["frozen"], spec["allowance"]
                ok = (
                    c <= frozen + allowance
                    if spec["bound"] == "not above"
                    else c >= frozen - allowance
                )
                if not ok:
                    unexpected.append((key, sub, c, frozen, allowance))
                continue
        if (key, sub) not in band:
            raise SystemExit(
                f"{phase}: no real band for {key} {sub}; the gate cannot "
                "silently skip a target"
            )
        lo, hi = band[(key, sub)]
        if lo <= c <= hi:
            continue
        if (key, sub) in ledger:
            deferred.append((key, sub, c, lo, hi))
        else:
            unexpected.append((key, sub, c, lo, hi))
    tail_hi = band.get(("spread_pmf", "4+"), (float("nan"), float("nan")))[1]
    tail = measured.get("spread_5plus", float("nan"))
    if not tail <= tail_hi:
        contracts.append(("spread_5plus", tail, tail_hi))
    unex = measured.get("unexecuted_share", float("nan"))
    if not unex <= UNEXECUTED_BOUND:
        contracts.append(("unexecuted_share", unex, UNEXECUTED_BOUND))
    return {"unexpected": unexpected, "deferred": deferred, "contracts": contracts}


def cell_name(key: str, sub: str | None) -> str:
    return key if sub is None else f"{key} {sub}"


def report(
    candidate: dict, real: dict, band: dict | None = None, phase: str = "all"
) -> None:
    header = f"{'statistic':<22}{'proto':>9}{'real':>9}"
    if band:
        header += f"{'p10':>9}{'p90':>9}  gate"
    print(header)
    outside = 0
    for key, sub in SCORE_KEYS:
        c = candidate[key] if sub is None else candidate[key][sub]
        r = real[key] if sub is None else real[key][sub]
        line = f"{cell_name(key, sub):<22}{c:>9.3f}{r:>9.3f}"
        if band:
            lo, hi = band.get((key, sub), (float("nan"), float("nan")))
            flag = ""
            if not np.isnan(lo) and not (lo <= c <= hi):
                flag = "OUT"
                outside += 1
            line += f"{lo:>9.3f}{hi:>9.3f}  {flag}"
        print(line)
    for name in DIAGNOSTICS:
        print(f"{name:<22}{candidate.get(name, float('nan')):>9.3f}")
    if band:
        graded = grade(candidate, phase, band, load_gate())
        print(f"gates outside the real cross-month band: {outside}")
        for key, sub, c, lo, hi in graded["deferred"]:
            print(f"deferred (ledger)   {cell_name(key, sub)}: {c:.4f}  band {lo:.4f}..{hi:.4f}")
        for key, sub, c, lo, hi in graded["unexpected"]:
            print(f"UNEXPECTED          {cell_name(key, sub)}: {c:.4f}  bound {lo:.4f}..{hi:.4f}")
        for name, value, bound in graded["contracts"]:
            print(f"CONTRACT            {name}: {value:.4f} over {bound:.4f}")
        verdict = not graded["unexpected"] and not graded["contracts"]
        print(
            f"unexpected tier-1 misses: {len(graded['unexpected'])}  "
            f"deferred: {len(graded['deferred'])}  "
            f"verdict {'PASS' if verdict else 'FAIL'}"
        )
    print(f"score {score(candidate, real):.3f}")


# ---------------------------------------------------------------- driver


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--phase", default="all")
    ap.add_argument("--parents", type=int, default=400_000)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--sigma-e", type=float, default=0.35)
    ap.add_argument("--df", type=float, default=4.0)
    ap.add_argument("--sig-ln", type=float, default=1.3)
    ap.add_argument("--rho-sig", type=float, default=0.995)
    ap.add_argument("--g", type=float, default=0.31)
    ap.add_argument("--a-t", type=float, default=0.32)
    ap.add_argument("--rho", type=float, default=0.9)
    ap.add_argument("--slack", type=float, default=1.0)
    ap.add_argument("--x0", type=float, default=100_000.0)
    ap.add_argument("--p-dep-lt", type=float, default=0.10)
    ap.add_argument("--p-dep-eq", type=float, default=0.60)
    ap.add_argument("--p-dep-gt", type=float, default=0.60)
    ap.add_argument("--p-narrow", type=float, default=0.62)
    ap.add_argument("--p-target-1", type=float, default=0.55)
    ap.add_argument("--p-target-2", type=float, default=0.40)
    ap.add_argument("--p-follow", type=float, default=0.6)
    # The per-state slack table exists as capability; both entries
    # default to the shared slack. The state-dependent fit was proposed
    # against a non-tracking attractor that turned out to be the price
    # floor artifact (see the x0 comment in simulate), so flat slack is
    # the mechanism of record until a real measurement separates the
    # states.
    ap.add_argument("--slack-1", type=float, default=None)
    ap.add_argument("--slack-3", type=float, default=None)
    # The replacement-queue renewal, the smallest nested family first:
    # one shared probability and one shared mean (seventh spar).
    ap.add_argument("--p-join", type=float, default=0.0)
    ap.add_argument("--join-mean", type=float, default=3.0)
    ap.add_argument("--join-cap-factor", type=float, default=1.5)
    ap.add_argument("--p-widen", type=float, default=0.18)
    ap.add_argument("--replenish-1", type=float, default=1.6)
    ap.add_argument("--replenish-2", type=float, default=3.2)
    ap.add_argument("--replenish-3", type=float, default=2.0)
    ap.add_argument(
        "--replenish-exp",
        type=float,
        default=0.0,
        help="ablation: nonzero restores the exponential replenishment "
        "law at this base, keyed by raw spread from replenish-1",
    )
    ap.add_argument("--depth-ratios", default=DEFAULT_RATIOS)
    ap.add_argument("--max-spread", type=int, default=8)
    ap.add_argument("--max-levels", type=int, default=8)
    ap.add_argument("--slots", type=int, default=5)
    ap.add_argument("--alpha", type=float, default=2.2)
    ap.add_argument("--mix", type=float, default=0.08)
    ap.add_argument("--grid", action="store_true")
    ap.add_argument(
        "--queue-dump",
        action="store_true",
        help="print the observed struck-touch distribution by spread "
        "state and queue creation path, the attribution the close's "
        "thin spread-2 touch needs",
    )
    ap.add_argument(
        "--regime-dump",
        action="store_true",
        help="print rolling per-window regime diagnostics (spread-1 "
        "share, mid stasis, signed response, observed witness) instead "
        "of the aggregate report, to locate a non-tracking epoch",
    )
    ap.add_argument(
        "--sweep",
        default=None,
        help="JSON file of knob name to value list, overriding the "
        "built-in grid sweep",
    )
    ap.add_argument(
        "--holdout",
        action="store_true",
        help="score the configuration against every real month block "
        "separately instead of the cross-month median",
    )
    # The fitted row from the single source of truth is the default for
    # every knob; the command line overrides individual values on top.
    known, _rest = ap.parse_known_args()
    ap.set_defaults(**book_config(known.phase))
    args = ap.parse_args()
    if args.slack_1 is None:
        args.slack_1 = args.slack
    if args.slack_3 is None:
        args.slack_3 = args.slack
    real = real_medians(args.phase)
    # The non-match arm draws from the deconvolved independent law and
    # the simulation carries the full seven-bucket deconvolved match
    # table - the adopted arrays from book-config.json, never a fresh
    # derivation (which could validate a law the preset does not ship;
    # `deconvolve_phases.py --write-config` is where derivation lands).
    args.p_match, f_law = adopted_size_channel(vars(args))
    rng = np.random.default_rng(args.seed)
    sign = splitting_signs(rng, args.parents, args.slots, args.alpha, args.mix)
    sizes = draw_sizes(rng, args.parents, f_law)
    if args.grid:
        import itertools

        if args.sweep:
            sweep = {k: tuple(v) for k, v in json.loads(Path(args.sweep).read_text()).items()}
        else:
            sweep = {
                "p_narrow": (0.62, 0.70, 0.78),
                "p_target_1": (0.45, 0.55, 0.65),
                "p_widen": (0.10, 0.14, 0.18),
                "replenish_2": (3.2, 4.2, 5.2),
                "p_dep_eq": (0.60, 0.70, 0.80),
            }
        names = list(sweep)
        band = real_band(args.phase)
        # Selection is lexicographic: fewest unexpected tier-1 band
        # misses first, mean-log score second - so the correlated easy
        # targets cannot dilute a load-bearing failure (the dilution
        # the mean-only grids exhibited).
        best: tuple = (999, 1e9, None)
        for values in itertools.product(*sweep.values()):
            cfg = argparse.Namespace(**vars(args))
            for name, value in zip(names, values):
                setattr(cfg, name, value)
            # The adopted mechanism is shared slack: a swept slack
            # carries into the per-state copies unless they are swept
            # themselves.
            if "slack" in names:
                if "slack_1" not in names:
                    cfg.slack_1 = cfg.slack
                if "slack_3" not in names:
                    cfg.slack_3 = cfg.slack
            sim = simulate(np.random.default_rng(args.seed + 1), sign, sizes, cfg)
            m = measure(sim)
            s = score(m, real)
            graded = grade(m, args.phase, band, load_gate())
            misses = len(graded["unexpected"]) + len(graded["contracts"])
            print(
                "  ".join(f"{n}={v}" for n, v in zip(names, values))
                + f"  miss {misses}  deferred {len(graded['deferred'])}  score {s:.3f}"
            )
            if (misses, s) < best[:2]:
                best = (misses, s, values)
        print(
            f"best miss {best[0]} score {best[1]:.3f} at "
            + "  ".join(f"{n}={v}" for n, v in zip(names, best[2]))
        )
        return
    sim = simulate(np.random.default_rng(args.seed + 1), sign, sizes, args)
    if args.queue_dump:
        spread = sim["pre_ask"] - sim["pre_bid"]
        touch = sim["pre_touch"]
        origin = sim["origin"]
        for state, lo, hi in (("1", 1, 2), ("2", 2, 3), ("3+", 3, None)):
            mask = (spread >= lo) if hi is None else (spread == lo)
            total = mask.sum()
            print(f"spread {state}: n {int(total)}")
            for path in sorted(set(origin[mask])):
                sub = mask & (origin == path)
                values = touch[sub]
                print(
                    f"  {path:<18} share {sub.sum() / total:.3f}  "
                    f"touch p50 {float(np.median(values)):.0f} "
                    f"p90 {float(np.quantile(values, 0.9)):.0f} "
                    f"mean {float(values.mean()):.2f}"
                )
        return
    if args.regime_dump:
        window = 20_000
        spread = sim["pre_ask"] - sim["pre_bid"]
        mid = (sim["pre_ask"] + sim["pre_bid"]) / 2.0
        sgn = sim["sign"]
        buys = sgn[:-1] > 0
        moved = np.where(
            buys,
            sim["pre_ask"][1:] > sim["pre_ask"][:-1],
            sim["pre_bid"][1:] < sim["pre_bid"][:-1],
        )
        executed = sim["executed"]
        touch = sim["pre_touch"]
        coin = sim["coin"]
        print(
            f"{'window':>8}{'s1':>7}{'dmid0':>7}{'imp10':>7}{'witness':>9}{'touch':>7}"
            f"{'lt':>6}{'eq':>6}{'gt':>6}{'coin':>6}"
        )
        for lo in range(0, sgn.size - window, window):
            hi = lo + window
            w_spread = spread[lo:hi]
            dmid = np.abs(np.diff(mid[lo:hi]))
            move = (mid[lo + 10 : hi] - mid[lo : hi - 10]) * sgn[lo : hi - 10]
            w_exec = executed[lo:hi]
            w_touch = touch[lo:hi]
            print(
                f"{lo:>8}{(w_spread == 1).mean():>7.2f}{(dmid < 0.25).mean():>7.2f}"
                f"{move.mean():>7.2f}{moved[lo : hi - 1].mean():>9.2f}"
                f"{w_touch.mean():>7.1f}"
                f"{(w_exec < w_touch).mean():>6.2f}{(w_exec == w_touch).mean():>6.2f}"
                f"{(w_exec > w_touch).mean():>6.2f}{coin[lo:hi].mean():>6.2f}"
            )
        return
    measured = measure(sim, real)
    if args.holdout:
        months = sorted(json.loads(TARGETS.read_text())["blocks"])
        for month in months:
            month_real = real_medians(args.phase, month)
            print(f"{month}  score {score(measured, month_real):.3f}")
        return
    report(measured, real, real_band(args.phase), args.phase)


if __name__ == "__main__":
    main()
