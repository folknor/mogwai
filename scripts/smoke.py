#!/usr/bin/env python3
"""End-to-end smoke test of the mogwai fake broker.

Arms a partial-fill divergence over the control plane, then submits an order over
the native WS gateway and checks the engine emits Accepted + a partial Filled
(leaves_qty > 0). Uses only the stdlib so there's nothing to install.
"""
import json
import socket
import urllib.request

HOST, PORT = "127.0.0.1", 8787


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
    s = socket.create_connection((HOST, PORT))
    key = "x3JJHMbDL1EzLkh9GBhXDw=="  # static key is fine for a smoke test
    s.sendall(
        f"GET /ws HTTP/1.1\r\nHost: {HOST}:{PORT}\r\nUpgrade: websocket\r\n"
        f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\n"
        f"Sec-WebSocket-Version: 13\r\n\r\n".encode()
    )
    buf = s.recv(4096)
    assert b"101 Switching Protocols" in buf, buf

    s.sendall(_encode(json.dumps(send_obj)))
    out = []
    for _ in range(expect):
        out.append(json.loads(_read_text(s)))
    s.close()
    return out


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
    b0 = s.recv(1)
    b1 = s.recv(1)[0]
    n = b1 & 0x7F
    if n == 126:
        n = int.from_bytes(s.recv(2), "big")
    elif n == 127:
        n = int.from_bytes(s.recv(8), "big")
    payload = b""
    while len(payload) < n:
        payload += s.recv(n - len(payload))
    return payload.decode()


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
        expect=2,
    )
    accepted, filled = msgs
    print("accepted:", accepted)
    print("filled:  ", filled)

    assert accepted["type"] == "OrderAccepted", accepted
    assert filled["type"] == "OrderFilled", filled
    assert float(filled["last_qty"]) == 3.0, filled
    assert float(filled["leaves_qty"]) == 7.0, filled
    print("PASS: partial fill round-tripped through the live WS path")


if __name__ == "__main__":
    main()
