"""First-pass inspection of the vendor CME 1-minute bar archives.

Reads the zipped semicolon-delimited CSVs in research/market-data and reports
what the files actually contain: span, row count, bar interval, session shape,
price grid and roll-adjustment evidence. Written to answer "are these usable as
a sampling frame for choosing tick-data windows", so it validates the input
before anything downstream trusts it.

Run: python3 analysis/inspect_cme_bars.py
"""

import collections
import datetime as dt
import os
import zipfile

MARKET_DATA = os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
    "research",
    "market-data",
)

ARCHIVES = ["nq-1m_bk.zip", "es-1m_bk.zip", "cl-1m_bk.zip", "gc-1m.zip"]


def parse_line(line):
    """DATE;TIME;O;H;L;C;V with DD/MM/YYYY and HH:MM or HH:MM:SS."""
    parts = line.rstrip("\r\n").split(";")
    if len(parts) != 7:
        return None
    # Every failure mode returns None so the caller's `bad` counter means what
    # its label says. Previously only the field-count check returned None and a
    # malformed number, a 13th month or a short time field raised instead, so
    # the run died on the offending row and "unparseable N" could only ever
    # report N field-count mismatches - the one failure it was least likely to
    # meet in a vendor archive. IndexError covers a time field with no colon
    # (hms[1] out of range); ValueError covers every bad number and every
    # out-of-range date component.
    try:
        d, t = parts[0], parts[1]
        day, month, year = (int(x) for x in d.split("/"))
        hms = [int(x) for x in t.split(":")]
        hour, minute = hms[0], hms[1]
        second = hms[2] if len(hms) > 2 else 0
        stamp = dt.datetime(year, month, day, hour, minute, second)
        o, h, low, c = (float(x) for x in parts[2:6])
        vol = int(parts[6])
    except (ValueError, IndexError):
        return None
    return stamp, o, h, low, c, vol


def decimals(value_str):
    if "." not in value_str:
        return 0
    return len(value_str.split(".")[1].rstrip("0"))


def inspect(name):
    path = os.path.join(MARKET_DATA, name)
    z = zipfile.ZipFile(path)
    member = z.infolist()[0].filename

    rows = 0
    bad = 0
    first = None
    last = None
    prev = None
    deltas = collections.Counter()
    dec_counts = collections.Counter()
    min_px = float("inf")
    max_px = float("-inf")
    zero_vol = 0
    minute_of_day = collections.Counter()
    weekday = collections.Counter()
    backwards = 0
    duplicate_stamps = 0
    yearly = collections.defaultdict(lambda: [0, None, None])

    with z.open(member) as fh:
        for raw in fh:
            line = raw.decode("ascii", "replace")
            parsed = parse_line(line)
            if parsed is None:
                bad += 1
                continue
            stamp, o, h, low, c, vol = parsed
            rows += 1
            if first is None:
                first = stamp
                dec_counts[decimals(line.split(";")[5])] += 1
            last = stamp
            if prev is not None:
                delta = int((stamp - prev).total_seconds())
                if delta < 0:
                    backwards += 1
                elif delta == 0:
                    duplicate_stamps += 1
                else:
                    deltas[delta] += 1
            prev = stamp
            if rows % 97 == 0:
                dec_counts[decimals(line.split(";")[5])] += 1
            min_px = min(min_px, low)
            max_px = max(max_px, h)
            if vol == 0:
                zero_vol += 1
            minute_of_day[stamp.hour] += 1
            weekday[stamp.weekday()] += 1
            slot = yearly[stamp.year]
            slot[0] += 1
            if slot[1] is None:
                slot[1] = c
            slot[2] = c

    print("=" * 70)
    print(name, " member:", member)
    print(f"  rows            {rows:,}   unparseable {bad}")
    print(f"  span            {first}  ->  {last}")
    print(f"  price range     {min_px:,.4f}  ->  {max_px:,.4f}")
    print(f"  zero-volume     {zero_vol:,}")
    print(f"  backwards       {backwards:,}   duplicate stamps {duplicate_stamps:,}")
    print("  top gaps (seconds between consecutive bars):")
    for delta, count in deltas.most_common(8):
        print(f"      {delta:>9,} s   {count:>12,}")
    print("  close-price decimal places (sampled):")
    for places, count in sorted(dec_counts.items()):
        print(f"      {places} dp   {count:,}")
    print("  bars by weekday (0=Mon):")
    for day in range(7):
        print(f"      {day}   {weekday.get(day, 0):,}")
    print("  bars by hour of day:")
    for hour in range(24):
        print(f"      {hour:02d}   {minute_of_day.get(hour, 0):,}")
    print("  year: rows, first close, last close")
    for year in sorted(yearly):
        count, opened, closed = yearly[year]
        print(f"      {year}   {count:>9,}   {opened:>12,.4f}   {closed:>12,.4f}")


def main():
    for name in ARCHIVES:
        inspect(name)


if __name__ == "__main__":
    main()
