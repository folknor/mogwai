#!/usr/bin/env python3
"""End-to-end smoke test of the mogwai fake broker.

Arms a partial-fill divergence over the control plane, then submits an order over
the native WS gateway and checks the engine emits Accepted + a partial Filled
(leaves_qty > 0). Uses only the stdlib so there's nothing to install.

Connects to a server at 127.0.0.1:8787; it does not start one. The windowing,
Unsubscribe-stop, and gap-cap steps need PACED replay against the bundled
fixture, so launch the server with a non-zero speed (at the default speed 0.0
those steps race and fail spuriously):

    MOGWAI_DATA_DIR=scripts/fixtures/replay MOGWAI_REPLAY_SPEED=1 brokkr run -p mogwai-server

then run this script.
"""
import json
import socket
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


def ws_roundtrip(send_obj: dict, expect: int) -> list:
    """Minimal RFC6455 client: handshake, send one text frame, read `expect` frames."""
    ws = WsClient()
    ws.send(send_obj)
    out = [ws.read() for _ in range(expect)]
    ws.close()
    return out


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

    msgs = ws_roundtrip(
        {
            "type": "SubmitOrder",
            "client_order_id": "O1",
            "symbol": "BTCUSDT",
            "side": "Buy",
            "order_type": "Limit",
            "quantity": "10",
            "price": "100",
            "time_in_force": "Gtc",
        },
        expect=3,
    )
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

    # Market-data replay: subscribe to a small pair and read the first 2 trades.
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

    ws = WsClient(timeout=3.0)
    ws.send({"type": "Subscribe", "symbols": ["KEUR"]})
    capped = [ws.read() for _ in range(4)]
    ws.close()
    for t in capped:
        print("capped:  ", t)
        assert t["type"] == "Trade", t
        assert t["symbol"] == "KEUR", t
    print("PASS: capped paced replay crosses a large historical gap")


if __name__ == "__main__":
    main()
