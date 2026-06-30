#!/usr/bin/env python3
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

    brokkr run -p mogwai-server -- serve

then run this script.

The default run keeps the server heartbeat off, so every step reads exactly
one frame per assertion without an interleaved Heartbeat. The StallData /
heartbeat (4255) reproduction lives behind `--heartbeat`, which runs only the
heartbeat step against a server started with server_heartbeat_ms enabled. The
`serve --config` flag is consumed by the server binary, not cargo, so it must
follow a `--` separator:

    brokkr run -p mogwai-server -- serve --config scripts/smoke-heartbeat.toml

then run `python3 scripts/smoke.py --heartbeat`.

Accelerated coherent-clock smoke:

    brokkr run -p mogwai-server -- serve --config scripts/smoke-accelerated.toml

then run `python3 scripts/smoke.py --accelerated`.
"""
import json
import socket
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

HOST, PORT = "127.0.0.1", 8787
# How far behind sim-now the windowed-subscribe / history probes anchor their
# start. The server now derives the tape origin from the clock at boot
# (data_origin = sim_now - backfill_horizon, 24h by default) and refuses a
# /trades start before that floor, so the window start can no longer be a frozen
# constant - it must sit inside [data_origin, sim_now]. One hour back is safely
# on-tape under the 24h horizon. main_default resolves the absolute start from
# the live clock at runtime.
WINDOW_LOOKBACK_NS = 3_600_000_000_000
ACCEL_DELAY_MS = 1000


def post_divergence(payload: dict) -> int:
    req = urllib.request.Request(
        f"http://{HOST}:{PORT}/control/divergence",
        data=json.dumps(payload).encode(),
        headers={"content-type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req) as r:
            return r.status
    except urllib.error.HTTPError as e:
        return e.code


def post_order(payload: dict) -> list:
    req = urllib.request.Request(
        f"http://{HOST}:{PORT}/orders",
        data=json.dumps(payload).encode(),
        headers={"content-type": "application/json"},
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
    if wall_ns is None:
        wall_ns = time.time_ns()
    if wall_ns <= clock["wall_anchor_ns"]:
        return clock["sim_epoch_ns"]
    return int(clock["sim_epoch_ns"] + (wall_ns - clock["wall_anchor_ns"]) * clock["speed"])


class WsClient:
    def __init__(self, timeout: float | None = None) -> None:
        self.s = socket.create_connection((HOST, PORT))
        if timeout is not None:
            self.s.settimeout(timeout)
        self._handshake()

    def _handshake(self) -> None:
        key = "x3JJHMbDL1EzLkh9GBhXDw=="  # static key is fine for a smoke test
        self.s.sendall(
            f"GET /ws HTTP/1.1\r\nHost: {HOST}:{PORT}\r\nUpgrade: websocket\r\n"
            f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\n"
            f"Sec-WebSocket-Version: 13\r\n\r\n".encode()
        )
        buf = self.s.recv(4096)
        assert b"101 Switching Protocols" in buf, buf

    def send(self, obj: dict) -> None:
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

    usdt = find_balance(account, "USDT")
    assert float(usdt["total"]) == -300.0, usdt
    assert float(usdt["locked"]) == 700.0, usdt
    assert float(usdt["free"]) == -1000.0, usdt
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
    ws.send(submit_order("D1"))
    delay_msgs = []
    gaps = []
    for _ in range(3):
        start = time.monotonic()
        delay_msgs.append(ws.read())
        gaps.append(time.monotonic() - start)
    ws.close()
    for msg in delay_msgs:
        print("delay:   ", msg)
    assert [msg["type"] for msg in delay_msgs] == [
        "OrderAccepted",
        "OrderFilled",
        "AccountState",
    ], delay_msgs
    for gap in gaps:
        assert gap >= 0.25, gaps

    ws = WsClient(timeout=1.0)
    ws.send({"type": "Subscribe", "symbols": ["BTCUSDT"]})
    start = time.monotonic()
    trade = ws.read()
    elapsed = time.monotonic() - start
    ws.close()
    print("delay-md:", trade)
    assert trade["type"] == "Trade", trade
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
    assert clock["sim_epoch_ns"] > 0, clock
    assert clock["speed"] > 1.0, clock
    assert post_divergence({"type": "ClearDivergences"}) == 202

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
    filled = msgs[1]
    account = msgs[2]
    assert filled["ts_event"] >= clock["sim_epoch_ns"], filled
    assert account["ts_event"] >= clock["sim_epoch_ns"], account
    assert filled["ts_event"] > order_start_wall, (filled, order_start_wall)
    expected_order_sim = sim_now(clock, (order_start_wall + order_end_wall) // 2)
    order_error = abs(filled["ts_event"] - expected_order_sim)
    print("accel-order-error-ns:", order_error)

    ws = WsClient(timeout=2.0)
    data_start_wall = time.time_ns()
    ws.send(
        {
            "type": "Subscribe",
            "symbols": ["BTCUSDT"],
            "start_ts": clock["sim_epoch_ns"],
        }
    )
    trade = ws.read()
    data_end_wall = time.time_ns()
    ws.close()
    print("accel-md:", trade)
    assert trade["type"] == "Trade", trade
    assert trade["ts_event"] >= clock["sim_epoch_ns"], trade
    assert (data_end_wall - data_start_wall) < 250_000_000, trade

    # Coherence budget, all terms in SIMULATED nanoseconds (skew is a sim-ns
    # difference). Three components, each justified rather than a fudge factor:
    #
    #   measured_wall_latency * speed
    #     The real round-trip wall latency projected onto the sim axis - the
    #     irreducible delivery cost that does not compress with speed.
    #   connect_catchup = (data_start_wall - wall_anchor) * speed
    #     The order fill is stamped sim_now at order time, while the FIRST trade
    #     read is the generator's first tick at ~sim_epoch. Their difference is
    #     dominated by the sim-time elapsed between the boot anchor and the
    #     connect, which this term bounds from above (data subscribe happens
    #     after the order, so it over-covers the order's own catch-up).
    #   FIRST_GAP_SLACK_NS
    #     The generator's first inter-arrival gap. trade.ts_event is
    #     sim_epoch + first_gap; in a sparse regime that gap can exceed the
    #     connect catch-up, flipping the sign of the skew, so the budget must
    #     also bound the largest plausible first gap (~2 simulated seconds).
    FIRST_GAP_SLACK_NS = 2_000_000_000
    measured_wall_latency = max(order_end_wall - order_start_wall, data_end_wall - data_start_wall)
    connect_catchup = int(
        max(0, data_start_wall - clock["wall_anchor_ns"]) * clock["speed"]
    )
    budget = int(measured_wall_latency * clock["speed"]) + connect_catchup + FIRST_GAP_SLACK_NS
    skew = abs(filled["ts_event"] - trade["ts_event"])
    print(
        "accel-coherence:",
        {"skew_ns": skew, "budget_ns": budget, "catchup_ns": connect_catchup},
    )
    assert skew <= budget, (skew, budget, filled, trade)

    assert post_divergence({"type": "DelayAcks", "ms": ACCEL_DELAY_MS}) == 202
    ws = WsClient(timeout=2.0)
    ws.send(submit_order("ACCEL-D"))
    start = time.monotonic()
    delay_msgs = [ws.read() for _ in range(3)]
    elapsed = time.monotonic() - start
    ws.close()
    for msg in delay_msgs:
        print("accel-dl:", msg)
    expected_delay = ACCEL_DELAY_MS / 1000.0 / clock["speed"]
    assert elapsed >= expected_delay * 0.5, (elapsed, expected_delay)
    assert elapsed < 0.5, elapsed
    assert post_divergence({"type": "DelayAcks", "ms": 0}) == 202
    print("PASS: accelerated coherent-clock smoke")


def main() -> None:
    args = sys.argv[1:]
    if args == ["--heartbeat"]:
        main_heartbeat()
    elif args == ["--accelerated"]:
        main_accelerated()
    elif not args:
        main_default()
    else:
        raise SystemExit("usage: smoke.py [--heartbeat|--accelerated]")


if __name__ == "__main__":
    main()
