#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 folknor
# SPDX-License-Identifier: AGPL-3.0-only

"""End-to-end smoke test of the mogwai fake broker.

Arms divergences over the control plane, then submits orders over the native WS
gateway and checks the resulting execution events. Also exercises replay timing
so DelayAcks and GoDark are verified at the socket writer. Uses only the stdlib
so there's nothing to install.

Connects to a server at 127.0.0.1:8787; it does not start one. The windowing,
Unsubscribe-stop, gap-cap, DelayAcks, and GoDark steps need PACED replay against
the generated stream. The built-in default and the committed mogwai.toml both
set speed = 1.0, so a plain launch paces correctly (unthrottled speed 0.0 makes
those steps race and fail spuriously):

    brokkr run mogwai -- serve -f

then run this script (`mogwai` is the bin target name, not the package).

The default run keeps the server heartbeat off, so every step reads exactly
one frame per assertion without an interleaved Heartbeat. The StallData /
heartbeat (4255) reproduction lives behind `--heartbeat`, which runs only the
heartbeat step against a server started with server_heartbeat_ms enabled. The
`serve --config` flag is consumed by the server binary, not cargo, so it must
follow a `--` separator:

    brokkr run mogwai -- serve -f --config scripts/smoke-heartbeat.toml

then run `python3 scripts/smoke.py --heartbeat`.

Accelerated coherent-clock smoke:

    brokkr run mogwai -- serve -f --config scripts/smoke-accelerated.toml

then run `python3 scripts/smoke.py --accelerated`.

Admission control (a refusal under an armed DelayAcks) runs behind
`--admission`, against a server whose held-lane budget is shrunk so the venue
refuses after a dozen orders instead of twelve thousand:

    brokkr run mogwai -- serve -f --config scripts/smoke-admission.toml

then run `python3 scripts/smoke.py --admission`.

Outbound command latency runs against `scripts/smoke-command-latency.toml`:

    brokkr run mogwai -- serve -f --config scripts/smoke-command-latency.toml
    python3 scripts/smoke.py --command-latency
"""
import json
import socket
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

HOST, PORT = "127.0.0.1", 8787
ACCOUNT_HEADER = "x-mogwai-account"
SMOKE_ACCOUNT = "SMOKE-001"
# How far behind sim-now the windowed-subscribe / history probes anchor their
# start. The server now derives the tape origin from the clock at boot
# (data_origin = sim_now - backfill_horizon, 24h by default) and refuses a
# /trades start before that floor, so the window start can no longer be a frozen
# constant - it must sit inside [data_origin, sim_now]. One hour back is safely
# on-tape under the 24h horizon. main_default resolves the absolute start from
# the live clock at runtime.
WINDOW_LOOKBACK_NS = 3_600_000_000_000
ACCEL_DELAY_MS = 1000
# Slack for comparing client-side wall reads against server-stamped sim
# instants, expressed as WALL nanoseconds and projected onto the sim axis at
# the run's speed. Covers the client and server sampling the wall clock a few
# ms apart plus scheduling jitter; at speed 100 every wall ms is 100 sim-ms,
# so an unslacked window edge would flake.
ACCEL_CLOCK_SLACK_WALL_NS = 50_000_000
# How long the accelerated gate polls /trades for its anchor tick before
# declaring the tape broken. The fitted ACD duration process is heavy-tailed
# (dispersion band up to ~4600s), so right after boot the window
# [sim_epoch, sim_now] can legitimately stay empty for a while: a worst-band
# lull is ~46 wall-seconds at speed 100. Twice that plus margin.
ACCEL_ANCHOR_TIMEOUT_S = 120.0


def account_headers(account_id: str) -> dict:
    return {"content-type": "application/json", ACCOUNT_HEADER: account_id}


def post_divergence(payload: dict, account_id: str = SMOKE_ACCOUNT) -> int:
    req = urllib.request.Request(
        f"http://{HOST}:{PORT}/control/divergence",
        data=json.dumps(payload).encode(),
        headers=account_headers(account_id),
        method="POST",
    )
    try:
        with urllib.request.urlopen(req) as r:
            return r.status
    except urllib.error.HTTPError as e:
        return e.code


def post_order(payload: dict, account_id: str = SMOKE_ACCOUNT) -> list:
    req = urllib.request.Request(
        f"http://{HOST}:{PORT}/orders",
        data=json.dumps(payload).encode(),
        headers=account_headers(account_id),
        method="POST",
    )
    with urllib.request.urlopen(req) as r:
        assert r.status == 200, r.status
        return json.loads(r.read().decode())


def fetch_trades(symbol: str, start: int | None, limit: int, regime: dict | None = None) -> list:
    params = {"symbol": symbol, "limit": limit}
    if start is not None:
        params["start"] = start
    if regime is not None:
        params["regime"] = json.dumps(regime)
    url = f"http://{HOST}:{PORT}/trades?{urllib.parse.urlencode(params)}"
    with urllib.request.urlopen(url) as r:
        assert r.status == 200, r.status
        return json.loads(r.read().decode())


def fetch_clock() -> dict:
    with urllib.request.urlopen(f"http://{HOST}:{PORT}/clock") as r:
        assert r.status == 200, r.status
        return json.loads(r.read().decode())


def list_accounts() -> list:
    with urllib.request.urlopen(f"http://{HOST}:{PORT}/accounts") as r:
        assert r.status == 200, r.status
        return json.loads(r.read().decode())


