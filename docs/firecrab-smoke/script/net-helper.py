#!/usr/bin/env python3
"""Manual smoke test for firecrab-net-helper.

Usage:
    python3 net-helper.py [socket-path] [operation]

operation defaults to ensure_firewall (still unimplemented) so the framing
check stays deterministic without privileges; pass ensure_bridge to exercise
the real bridge path as root (docs/firecrab-smoke/docs/bridge.md).

The helper should be running in another terminal, for example:
    FIRECRAB_NET_HELPER_SOCK=/tmp/firecrab-net.sock cargo run -p firecrab-net-helper
"""
import socket
import struct
import json
import os
import sys
import uuid

DEFAULT_PATH = "/tmp/firecrab-net.sock"
PATH = sys.argv[1] if len(sys.argv) > 1 else os.environ.get("FIRECRAB_NET_HELPER_SOCK", DEFAULT_PATH)
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
    if not header:
        return None  # helper가 연결을 끊음
    length = struct.unpack(">I", header)[0]
    return json.loads(recv_exact(sock, length))

s = socket.socket(socket.AF_UNIX)
try:
    s.connect(PATH)
except OSError as error:
    sys.exit(f"소켓 연결 실패 ({PATH}): {error} — helper 실행 여부와 경로를 확인하세요")

req = {"version": 1, "request_id": str(uuid.uuid4()),
       "request": {"operation": OPERATION}}
print("정상 요청  :", call(s, req))
print("버전 불일치:", call(s, dict(req, version=99)))
print("이후 종료  :", s.recv(4) == b"")
