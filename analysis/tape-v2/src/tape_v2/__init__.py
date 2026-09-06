"""tape-v2 command line entry point."""

from __future__ import annotations

import argparse
from pathlib import Path


def main(argv: list[str] | None = None) -> None:
    parser = argparse.ArgumentParser(prog="tape-v2")
    sub = parser.add_subparsers(dest="cmd", required=True)

    sub.add_parser(
        "index",
        help="scan the corpus and write data/corpus-index.parquet",
    )

    bars = sub.add_parser(
        "bars",
        help="write real front-month 1m bars in the chart gate's CSV shape",
    )
    bars.add_argument("--parent", required=True, help="ES, MNQ, CL, ...")
    bars.add_argument("--first", required=True, help="first split day")
    bars.add_argument("--last", required=True, help="last split day")

    extract = sub.add_parser(
        "extract",
        help="cache front-month 1m bars for a parent over a day range",
    )
    extract.add_argument("--parent", required=True)
    extract.add_argument("--first", required=True)
    extract.add_argument("--last", required=True)
    extract.add_argument("--workers", type=int, default=8)

    profile = sub.add_parser(
        "profile",
        help="per-minute-of-session profile and envelope fit of a tape",
    )
    profile.add_argument("--parent", required=True)
    profile.add_argument(
        "--label",
        default="real",
        help="`real` reads the extracted corpus bars; anything else names "
        "a candidate and needs --csv",
    )
    profile.add_argument(
        "--csv", nargs="*", type=Path, help="gen bars CSVs for a candidate"
    )

    battery = sub.add_parser(
        "battery", help="containment of a candidate profile in the real one"
    )
    battery.add_argument("--parent", required=True)
    battery.add_argument("--candidate", required=True)

    chart = sub.add_parser(
        "envelope-chart", help="render real and candidate envelopes"
    )
    chart.add_argument("--parent", required=True)
    chart.add_argument("--candidate", nargs="*", default=[])
    chart.add_argument("--out", required=True, type=Path)
    chart.add_argument("--title", default=None)

    env_toml = sub.add_parser(
        "envelope-toml",
        help="write a preset's envelope block from the real profile",
    )
    env_toml.add_argument("--parent", required=True)
    env_toml.add_argument("--corpus", required=True)
    env_toml.add_argument("--window", required=True)

    probe = sub.add_parser(
        "status-probe", help="print a day of status records for a parent"
    )
    probe.add_argument("--parent", required=True)
    probe.add_argument("--day", required=True)

    args = parser.parse_args(argv)

    if args.cmd == "index":
        from .corpus import build_index

        build_index()
    elif args.cmd == "bars":
        from .bars import write_bars

        write_bars(args.parent, args.first, args.last)
    elif args.cmd == "extract":
        from .frontmonth import extract

        extract(args.parent, args.first, args.last, args.workers)
    elif args.cmd == "profile":
        from .profile import profile_gen, profile_real

        if args.label == "real":
            profile_real(args.parent)
        else:
            if not args.csv:
                parser.error("a candidate profile needs --csv")
            profile_gen(args.parent, args.label, args.csv)
    elif args.cmd == "battery":
        from .battery import run_battery

        run_battery(args.parent, args.candidate)
    elif args.cmd == "envelope-chart":
        from .envelope_chart import render
        from .profile import load_profile

        _, real = load_profile(args.parent, "real")
        candidates = [
            (label, load_profile(args.parent, label)[1])
            for label in args.candidate
        ]
        title = args.title or f"{args.parent} activity envelope"
        render(real, candidates, args.out, title)
    elif args.cmd == "envelope-toml":
        from .envelope_toml import write_envelope_toml

        write_envelope_toml(args.parent, args.corpus, args.window)
    elif args.cmd == "status-probe":
        from .status_probe import status_probe

        status_probe(args.parent, args.day)