def delete_account(account_id: str) -> int:
    req = urllib.request.Request(
        f"http://{HOST}:{PORT}/accounts/{urllib.parse.quote(account_id)}",
        method="DELETE",
    )
    try:
        with urllib.request.urlopen(req) as r:
            return r.status
    except urllib.error.HTTPError as e:
        return e.code


def fetch_account(account_id: str = SMOKE_ACCOUNT) -> dict:
    req = urllib.request.Request(
        f"http://{HOST}:{PORT}/account", headers={ACCOUNT_HEADER: account_id}
    )
    with urllib.request.urlopen(req) as r:
        assert r.status == 200, r.status
        return json.loads(r.read().decode())


def ws_roundtrip(send_obj: dict, expect: int) -> list:
    """Minimal RFC6455 client: handshake, send one text frame, read `expect` frames."""
    ws = WsClient()
    ws.send(send_obj)
    out = [ws.read() for _ in range(expect)]
    ws.close()
    return out


def read_non_heartbeat(ws: "WsClient", timeout: float) -> dict:
    """Drain interleaved Heartbeat frames and return the first real frame.

    Heartbeats arrive on their own cadence (B4 uses tokio interval, first tick
    fires immediately on connect), so a post-stall recovery read on the
    heartbeat-enabled socket must skip them. A per-read socket timeout is
    treated as "no frame yet, keep waiting" until the overall deadline.
    """
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        ws.s.settimeout(max(0.05, deadline - time.monotonic()))
        try:
            msg = ws.read()
        except socket.timeout:
            continue
        if msg["type"] != "Heartbeat":
            return msg
    raise AssertionError("timed out waiting for non-heartbeat frame")


def submit_order(client_order_id: str) -> dict:
    return {
        "type": "SubmitOrder",
        "client_order_id": client_order_id,
        "symbol": "BTCUSDT",
        "side": "Buy",
        "order_type": "Limit",
        "quantity": "10",
        "price": "100",
        "time_in_force": "Gtc",
    }


def modify_order(client_order_id: str, price: str | None = None, quantity: str | None = None) -> dict:
    return {
        "type": "ModifyOrder",
        "client_order_id": client_order_id,
        "price": price,
        "quantity": quantity,
    }


def find_balance(account: dict, currency: str) -> dict:
    for balance in account["balances"]:
        if balance["currency"] == currency:
            return balance
    raise AssertionError(f"missing balance {currency}: {account}")


def find_position(account: dict, symbol: str) -> dict:
    for position in account["positions"]:
        if position["symbol"] == symbol:
            return position
    raise AssertionError(f"missing position {symbol}: {account}")


def mean_event_gap(trades: list) -> float:
    gaps = [
        trades[i]["ts_event"] - trades[i - 1]["ts_event"]
        for i in range(1, len(trades))
    ]
    return sum(gaps) / len(gaps)


def sim_now(clock: dict, wall_ns: int | None = None) -> int:
    # /clock now returns a ServerClock envelope: the affine map lives under
    # "sim", alongside server_now_ns, data_origin_ns and backfill_horizon_ns.
    sim = clock["sim"]
    if wall_ns is None:
        wall_ns = time.time_ns()
    if wall_ns <= sim["wall_anchor_ns"]:
        return sim["sim_epoch_ns"]
    return int(sim["sim_epoch_ns"] + (wall_ns - sim["wall_anchor_ns"]) * sim["speed"])


class WsClient:
    def __init__(self, timeout: float | None = None, account_id: str = SMOKE_ACCOUNT) -> None:
        self.s = socket.create_connection((HOST, PORT))
        if timeout is not None:
            self.s.settimeout(timeout)
        self._handshake(account_id)
        self.generation = 0

    def _handshake(self, account_id: str) -> None:
        key = "x3JJHMbDL1EzLkh9GBhXDw=="  # static key is fine for a smoke test
        self.s.sendall(
            f"GET /ws?account={urllib.parse.quote(account_id)} HTTP/1.1\r\nHost: {HOST}:{PORT}\r\nUpgrade: websocket\r\n"
            f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\n"
            f"Sec-WebSocket-Version: 13\r\n\r\n".encode()
        )
        buf = self.s.recv(4096)
        assert b"101 Switching Protocols" in buf, buf

    def send(self, obj: dict) -> None:
        if obj.get("type") == "Subscribe" and "symbols" in obj:
            start_ts = obj.pop("start_ts", None)
            regime = obj.pop("regime", None)
            subscriptions = []
            for symbol in obj.pop("symbols"):
                self.generation += 1
                entry = {"generation": self.generation, "symbol": symbol}
                if start_ts is not None:
                    entry["start_ts"] = start_ts
                if regime is not None:
                    entry["regime"] = regime
                subscriptions.append(entry)
            obj["subscriptions"] = subscriptions
        self.s.sendall(_encode(json.dumps(obj)))

    def read(self) -> dict:
        return json.loads(_read_text(self.s))

    def close(self) -> None:
        self.s.close()


def _encode(text: str) -> bytes:
    data = text.encode()
    header = bytearray([0x81])  # FIN + text opcode
    mask = b"\x00\x00\x00\x00"  # mask of zeros => payload unchanged
    n = len(data)
    if n < 126:
        header.append(0x80 | n)
    elif n < 65536:
        header.append(0x80 | 126)
        header += n.to_bytes(2, "big")
    else:
        header.append(0x80 | 127)
        header += n.to_bytes(8, "big")
    return bytes(header) + mask + data


