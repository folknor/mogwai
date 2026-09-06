"""Sub-minute targets: the Tier 1 and Tier 2 statistics below the minute.

The same function runs on real prints (the extracted `tbbo`, one parent's
front month) and on a generated tape (`mogwai gen --type trades` CSVs),
so the two sides are measured by one definition. Prints are grouped into
parents by the frozen rule `mogwai_lab::stream::group_parents_batch`:
contiguous runs of one `(ts_event, side)`, an unsided print closing the
open run. A parent is one aggressor's match event; its prints are the
sweep.

The real side is measured in blocks of one calendar month, the candidate
side in blocks of one seed (one CSV), and a comparison reports the real
median across months with its p10 to p90 spread beside the candidate
median across seeds: the programme's containment criterion, a candidate
inside the real cross-block band, applied per statistic.

What is reported, per phase and pooled:

- rate: parents per second over the phase's nominal minutes.
- gaps: the inter-parent gap inside a session phase, in ms; the fraction
  under 1, 10 and 100 ms; and the gap normalised by the containing
  minute's own rate (`u = gap * parents_in_minute / 60`), whose law is
  Exp(1) for a Poisson placement (p50 0.69, p90 2.30, p99 4.61, cv 1) and
  whose consecutive-gap autocorrelation is zero for it.
- dispersion: the count dispersion inside a fixed window, relative to the
  window's own total, at four scales: 10 ms and 100 ms bins in a second,
  1 s and 10 s bins in a minute. Uniform placement of the window's count
  gives exactly 1.0 at every scale whatever the envelope or the texture
  does to the count itself; above one is clustering inside the window.
- sweep: the print count per parent (children), the level count, the
  parent size and the print size, as pmfs; on the real side also how the
  parent size compares with the touch it hit (`tbbo` carries the book
  before the trade).
- sign: the probability a parent takes the previous parent's side, the
  side autocorrelation out to a thousand parents, and the run-length
  tail against the Markov chain the persistence knob implies.
- price: the tick change of the last print between consecutive parents;
  on the real side the signed mid move after a parent at one, ten and a
  hundred parents, and the spread at parent instants.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import numpy as np
import polars as pl

from .corpus import DATA_DIR
from .session import PHASES, session_columns

NS = 1_000_000_000
SIGN_LAGS = [1, 2, 3, 5, 10, 20, 50, 100, 200, 500, 1000]
PHASE_NAMES = [name for name, _lo, _hi in PHASES]
PHASE_MINUTES = {name: hi - lo for name, lo, hi in PHASES}
SIZE_EDGES = [1, 2, 3, 4, 5, 10, 20, 50]
SIZE_LABELS = ["1", "2", "3", "4", "5", "6-10", "11-20", "21-50", "51+"]


def size_bucket(expr: pl.Expr) -> pl.Expr:
    out = pl.lit("51+")
    for edge, label in reversed(list(zip(SIZE_EDGES, SIZE_LABELS[:-1], strict=True))):
        out = pl.when(expr <= edge).then(pl.lit(label)).otherwise(out)
    return out


# ---------------------------------------------------------------- inputs

PRINT_SCHEMA = [
    "ts_event",
    "sign",
    "price_ticks",
    "size",
    "bid_ticks",
    "ask_ticks",
    "bid_sz",
    "ask_sz",
]


def prints_from_real(parent: str, first: str, last: str) -> pl.DataFrame:
    from .micro import load_tbbo

    frame = load_tbbo(parent, first, last)
    side = (
        pl.when(pl.col("side") == "B")
        .then(1)
        .when(pl.col("side") == "A")
        .then(-1)
        .otherwise(0)
    )
    return frame.with_columns(side.alias("sign").cast(pl.Int8)).select(PRINT_SCHEMA)


def prints_from_csv(path: Path, tick: float) -> pl.DataFrame:
    frame = pl.read_csv(
        path,
        schema_overrides={"ts_event": pl.Int64, "price": pl.Float64, "size": pl.Float64},
    )
    side = (
        pl.when(pl.col("aggressor") == "buyer")
        .then(1)
        .when(pl.col("aggressor") == "seller")
        .then(-1)
        .otherwise(0)
    )
    return (
        frame.with_columns(
            side.alias("sign").cast(pl.Int8),
            (pl.col("price") / tick).round(0).cast(pl.Int64).alias("price_ticks"),
            pl.col("size").round(0).cast(pl.Int64),
            pl.lit(None, dtype=pl.Int64).alias("bid_ticks"),
            pl.lit(None, dtype=pl.Int64).alias("ask_ticks"),
            pl.lit(None, dtype=pl.Int64).alias("bid_sz"),
            pl.lit(None, dtype=pl.Int64).alias("ask_sz"),
        )
        .select(PRINT_SCHEMA)
        .sort("ts_event")
    )


# --------------------------------------------------------------- parents


def group_parents(
    prints: pl.DataFrame, merge_within_ns: int = 0, utc_offset_minutes: int | None = None
) -> pl.DataFrame:
    """One row per parent under the frozen `(ts, side)` run rule.

    A real sweep's prints share one `ts_event`; the generator stamps its
    children `INTRA_EVENT_STEP_NS` (one microsecond) apart, so a candidate
    is grouped with `merge_within_ns` of ten microseconds: a same-side
    print that close to the previous one is the same sweep. Genuine
    parents ten microseconds apart are one in four thousand at the busiest
    generated rate, which is below anything reported here.
    """
    sided = prints.filter(pl.col("sign") != 0)
    key_change = (
        (pl.col("ts_event") - pl.col("ts_event").shift(1)) > merge_within_ns
    ) | (pl.col("sign") != pl.col("sign").shift(1))
    sided = sided.with_columns(
        key_change.fill_null(True).cast(pl.Int64).cum_sum().alias("parent")
    )
    parents = (
        sided.group_by("parent", maintain_order=True)
        .agg(
            pl.col("ts_event").first(),
            pl.col("sign").first(),
            pl.len().alias("children"),
            pl.col("price_ticks").n_unique().alias("levels"),
            pl.col("size").sum().alias("size"),
            pl.col("price_ticks").first().alias("price_first"),
            pl.col("price_ticks").last().alias("price_last"),
            pl.col("bid_ticks").first(),
            pl.col("ask_ticks").first(),
            pl.col("bid_sz").first(),
            pl.col("ask_sz").first(),
        )
        .drop("parent")
    )
    parents = session_columns(parents, "ts_event", utc_offset_minutes)
    return parents.with_columns(
        (pl.col("ts_event") // NS).alias("second"),
        (pl.col("ts_event") // (60 * NS)).alias("minute"),
    )


# ------------------------------------------------------------ statistics


def quantiles(values: np.ndarray, qs: list[float]) -> dict[str, float]:
    if values.size == 0:
        return {f"p{int(q * 100)}": float("nan") for q in qs}
    return {f"p{int(q * 100)}": float(np.quantile(values, q)) for q in qs}


def pmf(series: pl.Series, keys: list[str]) -> dict[str, float]:
    counts = series.value_counts()
    total = counts["count"].sum()
    lookup = dict(zip(counts[series.name].to_list(), counts["count"].to_list(), strict=True))
    return {k: lookup.get(k, 0) / total for k in keys}


def segment_lag_mask(same: np.ndarray, lag: int, size: int) -> np.ndarray:
    """Rows whose partner `lag` ahead lies in the same segment."""
    cum = np.concatenate([[0], np.cumsum(same.astype(np.int64))])
    span = cum[lag + 1 : lag + 1 + size] - cum[1 : 1 + size]
    return span == lag


def corr(a: np.ndarray, b: np.ndarray) -> float:
    aa = a - a.mean()
    bb = b - b.mean()
    denom = np.sqrt((aa**2).mean() * (bb**2).mean())
    return float((aa * bb).mean() / denom) if denom > 0 else float("nan")


def within_segments(parents: pl.DataFrame) -> pl.DataFrame:
    """Mark consecutive parents that share a session and a phase."""
    same = (pl.col("session_date") == pl.col("session_date").shift(1)) & (
        pl.col("phase") == pl.col("phase").shift(1)
    )
    return parents.with_columns(
        same.fill_null(False).alias("same_segment"),
        (pl.col("ts_event") - pl.col("ts_event").shift(1)).alias("gap_ns"),
        pl.col("sign").shift(1).alias("prev_sign"),
        pl.col("price_last").shift(1).alias("prev_price"),
    )


def gap_stats(seg: pl.DataFrame) -> dict:
    gaps = seg.filter(pl.col("same_segment"))
    if gaps.height == 0:
        return {}
    per_minute = gaps.group_by("minute").len().rename({"len": "n_minute"})
    gaps = gaps.join(per_minute, on="minute", how="left")
    gap_ms = gaps["gap_ns"].to_numpy() / 1e6
    u = gaps["gap_ns"].to_numpy() / 1e9 * gaps["n_minute"].to_numpy() / 60.0
    log_u = np.log(np.maximum(u, 1e-12))
    same_next = gaps["same_segment"].to_numpy()
    acf = {}
    for lag in (1, 2, 3, 5, 10):
        if lag >= log_u.size:
            break
        keep = segment_lag_mask(same_next, lag, log_u.size - lag)
        if keep.sum() < 100:
            continue
        acf[str(lag)] = corr(log_u[:-lag][keep], log_u[lag:][keep])
    return {
        "n": int(gap_ms.size),
        "ms": quantiles(gap_ms, [0.1, 0.5, 0.9, 0.99]),
        "mean_ms": float(gap_ms.mean()),
        "frac_lt_1ms": float((gap_ms < 1).mean()),
        "frac_lt_10ms": float((gap_ms < 10).mean()),
        "frac_lt_100ms": float((gap_ms < 100).mean()),
        "frac_lt_1s": float((gap_ms < 1000).mean()),
        "u": quantiles(u, [0.1, 0.5, 0.9, 0.99]),
        "u_cv": float(u.std() / u.mean()),
        "log_u_sd": float(log_u.std()),
        "log_u_acf": acf,
    }


def dispersion(parents: pl.DataFrame, window_ns: int, bin_ns: int) -> float:
    """Within-window dispersion at `bin_ns` relative to uniform placement."""
    k = window_ns // bin_ns
    frame = parents.select(
        (pl.col("ts_event") // window_ns).alias("w"),
        ((pl.col("ts_event") % window_ns) // bin_ns).alias("b"),
    )
    per_bin = frame.group_by("w", "b").len()
    per_window = per_bin.group_by("w").agg(pl.col("len").sum().alias("n"))
    sum_c2 = float((per_bin["len"].cast(pl.Float64) ** 2).sum())
    n = per_window["n"].cast(pl.Float64)
    sum_n2 = float((n**2).sum())
    sum_n = float(n.sum())
    if sum_n == 0:
        return float("nan")
    numerator = sum_c2 - sum_n2 / k
    denominator = sum_n * (1.0 - 1.0 / k)
    return numerator / denominator


def sweep_stats(parents: pl.DataFrame) -> dict:
    children = parents["children"]
    levels = parents["levels"]
    multi = children > 1
    out = {
        "n": parents.height,
        "children_mean": float(children.mean()),
        "children_single_frac": float((children == 1).mean()),
        "children_pmf": pmf(
            children.clip(upper_bound=6).cast(pl.Utf8).replace({"6": "6+"}).alias("c"),
            ["1", "2", "3", "4", "5", "6+"],
        ),
        "levels_mean": float(levels.mean()),
        "levels_pmf": pmf(
            levels.clip(upper_bound=4).cast(pl.Utf8).replace({"4": "4+"}).alias("l"),
            ["1", "2", "3", "4+"],
        ),
        "multi_level_given_multi_print": float((levels.filter(multi) > 1).mean())
        if multi.sum() > 0
        else float("nan"),
        "parent_size_mean": float(parents["size"].mean()),
        "parent_size": quantiles(parents["size"].to_numpy().astype(float), [0.5, 0.9, 0.99]),
        "parent_size_pmf": pmf(
            parents.select(size_bucket(pl.col("size")).alias("s"))["s"], SIZE_LABELS
        ),
    }
    if parents["ask_sz"].null_count() < parents.height:
        touch = pl.when(pl.col("sign") > 0).then(pl.col("ask_sz")).otherwise(pl.col("bid_sz"))
        with_touch = parents.with_columns(touch.alias("touch")).filter(pl.col("touch") > 0)
        ratio = with_touch["size"].to_numpy() / with_touch["touch"].to_numpy()
        at_touch = with_touch.filter(
            pl.when(pl.col("sign") > 0)
            .then(pl.col("price_first") == pl.col("ask_ticks"))
            .otherwise(pl.col("price_first") == pl.col("bid_ticks"))
        ).height
        out["touch"] = {
            "n": with_touch.height,
            "first_print_at_touch": at_touch / with_touch.height,
            "size_ge_touch": float((ratio >= 1.0).mean()),
            "size_gt_touch": float((ratio > 1.0).mean()),
            "size_over_touch": quantiles(ratio, [0.5, 0.9, 0.99]),
            "touch_size": quantiles(
                with_touch["touch"].to_numpy().astype(float), [0.1, 0.5, 0.9]
            ),
            "multi_level_given_size_ge_touch": float(
                (with_touch.filter(ratio >= 1.0)["levels"] > 1).mean()
            ),
        }
    return out


def print_size_stats(prints: pl.DataFrame) -> dict:
    sized = prints.filter(pl.col("sign") != 0)
    return {
        "mean": float(sized["size"].mean()),
        "pmf": pmf(sized.select(size_bucket(pl.col("size")).alias("s"))["s"], SIZE_LABELS),
    }


def sign_stats(seg: pl.DataFrame) -> dict:
    sign = seg["sign"].to_numpy().astype(np.float64)
    same = seg["same_segment"].to_numpy()
    out: dict = {"n": int(sign.size)}
    if sign.size < 1000:
        return out
    consecutive = seg.filter(pl.col("same_segment"))
    out["p_same"] = float((consecutive["sign"] == consecutive["prev_sign"]).mean())
    acf = {}
    for lag in SIGN_LAGS:
        if lag >= sign.size:
            break
        keep = segment_lag_mask(same, lag, sign.size - lag)
        if keep.sum() < 100:
            continue
        acf[str(lag)] = corr(sign[:-lag][keep], sign[lag:][keep])
    out["acf"] = acf
    breaks = np.ones(sign.size, dtype=bool)
    breaks[1:] = (sign[1:] != sign[:-1]) | ~same[1:]
    starts = np.flatnonzero(breaks)
    lengths = np.diff(np.concatenate([starts, [sign.size]]))
    out["run_mean"] = float(lengths.mean())
    out["run_ge_5"] = float((lengths >= 5).mean())
    out["run_ge_10"] = float((lengths >= 10).mean())
    out["run_ge_20"] = float((lengths >= 20).mean())
    p = out["p_same"]
    out["markov_run_ge_5"] = float(p**4)
    out["markov_run_ge_10"] = float(p**9)
    out["markov_run_ge_20"] = float(p**19)
    return out


def impact_stats(mid: np.ndarray, sign: np.ndarray, valid: np.ndarray, same: np.ndarray) -> dict:
    """The signed mid move after a parent: `mid` is the mid before each
    parent trades, so the move over `lag` parents is what that parent and
    the ones after it did to the book."""
    impact = {}
    for lag in (1, 10, 100):
        if lag >= mid.size:
            break
        keep = segment_lag_mask(same, lag, mid.size - lag) & valid[:-lag] & valid[lag:]
        move = (mid[lag:][keep] - mid[:-lag][keep]) * sign[:-lag][keep]
        impact[str(lag)] = {
            "mean_ticks": float(move.mean()),
            "p_with": float((move > 0).mean()),
            "p_against": float((move < 0).mean()),
        }
    return impact


def price_stats(seg: pl.DataFrame) -> dict:
    consecutive = seg.filter(pl.col("same_segment"))
    change = (consecutive["price_last"] - consecutive["prev_price"]).abs()
    out = {
        "n": consecutive.height,
        "abs_change_pmf": pmf(
            change.clip(upper_bound=3).cast(pl.Utf8).replace({"3": "3+"}).alias("d"),
            ["0", "1", "2", "3+"],
        ),
        "abs_change_mean": float(change.mean()),
    }
    sign = seg["sign"].to_numpy().astype(np.float64)
    same = seg["same_segment"].to_numpy()
    if seg["bid_ticks"].null_count() < seg.height:
        valid_expr = (pl.col("bid_ticks") > 0) & (pl.col("ask_ticks") > pl.col("bid_ticks"))
        book = seg.filter(valid_expr)
        spread = book["ask_ticks"] - book["bid_ticks"]
        out["spread_pmf"] = pmf(
            spread.clip(upper_bound=4).cast(pl.Utf8).replace({"4": "4+"}).alias("s"),
            ["1", "2", "3", "4+"],
        )
        out["spread_mean"] = float(spread.mean())
        mid = (
            seg["bid_ticks"].fill_null(0) + seg["ask_ticks"].fill_null(0)
        ).to_numpy().astype(np.float64) / 2.0
        valid = seg.select(valid_expr.fill_null(False).alias("v"))["v"].to_numpy()
        out["impact"] = impact_stats(mid, sign, valid, same)
    else:
        # A generated tape carries no book in its trades CSV, but its
        # print sits at the touch, half the quoted width off the book mid
        # on the aggressor's side, so the mid after a parent is its first
        # print less that half width, and the mid before parent k is the
        # mid after parent k - 1.
        post_mid = (
            seg["price_first"].to_numpy().astype(np.float64)
            - CANDIDATE_HALF_WIDTH_TICKS * sign
        )
        pre_mid = np.concatenate([[np.nan], post_mid[:-1]])
        valid = ~np.isnan(pre_mid) & np.concatenate([[False], same[1:]])
        out["impact"] = impact_stats(np.nan_to_num(pre_mid), sign, valid, same)
    return out


def phase_block(parents: pl.DataFrame, prints: pl.DataFrame, sessions: int, phase: str) -> dict:
    seg = within_segments(parents)
    if phase != "all":
        seg = seg.filter(pl.col("phase") == phase)
        minutes = PHASE_MINUTES[phase]
    else:
        seg = seg.filter(pl.col("phase") != "closed")
        minutes = sum(PHASE_MINUTES.values())
    if seg.height == 0:
        return {}
    rate = seg.height / (sessions * minutes * 60.0)
    return {
        "sessions": sessions,
        "parents": seg.height,
        "rate_per_s": rate,
        "gaps": gap_stats(seg),
        "dispersion": {
            "10ms_in_1s": dispersion(seg, NS, NS // 100),
            "100ms_in_1s": dispersion(seg, NS, NS // 10),
            "1s_in_1m": dispersion(seg, 60 * NS, NS),
            "10s_in_1m": dispersion(seg, 60 * NS, 10 * NS),
        },
        "sweep": sweep_stats(seg),
        "print_size": print_size_stats(prints) if phase == "all" else {},
        "sign": sign_stats(seg),
        "price": price_stats(seg),
    }


CANDIDATE_MERGE_NS = 10_000
CANDIDATE_UTC_OFFSET_MINUTES = -300
# The MNQ preset quotes a two-tick width, so a print is one tick off the
# book mid.
CANDIDATE_HALF_WIDTH_TICKS = 1.0


def compute_targets(prints: pl.DataFrame, candidate: bool = False) -> dict:
    if candidate:
        parents = group_parents(prints, CANDIDATE_MERGE_NS, CANDIDATE_UTC_OFFSET_MINUTES)
    else:
        parents = group_parents(prints)
    sessions = parents.filter(pl.col("phase") != "closed")["session_date"].n_unique()
    out = {}
    for phase in ["all", *PHASE_NAMES]:
        block = phase_block(parents, prints, sessions, phase)
        if block:
            out[phase] = block
    return out


# ---------------------------------------------------------------- driver


def targets_path(parent: str, label: str) -> Path:
    return DATA_DIR / "micro" / f"{parent}-{label}-targets.json"


def real_months(parent: str, first: str | None, last: str | None) -> list[str]:
    from .micro import micro_dir

    days = [p.stem for p in sorted(micro_dir("tbbo", parent).glob("*.parquet"))]
    if first:
        days = [d for d in days if d >= first]
    if last:
        days = [d for d in days if d <= last]
    return sorted({d[:6] for d in days})


def run_stats(
    parent: str,
    label: str,
    csvs: list[Path],
    first: str | None,
    last: str | None,
    tick: float | None,
) -> None:
    blocks: dict[str, dict] = {}
    if label == "real":
        for month in real_months(parent, first, last):
            prints = prints_from_real(parent, month + "01", month + "31")
            print(f"{parent} {month}: {prints.height} prints", file=sys.stderr)
            blocks[month] = compute_targets(prints)
    else:
        if tick is None:
            raise SystemExit("a candidate needs --tick")
        for path in csvs:
            prints = prints_from_csv(path, tick)
            print(f"{parent} {path.stem}: {prints.height} prints", file=sys.stderr)
            blocks[path.stem] = compute_targets(prints, candidate=True)
    path = targets_path(parent, label)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps({"blocks": blocks}, indent=1))
    print(f"wrote {path}")
    print_table(blocks, label)
    real_path = targets_path(parent, "real")
    if label != "real" and real_path.exists():
        real = json.loads(real_path.read_text())["blocks"]
        print_comparison(real, blocks, label)


def flatten(block: dict) -> dict[str, float]:
    """The scalar lines a phase is compared on."""
    nan = float("nan")
    g = block.get("gaps", {})
    d = block.get("dispersion", {})
    s = block.get("sweep", {})
    sg = block.get("sign", {})
    p = block.get("price", {})
    return {
        "parents/s": block.get("rate_per_s", nan),
        "gap p50 ms": g.get("ms", {}).get("p50", nan),
        "gap p90 ms": g.get("ms", {}).get("p90", nan),
        "gap p99 ms": g.get("ms", {}).get("p99", nan),
        "gap <1ms": g.get("frac_lt_1ms", nan),
        "gap <10ms": g.get("frac_lt_10ms", nan),
        "gap <100ms": g.get("frac_lt_100ms", nan),
        "u p50": g.get("u", {}).get("p50", nan),
        "u p90": g.get("u", {}).get("p90", nan),
        "u p99": g.get("u", {}).get("p99", nan),
        "u cv": g.get("u_cv", nan),
        "log u acf1": g.get("log_u_acf", {}).get("1", nan),
        "log u acf5": g.get("log_u_acf", {}).get("5", nan),
        "disp 10ms/1s": d.get("10ms_in_1s", nan),
        "disp 100ms/1s": d.get("100ms_in_1s", nan),
        "disp 1s/1m": d.get("1s_in_1m", nan),
        "disp 10s/1m": d.get("10s_in_1m", nan),
        "children mean": s.get("children_mean", nan),
        "children single": s.get("children_single_frac", nan),
        "children 2": s.get("children_pmf", {}).get("2", nan),
        "children 3": s.get("children_pmf", {}).get("3", nan),
        "children 6+": s.get("children_pmf", {}).get("6+", nan),
        "levels mean": s.get("levels_mean", nan),
        "levels 2": s.get("levels_pmf", {}).get("2", nan),
        "multi lvl|multi": s.get("multi_level_given_multi_print", nan),
        "parent size mean": s.get("parent_size_mean", nan),
        "parent size p90": s.get("parent_size", {}).get("p90", nan),
        "parent size p99": s.get("parent_size", {}).get("p99", nan),
        "parent size 1": s.get("parent_size_pmf", {}).get("1", nan),
        "parent size 6-10": s.get("parent_size_pmf", {}).get("6-10", nan),
        "parent size 11-20": s.get("parent_size_pmf", {}).get("11-20", nan),
        "size>=touch": s.get("touch", {}).get("size_ge_touch", nan),
        "size>touch": s.get("touch", {}).get("size_gt_touch", nan),
        "touch p50": s.get("touch", {}).get("touch_size", {}).get("p50", nan),
        "p same side": sg.get("p_same", nan),
        "sign acf1": sg.get("acf", {}).get("1", nan),
        "sign acf2": sg.get("acf", {}).get("2", nan),
        "sign acf5": sg.get("acf", {}).get("5", nan),
        "sign acf10": sg.get("acf", {}).get("10", nan),
        "sign acf50": sg.get("acf", {}).get("50", nan),
        "sign acf100": sg.get("acf", {}).get("100", nan),
        "sign acf1000": sg.get("acf", {}).get("1000", nan),
        "run mean": sg.get("run_mean", nan),
        "run >=5": sg.get("run_ge_5", nan),
        "run >=10": sg.get("run_ge_10", nan),
        "markov run >=10": sg.get("markov_run_ge_10", nan),
        "dprice 0": p.get("abs_change_pmf", {}).get("0", nan),
        "dprice 1": p.get("abs_change_pmf", {}).get("1", nan),
        "dprice 2": p.get("abs_change_pmf", {}).get("2", nan),
        "dprice 3+": p.get("abs_change_pmf", {}).get("3+", nan),
        "spread 1": p.get("spread_pmf", {}).get("1", nan),
        "spread 2": p.get("spread_pmf", {}).get("2", nan),
        "impact1 ticks": p.get("impact", {}).get("1", {}).get("mean_ticks", nan),
        "impact10 ticks": p.get("impact", {}).get("10", {}).get("mean_ticks", nan),
        "impact100 ticks": p.get("impact", {}).get("100", {}).get("mean_ticks", nan),
    }


def _row(name: str, values: list, width: int = 9) -> str:
    cells = []
    for v in values:
        if isinstance(v, float):
            cells.append(f"{v:{width}.3f}")
        else:
            cells.append(f"{v!s:>{width}}")
    return f"{name:<20}" + "".join(cells)


def block_medians(blocks: dict[str, dict], phase: str) -> dict[str, tuple[float, float, float]]:
    """Per statistic: median, p10, p90 across blocks that carry the phase."""
    rows = [flatten(b[phase]) for b in blocks.values() if phase in b]
    keys = flatten(next(iter(blocks.values()))["all"]).keys()
    out = {}
    for key in keys:
        values = np.array([r[key] for r in rows], dtype=float)
        values = values[~np.isnan(values)]
        if values.size == 0:
            out[key] = (float("nan"), float("nan"), float("nan"))
        else:
            out[key] = (
                float(np.median(values)),
                float(np.quantile(values, 0.1)),
                float(np.quantile(values, 0.9)),
            )
    return out


def print_table(blocks: dict[str, dict], label: str) -> None:
    phases = [p for p in ["all", *PHASE_NAMES] if any(p in b for b in blocks.values())]
    print(f"\n{label}: {len(blocks)} blocks, median across blocks")
    print(_row("", phases))
    medians = {p: block_medians(blocks, p) for p in phases}
    for key in medians["all"]:
        print(_row(key, [medians[p][key][0] for p in phases]))


def print_comparison(real: dict, cand: dict, label: str) -> None:
    phases = [
        p
        for p in ["all", "asia", "london", "ny_open", "ny_close"]
        if any(p in b for b in real.values()) and any(p in b for b in cand.values())
    ]
    print(f"\nreal vs {label}: real median [p10, p90] across months, then {label} median")
    print(_row("", [c for p in phases for c in (p[:8], "lo", "hi", label[:8])], width=8))
    real_rows = {p: block_medians(real, p) for p in phases}
    cand_rows = {p: block_medians(cand, p) for p in phases}
    for key in real_rows["all"]:
        cells: list = []
        for p in phases:
            med, lo, hi = real_rows[p][key]
            c = cand_rows[p][key][0]
            cells.extend([med, lo, hi, c])
        line = _row(key, cells, width=8)
        flags = []
        for p in phases:
            _med, lo, hi = real_rows[p][key]
            c = cand_rows[p][key][0]
            if np.isnan(c) or np.isnan(lo):
                flags.append(" ")
            elif lo <= c <= hi:
                flags.append(".")
            else:
                flags.append("X")
        print(line + "  " + "".join(flags))
