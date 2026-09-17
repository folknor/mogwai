#!/usr/bin/env python3
"""One-shot: write the ratified gate semantics into book-config.json -
the declared sampling allowance with its recorded effective limits on
the three pooled frozen exceptions, and the ninth-spar deferral ledger
as machine-readable data (cell, deferred value at the pinned recipe,
the phase band it missed). Values are the adjudication record, entered
from the 2026-09-17 probe at seed 1, sim seed 2, 400k parents."""

from __future__ import annotations

import json
from pathlib import Path

HERE = Path(__file__).resolve().parent
CONFIG = HERE / "book-config.json"

ALLOWANCE = 0.005

LEDGER = {
    "comment": "The deferral ledger, machine-readable: the ninth-spar "
    "adjudication plus the tenth spar's three dprice-1 cells (basis: "
    "the real target is the fast bucket, gaps under 100 ms, while the "
    "parent-indexed prototype measures all consecutive pairs and "
    "cannot condition on elapsed time - the same identification gap "
    "that created the witness deferrals). Each cell is remeasured at "
    "the engine gate against its original phase band - a cell outside "
    "there fails acceptance and blocks the tape bless; no deferred "
    "value is an alternative bound, and the engine never takes this "
    "bypass. The recorded value is the pinned-recipe prototype "
    "measurement (seed 1, sim seed 2, 400k parents), historical "
    "evidence only. The battery classifies a cell here as deferred "
    "only when its measurement is finite and its band present.",
    "asia": [
        {"key": "witness", "sub": "lt", "value": 0.2249, "band_lo": 0.1060, "band_hi": 0.1987},
        {"key": "witness", "sub": "eq", "value": 0.7856, "band_lo": 0.7935, "band_hi": 0.8391},
        {"key": "size_pmf_11_20", "value": 0.0153, "band_lo": 0.0125, "band_hi": 0.0142},
    ],
    "london": [
        {"key": "dprice_pmf", "sub": "0", "value": 0.2349, "band_lo": 0.2349, "band_hi": 0.3807},
        {"key": "dprice_pmf", "sub": "1", "value": 0.4021, "band_lo": 0.4039, "band_hi": 0.4338},
        {"key": "witness", "sub": "lt", "value": 0.2581, "band_lo": 0.1030, "band_hi": 0.2073},
        {"key": "witness", "sub": "eq", "value": 0.7910, "band_lo": 0.8172, "band_hi": 0.8351},
        {"key": "witness", "sub": "gt", "value": 0.7871, "band_lo": 0.6757, "band_hi": 0.7594},
        {"key": "size_pmf_1", "value": 0.5680, "band_lo": 0.5901, "band_hi": 0.6056},
        {"key": "size_pmf_11_20", "value": 0.0156, "band_lo": 0.0114, "band_hi": 0.0133},
    ],
    "ny_open": [
        {"key": "dprice_pmf", "sub": "1", "value": 0.4290, "band_lo": 0.3835, "band_hi": 0.4087},
        {"key": "witness", "sub": "eq", "value": 0.6921, "band_lo": 0.7048, "band_hi": 0.7379},
        {"key": "touch_p90_1", "value": 7.0, "band_lo": 4.0, "band_hi": 5.7},
        {"key": "touch_p90_2", "value": 12.0, "band_lo": 6.3, "band_hi": 10.7},
        {"key": "size_p99", "value": 17.0, "band_lo": 13.3, "band_hi": 15.0},
        {"key": "size_pmf_11_20", "value": 0.0147, "band_lo": 0.0103, "band_hi": 0.0125},
    ],
    "ny_close": [
        {"key": "dprice_pmf", "sub": "1", "value": 0.4570, "band_lo": 0.3966, "band_hi": 0.4165},
        {"key": "witness", "sub": "eq", "value": 0.7350, "band_lo": 0.7057, "band_hi": 0.7346},
        {"key": "witness", "sub": "gt", "value": 0.6201, "band_lo": 0.5327, "band_hi": 0.6098},
        {"key": "touch_p50_3", "value": 4.0, "band_lo": 2.0, "band_hi": 3.0},
        {"key": "size_pmf_1", "value": 0.5757, "band_lo": 0.5886, "band_hi": 0.6210},
    ],
}