def _read_text(s: socket.socket) -> str:
    b0 = _recv_exact(s, 1)
    assert b0 == b"\x81", b0
    b1 = _recv_exact(s, 1)[0]
    n = b1 & 0x7F
    if n == 126:
        n = int.from_bytes(_recv_exact(s, 2), "big")
    elif n == 127:
        n = int.from_bytes(_recv_exact(s, 8), "big")
    return _recv_exact(s, n).decode()


def _recv_exact(s: socket.socket, n: int) -> bytes:
    payload = b""
    while len(payload) < n:
        chunk = s.recv(n - len(payload))
        if not chunk:
            raise EOFError("socket closed")
        payload += chunk
    return payload


def main_default() -> None:
    # Resolve a coherent window start from the live clock: the tape origin tracks
    # the clock now, so a frozen constant would fall before data_origin and be
    # refused. One hour behind sim-now is on-tape under the 24h horizon.
    window_start_ts = sim_now(fetch_clock()) - WINDOW_LOOKBACK_NS
    # The committed mogwai.toml (and the built-in default) fund the account
    # with 1,000,000 USDT before the run; the ledger books fill deltas on top
    # of that seed, and the funded account enforces free-balance checks.
    initial_account = fetch_account()
    print("initial:", initial_account)
    initial_usdt = find_balance(initial_account, "USDT")
    assert float(initial_usdt["total"]) == 1_000_000.0, initial_usdt
    assert float(initial_usdt["free"]) == 1_000_000.0, initial_usdt
    assert float(initial_usdt["locked"]) == 0.0, initial_usdt
    assert len(initial_account["balances"]) == 1, initial_account
    assert initial_account["positions"] == [], initial_account

    assert post_divergence(
        {"type": "PartialFillNext", "client_order_id": "O1", "fraction": "0.3"}
    ) == 202
    print("armed PartialFillNext(O1, 0.3)")

    msgs = ws_roundtrip(submit_order("O1"), expect=3)
    accepted, filled, account = msgs
    print("accepted:", accepted)
    print("filled:  ", filled)
    print("account: ", account)

    assert accepted["type"] == "OrderAccepted", accepted
    assert filled["type"] == "OrderFilled", filled
    assert float(filled["last_qty"]) == 3.0, filled
    assert float(filled["leaves_qty"]) == 7.0, filled
    assert account["type"] == "AccountState", account

    pos = find_position(account, "BTCUSDT")
    assert float(pos["quantity"]) == 3.0, pos
    assert float(pos["avg_px"]) == 100.0, pos

    btc = find_balance(account, "BTC")
    assert float(btc["total"]) == 3.0, btc
    assert float(btc["locked"]) == 0.0, btc
    assert float(btc["free"]) == 3.0, btc

    # Off the 1,000,000 seed: the 3-lot fill spends 300, the resting 7-lot
    # remainder reserves 700, and free is total minus the reservation.
    usdt = find_balance(account, "USDT")
    assert float(usdt["total"]) == 999_700.0, usdt
    assert float(usdt["locked"]) == 700.0, usdt
    assert float(usdt["free"]) == 999_000.0, usdt

    pulled_account = fetch_account()
    print("pulled:  ", pulled_account)
    pulled_btc = find_balance(pulled_account, "BTC")
    assert float(pulled_btc["total"]) == 3.0, pulled_btc
    print("PASS: partial fill round-tripped through the live WS path")

    assert post_divergence({"type": "DuplicateNextFill"}) == 202
    dup_msgs = ws_roundtrip(submit_order("DUP1"), expect=4)
    for msg in dup_msgs:
        print("dup:     ", msg)
    assert [msg["type"] for msg in dup_msgs] == [
        "OrderAccepted",
        "OrderFilled",
        "OrderFilled",
        "AccountState",
    ], dup_msgs
    assert dup_msgs[1]["trade_id"] == dup_msgs[2]["trade_id"], dup_msgs
    print("PASS: DuplicateNextFill doubled the live fill event")

    http_msgs = post_order(submit_order("HTTP1"))
    for msg in http_msgs:
        print("http:    ", msg)
    assert [msg["type"] for msg in http_msgs] == [
        "OrderAccepted",
        "OrderFilled",
        "AccountState",
    ], http_msgs
    assert http_msgs[0]["client_order_id"] == "HTTP1", http_msgs
    assert http_msgs[1]["client_order_id"] == "HTTP1", http_msgs
    print("PASS: SubmitOrder round-tripped through the HTTP order path")

    assert post_divergence({"type": "DropNextAccountUpdate"}) == 202
    ws = WsClient(timeout=0.2)
    ws.send(submit_order("DROP1"))
    drop_msgs = [ws.read() for _ in range(2)]
    for msg in drop_msgs:
        print("drop:    ", msg)
    try:
        extra = ws.read()
    except socket.timeout:
        extra = None
    ws.close()
    assert [msg["type"] for msg in drop_msgs] == ["OrderAccepted", "OrderFilled"], drop_msgs
    assert extra is None, extra
    print("PASS: DropNextAccountUpdate swallowed the live account update")

    assert post_divergence(
        {"type": "PartialFillNext", "client_order_id": "M1", "fraction": "0.3"}
    ) == 202
    modify_submit = ws_roundtrip(submit_order("M1"), expect=3)
    for msg in modify_submit:
        print("modify: ", msg)
    assert [msg["type"] for msg in modify_submit] == [
        "OrderAccepted",
        "OrderFilled",
        "AccountState",
    ], modify_submit
    before_reprice = float(find_balance(modify_submit[2], "USDT")["locked"])

    modify_msgs = ws_roundtrip(modify_order("M1", price="200"), expect=2)
    for msg in modify_msgs:
        print("modify: ", msg)
    assert modify_msgs[0]["type"] == "OrderUpdated", modify_msgs
    assert float(modify_msgs[0]["leaves_qty"]) == 7.0, modify_msgs
    assert float(modify_msgs[0]["price"]) == 200.0, modify_msgs
    assert modify_msgs[1]["type"] == "AccountState", modify_msgs
    after_reprice = float(find_balance(modify_msgs[1], "USDT")["locked"])
    assert after_reprice == before_reprice + 700.0, modify_msgs

    reject_msgs = ws_roundtrip(modify_order("GHOST", price="1"), expect=1)
    for msg in reject_msgs:
        print("modify: ", msg)
    assert reject_msgs[0]["type"] == "OrderModifyRejected", reject_msgs
    assert reject_msgs[0]["venue_order_id"] is None, reject_msgs
    print("PASS: ModifyOrder reprices the resting reservation")

    # Market-data replay: subscribe to the venue's listed instrument and read the
    # first 2 trades. The generator only synthesizes a tape for a configured
    # symbol (the per-symbol instrument set), mirroring the engine rejecting an
    # order for an unknown instrument, so the subscription must name a listed
    # symbol - the default venue lists BTCUSDT.
    ticks = ws_roundtrip({"type": "Subscribe", "symbols": ["BTCUSDT"]}, expect=2)
    for t in ticks:
        print("tick:    ", t)
        assert t["type"] == "Trade", t
        assert t["symbol"] == "BTCUSDT", t
    print("PASS: historical trades replayed over the live WS path")

    windowed = ws_roundtrip(
        {"type": "Subscribe", "symbols": ["BTCUSDT"], "start_ts": window_start_ts},
        expect=1,
    )[0]
    print("windowed:", windowed)
    assert windowed["type"] == "Trade", windowed
    assert windowed["symbol"] == "BTCUSDT", windowed
    assert windowed["ts_event"] >= window_start_ts, windowed
    print("PASS: Subscribe.start_ts is wired through live replay")

    # Attributability end to end: ONE Subscribe naming a listed symbol and an
    # unlisted one gets exactly one coalesced SubscriptionIssues frame, whose
    # single outcome names the SECOND entry's generation and symbol - while the
    # first entry's ticks keep arriving. No unit test spans this whole path, and
    # before the per-entry generation the diagnostic named no subscription at all.
    attrib = WsClient(timeout=1.5)
    attrib.send(
        {
            "type": "Subscribe",
            "subscriptions": [
                {"generation": 900, "symbol": "BTCUSDT"},
                {"generation": 901, "symbol": "NOPECOIN"},
            ],
        }
    )
    issues = None
    traded = False
    for _ in range(200):
        frame = attrib.read()
        if frame["type"] == "SubscriptionIssues":
            assert issues is None, ("exactly one coalesced frame", issues, frame)
            issues = frame
        elif frame["type"] == "Trade":
            assert frame["symbol"] == "BTCUSDT", frame
            traded = True
        if issues is not None and traded:
            break
    print("issues:  ", issues)
    assert issues is not None, "no SubscriptionIssues frame arrived"
    assert issues["issues_total"] == 1, issues
    assert issues["refusals_total"] == 1, issues
    assert len(issues["entries"]) == 1, issues
    entry = issues["entries"][0]
    assert entry["generation"] == 901, entry
    assert entry["symbol"] == "NOPECOIN", entry
    assert entry["issue"]["kind"] == "UnknownSymbol", entry
    assert traded, "the listed symbol's ticks must keep arriving"
    attrib.close()
    print("PASS: a subscription diagnostic names the entry it describes")

    # Measure the regime effect from the tape HEAD (start omitted = from origin),
    # not from a windowed start. Clean and drought are independent realizations of
    # the same seed; the generator's arrival envelope is keyed to each tape's own
    # clock_ns, which under a thin regime advances ~thin_factor faster. From the
    # head the two share RNG/ACD state tick-for-tick, so the thin-factor shows as a
    # clean multiple; after a deep seek their clocks have diverged into different
    # session phases and the gap comparison is meaningless (it can even invert).
    clean_trades = fetch_trades("BTCUSDT", None, 200)
    drought_trades = fetch_trades(
        "BTCUSDT",
        None,
        200,
        {"type": "LiquidityDrought", "thin_factor": 5.0},
    )
    clean_gap = mean_event_gap(clean_trades)
    drought_gap = mean_event_gap(drought_trades)
    print("regime: ", {"clean_gap": clean_gap, "drought_gap": drought_gap})
    assert drought_gap >= clean_gap * 3.0, (clean_gap, drought_gap)
    print("PASS: LiquidityDrought stretches event-time market data gaps")

    ws = WsClient(timeout=1.5)
    ws.send({"type": "Subscribe", "symbols": ["BTCUSDT"]})
    first = ws.read()
    print("first:   ", first)
    assert first["type"] == "Trade", first
    ws.send({"type": "Unsubscribe", "symbols": ["BTCUSDT"]})
    try:
        extra = ws.read()
    except socket.timeout:
        extra = None
    ws.close()
    assert extra is None, extra
    print("PASS: Unsubscribe stops the live replay")

    ws = WsClient(timeout=5.0)
    ws.send({"type": "Subscribe", "symbols": ["BTCUSDT"]})
    capped = [ws.read() for _ in range(4)]
    ws.close()
    for t in capped:
        print("capped:  ", t)
        assert t["type"] == "Trade", t
        assert t["symbol"] == "BTCUSDT", t
    print("PASS: capped paced replay delivers generated trades")

    assert post_divergence({"type": "DelayAcks", "ms": 300}) == 202
    ws = WsClient(timeout=2.0)
    send_instant = time.monotonic()
    ws.send(submit_order("D1"))
    delay_msgs = []
    arrivals = []
    for _ in range(3):
        delay_msgs.append(ws.read())
        arrivals.append(time.monotonic() - send_instant)
    ws.close()
    for msg in delay_msgs:
        print("delay:   ", msg)
    assert [msg["type"] for msg in delay_msgs] == [
        "OrderAccepted",
        "OrderFilled",
        "AccountState",
    ], delay_msgs
    # DelayAcks holds every execution event `ms` from its PRODUCTION instant
    # (the exec pump anchors each event's deadline at enqueue). The engine
    # emits the whole order-entry batch at one instant, so the three events
    # share one deadline: each arrives >= ~250ms after submit (slack under the
    # armed 300), and the batch lands together rather than serialized a
    # further window per event (serialized would put the third past ~900ms).
    for arrival in arrivals:
        assert arrival >= 0.25, arrivals
    assert arrivals[-1] < 0.6, arrivals

    # Probe that DelayAcks delays execution events but NOT market data. Use a
    # WINDOWED subscribe: its first tick is historical backfill seeked from a past
    # cursor, so the server emits it immediately and the strict <0.2s latency bound
    # cleanly proves the 300ms ack delay did not leak onto the data path. A fresh
    # (start-less) subscribe seeks live to sim-now and the server now paces that
    # first tick to its own timestamp (up to gap_cap_ms behind), which is correct
    # live behavior but no longer an immediate-delivery probe.
    ws = WsClient(timeout=1.0)
    ws.send({"type": "Subscribe", "symbols": ["BTCUSDT"], "start_ts": window_start_ts})
    start = time.monotonic()
    trade = ws.read()
    elapsed = time.monotonic() - start
    ws.close()
    print("delay-md:", trade)
    assert trade["type"] == "Trade", trade
    assert trade["ts_event"] >= window_start_ts, trade
    assert elapsed < 0.2, elapsed

    assert post_divergence({"type": "DelayAcks", "ms": 0}) == 202
    ws = WsClient(timeout=1.0)
    ws.send(submit_order("D2"))
    start = time.monotonic()
    prompt = ws.read()
    elapsed = time.monotonic() - start
    ws.close()
    print("prompt:  ", prompt)
    assert prompt["type"] == "OrderAccepted", prompt
    assert elapsed < 0.2, elapsed
    print("PASS: DelayAcks delays only execution events and disarms")

    assert post_divergence({"type": "GoDark", "ms": 500}) == 202
    ws = WsClient(timeout=0.3)
    ws.send({"type": "Subscribe", "symbols": ["BTCUSDT"]})
    try:
        dark = ws.read()
    except socket.timeout:
        dark = None
    assert dark is None, dark
    time.sleep(0.6)
    # The blackout has lifted; ticks produced while dark were dropped, so the
    # recovered frame is the next generated tick delivered after the blackout.
    # The short 0.3s above only proves the window was silent.
    ws.s.settimeout(3.0)
    recovered = ws.read()
    ws.close()
    print("recovered:", recovered)
    assert recovered["type"] == "Trade", recovered
    assert recovered["symbol"] == "BTCUSDT", recovered
    assert recovered["ts_event"] >= window_start_ts, recovered
    print("PASS: GoDark drops blackout frames instead of buffering them")

    assert post_divergence({"type": "GoDark", "ms": 3_600_001}) == 400
    assert post_divergence({"type": "DelayAcks", "ms": 3_600_001}) == 400

    assert post_divergence({"type": "GoDark", "ms": 60_000}) == 202
    ws = WsClient(timeout=0.3)
    ws.send({"type": "Subscribe", "symbols": ["BTCUSDT"], "start_ts": window_start_ts})
    try:
        dark = ws.read()
    except socket.timeout:
        dark = None
    ws.close()
    assert dark is None, dark

    assert post_divergence({"type": "ClearDivergences"}) == 202
    ws = WsClient(timeout=3.0)
    ws.send({"type": "Subscribe", "symbols": ["BTCUSDT"], "start_ts": window_start_ts})
    recovered = ws.read()
    ws.close()
    print("cleared:  ", recovered)
    assert recovered["type"] == "Trade", recovered
    assert recovered["symbol"] == "BTCUSDT", recovered
    assert recovered["ts_event"] >= window_start_ts, recovered

    assert post_divergence({"type": "DelayAcks", "ms": 60_000}) == 202
    assert post_divergence({"type": "ClearDivergences"}) == 202
    ws = WsClient(timeout=1.0)
    ws.send(submit_order("D3"))
    start = time.monotonic()
    prompt = ws.read()
    elapsed = time.monotonic() - start
    ws.close()
    print("cleared-delay:", prompt)
    assert prompt["type"] == "OrderAccepted", prompt
    assert elapsed < 0.2, elapsed
    print(
        "PASS: ClearDivergences lifts a live dark/delay window "
        "and over-bound ms is rejected"
    )

    assert post_divergence({"type": "StallData", "ms": 500}) == 202
    ws = WsClient(timeout=0.25)
    ws.send({"type": "Subscribe", "symbols": ["BTCUSDT"], "start_ts": window_start_ts})
    try:
        stalled = ws.read()
    except socket.timeout:
        stalled = None
    assert stalled is None, stalled

    ws.s.settimeout(1.5)
    ws.send(submit_order("STALL1"))
    stall_exec = [ws.read() for _ in range(3)]
    for msg in stall_exec:
        print("stall-ex:", msg)
    assert [msg["type"] for msg in stall_exec] == [
        "OrderAccepted",
        "OrderFilled",
        "AccountState",
    ], stall_exec

    time.sleep(0.6)
    ws.s.settimeout(3.0)
    recovered = ws.read()
    ws.close()
    print("stall-rec:", recovered)
    assert recovered["type"] == "Trade", recovered
    assert recovered["symbol"] == "BTCUSDT", recovered
    assert recovered["ts_event"] >= window_start_ts, recovered

    assert post_divergence({"type": "StallData", "ms": 3_600_001}) == 400

    assert post_divergence({"type": "StallData", "ms": 60_000}) == 202
    ws = WsClient(timeout=0.25)
    ws.send({"type": "Subscribe", "symbols": ["BTCUSDT"], "start_ts": window_start_ts})
    try:
        stalled = ws.read()
    except socket.timeout:
        stalled = None
    assert stalled is None, stalled

    assert post_divergence({"type": "ClearDivergences"}) == 202
    ws.s.settimeout(3.0)
    recovered = ws.read()
    ws.close()
    print("stall-cl:", recovered)
    assert recovered["type"] == "Trade", recovered
    assert recovered["symbol"] == "BTCUSDT", recovered
    assert recovered["ts_event"] >= window_start_ts, recovered
    print("PASS: StallData drops market data while execution frames flow")

    # The live isolation proof: distinct websocket account identities share a
    # venue but never an execution ledger or execution stream.
    second_account = "SMOKE-002"
    second_before = fetch_account(second_account)
    first_ws = WsClient(timeout=1.0, account_id=SMOKE_ACCOUNT)
    second_ws = WsClient(timeout=0.2, account_id=second_account)
    first_ws.send(submit_order("MULTI-ACCOUNT-1"))
    first_events = [first_ws.read() for _ in range(3)]
    first_ws.close()
    try:
        second_event = second_ws.read()
    except socket.timeout:
        second_event = None
    second_ws.close()
    second_after = fetch_account(second_account)
    assert [event["type"] for event in first_events] == [
        "OrderAccepted", "OrderFilled", "AccountState"
    ], first_events
    assert second_after["account_id"] == second_account, second_after
    assert second_after["balances"] == second_before["balances"], (second_before, second_after)
    assert second_after["positions"] == second_before["positions"], (second_before, second_after)
    assert second_event is None or second_event["type"] not in {
        "OrderAccepted", "OrderFilled", "AccountState"
    }, second_event
    print("PASS: two live accounts keep execution state and streams isolated")

    # The account control plane, over the live socket rather than a handler
    # test: a listing names both accounts, teardown reclaims the ledger, and a
    # request under the destroyed id auto-creates a FRESH account rather than
    # resurrecting the old one.
    listed = {row["account_id"] for row in list_accounts()}
    assert {SMOKE_ACCOUNT, second_account} <= listed, listed
    assert delete_account(second_account) == 200
    assert second_account not in {row["account_id"] for row in list_accounts()}
    assert delete_account(second_account) == 404
    reborn = fetch_account(second_account)
    assert reborn["positions"] == [], reborn
    assert delete_account(second_account) == 200
    print("PASS: account listing, teardown and re-creation behave over the live socket")


