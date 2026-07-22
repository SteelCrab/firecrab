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

PATH = sys.argv[1] if len(sys.argv) > 1 else sys.exit(
    "usage: net-helper-client.py <socket-path> [operation] [key=value ...]"
)
OPERATION = sys.argv[2] if len(sys.argv) > 2 else "ensure_firewall"

# Operations beyond ensure_bridge/ensure_firewall take extra fields
# (create_tap/delete_tap need vm_id, apply_vm_policy needs more) — the
# request is internally tagged on "operation", so these merge in alongside
# it. Passing a bare UUID with no "vm_id=" prefix is silently dropped by the
# server as an unparseable request (no vm_id field at all), which looks like
# a hang/EOF on the client side, not a helpful error — always use key=value.
EXTRA_FIELDS = {}
for arg in sys.argv[3:]:
    key, sep, value = arg.partition("=")
    if not sep:
        sys.exit(f"잘못된 인자 {arg!r} — key=value 형식으로 주세요 (예: vm_id=<uuid>)")
    EXTRA_FIELDS[key] = value


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

req = {
    "version": 1,
    "request_id": str(uuid.uuid4()),
    "request": {"operation": OPERATION, **EXTRA_FIELDS},
}
print(call(s, req))