def main() -> None:
    cfg = json.loads(CONFIG.read_text())
    exceptions = cfg["exceptions"]
    exceptions["comment"] = (
        "The three frozen tier-1 exceptions of the POOLED row only: each "
        "sits one to two percent relative beyond every real month, the "
        "omitted mechanism is identified (marketable-limit truncation, "
        "absorbed by the deconvolved match table), and the pooled gates - "
        "prototype and engine alike - bind them one-sided at the frozen "
        "value plus the declared sampling allowance, an explicit "
        "acceptance policy covering the rounding of the frozen constants "
        "and realization noise, applied once, outward, to these three "
        "bounds and nothing else. On phase rows these cells take their "
        "ordinary phase bands. Effective pooled limits are recorded so "
        "the allowance cannot obscure the actual gate."
    )
    for name, effective in [
        ("size_gt_touch", 0.097 + ALLOWANCE),
        ("levels_mean", 1.067 - ALLOWANCE),
        ("dprice_pmf_1", 0.419 + ALLOWANCE),
    ]:
        exceptions[name]["allowance"] = ALLOWANCE
        exceptions[name]["effective"] = round(effective, 3)
    # The levels_mean exception's gate effect is retired (tenth spar):
    # its evidence was the defective exhausted-levels observation, which
    # undershot the real distinct-prices convention by construction. The
    # corrected pooled cell reads 1.148 against band 1.085..1.121 - out
    # on the OTHER side - so the one-sided floor bound covers nothing
    # real. The historical ruling stands as history; the cell grades
    # against its ordinary band until the owner rules on the fork.
    exceptions["levels_mean"]["retired"] = (
        "2026-09-17, tenth spar: frozen on the defective exhausted-levels "
        "statistic; the corrected observation sits above the band, not "
        "below it. The cell takes its ordinary band."
    )
    cfg["deferrals"] = LEDGER
    # The neighborhood impact criterion (owner ruling, 2026-09-17): the
    # binding contract is the collapse floor - every jittered sample
    # keeps every impact lag at half of real or better - and the share
    # of samples under the old three-quarters mark is reported per lag
    # as a diagnostic. The 0.75-everywhere criterion was never met by
    # the recorded ninth-spar acceptance (worst points at 0.54-0.87 of
    # real were called graceful), so the criterion and the accepted
    # history conflicted; adopting the floor is a declared weakening of
    # neighborhood acceptance. An absolute floor cannot distinguish
    # smooth degradation from a nearby cliff, so the close's measured
    # cliff stays recorded as an unresolved robustness limitation in
    # notes/book-dynamics-spec.md.
    cfg["neighborhood"] = {
        "comment": "The neighborhood impact contract: every jittered "
        "sample keeps every impact lag at collapse_floor of real or "
        "better; the share of samples under report_threshold is a "
        "reported per-lag diagnostic, not a verdict. Declared weakening "
        "of the original 0.75-everywhere criterion, owner ruling "
        "2026-09-17; the close's measured cliff remains an unresolved "
        "robustness limitation either way.",
        "collapse_floor": 0.5,
        "report_threshold": 0.75,
    }
    cfg["gate_recipe"] = {
        "comment": "The pinned sampling recipe of the adjudicated center "
        "verdict: deterministic reproduction of the ninth-spar grading. "
        "Other seeds' band misses are diagnostics, not verdicts.",
        "seed": 1,
        "sim_seed": 2,
        "parents": 400000,
        "sign_slots": 5,
        "sign_alpha": 2.2,
        "sign_mix": 0.08,
    }
    CONFIG.write_text(json.dumps(cfg, indent=1) + "\n")
    print(f"wrote {CONFIG}")


if __name__ == "__main__":
    main()