def main_heartbeat() -> None:
    # Same runtime-resolved window start as main_default: the tape origin tracks
    # the clock, so the windowed subscribe must anchor inside [data_origin, sim_now]
    # rather than at a frozen constant.
    window_start_ts = sim_now(fetch_clock()) - WINDOW_LOOKBACK_NS

    assert post_divergence({"type": "ClearDivergences"}) == 202
    assert post_divergence({"type": "StallData", "ms": 700}) == 202

    ws = WsClient(timeout=0.12)
    ws.send({"type": "Subscribe", "symbols": ["BTCUSDT"], "start_ts": window_start_ts})
    deadline = time.monotonic() + 0.55
    heartbeats = 0
    market_data = []
    while time.monotonic() < deadline:
        try:
            msg = ws.read()
        except socket.timeout:
            continue
        print("hb-stall:", msg)
        if msg["type"] == "Heartbeat":
            heartbeats += 1
        elif msg["type"] in ("Trade", "Quote"):
            market_data.append(msg)
        else:
            raise AssertionError(msg)

    assert heartbeats > 0, "heartbeat-enabled server did not emit a heartbeat"
    assert market_data == [], market_data

    time.sleep(0.3)
    recovered = read_non_heartbeat(ws, 3.0)
    ws.close()
    print("hb-rec:  ", recovered)
    assert recovered["type"] == "Trade", recovered
    assert recovered["symbol"] == "BTCUSDT", recovered
    assert recovered["ts_event"] >= window_start_ts, recovered
    assert post_divergence({"type": "ClearDivergences"}) == 202
    print("PASS: Heartbeat keeps the socket frame-active through StallData")


