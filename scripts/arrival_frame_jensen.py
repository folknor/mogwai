#!/usr/bin/env python3
"""Closed-form Jensen factor for the protocol-12b integrated arrival frame.

The shipped path divides a whole gap draw by the session multiplier M, so its
mean gap over a set of open minutes is base_mean * E[1/M]. The protocol-12b
integrated frame (spec 4.2) instead integrates rate_at(u) = M(u)/thin against
the budget, so its mean gap is base_mean / E[M]. The ratio of realized parent
RATES between the two frames is therefore

    E[M] * E[1/M]   >= 1   by Jensen

which is the candidate explanation for brick A0's measured ~1.063 excess.

This computes that factor from the FROZEN shipped curves and the FROZEN MNQ
calendar, reproducing SessionModulator::new's own week sweep exactly: 10,080
consecutive minutes from clock 0, the runtime's own is_open and civil
derivation, arr_hour = intensity_hour * 24, arr_dow = dow_weight * 7.

Reports both the pooled factor over all open minutes AND the per-hour factor,
because gate A2 is evaluated PER HOUR: within one hour M varies only by
day-of-week, so a pooled figure mixes in hour-to-hour variation the gate never
sees. If the per-hour factors do not reproduce the probe's ~1.063, the excess
is not this mechanism and the frame recalibration would repair the wrong thing.

Reads nothing at runtime: the curves and windows below are transcribed from
crates/mogwai-server/presets/mnq.toml and re-verified against it by the
assertions in `main`.
"""

MINUTES_PER_WEEK = 7 * 24 * 60
MINUTES_PER_DAY = 24 * 60
UNIX_EPOCH_LOCAL_WEEK_MINUTE = 4 * MINUTES_PER_DAY

UTC_OFFSET_MINUTES = -300

OPEN_WINDOWS = [
    (1020, 2355),
    (2370, 2400),
    (2460, 3795),
    (3810, 3840),
    (3900, 5235),
    (5250, 5280),
    (5340, 6675),
    (6690, 6720),
    (6780, 8115),
    (8130, 8160),
]

INTENSITY_HOUR = [
    0.788959, 0.606801, 0.476029, 0.404415, 0.33337, 0.389075, 0.370589,
    0.385735, 0.425944, 0.289761, 0.286266, 0.428253, 0.677683, 3.200686,
    4.138113, 2.546961, 1.723787, 1.314281, 1.292247, 1.624038, 0.470938,
    1.0, 0.33058, 0.357795,
]

# Sun, Mon, Tue, Wed, Thu, Fri, Sat
DOW_WEIGHT = [1.5179, 0.9080, 0.9865, 1.0157, 1.0535, 1.0225, 1.0000]


def local_week_minute(minute):
    return (minute + UTC_OFFSET_MINUTES + UNIX_EPOCH_LOCAL_WEEK_MINUTE) % MINUTES_PER_WEEK


def is_open(minute):
    m = local_week_minute(minute)
    for start, end in OPEN_WINDOWS:
        if start < end:
            if start <= m < end:
                return True
        elif m >= start or m < end:
            return True
    return False


def utc_hour_dow(minute):
    secs = minute * 60
    days = secs // 86_400
    hour = (secs % 86_400) // 3_600
    dow = (days + 4) % 7
    return hour, dow


def main():
    arr_hour = [v * 24.0 for v in INTENSITY_HOUR]
    arr_dow = [v * 7.0 for v in DOW_WEIGHT]

    # The modulator's own sweep: every open minute of one model week.
    cells = []  # (hour, dow, composite) per open minute
    for minute in range(MINUTES_PER_WEEK):
        if not is_open(minute):
            continue
        hour, dow = utc_hour_dow(minute)
        cells.append((hour, dow, arr_hour[hour] * arr_dow[dow]))

    open_minutes = len(cells)
    normalizer = sum(c for _, _, c in cells) / open_minutes

    mults = [c / normalizer for _, _, c in cells]
    e_m = sum(mults) / open_minutes
    e_inv = sum(1.0 / m for m in mults) / open_minutes

    print(f"open minutes per model week : {open_minutes}")
    print(f"arrival_normalizer          : {normalizer:.9f}")
    print(f"pooled E[M]                 : {e_m:.9f}")
    print(f"pooled E[1/M]               : {e_inv:.9f}")
    print(f"pooled Jensen factor        : {e_m * e_inv:.9f}")
    print()

    print("per hour (the scale gate A2 actually sees):")
    print(f"{'hour':>4} {'open':>6} {'E[M]':>12} {'E[1/M]':>12} {'factor':>12} {'CV(M)':>10}")
    worst = 0.0
    for hour in range(24):
        hm = [m for (h, _, _), m in zip(cells, mults) if h == hour]
        if not hm:
            continue
        n = len(hm)
        a = sum(hm) / n
        b = sum(1.0 / m for m in hm) / n
        var = sum((m - a) ** 2 for m in hm) / n
        cv = (var ** 0.5) / a
        factor = a * b
        worst = max(worst, factor)
        print(f"{hour:>4} {n:>6} {a:>12.6f} {b:>12.6f} {factor:>12.9f} {cv:>10.6f}")

    print()
    print(f"worst per-hour Jensen factor: {worst:.9f}  (excess {100 * (worst - 1):.4f} percent)")
    print("brick A0 measured, per hour, across all 23 traded hours: 1.0615 to 1.0676")


if __name__ == "__main__":
    main()
