#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 folknor
# SPDX-License-Identifier: AGPL-3.0-only
"""Generate the CPython differential fixture for `mogwai_lab::exact`.

`crates/mogwai-lab/src/exact.rs` computes the population variance as the exact
rational `(n*sum(x^2) - (sum x)^2) / n^2` and rounds once, which is what
`statistics.pvariance` does. Six hand-picked cases do not establish that, and
hand-picked cases are exactly how three successive ULP ceilings got claimed and
refuted on the approach this replaced. So the parity claim rests on a generated
sweep instead.

The families are chosen for what they stress, not for coverage theatre:

- `ordinary`   - gap-like series across several magnitudes.
- `clustered`  - tiny spread about a large mean. This is the ill-conditioned
                 family where the old `py_fsum`-over-squared-deviations
                 approach was wrong by a factor of three.
- `adjacent`   - values one to three representable steps apart, the extreme of
                 the same family.
- `wide`       - terms spanning up to 24 decades, where naive accumulation
                 loses the small ones entirely.
- `pow2`/`int` - values whose arithmetic should be exactly representable, so a
                 rounding bug shows up as a difference rather than a wobble.
- `identical`  - exact zero variance, including the cancellation path.

Inputs and expected outputs are both recorded as raw bit patterns: a decimal
literal would put CPython's float parser and Rust's on the critical path of a
test that is supposed to be about arithmetic.

Regenerate with `python3 scripts/gen_pvariance_cases.py`. It is deterministic.
"""

import json
import os
import random
import statistics
import struct

CASES_PATH = "crates/mogwai-lab/tests/data/pvariance_cases.json"
SEED = 20260808


def bits(value: float) -> str:
    return f"{struct.unpack('<Q', struct.pack('<d', value))[0]:016x}"


def build() -> list[dict]:
    rng = random.Random(SEED)
    cases: list[dict] = []

    def add(gaps: list[float], why: str) -> None:
        cases.append(
            {
                "why": why,
                "gaps": [bits(g) for g in gaps],
                "expected": bits(statistics.pvariance(gaps)),
            }
        )

    for _ in range(220):
        n = rng.choice([2, 3, 4, 7, 16, 64, 257])
        scale = 10.0 ** rng.randint(-6, 4)
        add([rng.uniform(1e-9, 1.0) * scale for _ in range(n)], "ordinary")

    for _ in range(220):
        n = rng.choice([2, 3, 5, 12, 40])
        base = 10.0 ** rng.randint(-3, 4)
        spread = base * 10.0 ** rng.randint(-16, -6)
        add([base + rng.uniform(-spread, spread) for _ in range(n)], "clustered")

    for _ in range(120):
        anchor = struct.unpack("<Q", struct.pack("<d", rng.uniform(0.1, 1000.0)))[0]
        add(
            [
                struct.unpack("<d", struct.pack("<Q", anchor + rng.randint(0, 3)))[0]
                for _ in range(rng.choice([2, 3, 5]))
            ],
            "adjacent",
        )

    for _ in range(120):
        n = rng.choice([3, 6, 20])
        add(
            [10.0 ** rng.randint(-12, 12) * rng.uniform(0.5, 2.0) for _ in range(n)],
            "wide",
        )

    for _ in range(60):
        add([float(2 ** rng.randint(-40, 40)) for _ in range(rng.choice([2, 3, 8]))], "pow2")

    for _ in range(60):
        add([float(rng.randint(1, 10**6)) for _ in range(rng.choice([2, 3, 9]))], "int")

    for _ in range(20):
        value = rng.uniform(1e-6, 1e6)
        add([value] * rng.choice([2, 3, 10]), "identical")

    return cases


def main() -> None:
    cases = build()
    os.makedirs(os.path.dirname(CASES_PATH), exist_ok=True)
    with open(CASES_PATH, "w", encoding="utf-8") as handle:
        json.dump(cases, handle, indent=1)
        handle.write("\n")

    nonzero = sum(1 for c in cases if c["expected"] != "0" * 16)
    print(f"wrote {len(cases)} cases to {CASES_PATH}")
    print(f"  nonzero expected variances: {nonzero}")
    families: dict[str, int] = {}
    for case in cases:
        families[case["why"]] = families.get(case["why"], 0) + 1
    for name in sorted(families):
        print(f"  {name}: {families[name]}")


if __name__ == "__main__":
    main()
