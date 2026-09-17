#!/usr/bin/env python3
"""Transient probe: is the adopted size channel in book-config.json
bit-identical to the runtime derivation the battery used before the
artifact chain closed? Any nonzero delta names the divergence."""

from __future__ import annotations

import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

from proto_book import (  # noqa: E402
    MATCH_BUCKETS,
    SIZE_SUPPORT,
    adopted_size_channel,
    book_config,
    deconvolve_sizes,
    real_medians,
)

for phase in ["all", "asia", "london", "ny_open", "ny_close"]:
    p_old, f_old = deconvolve_sizes(real_medians(phase))
    p_new, f_new = adopted_size_channel(book_config(phase))
    dp = max(abs(p_new[b] - p_old.get(b, 0.0)) for b in MATCH_BUCKETS)
    df = max(abs(f_new[k] - f_old.get(k, 0.0)) for k in SIZE_SUPPORT)
    print(f"{phase:>9}  max dp {dp:.3e}  max dF {df:.3e}")
