#!/usr/bin/env python3
"""Minimal manual client for firecrab-net-helper.

Usage:
    python3 net-helper-client.py <socket-path> [operation]

operation defaults to ensure_firewall. The helper must already be running
(see docs/tests/*.md for the relevant cargo run command).
"""
import json
import socket
import struct
import sys
import uuid

PATH = sys.argv[1] if len(sys.argv) > 1 else sys.exit("usage: net-helper-client.py <socket-path> [operation]")
OPERATION = sys.argv[2] if len(sys.argv) > 2 else "ensure_firewall"


def recv_exact(sock, length):
    chunks = []
    remaining = length
    while remaining:
        chunk = sock.recv(remaining)
        if not chunk:
            raise EOFError(f"helper closed connection while {remaining} bytes were still expected")
        chunks.append(chunk)
        remaining -= len(chunk)
    return b"".join(chunks)


def call(sock, envelope):
    payload = json.dumps(envelope).encode()
    sock.sendall(struct.pack(">I", len(payload)) + payload)
    header = recv_exact(sock, 4)
    length = struct.unpack(">I", header)[0]
    return json.loads(recv_exact(sock, length))


s = socket.socket(socket.AF_UNIX)
try:
    s.connect(PATH)
except OSError as error:
    sys.exit(f"소켓 연결 실패 ({PATH}): {error} — helper 실행 여부와 경로를 확인하세요")

req = {"version": 1, "request_id": str(uuid.uuid4()), "request": {"operation": OPERATION}}
print(call(s, req))