def main_accelerated() -> None:
    clock = fetch_clock()
    print("clock:   ", clock)
    # The affine map lives under "sim" in the ServerClock envelope.
    sim = clock["sim"]
    assert sim["sim_epoch_ns"] > 0, clock
    assert sim["speed"] > 1.0, clock
    assert post_divergence({"type": "ClearDivergences"}) == 202
    slack_ns = int(ACCEL_CLOCK_SLACK_WALL_NS * sim["speed"])

    # Execution-side coherence: the engine stamps every event at sim-now while
    # the request is in flight, so each ts_event must land inside the
    # roundtrip's wall window projected onto the sim axis (plus the wall-read
    # slack). This asserts the execution path rides the advertised clock
    # directly, rather than comparing it against a market-data timestamp whose
    # distance is a property of the tape.
    order_start_wall = time.time_ns()
    msgs = ws_roundtrip(submit_order("ACCEL1"), expect=3)
    order_end_wall = time.time_ns()
    for msg in msgs:
        print("accel-ex:", msg)
    assert [msg["type"] for msg in msgs] == [
        "OrderAccepted",
        "OrderFilled",
        "AccountState",
    ], msgs
    exec_lo = sim_now(clock, order_start_wall) - slack_ns
    exec_hi = sim_now(clock, order_end_wall) + slack_ns
    for msg in msgs:
        assert exec_lo <= msg["ts_event"] <= exec_hi, (msg, exec_lo, exec_hi)
    print("PASS: execution events are stamped on the advertised sim axis")

    # Market-data anchor: the first REAL tick on the tape at-or-after
    # sim_epoch, found via /trades. A previous version subscribed at sim_epoch
    # and bounded the first frame's arrival with a fixed ~2s first-gap slack,
    # but the fitted ACD duration process is heavy-tailed (dispersion index in
    # the hundreds of seconds): the tape realizes multi-minute lulls, and a
    # random sim instant sits mid-lull with high probability (the expected gap
    # straddling an instant is length-biased), so ANY fixed first-gap constant
    # only passes when the tape near epoch happens to be dense. /trades caps
    # its window at sim-now, so an empty page just means the tape is still
    # mid-lull and sim time has to advance; poll until the tick exists.
    anchor_deadline = time.monotonic() + ACCEL_ANCHOR_TIMEOUT_S
    while True:
        page = fetch_trades("BTCUSDT", sim["sim_epoch_ns"], 1)
        if page:
            anchor = page[0]
            break
        assert time.monotonic() < anchor_deadline, (
            f"no on-tape tick at-or-after sim_epoch within {ACCEL_ANCHOR_TIMEOUT_S}s; "
            "a lull this long is outside the fitted dispersion band"
        )
        time.sleep(0.25)
    print("anchor:  ", anchor)

    # Subscribing from the anchor's exact ts_event must (a) replay the SAME
    # tick first - the live replay and /trades are seeks into one shared tape,
    # and a seek to an emitted tick's exact ts_event re-emits that boundary
    # tick - and (b) deliver it immediately: the anchor is at-or-behind
    # sim-now, so its deadline-pacing target is already in the past. Backfill
    # immediacy is the delivery property the old fixed-slack version was
    # actually after, now asserted with no assumption about tape density.
    ws = WsClient(timeout=2.0)
    data_start_wall = time.time_ns()
    ws.send(
        {
            "type": "Subscribe",
            "symbols": ["BTCUSDT"],
            "start_ts": anchor["ts_event"],
        }
    )
    trade = ws.read()
    data_end_wall = time.time_ns()
    ws.close()
    print("accel-md:", trade)
    assert trade["type"] == "Trade", trade
    assert {key: trade[key] for key in anchor} == anchor, (trade, anchor)
    assert (data_end_wall - data_start_wall) < 250_000_000, (
        data_end_wall - data_start_wall
    )
    # Data-side coherence: a served tick is stamped on the sim axis and the
    # tape never runs ahead of the clock.
    assert trade["ts_event"] <= sim_now(clock, data_end_wall) + slack_ns, trade
    print("PASS: live replay serves the shared tape from a real anchor tick")

    assert post_divergence({"type": "DelayAcks", "ms": ACCEL_DELAY_MS}) == 202
    ws = WsClient(timeout=2.0)
    ws.send(submit_order("ACCEL-D"))
    start = time.monotonic()
    delay_msgs = [ws.read() for _ in range(3)]
    elapsed = time.monotonic() - start
    ws.close()
    for msg in delay_msgs:
        print("accel-dl:", msg)
    expected_delay = ACCEL_DELAY_MS / 1000.0 / sim["speed"]
    assert elapsed >= expected_delay * 0.5, (elapsed, expected_delay)
    assert elapsed < 0.5, elapsed
    assert post_divergence({"type": "DelayAcks", "ms": 0}) == 202
    print("PASS: accelerated coherent-clock smoke")


