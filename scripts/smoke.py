#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 folknor
# SPDX-License-Identifier: AGPL-3.0-only

"""Reference implementation of the mogwai launcher contract, plus a smoke run.

This script IS the contract from docs/cli.md, executed:

  1. Spawn `mogwai serve --config <path>` as a DIRECT child, capturing both
     stdout and stderr. There is no endpoint flag and no fd to nominate: the
     venue always binds an ephemeral loopback port and reports it on stdout.
  2. Read one line of stdout. Stdout closing without a line means the venue
     failed to boot; the child's stderr and exit status say why.
  3. Parse the ReadyRecord, checking `version` FIRST. Use `addr` as the
     endpoint.
  4. Run. On RunComplete the child exits 0 on its own; otherwise SIGTERM it.

Step 1's "direct child" is load-bearing and not stylistic: the venue installs
PR_SET_PDEATHSIG against its IMMEDIATE parent, so a wrapper process between the
launcher and the venue (a shell, `cargo run`, a double fork) wires the death
watch to the wrapper and re-creates the orphaned-venue defect. This script
therefore builds the binary first and then execs the binary itself.

Usage:

    python3 scripts/smoke.py [MODE] [--config PATH] [--duration DURATION]

MODE defaults to `default`. Every mode defaults `--config` to its own file, so
the bare mode word still works; `--config` overrides it.
"""

import argparse
import base64
import json
import os
import socket
import subprocess
import sys
import threading
import time
import tomllib
import traceback
import urllib.request
from decimal import ROUND_DOWN, ROUND_UP, Decimal

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# Readiness-record schema this launcher understands. Step 3 refuses anything
# else rather than reading fields that may have moved.
READY_VERSION = 6

# Each mode's default venue config, relative to scripts/. `None` means the
# venue's own built-in defaults.
MODE_CONFIGS = {
    "default": "smoke-default.toml",
    "heartbeat": "smoke-heartbeat.toml",
    "accelerated": "smoke-accelerated.toml",
    "admission": "smoke-admission.toml",
    "command-latency": "smoke-command-latency.toml",
    "band": "smoke-band.toml",
    "band-swept": "smoke-band-swept.toml",
    "stop": "smoke-stop.toml",
    "futures": "../crates/mogwai-cli/tests/configs/mnq.toml",
    "fees": "../crates/mogwai-cli/tests/configs/fees.toml",
}


# --------------------------------------------------------------------------
# Step 1-3: the launcher contract.
# --------------------------------------------------------------------------


def boot_key(config: str | None) -> str | None:
    """The config's own name for its boot river, unresolved.

    Either the top-level `symbol` key or, when that is absent, the
    `[instrument] preset` name. This is what the operator TYPED, which is not
    necessarily what the venue serves: preset lookup folds case.
    """
    if config is None:
        return None
    with open(config, "rb") as handle:
        doc = tomllib.load(handle)
    return doc.get("symbol") or doc.get("instrument", {}).get("preset")


def boot_symbol(config: str | None, served: list[str]) -> str:
    """The served spelling of this venue's boot river.

    The venue reports no symbol (ReadyRecord 6): it serves any river its config
    resolves. The served spelling is the venue's to state, so it comes from
    `/instruments`, and the config key is used only to SELECT which served
    entry is the boot river - case-insensitively, because preset lookup is.
    Re-implementing the venue's resolution in `tomllib` would be a second,
    drifting copy of it.
    """
    key = boot_key(config)
    if key is None:
        assert len(served) == 1, f"no boot key, and the venue serves {served}"
        return served[0]
    matches = [entry for entry in served if entry.lower() == key.lower()]
    assert len(matches) == 1, f"config boot key {key} matches {matches} of {served}"
    return matches[0]


def venue_binary() -> str:
    """Build the venue, then return the binary's OWN path.

    The build is not optional and is not skipped when a binary already exists: a
    smoke run against a stale binary asserts things about code that is not in
    the tree, which is worse than no smoke run at all.
    """
    candidates = [
        os.path.join(REPO, "target", profile, "mogwai") for profile in ("release", "debug")
    ]
    subprocess.run(
        ["brokkr", "run", "mogwai", "--", "--version"],
        cwd=REPO,
        check=True,
        stdout=subprocess.DEVNULL,
    )
    for path in candidates:
        if os.path.exists(path):
            return path
    raise AssertionError(f"no mogwai binary at any of {candidates}")


