#!/usr/bin/env python3
"""Which component lowers the prototype's range efficiency?

Range over realised volatility per session has one distribution for
every time-changed Brownian path, median near 1.5. The prototype reads
1.35 over 80 sessions. This simulates eight weeks per configuration with
one component switched off at a time, plus a Gaussian control, and
reports the median efficiency and the mean per-session (sum r)^2 / sum
r^2, both with standard errors.

    uv --directory analysis/tape-v2 run python proto_efficiency_probe.py
"""

from __future__ import annotations

import numpy as np

import proto_engine as pe

WEEKS = 8


def sessions_from(seconds, open_mask, path):
    n = len(seconds) // 60 * 60
    px = path[:n].reshape(-1, 60)
    om = open_mask[:n].reshape(-1, 60).any(axis=1)
    sec = seconds[:n].reshape(-1, 60)[:, 0]
    minute, sday = pe.session_minute(sec)
    out = []
    current = None
    key = None
    for i in range(len(px)):
        if not om[i]:
            continue
        k = (int(sday[i]), int(sec[i] // 86400) if minute[i] < 7 * 60 else int(sec[i] // 86400) + 1)
        # A session is a run of open minutes; the closure breaks it.
        if current is None or (i > 0 and not om[i - 1]):
            if current is not None and len(current["c"]) >= 1300:
                out.append(current)
            current = {"h": [], "l": [], "c": []}
        current["h"].append(px[i].max())
        current["l"].append(px[i].min())
        current["c"].append(px[i][-1])
    if current is not None and len(current["c"]) >= 1300:
        out.append(current)
    return out


def stats(sessions):
    eff = []
    ratio = []
    for s in sessions:
        c = np.array(s["c"])
        r = np.diff(c)
        rv = np.sqrt(np.sum(r * r))
        if rv == 0:
            continue
        eff.append((max(s["h"]) - min(s["l"])) / rv)
        ratio.append(np.sum(r) ** 2 / np.sum(r * r))
    e = np.array(eff)
    q = np.array(ratio)
    boot = [np.median(np.random.default_rng(i).choice(e, len(e))) for i in range(200)]
    return (
        f"n={len(e):3d}  efficiency median {np.median(e):.3f} (se {np.std(boot):.3f})  "
        f"session ratio mean {q.mean():.3f} (se {q.std() / np.sqrt(len(q)):.3f})"
    )


def main() -> None:
    base = pe.Params()
    configs = [
        ("full", {}),
        ("no jumps", {"jumps_per_session": 0.0}),
        ("student df 60", {"student_df": 60.0}),
        ("no texture", {"texture_s0": 0.0}),
        ("no level", {"level_sd": 0.0, "sigma_level_extra_sd": 0.0}),
        ("gaussian, flat", {"student_df": 60.0, "texture_s0": 0.0, "level_sd": 0.0, "sigma_level_extra_sd": 0.0, "jumps_per_session": 0.0}),
    ]
    for label, changes in configs:
        p = pe.Params(**{**base.__dict__, **changes})
        seconds, open_mask, counts, path = pe.simulate(11, WEEKS, p)
        print(f"{label:<16} {stats(sessions_from(seconds, open_mask, path))}")
    # Control: iid Gaussian minute returns, 1380 per session, 40 sessions,
    # with the range from a 60-step path inside each minute.
    rng = np.random.default_rng(5)
    sessions = []
    for _ in range(40):
        steps = rng.standard_normal(1380 * 60)
        path = np.cumsum(steps).reshape(1380, 60)
        sessions.append({"h": path.max(axis=1), "l": path.min(axis=1), "c": path[:, -1]})
    print(f"{'iid control':<16} {stats(sessions)}")


if __name__ == "__main__":
    main()
