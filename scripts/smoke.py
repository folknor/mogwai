#!/usr/bin/env python3
"""End-to-end smoke test of the mogwai fake broker.

Arms divergences over the control plane, then submits orders over the native WS
gateway and checks the resulting execution events. Also exercises replay timing
so DelayAcks and GoDark are verified at the socket writer. Uses only the stdlib
so there's nothing to install.

Connects to a server at 127.0.0.1:8787; it does not start one. The windowing,
Unsubscribe-stop, gap-cap, DelayAcks, and GoDark steps need PACED replay against
the generated stream. The committed mogwai.toml sets speed = 1.0, so a plain
launch paces correctly (unthrottled speed 0.0 makes those steps race and fail
spuriously):

    brokkr run -p mogwai-server

then run this script.
"""
import json
import socket
import time
import urllib.parse
import urllib.request

HOST, PORT = "127.0.0.1", 8787
WINDOW_START_TS = 86_401_000_000_000


def post_divergence(payload: dict) -> int:
    req = urllib.request.Request(
        f"http://{HOST}:{PORT}/control/divergence",
        data=json.dumps(payload).encode(),
        headers={"content-type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req) as r:
        return r.status


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


def fetch_trades(symbol: str, start: int, limit: int, regime: dict | None = None) -> list:
    params = {"symbol": symbol, "start": start, "limit": limit}
    if regime is not None:
        params["regime"] = json.dumps(regime)
    url = f"http://{HOST}:{PORT}/trades?{urllib.parse.urlencode(params)}"
    with urllib.request.urlopen(url) as r:
        assert r.status == 200, r.status
        return json.loads(r.read().decode())


def ws_roundtrip(send_obj: dict, expect: int) -> list:
    """Minimal RFC6455 client: handshake, send one text frame, read `expect` frames."""
    ws = WsClient()
    ws.send(send_obj)
    out = [ws.read() for _ in range(expect)]
    ws.close()
    return out


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


def main() -> None:
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

    # Market-data replay: subscribe to an arbitrary pair name and read the first 2 trades.
    ticks = ws_roundtrip({"type": "Subscribe", "symbols": ["KEUR"]}, expect=2)
    for t in ticks:
        print("tick:    ", t)
        assert t["type"] == "Trade", t
        assert t["symbol"] == "KEUR", t
    print("PASS: historical trades replayed over the live WS path")

    windowed = ws_roundtrip(
        {"type": "Subscribe", "symbols": ["KEUR"], "start_ts": WINDOW_START_TS},
        expect=1,
    )[0]
    print("windowed:", windowed)
    assert windowed["type"] == "Trade", windowed
    assert windowed["symbol"] == "KEUR", windowed
    assert windowed["ts_event"] >= WINDOW_START_TS, windowed
    print("PASS: Subscribe.start_ts is wired through live replay")

    clean_trades = fetch_trades("KEUR", WINDOW_START_TS, 200)
    drought_trades = fetch_trades(
        "KEUR",
        WINDOW_START_TS,
        200,
        {"type": "LiquidityDrought", "thin_factor": 5.0},
    )
    clean_gap = mean_event_gap(clean_trades)
    drought_gap = mean_event_gap(drought_trades)
    print("regime: ", {"clean_gap": clean_gap, "drought_gap": drought_gap})
    assert drought_gap >= clean_gap * 3.0, (clean_gap, drought_gap)
    print("PASS: LiquidityDrought stretches event-time market data gaps")

    ws = WsClient(timeout=1.5)
    ws.send({"type": "Subscribe", "symbols": ["KEUR"]})
    first = ws.read()
    print("first:   ", first)
    assert first["type"] == "Trade", first
    ws.send({"type": "Unsubscribe", "symbols": ["KEUR"]})
    try:
        extra = ws.read()
    except socket.timeout:
        extra = None
    ws.close()
    assert extra is None, extra
    print("PASS: Unsubscribe stops the live replay")

    ws = WsClient(timeout=5.0)
    ws.send({"type": "Subscribe", "symbols": ["KEUR"]})
    capped = [ws.read() for _ in range(4)]
    ws.close()
    for t in capped:
        print("capped:  ", t)
        assert t["type"] == "Trade", t
        assert t["symbol"] == "KEUR", t
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
    ws.send({"type": "Subscribe", "symbols": ["KEUR"]})
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
    ws.send({"type": "Subscribe", "symbols": ["KEUR"]})
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
    assert recovered["symbol"] == "KEUR", recovered
    assert recovered["ts_event"] >= WINDOW_START_TS, recovered
    print("PASS: GoDark drops blackout frames instead of buffering them")


if __name__ == "__main__":
    main()
