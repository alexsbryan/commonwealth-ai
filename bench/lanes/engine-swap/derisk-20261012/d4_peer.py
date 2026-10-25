#!/usr/bin/env python3
"""A peer's memory, read over ggml-rpc's own wire, so the split is planned
against the number the peer advertises to the head (not a guess from the
model on disk).

HELLO is | cmd:1 | input_size:8 | conn_caps:24 | -> | size:8 | rsp:28 |;
GET_DEVICE_COUNT is | cmd:1 | size:8(=0) | -> | size:8 | count:4 |;
GET_DEVICE_MEMORY is | cmd:1 | size:8 | device:4 | -> | size:8 | free:8 |
total:8 |. Sizes from ggml-rpc.cpp:83-97 and transport.h:11.

A server may serve more than one device (Metal emulates virtual devices, so a
Mac can report a second one with 0 bytes). Only devices with a nonzero total
are this peer's real capacity, and a device index is that SERVER's index — the
name our planner writes into `-ot` is RPC<index>[host:port].

Prints `free_gib <n>`, `total_gib <n>`, `devices <real>/<all>`, and one
`device <i> <free_gib> <total_gib>` per real device. Exit 1 when no device
answers, because an unreadable peer is not a zero.
"""
import socket
import struct
import sys

CMD_GET_DEVICE_MEMORY = 11
CMD_HELLO = 14
CMD_DEVICE_COUNT = 15
CAPS = 24
HELLO_RSP = 4 + CAPS


def recv_exact(sock, n):
    out = b""
    while len(out) < n:
        chunk = sock.recv(n - len(out))
        if not chunk:
            raise ConnectionError(f"closed after {len(out)}/{n} bytes")
        out += chunk
    return out


def request(sock, cmd, payload, want):
    sock.sendall(struct.pack("<B", cmd) + struct.pack("<Q", len(payload)) + payload)
    size = struct.unpack("<Q", recv_exact(sock, 8))[0]
    if size != want:
        raise ConnectionError(f"cmd {cmd}: response {size} bytes, want {want}")
    return recv_exact(sock, size)


def probe(endpoint):
    host, _, port = endpoint.rpartition(":")
    sock = socket.create_connection((host, int(port)), timeout=15)
    try:
        request(sock, CMD_HELLO, bytes(CAPS), HELLO_RSP)
        count = struct.unpack("<I", request(sock, CMD_DEVICE_COUNT, b"", 4))[0]
        devices = []
        for i in range(count):
            free, total = struct.unpack("<QQ", request(sock, CMD_GET_DEVICE_MEMORY, struct.pack("<I", i), 16))
            devices.append((i, free, total))
    finally:
        sock.close()
    return devices


def main():
    if len(sys.argv) != 2:
        print("usage: d4_peer.py host:port", file=sys.stderr)
        return 2
    try:
        devices = probe(sys.argv[1])
    except (OSError, ConnectionError) as e:
        print(f"peer {sys.argv[1]}: {e}", file=sys.stderr)
        return 1
    real = [(i, f, t) for i, f, t in devices if t > 0]
    if not real:
        print(f"peer {sys.argv[1]}: {len(devices)} device(s), none with memory", file=sys.stderr)
        return 1
    gib = 2**30
    free = sum(f for _, f, _ in real) / gib
    total = sum(t for _, _, t in real) / gib
    print(f"free_gib {free:.1f}")
    print(f"total_gib {total:.1f}")
    print(f"devices {len(real)}/{len(devices)}")
    for i, f, t in real:
        print(f"device {i} {f / gib:.1f} {t / gib:.1f}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