def main_admission() -> None:
    """Admission control end to end: I4 and I5 together on a live socket.

    Arms a DelayAcks window, then submits until the connection's held-lane byte
    budget cannot cover another order's worst-case output. Two things must then
    be true at once, and neither is visible from a unit test:

    - the refusal comes back PROMPTLY, ahead of every execution event still
      sitting in the delay pump. `DelayAcks` holds the delivery of engine
      output; it never holds admission, which rides the priority lane (I4).
    - the refusal is on the wire and attributed to the order it refused, rather
      than the venue silently dropping the command (I5).

    Then the window is cleared and every admitted order's ack arrives: a refused
    reservation is a refusal to START work, not a loss of work already done.

    Needs the small-budget server config, or saturating the shipped 8 MiB budget
    would take twelve thousand orders:

        brokkr run mogwai -- serve -f --config scripts/smoke-admission.toml
    """
    assert post_divergence({"type": "ClearDivergences"}) == 202
    # Long enough that nothing drains while the submits are in flight, short
    # enough that the drain below is a wait and not a coffee break. A frame the
    # pump has already dequeued sleeps out ITS deadline - ClearDivergences
    # reaches the frames still queued behind it, not that one - so this value,
    # not the clear, bounds the drain.
    assert post_divergence({"type": "DelayAcks", "ms": 4_000}) == 202

    ws = WsClient(timeout=0.2)
    submitted = 0
    refusal = None
    first_frame = None
    # Submit in small batches, checking after each whether the venue has refused
    # yet. The cap is far above the ~dozen orders the configured budget holds, so
    # exhausting it means the budget is not bounding anything.
    while submitted < 400 and refusal is None:
        for _ in range(4):
            submitted += 1
            ws.send(submit_order(f"ADM-{submitted}"))
        try:
            msg = ws.read()
        except socket.timeout:
            continue
        if first_frame is None:
            first_frame = msg
        if msg["type"] == "AdmissionRejected":
            refusal = msg

    print("refusal: ", refusal)
    assert refusal is not None, f"no refusal after {submitted} submits"
    # The refusal overtook every held ack: nothing else had arrived yet.
    assert first_frame is refusal, first_frame
    assert refusal["subject"]["kind"] == "Submit", refusal
    assert refusal["subject"]["client_order_id"].startswith("ADM-"), refusal
    assert refusal["reason"], refusal

    # Clear the window and drain: every order the venue admitted is still
    # answered, and every one it refused said so.
    assert post_divergence({"type": "ClearDivergences"}) == 202
    accepted, refused = 0, 1
    deadline = time.monotonic() + 20.0
    while accepted + refused < submitted and time.monotonic() < deadline:
        try:
            msg = ws.read()
        except socket.timeout:
            continue
        if msg["type"] == "OrderAccepted":
            accepted += 1
        elif msg["type"] == "AdmissionRejected":
            refused += 1
    ws.close()

    print(f"drained: {accepted} accepted, {refused} refused, {submitted} submitted")
    assert accepted > 0, "the held acks never arrived after the window cleared"
    assert accepted + refused == submitted, (accepted, refused, submitted)
    print("PASS: admission refuses visibly and promptly under an armed DelayAcks")


