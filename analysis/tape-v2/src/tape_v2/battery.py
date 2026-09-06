"""The activity half of the validation battery: containment of a
generated tape's per-minute volume and range inside the real cross-session
distribution.

For every (session, minute) of the candidate, the value is checked against
the real p10 and p90 at that minute of session. If the candidate were
drawn from the real distribution, 80 percent of its values would sit
inside the band. Per phase, the report gives the candidate's median over
the real median (a ratio that cannot hide behind a share), the share
inside the band, and the share above p90, which is where a tape that is
too active shows up.

Two views are reported for each quantity. `level` compares raw minutes,
which folds in the slow regime: the real activity level moves by a factor
of several across the year, so a candidate at one level can only sit
inside that band by luck. `shape` compares per-session normalised
minutes, where a candidate is judged on where its activity falls within
the session regardless of how much there is. The programme's L0 owns
shape and L1 owns level, so the two rows name which layer a miss belongs
to.
"""

from __future__ import annotations

import json

import polars as pl

from .corpus import DATA_DIR
from .profile import load_profile
from .session import PHASES

VIEWS = {
    "level": ("volume", "range"),
    "shape": ("volume_norm", "range_norm"),
}


def containment(
    real_profile: pl.DataFrame,
    candidate_matrix: pl.DataFrame,
    volume_col: str,
    range_col: str,
) -> pl.DataFrame:
    bands = real_profile.select(
        "session_minute",
        pl.col(f"{volume_col}_p10").alias("v10"),
        pl.col(f"{volume_col}_p50").alias("v50"),
        pl.col(f"{volume_col}_p90").alias("v90"),
        pl.col(f"{range_col}_p10").alias("r10"),
        pl.col(f"{range_col}_p50").alias("r50"),
        pl.col(f"{range_col}_p90").alias("r90"),
    )
    joined = candidate_matrix.join(bands, on="session_minute", how="inner")
    vol = pl.col(volume_col)
    rng = pl.col(range_col)
    return joined.group_by("phase").agg(
        pl.len().alias("n"),
        (vol.median() / pl.col("v50").median()).alias("vol_ratio"),
        ((vol >= pl.col("v10")) & (vol <= pl.col("v90")))
        .mean()
        .alias("vol_inside"),
        (vol > pl.col("v90")).mean().alias("vol_above"),
        (rng.median() / pl.col("r50").median()).alias("rng_ratio"),
        ((rng >= pl.col("r10")) & (rng <= pl.col("r90")))
        .mean()
        .alias("rng_inside"),
        (rng > pl.col("r90")).mean().alias("rng_above"),
    )


def run_battery(parent: str, candidate_label: str) -> dict:
    real_matrix, real_profile = load_profile(parent, "real")
    cand_matrix, _ = load_profile(parent, candidate_label)
    print(
        f"\n{parent}: {candidate_label} against real, "
        f"{real_matrix['session_key'].n_unique()} real sessions, "
        f"{cand_matrix['session_key'].n_unique()} candidate sessions"
    )
    out: dict = {"parent": parent, "candidate": candidate_label, "views": {}}
    for view, (volume_col, range_col) in VIEWS.items():
        report = containment(real_profile, cand_matrix, volume_col, range_col)
        rows = {r["phase"]: r for r in report.to_dicts()}
        out["views"][view] = rows
        print(f"\n{view}:")
        print(
            "phase      vol_ratio  vol_inside  vol_above  "
            "rng_ratio  rng_inside  rng_above"
        )
        for name, _, _ in PHASES:
            r = rows.get(name)
            if r is None:
                continue
            print(
                f"{name:9s}  {r['vol_ratio']:9.2f}  {r['vol_inside']:10.2f}  "
                f"{r['vol_above']:9.2f}  {r['rng_ratio']:9.2f}  "
                f"{r['rng_inside']:10.2f}  {r['rng_above']:9.2f}"
            )
    print("(a matching tape reads ratio 1.00, inside 0.80, above 0.10)")
    out_dir = DATA_DIR / "battery"
    out_dir.mkdir(parents=True, exist_ok=True)
    with open(out_dir / f"{parent}-{candidate_label}.json", "w") as f:
        json.dump(out, f, indent=1)
    return out