class Venue:
    """One venue process and the record it reported."""

    def __init__(self, config: str | None, duration: str | None) -> None:
        command = [venue_binary(), "serve"]
        if config:
            command.extend(["--config", config])
        if duration:
            command.extend(["--duration", duration])
        # The readiness record is one JSON line on STDOUT, so capturing it needs
        # no inherited pipe and no fd bookkeeping - the venue cannot be told the
        # wrong number because it is never told a number.
        self.child = subprocess.Popen(
            command,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        # A launcher that CAPTURES the child's stderr must also DRAIN it. The
        # venue logs to stderr by design (decision 5), a pipe holds only about
        # 64 KiB, and a full pipe blocks the writer - so an undrained capture
        # wedges the whole venue mid-run, which reads exactly like a hung
        # venue. Drain it on a thread from the moment of spawn, so the boot
        # failure below still has the whole log to report.
        self.stderr_lines: list[str] = []
        self._drain = threading.Thread(target=self._drain_stderr, daemon=True)
        self._drain.start()
        assert self.child.stdout is not None
        line = self.child.stdout.readline()
        if not line:
            self.child.wait(timeout=10)
            self._drain.join(timeout=5)
            raise AssertionError(
                f"venue failed before readiness: {''.join(self.stderr_lines)}"
            )
        record = json.loads(line)
        if record["version"] != READY_VERSION:
            raise AssertionError(f"unsupported readiness version: {record['version']}")
        self.record = record
        self.addr = record["addr"]
        # The record names no river, so the boot symbol is resolved here, once,
        # from the venue's own instrument list plus this config's boot key.
        instruments = self.http("/instruments")
        served = [entry["symbol"] for entry in instruments]
        self.symbol = boot_symbol(config, served)

    def _drain_stderr(self) -> None:
        if self.child.stderr is None:
            return
        for line in self.child.stderr:
            self.stderr_lines.append(line)

    def http(self, path: str, timeout: float = 10.0) -> object:
        with urllib.request.urlopen(f"http://{self.addr}{path}", timeout=timeout) as resp:
            body = resp.read()
        return json.loads(body) if body else None

    def post(self, path: str, payload: dict, timeout: float = 10.0) -> int:
        request = urllib.request.Request(
            f"http://{self.addr}{path}",
            data=json.dumps(payload).encode(),
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        try:
            with urllib.request.urlopen(request, timeout=timeout) as resp:
                return resp.status
        except urllib.error.HTTPError as err:
            return err.code

    def stop(self) -> int:
        if self.child.poll() is None:
            self.child.terminate()
        status = self.child.wait(timeout=15)
        self._drain.join(timeout=5)
        return status

    def __enter__(self) -> "Venue":
        return self

    def __exit__(self, *_exc: object) -> None:
        self.stop()


# --------------------------------------------------------------------------
# A minimal websocket client. Hand-rolled so the smoke has no dependency
# outside the standard library, which is what lets it be the reference the
# launcher contract points at.
# --------------------------------------------------------------------------


class WsClient:
    def __init__(self, addr: str, symbol: str, timeout: float = 30.0) -> None:
        host, port = addr.rsplit(":", 1)
        self.sock = socket.create_connection((host, int(port)), timeout=timeout)
        self.sock.settimeout(timeout)
        key = base64.b64encode(os.urandom(16)).decode()
        handshake = (
            f"GET /ws?symbol={symbol} HTTP/1.1\r\nHost: {addr}\r\nUpgrade: websocket\r\n"
            f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\n"
            "Sec-WebSocket-Version: 13\r\n\r\n"
        )
        self.sock.sendall(handshake.encode())
        self.buffer = b""
        while b"\r\n\r\n" not in self.buffer:
            chunk = self.sock.recv(4096)
            if not chunk:
                raise AssertionError("the venue closed during the websocket handshake")
            self.buffer += chunk
        head, self.buffer = self.buffer.split(b"\r\n\r\n", 1)
        if b"101" not in head.split(b"\r\n")[0]:
            raise AssertionError(f"websocket upgrade refused: {head!r}")
        self.closed_code: int | None = None

    def send(self, obj: dict) -> None:
        payload = json.dumps(obj).encode()
        mask = os.urandom(4)
        masked = bytes(byte ^ mask[i % 4] for i, byte in enumerate(payload))
        header = bytearray([0x81])
        length = len(payload)
        if length < 126:
            header.append(0x80 | length)
        elif length < 1 << 16:
            header.append(0x80 | 126)
            header += length.to_bytes(2, "big")
        else:
            header.append(0x80 | 127)
            header += length.to_bytes(8, "big")
        self.sock.sendall(bytes(header) + mask + masked)

    def _fill(self, count: int) -> bytes:
        while len(self.buffer) < count:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise TimeoutError("the venue closed the socket")
            self.buffer += chunk
        head, self.buffer = self.buffer[:count], self.buffer[count:]
        return head

    def recv(self, timeout: float = 30.0) -> dict | None:
        """Next application frame, or None once the peer closes."""
        deadline = time.monotonic() + timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError("no frame arrived within the deadline")
            self.sock.settimeout(remaining)
            try:
                first, second = self._fill(2)
            except TimeoutError:
                return None
            opcode = first & 0x0F
            length = second & 0x7F
            if length == 126:
                length = int.from_bytes(self._fill(2), "big")
            elif length == 127:
                length = int.from_bytes(self._fill(8), "big")
            payload = self._fill(length) if length else b""
            if opcode == 0x8:
                self.closed_code = int.from_bytes(payload[:2], "big") if payload else None
                return None
            if opcode == 0x9:
                continue
            if opcode == 0x1:
                return json.loads(payload)

    def until(self, predicate, timeout: float = 30.0) -> dict | None:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            frame = self.recv(timeout=max(0.1, deadline - time.monotonic()))
            if frame is None:
                return None
            if predicate(frame):
                return frame
        return None

    def close(self) -> None:
        try:
            self.sock.close()
        except OSError:
            pass


def order(client_order_id: str, symbol: str, **overrides: object) -> dict:
    payload = {
        "type": "SubmitOrder",
        "client_order_id": client_order_id,
        "symbol": symbol,
        "side": "Buy",
        "order_type": "Market",
        "quantity": "0.001",
        "time_in_force": "Gtc",
    }
    payload.update(overrides)
    return payload


# --------------------------------------------------------------------------
# The shared surface every mode asserts first.
# --------------------------------------------------------------------------


def check_common(venue: Venue) -> None:
    """One run, one instrument, one reachable endpoint, one servable warmup."""
    with urllib.request.urlopen(f"http://{venue.addr}/health", timeout=10) as resp:
        body = json.loads(resp.read())
        assert body["status"] == "ok", f"/health answered {body!r} instead of ok"
        assert body["oms_type"] in ("netting", "hedging")

    instruments = venue.http("/instruments")
    served = [entry["symbol"] for entry in instruments]
    assert venue.symbol in served, (
        f"the venue serves {served}, the boot river is {venue.symbol}"
    )

    clock = venue.http("/clock")
    assert clock["data_origin_ns"] == venue.record["data_origin_ns"], (
        f"/clock reports data_origin_ns {clock['data_origin_ns']}, the readiness "
        f"record reports {venue.record['data_origin_ns']}"
    )
    assert clock["warmup_ns"] == venue.record["warmup_ns"], (
        "the clock's warmup horizon and the readiness record's must agree"
    )
    assert (
        venue.record["run_start_ns"] - venue.record["warmup_ns"]
        == venue.record["data_origin_ns"]
    ), "data_origin_ns = run_start_ns - warmup_ns"

    # The declared warmup is MATERIALIZED, not merely declared: the earliest
    # servable instant answers with trades the moment readiness lands.
    floor = venue.record["data_origin_ns"]
    warm = venue.http(f"/trades?symbol={venue.symbol}&start={floor}&limit=20")
    assert warm, "the earliest servable instant returned no trades"

    unconfigured = venue.http(
        f"/trades?symbol=NOT-A-SYMBOL&start={floor}&limit=5"
    )
    assert unconfigured, "an unconfigured symbol returned no synthesized history"
    advertised = venue.http("/instruments")
    assert any(row["symbol"] == "NOT-A-SYMBOL" for row in advertised), (
        "a history-materialized symbol was not advertised by /instruments"
    )
    unconfigured_ws = WsClient(venue.addr, "NOT-A-SYMBOL")
    try:
        frame = unconfigured_ws.until(
            lambda candidate: candidate.get("type") in ("Trade", "Quote")
        )
        assert frame and frame["symbol"] == "NOT-A-SYMBOL", (
            f"unconfigured socket delivered the wrong label: {frame}"
        )
    finally:
        unconfigured_ws.close()

    # And the two boundaries are refused by name rather than served short.
    for start, label in (
        (floor - 1, "before the floor"),
        (venue.record["run_start_ns"] + 86_400_000_000_000, "after sim-now"),
    ):
        try:
            venue.http(f"/trades?symbol={venue.symbol}&start={start}")
            raise AssertionError(f"a window {label} was served instead of refused")
        except urllib.error.HTTPError as err:
            assert err.code == 400, f"a window {label} must be a 400, got {err.code}"


def check_seed_logs(venue: Venue) -> None:
    """The run root and each materialized river are independently logged."""
    seed_lines = [line for line in venue.stderr_lines if "run seeds fixed" in line]
    assert len(seed_lines) == 1, f"expected one run-seed line, got {seed_lines}"
    assert "run_seed" in seed_lines[0] and "fill_seed" in seed_lines[0], seed_lines[0]
    assert "tape_seed" not in seed_lines[0], seed_lines[0]

    river_lines = [line for line in venue.stderr_lines if "river materialized" in line]
    assert len(river_lines) >= 2, f"expected boot and history rivers, got {river_lines}"
    assert any(venue.symbol in line and "tape_seed" in line for line in river_lines), river_lines
    assert any("NOT-A-SYMBOL" in line and "tape_seed" in line for line in river_lines), river_lines


# --------------------------------------------------------------------------
# Modes.
# --------------------------------------------------------------------------


def mode_default(venue: Venue) -> str:
    ws = WsClient(venue.addr, venue.symbol)
    try:
        # No Subscribe frame is sent, and none exists to send: the venue pushes
        # the run's one tape on upgrade.
        #
        # DRAINED, not asserted on the NEXT frame. This used to require the very
        # first market frame to be the BBO snapshot and flaked on it twice in one
        # day. Snapshot-first is NOT a wire contract and the server never claimed
        # it was: `Tape::subscribe_with_snapshot` returns an OPTION, and a boat
        # that has not yet published a quote has no BBO to hand over, so a socket
        # binding in the instant between the tape's first trade and its first
        # quote legitimately sees the trade first. There is no stale-BBO hole
        # behind that - the snapshot is absent only when no quote exists yet, and
        # the tape's own first quote follows immediately. What the venue does
        # promise, and what is worth smoking, is that a bound socket receives a
        # well-formed two-sided quote unbidden.
        quote = ws.until(lambda frame: frame.get("type") == "Quote")
        assert quote, "the venue pushed no BBO quote to a bound socket"
        assert quote["symbol"] == venue.symbol
        assert Decimal(quote["bid_px"]) < Decimal(quote["ask_px"])
        assert Decimal(quote["bid_sz"]) > 0 and Decimal(quote["ask_sz"]) > 0
        trade = ws.until(lambda frame: frame.get("type") == "Trade")
        assert trade, "the venue did not push its tape unbidden"
        assert trade["symbol"] == venue.symbol, (
            f"the pushed tape carries {trade['symbol']}, the run serves {venue.symbol}"
        )

        ws.send(order("SMOKE-1", venue.symbol))
        accepted = ws.until(
            lambda frame: frame.get("type")
            in ("OrderAccepted", "OrderFilled", "OrderRejected")
        )
        assert accepted, "the order socket answered nothing"
        assert accepted["type"] != "OrderRejected", accepted

        # Market slippage, observed end to end: the fill price against the tape
        # the venue itself read when it decided the order. Adverse or equal,
        # never better - equality is admitted because u = 0 is a legitimate
        # draw, and this is the only gate in the suite that watches a fill PRICE.
        #
        # The reading is memoized on the fill-sweep-interval bucket (100 ms by
        # default - see MarketReadingCache) and is taken when the submit reaches
        # the handler, not at the fill instant, so the print the venue actually
        # read cannot be named from outside - only bracketed by the lookback
        # below. The fill band is adverse, so the surviving statement is
        # directional: a market buy fills at or above the LOWEST print in that
        # bracket, a market sell at or below the highest. A fill decided off no
        # reading at all breaks it; a fill decided off a slightly stale reading
        # does not.
        bucket_ns = 100_000_000

        def slippage_is_adverse(client_order_id: str, side: str) -> None:
            ws.send(order(client_order_id, venue.symbol, side=side))
            fill = ws.until(
                lambda frame: frame.get("type") in ("OrderFilled", "OrderRejected")
                and frame.get("client_order_id") == client_order_id
            )
            assert fill and fill["type"] == "OrderFilled", fill
            ts = int(fill["ts_event"])
            reading_ts = ts // bucket_ns * bucket_ns
            tape = venue.http(
                f"/trades?symbol={venue.symbol}&start={reading_ts - 300_000_000_000}"
                f"&end={reading_ts}&limit=50000"
            )
            assert tape, f"no tape reading at the {side} fill"
            prices = [float(trade["price"]) for trade in tape]
            got = float(fill["last_px"])
            if side == "Buy":
                floor = min(prices)
                assert got >= floor, (
                    f"a market buy filled below every print it could have read: "
                    f"{got} < {floor}"
                )
            else:
                ceiling = max(prices)
                assert got <= ceiling, (
                    f"a market sell filled above every print it could have read: "
                    f"{got} > {ceiling}"
                )

        slippage_is_adverse("SMOKE-SLIP-BUY", "Buy")
        slippage_is_adverse("SMOKE-SLIP-SELL", "Sell")

        # The one ledger answers for the order the same socket just worked.
        ws.send({"type": "QueryOrders", "request_id": "SMOKE-Q", "open_only": False})
        snapshot = ws.until(lambda frame: frame.get("type") == "OrderStatusSnapshot")
        assert snapshot, "the venue-truth query went unanswered"
        assert any(row["client_order_id"] == "SMOKE-1" for row in snapshot["orders"]), (
            f"SMOKE-1 is absent from the venue-truth snapshot: {snapshot['orders']}"
        )

        account = venue.http("/account")
        assert account["balances"], "the run's one ledger reports its funding"
        return "tape pushed unbidden, order worked, one ledger answered"
    finally:
        ws.close()


def mode_futures(venue: Venue) -> str:
    ws = WsClient(venue.addr, venue.symbol)
    try:
        trade = ws.until(lambda frame: frame.get("type") == "Trade")
        assert trade and Decimal(trade["size"]) >= 1
        assert Decimal(trade["size"]) == Decimal(trade["size"]).to_integral_value()
        ws.send(order("FUTURES-1", venue.symbol, quantity="1"))
        fill = ws.until(
            lambda frame: frame.get("type") in ("OrderFilled", "OrderRejected")
            and frame.get("client_order_id") == "FUTURES-1"
        )
        assert fill and fill["type"] == "OrderFilled", fill
        account = ws.until(
            lambda frame: frame.get("type") == "AccountState"
            and frame.get("positions")
        )
        assert account and account.get("margins"), account
        assert account["margins"][0]["currency"] == "USD"
        return "whole-contract tape, futures fill, and posted margin worked"
    finally:
        ws.close()


def mode_fees(venue: Venue) -> str:
    ws = WsClient(venue.addr, venue.symbol)
    try:
        assert ws.until(lambda frame: frame.get("type") == "Trade")
        ws.send(order("FEES-1", venue.symbol, quantity="1"))
        fill = ws.until(
            lambda frame: frame.get("type") in ("OrderFilled", "OrderRejected")
            and frame.get("client_order_id") == "FEES-1"
        )
        assert fill and fill["type"] == "OrderFilled", fill
        assert Decimal(fill["commission"]) > 0, fill
        assert fill["commission_currency"] == "USD"
        assert fill["liquidity_side"] == "taker"
        return "non-zero taker commission reached the live wire"
    finally:
        ws.close()


def mode_heartbeat(venue: Venue) -> str:
    ws = WsClient(venue.addr, venue.symbol)
    try:
        beats = 0
        deadline = time.monotonic() + 20
        while beats < 2 and time.monotonic() < deadline:
            frame = ws.recv(timeout=10)
            if frame is None:
                break
            if frame.get("type") == "Heartbeat":
                beats += 1
        assert beats >= 2, f"expected server heartbeats, saw {beats}"
    finally:
        ws.close()
    return f"{beats} server heartbeats on an otherwise idle socket"


def mode_accelerated(venue: Venue) -> str:
    clock = venue.http("/clock")
    assert clock["sim"]["speed"] > 1.0, f"the accelerated venue reports {clock['sim']}"

    ws = WsClient(venue.addr, venue.symbol)
    try:
        started = time.monotonic()
        first = ws.until(lambda frame: frame.get("type") == "Trade")
        assert first, "no tape on the accelerated venue"
        last = first
        seen = 1
        while time.monotonic() - started < 5:
            frame = ws.recv(timeout=5)
            if frame is None:
                break
            if frame.get("type") == "Trade":
                last = frame
                seen += 1
        wall = time.monotonic() - started
        sim_span = last["ts_event"] - first["ts_event"]
        assert seen > 1, "the accelerated tape produced a single trade"
        assert sim_span > wall * 1e9, (
            f"the tape covered {sim_span} sim ns in {wall:.1f} wall s; "
            "acceleration is not reaching the feed"
        )
    finally:
        ws.close()
    return f"{seen} trades spanning {sim_span / 1e9:.0f} sim s in {wall:.1f} wall s"


def mode_admission(venue: Venue) -> str:
    ws = WsClient(venue.addr, venue.symbol)
    try:
        # A shrunk held-lane budget puts the refusal a few orders away instead
        # of twelve thousand. What is under test is the REFUSAL, which is the
        # shipped branch either way.
        for index in range(400):
            ws.send(order(f"ADMIT-{index}", venue.symbol))
        refusal = ws.until(
            lambda frame: frame.get("type") == "AdmissionRejected", timeout=30
        )
        assert refusal, "the venue never refused for capacity under a shrunk budget"
        assert refusal["subject"], "a refusal names what it refused"
        assert "reason" in refusal, f"a refusal states a reason: {refusal}"
    finally:
        ws.close()
    return f"venue refused for capacity: {refusal['reason']}"


def mode_command_latency(venue: Venue) -> str:
    armed = venue.post(
        "/control/divergence",
        {
            "type": "CommandLatency",
            "submit_act_ms": 400,
            "modify_act_ms": 0,
            "cancel_act_ms": 0,
            "submit_ack_ms": 0,
            "modify_ack_ms": 0,
            "cancel_ack_ms": 0,
        },
    )
    assert armed == 202, f"arming refused with {armed}"

    ws = WsClient(venue.addr, venue.symbol)
    try:
        started = time.monotonic()
        ws.send(order("LATENCY-1", venue.symbol))
        answer = ws.until(
            lambda frame: frame.get("type")
            in ("OrderAccepted", "OrderFilled", "OrderRejected", "AdmissionRejected")
        )
        elapsed = time.monotonic() - started
        assert answer, "the delayed command was never answered"
        assert elapsed >= 0.35, (
            f"an armed 400 ms act delay produced an answer in {elapsed * 1000:.0f} ms"
        )
    finally:
        ws.close()
    return f"armed act delay realized as {elapsed * 1000:.0f} ms"


def mode_band(venue: Venue) -> str:
    # A resting limit the market has to come to: it fills only once a trade
    # prints strictly through its price.
    sim_now = int(venue.http("/clock")["server_now_ns"])
    anchor = venue.http(
        f"/trades?symbol={venue.symbol}&start={sim_now - 300_000_000_000}&end={sim_now}&limit=10000"
    )
    assert anchor, "no anchor print"
    price = float(anchor[-1]["price"])

    ws = WsClient(venue.addr, venue.symbol)
    try:
        ws.send(
            order(
                "BAND-1",
                venue.symbol,
                order_type="Limit",
                side="Buy",
                price=f"{price:.2f}",
            )
        )
        accepted = ws.until(
            lambda frame: frame.get("type") in ("OrderAccepted", "OrderRejected")
        )
        assert accepted, "the banded limit was never answered"
        assert accepted["type"] == "OrderAccepted", accepted
        # It must REST rather than fill on submit. Priced AT the last print, no
        # draw can put its trigger above the market, and u = 0 still needs a
        # print strictly THROUGH the price, which the submit instant by
        # definition does not have. The `OrderAccepted` just asserted IS that
        # property: had the submit path filled, the answer to the submit would
        # have been an `OrderFilled`.
        #
        # The venue-truth query that follows is a SECOND round trip, and it
        # RACES the sweep - which is the point of the widened acceptance below
        # rather than a flake being tolerated. The sweep runs every 50 ms under
        # smoke-band.toml, the raw-fill tape prints tens of times a second, and
        # the calibrated `fill_band_vol_mult = 0.005` displaces the trigger by
        # only a few ticks (about 0.1 basis points), so the print that fills
        # this order routinely lands before the query is answered. The former
        # unconditional "still open" assertion only held because the old `0.5`
        # multiplier was clamp-saturated at 200 ticks and took long enough to
        # cross that the query always won; it encoded the mis-calibration, not
        # the model.
        #
        # So the query asserts the DISJUNCTION that is actually true of a
        # correctly resting order: it is either still open, or it left the book
        # by being filled UNSOLICITED by the run's sweep. What it still rules
        # out is the order vanishing - filled on the submit path, rejected after
        # acceptance, or silently dropped.
        ws.send({"type": "QueryOrders", "request_id": "BAND-Q", "open_only": True})
        outcome = ws.until(
            lambda frame: frame.get("type") == "OrderStatusSnapshot"
            or (
                frame.get("type") == "OrderFilled"
                and frame.get("client_order_id") == "BAND-1"
            )
        )
        assert outcome, "the venue-truth query went unanswered"
        if outcome["type"] == "OrderFilled":
            return "banded limit rested, then the run's sweep filled it unsolicited"
        resting = [row for row in outcome["orders"] if row["client_order_id"] == "BAND-1"]
        assert resting, (
            "a banded limit must REST, not fill on submit: BAND-1 is neither open "
            "nor reported filled"
        )
    finally:
        ws.close()
    return "banded limit rested rather than filling on submit"


def mode_band_swept(venue: Venue) -> str:
    """The fill nobody asked for: the run's sweep, not a command response.

    The order is placed at the latest print, so it is not strictly through its
    trigger on arrival. A later clean print can cross it and the resulting fill
    arrives unsolicited on the open socket.

    A downward-drifting tape is what makes this fill, so the wall bound is
    generous and the submit is retried at a fresh anchor price rather than
    asserting that one draw of the generator must cooperate.
    """
    ws = WsClient(venue.addr, venue.symbol)
    try:
        for attempt in range(3):
            client_order_id = f"BAND-SWEPT-{attempt}"
            sim_now = int(venue.http("/clock")["server_now_ns"])
            anchor = venue.http(
                f"/trades?symbol={venue.symbol}&start={sim_now - 300_000_000_000}&end={sim_now}&limit=10000"
            )
            assert anchor, "no anchor print"
            price = float(anchor[-1]["price"])
            ws.send(
                order(
                    client_order_id,
                    venue.symbol,
                    order_type="Limit",
                    side="Buy",
                    price=f"{price:.2f}",
                )
            )
            accepted = ws.until(
                lambda frame: frame.get("type") in ("OrderAccepted", "OrderRejected")
                and frame.get("client_order_id") == client_order_id
            )
            assert accepted and accepted["type"] == "OrderAccepted", accepted
            # The half the old 1.5x-through construction could not make: it did
            # NOT fill on the submit path, so whatever fills it later is the
            # venue's own sweep and nothing else. The `OrderAccepted` above is
            # what establishes that; the venue-truth query below only corroborates
            # it, and it cannot be required to find the order STILL open.
            #
            # This config runs at speed 100 with a 50 ms sweep, so a simulated
            # five seconds elapse per wall second, and the calibrated
            # `fill_band_vol_mult = 0.005` puts the trigger a few ticks from the
            # stated price. The sweep therefore routinely fills this order before
            # the query round trip completes. Requiring an open row encoded the
            # old clamp-saturated `0.5` band, under which crossing 200 ticks took
            # long enough that the query always won the race.
            #
            # So an already-delivered fill SATISFIES this mode rather than
            # failing it: the point of the mode is that the fill arrives
            # unsolicited from the run, and a fill that beat the query is still a
            # fill the client never asked for.
            ws.send({"type": "QueryOrders", "request_id": "BAND-SWEPT-Q", "open_only": True})
            fill = ws.until(
                lambda frame: (
                    frame.get("type") == "OrderFilled"
                    and frame.get("client_order_id") == client_order_id
                )
                or frame.get("type") == "OrderStatusSnapshot",
                timeout=90,
            )
            if fill is not None and fill.get("type") == "OrderStatusSnapshot":
                assert any(
                    row["client_order_id"] == client_order_id for row in fill["orders"]
                ), "the band-swept order filled on submit instead of resting"
                # Bounded by the accelerated tape's dwell allowance, not by the
                # sweep timer: the fill waits on the next PRINT.
                fill = ws.until(
                    lambda frame: frame.get("type") == "OrderFilled"
                    and frame.get("client_order_id") == client_order_id,
                    timeout=90,
                )
            if fill:
                break
        assert fill, "the run's sweep never filled a limit the tape traded through"
    finally:
        ws.close()
    return "the run sweep delivered an unsolicited fill for a banded resting limit"


def mode_stop(venue: Venue) -> str:
    """A stop-market rests, the tape triggers it, and it fills adversely.

    Three claims, each read off a fact the VENUE states rather than off a
    wall-clock guess or a hope about which way the tape drew:

    - RESTED. Not "the submit answered OrderAccepted": a conditional whose
      trigger is already touched by the acceptance-instant market reading is
      answered `OrderAccepted` too, with its trigger and fill following in the
      same batch. What separates the two is the venue's own record -
      `ts_triggered == ts_accepted` is the acceptance-instant hit, while
      `ts_triggered > ts_accepted` can only be a later sweep pass over the
      canonical tape. The comparison is exact, so it does not care which path
      the generator drew.
    - TRIGGERED BY THE TAPE. The `OrderTriggered` event, waited on as a
      CONDITION and with nothing interposed that could race it.
    - FILLED ADVERSELY. At or below the trigger for a sell, at or above it for
      a buy. Never the client's own stop price, which is the lie the fill band
      exists to remove.

    Two things make the outcome independent of the drawn seed.

    The OFFSET is sized from the tape's own observed recent movement rather than
    fixed at one tick. One tick below the last print sits inside the staleness
    the acceptance reading itself carries - that reading is memoized on the
    fill-sweep bucket, which at this config's speed 100 is several simulated
    seconds of tape - so a one-tick stop is touched on acceptance about half the
    time and simply never rests.

    The DIRECTION is not chosen. A single stop below the market is a bet that
    the drawn path falls, and the fitted generator produces plenty of paths that
    trend the other way for minutes on end; that bet is exactly what a
    seed-independent gate cannot contain. So both sides are armed at once - a
    protective reduce-only sell stop below the position, and a buy stop above -
    and whichever one the tape reaches proves the three claims. The tape then
    has no way to slip the gate: it must move by the offset in SOME direction,
    and over a window a hundred times longer than the one the offset was
    measured from, staying inside that band is not something a path does.
    """
    sim_now = int(venue.http("/clock")["server_now_ns"])
    recent = venue.http(
        f"/trades?symbol={venue.symbol}&start={sim_now - 30_000_000_000}&end={sim_now}&limit=10000"
    )
    assert recent, "no recent tape to size the stop offset from"
    prices = [Decimal(row["price"]) for row in recent]
    # A floor keeps the offset meaningful if the sampled window happens to be
    # flat; the observed range is what carries it in every ordinary case.
    offset = max(2 * (max(prices) - min(prices)), Decimal("0.10"))

    ws = WsClient(venue.addr, venue.symbol)
    try:
        # The loop exists for ONE residual case: a stop already through its
        # trigger at the acceptance instant proves triggering and adverse
        # filling but not resting, and so is not accepted as a pass. Each
        # attempt re-establishes its own position, because the sell stop is
        # reduce-only and an attempt that spent the position would leave the
        # next one holding a stop that can only be canceled for want of an
        # exposure to close. The old loop reused a single position and so turned
        # every retry into a guaranteed non-fill.
        for attempt in range(3):
            position_id = f"STOP-POSITION-{attempt}"
            ws.send(order(position_id, venue.symbol))
            position_fill = ws.until(
                lambda frame: frame.get("type") == "OrderFilled"
                and frame.get("client_order_id") == position_id
            )
            assert position_fill, (
                "could not establish the position the protective stop reduces"
            )

            sim_now = int(venue.http("/clock")["server_now_ns"])
            anchor = venue.http(
                f"/trades?symbol={venue.symbol}&start={sim_now - 300_000_000_000}&end={sim_now}&limit=10000"
            )
            assert anchor, "no anchor print"
            # Decimal, not float, and the SENT string is what is asserted
            # against. The venue echoes back exactly the number it was given, so
            # comparing the echo to an unrounded binary float that was rounded
            # on its way out fails for every anchor whose cent arithmetic is not
            # exact in base two - about a third of them, which is what made this
            # mode fail intermittently rather than never.
            last = Decimal(anchor[-1]["price"])
            triggers = {
                f"STOP-DOWN-{attempt}": (
                    "Sell",
                    True,
                    (last - offset).quantize(Decimal("0.01"), rounding=ROUND_DOWN),
                ),
                f"STOP-UP-{attempt}": (
                    "Buy",
                    False,
                    (last + offset).quantize(Decimal("0.01"), rounding=ROUND_UP),
                ),
            }
            for order_id, (side, reduce_only, trigger) in triggers.items():
                ws.send(
                    order(
                        order_id,
                        venue.symbol,
                        order_type="StopMarket",
                        side=side,
                        trigger_price=f"{trigger}",
                        reduce_only=reduce_only,
                    )
                )
                accepted = ws.until(
                    lambda frame, want=order_id: frame.get("type")
                    in ("OrderAccepted", "OrderRejected")
                    and frame.get("client_order_id") == want
                )
                assert accepted and accepted["type"] == "OrderAccepted", accepted

            # Nothing is interposed between here and the two events this mode
            # exists to observe. A venue-truth query issued at this point is a
            # second round trip that RACES the sweep, and `until` discards every
            # frame it passes over - so a stop that triggered and filled before
            # the snapshot came back had its `OrderTriggered` and `OrderFilled`
            # thrown away by the very read meant to corroborate them. That is
            # what made this mode fail about half the time. The query is now
            # asked AFTER the events, where it cannot consume them, and asked
            # about ONE id, which the venue answers whatever its status.
            triggered = ws.until(
                lambda frame: frame.get("type") == "OrderTriggered"
                and frame.get("client_order_id") in triggers,
                timeout=120,
            )
            if not triggered:
                continue
            client_order_id = triggered["client_order_id"]
            side, _reduce_only, trigger = triggers[client_order_id]
            wire_trigger = f"{trigger}"
            fill = ws.until(
                lambda frame: frame.get("type") == "OrderFilled"
                and frame.get("client_order_id") == client_order_id
            )
            assert fill, "a triggered stop did not fill"
            # The fill is priced off the print that TRIGGERED it, slipped
            # adversely - down for a sell, up for a buy. Never the stop price
            # itself, which is the client's own number and the lie the fill
            # band exists to remove.
            filled_px = Decimal(fill["last_px"])
            if side == "Sell":
                assert filled_px <= trigger, (
                    f"a sell stop must fill at or below its trigger "
                    f"{wire_trigger}, got {fill['last_px']}"
                )
            else:
                assert filled_px >= trigger, (
                    f"a buy stop must fill at or above its trigger "
                    f"{wire_trigger}, got {fill['last_px']}"
                )

            ws.send(
                {
                    "type": "QueryOrders",
                    "request_id": f"STOP-Q-{attempt}",
                    "client_order_id": client_order_id,
                }
            )
            snapshot = ws.until(lambda frame: frame.get("type") == "OrderStatusSnapshot")
            assert snapshot, "the venue-truth query went unanswered"
            row = next(
                (
                    row
                    for row in snapshot["orders"]
                    if row["client_order_id"] == client_order_id
                ),
                None,
            )
            assert row, f"the venue has no record of {client_order_id}"
            assert Decimal(row["trigger_price"]) == trigger, (
                f"the venue echoed trigger_price {row['trigger_price']} for an "
                f"order submitted at {wire_trigger}"
            )
            assert row.get("ts_triggered") is not None, (
                f"the venue delivered OrderTriggered but records no ts_triggered: {row}"
            )
            assert row["ts_triggered"] >= row["ts_accepted"], (
                f"a stop cannot trigger before it was accepted: {row}"
            )
            if row["ts_triggered"] == row["ts_accepted"]:
                # Already through its trigger at the acceptance instant, so this
                # attempt never rested. It proves the other two claims, and is
                # still not accepted as a pass.
                continue
            rested_s = (row["ts_triggered"] - row["ts_accepted"]) / 1e9
            return (
                f"stop-market rested {rested_s:.1f} sim s at a trigger {offset} "
                f"{'below' if side == 'Sell' else 'above'} the tape, then "
                f"triggered and filled adversely at {fill['last_px']}"
            )
        raise AssertionError("the run sweep never triggered a rested stop-market")
    finally:
        ws.close()


def mode_duration(venue: Venue, duration: str) -> str:
    """The declared-duration path: announced completion, then exit 0."""
    ws = WsClient(venue.addr, venue.symbol)
    try:
        completion = ws.until(
            lambda frame: frame.get("type") == "RunComplete", timeout=180
        )
        assert completion, "the run ended without announcing its completion"
        assert completion["elapsed_ns"] > 0, (
            f"a completed run reports positive elapsed time, got {completion['elapsed_ns']}"
        )
        # The socket is closed with WS 1000 right behind the announcement.
        ws.recv(timeout=10)
        assert ws.closed_code in (None, 1000), (
            f"a completed run closes with 1000, got {ws.closed_code}"
        )
    finally:
        ws.close()
    status = venue.child.wait(timeout=60)
    assert status == 0, f"a planned completion is exit 0, got {status}"
    return f"declared {duration} run announced completion and exited 0"


MODES = {
    "default": mode_default,
    "heartbeat": mode_heartbeat,
    "accelerated": mode_accelerated,
    "admission": mode_admission,
    "command-latency": mode_command_latency,
    "band": mode_band,
    "band-swept": mode_band_swept,
    "stop": mode_stop,
    "futures": mode_futures,
    "fees": mode_fees,
}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "mode",
        nargs="?",
        default="default",
        choices=sorted(MODES),
        help="which smoke to run (default: default)",
    )
    parser.add_argument(
        "--config",
        help="venue config forwarded to the spawned run; defaults to the mode's own file",
    )
    parser.add_argument(
        "--duration",
        help="declared sim duration forwarded to the venue, e.g. 30s",
    )
    parsed = parser.parse_args()

    config = parsed.config
    if config is None:
        default_config = MODE_CONFIGS[parsed.mode]
        if default_config:
            config = os.path.join(REPO, "scripts", default_config)

    with Venue(config, parsed.duration) as venue:
        check_common(venue)
        detail = MODES[parsed.mode](venue)
        if parsed.duration:
            detail = f"{detail}; {mode_duration(venue, parsed.duration)}"

    check_seed_logs(venue)

    print(f"PASS [{parsed.mode}] {venue.addr}: {detail}")


def failure_reason(err: BaseException) -> str:
    """A reason for EVERY failure, including a bare `assert x` with no message.

    A gate whose operator is an agent cannot afford `FAIL:` with nothing after
    it. `str(err)` is empty for an unannotated assertion and for several stdlib
    exceptions, so the location and the failing source line are appended
    unconditionally: those are the two facts that are always available and they
    are enough to name what broke.
    """
    frames = traceback.extract_tb(err.__traceback__)
    where = ""
    if frames:
        last = frames[-1]
        where = f"{os.path.basename(last.filename)}:{last.lineno}"
        if last.line:
            where = f"{where} {last.line}"
    message = str(err).strip()
    kind = type(err).__name__
    if message and where:
        return f"{kind}: {message} [at {where}]"
    if message:
        return f"{kind}: {message}"
    if where:
        return f"{kind} with no message, at {where}"
    return f"{kind} with no message and no traceback"


if __name__ == "__main__":
    try:
        main()
    except Exception as err:  # noqa: BLE001 - the gate reports, it does not raise
        print(f"FAIL: {failure_reason(err)}", file=sys.stderr)
        raise SystemExit(1) from err