def main_command_latency() -> None:
    """Prove a delayed submit yields to an immediate cancel on one socket.

    Looped five times inside the step rather than left to the operator to
    repeat: a race that only sometimes reverses at an 800:0 ratio is not a knob
    anyone can build a scenario on, so the threshold is one command.
    """
    for attempt in range(5):
        raced = f"O-RACE-{attempt}"
        assert post_divergence({"type": "CommandLatency", "submit_act_ms": 800}) == 202
        ws = WsClient()
        ws.send(submit_order(raced))
        ws.send({"type": "CancelOrder", "client_order_id": raced})

        # The cancel reaches a book the submit has not touched yet, so it is
        # rejected against an unknown order - it names no venue id, because the
        # venue has never issued one. This is the outcome that is impossible
        # without an act latency.
        first = read_non_heartbeat(ws, 3.0)
        assert first["type"] == "OrderCancelRejected", first
        assert first["client_order_id"] == raced, first
        assert first.get("venue_order_id") is None, first

        # ...and only then does the delayed submit land.
        accepted = read_non_heartbeat(ws, 3.0)
        assert accepted["type"] == "OrderAccepted", accepted
        assert accepted["client_order_id"] == raced, accepted

        # Disarm proof, in the shape the DelayAcks step already uses: with all
        # six fields zero the ordinary ordering returns and the cancel finds the
        # order it names.
        assert post_divergence({"type": "CommandLatency"}) == 202
        ordinary = f"O-ORDINARY-{attempt}"
        ws.send(submit_order(ordinary))
        ws.send({"type": "CancelOrder", "client_order_id": ordinary})
        frame = read_non_heartbeat(ws, 3.0)
        while frame["type"] not in ("OrderCanceled", "OrderCancelRejected"):
            assert frame["type"] != "OrderAccepted" or frame["client_order_id"] == ordinary, frame
            frame = read_non_heartbeat(ws, 3.0)
        assert frame["client_order_id"] == ordinary, frame
        assert frame.get("venue_order_id") is not None, frame
        ws.close()
    print("PASS: command latency detaches delayed submits from the socket read loop")


def main() -> None:
    args = sys.argv[1:]
    if args == ["--heartbeat"]:
        main_heartbeat()
    elif args == ["--accelerated"]:
        main_accelerated()
    elif args == ["--admission"]:
        main_admission()
    elif args == ["--command-latency"]:
        main_command_latency()
    elif not args:
        main_default()
    else:
        raise SystemExit("usage: smoke.py [--heartbeat|--accelerated|--admission|--command-latency]")


if __name__ == "__main__":
    main()
