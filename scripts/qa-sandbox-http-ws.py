#!/usr/bin/env python3
"""Bounded public-payload fixture. It has no file, environment or account routes."""
import argparse
import base64
import binascii
import hashlib
import json
import os
from pathlib import Path
import re
import resource
import socket
import socketserver
import struct
import threading
import time
from urllib.parse import urlsplit

PAYLOAD_LIMIT = 4096
HEADER_LIMIT = 4096
FRAME_LIMIT = 64
MESSAGE_LIMIT = 16
HEALTH = b'{"fixture":"firecrab-414-http-ws","ok":true}\n'
GUID = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
HEADER_NAME = re.compile(r"[!#$%&'*+.^_`|~0-9A-Za-z-]+\Z")
CLOSE_CODES = {1000, 1001, 1002, 1003, 1007, 1008, 1009, 1010, 1011, 1012, 1013, 1014}


class ProtocolError(Exception):
    """A fixed HTTP status or WebSocket close code, never an input echo."""


class Reader:
    """Exact reads share one absolute deadline, including slow header/frame senders."""

    def __init__(self, connection, seconds):
        self.connection = connection
        self.deadline = time.monotonic() + seconds

    def read(self, length):
        result = bytearray()
        while len(result) < length:
            remaining = self.deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError
            self.connection.settimeout(min(remaining, 2))
            part = self.connection.recv(length - len(result))
            if not part:
                raise ConnectionError
            result.extend(part)
        return bytes(result)

    def headers(self):
        block = bytearray()
        while len(block) < HEADER_LIMIT:
            block.extend(self.read(1))
            if block.endswith(b"\r\n\r\n"):
                return bytes(block)
        raise ProtocolError(431)


def request_headers(block):
    """Reject duplicate, folded and ambiguous HTTP framing before any upgrade."""
    try:
        lines = block.decode("ascii").split("\r\n")
        method, path, version = lines[0].split(" ")
    except (UnicodeError, ValueError):
        raise ProtocolError(400) from None
    if version != "HTTP/1.1":
        raise ProtocolError(505)
    headers = {}
    for line in lines[1:-2]:
        name, separator, value = line.partition(":")
        if not separator or not HEADER_NAME.fullmatch(name):
            raise ProtocolError(400)
        value = value.strip(" \t")
        name = name.lower()
        if name in headers or any(ord(c) < 32 or ord(c) == 127 for c in value):
            raise ProtocolError(400)
        headers[name] = value
    if not headers.get("host") or any(name in headers for name in
                                      ("transfer-encoding", "expect", "content-encoding")):
        raise ProtocolError(400)
    raw_length = headers.get("content-length", "0")
    if not re.fullmatch(r"[0-9]+", raw_length):
        raise ProtocolError(400)
    length = int(raw_length)
    if length > PAYLOAD_LIMIT:
        raise ProtocolError(413)
    return method, path, headers, length


def websocket_key(method, headers, length, allowed_origin):
    """Negotiate version 13 without extensions, compression or a subprotocol."""
    if (method != "GET" or length or headers.get("upgrade", "").lower() != "websocket"
            or "upgrade" not in {part.strip().lower() for part in
                                 headers.get("connection", "").split(",")}
            or headers.get("sec-websocket-version") != "13"):
        raise ProtocolError(400)
    if "origin" in headers and headers["origin"] != allowed_origin:
        raise ProtocolError(403)
    key = headers.get("sec-websocket-key", "")
    try:
        decoded = base64.b64decode(key, validate=True)
    except (ValueError, binascii.Error):
        raise ProtocolError(400) from None
    if len(decoded) != 16 or base64.b64encode(decoded).decode("ascii") != key:
        raise ProtocolError(400)
    return base64.b64encode(hashlib.sha1(key.encode("ascii") + GUID).digest())


def frame(connection, opcode, payload):
    """The server always sends final, unmasked frames with a bounded payload."""
    header = bytes([0x80 | opcode])
    header += bytes([len(payload)]) if len(payload) < 126 else b"\x7e" + struct.pack("!H", len(payload))
    connection.sendall(header + payload)


def read_frame(reader):
    """Client frames must be masked, minimally encoded and use only known opcodes."""
    first, second = reader.read(2)
    final, opcode = bool(first & 0x80), first & 15
    if first & 0x70 or not second & 0x80 or opcode not in (0, 1, 2, 8, 9, 10):
        raise ProtocolError(1002)
    length = second & 127
    if length == 126:
        length = struct.unpack("!H", reader.read(2))[0]
        if length < 126:
            raise ProtocolError(1002)
    elif length == 127:
        length = struct.unpack("!Q", reader.read(8))[0]
        if length < 65536 or length >> 63:
            raise ProtocolError(1002)
    if opcode >= 8 and (not final or length > 125):
        raise ProtocolError(1002)
    if length > PAYLOAD_LIMIT:
        raise ProtocolError(1009)
    mask = reader.read(4)
    payload = reader.read(length)
    return final, opcode, bytes(value ^ mask[index % 4] for index, value in enumerate(payload))


def validate_close(payload):
    """Reserved codes and invalid UTF-8 close reasons fail closed."""
    if not payload:
        return
    if len(payload) == 1:
        raise ProtocolError(1002)
    code = struct.unpack("!H", payload[:2])[0]
    if code not in CLOSE_CODES and not 3000 <= code <= 4999:
        raise ProtocolError(1002)
    payload[2:].decode("utf-8")


def websocket(connection, reader):
    """Echo complete text/binary messages; control frames can interrupt fragmentation."""
    fragments, fragment_opcode, messages = bytearray(), None, 0
    try:
        for _ in range(FRAME_LIMIT):
            final, opcode, payload = read_frame(reader)
            if opcode == 8:
                validate_close(payload)
                frame(connection, 8, payload)
                return
            if opcode == 9:
                frame(connection, 10, payload)
                continue
            if opcode == 10:
                continue
            if opcode == 0:
                if fragment_opcode is None:
                    raise ProtocolError(1002)
                fragments.extend(payload)
            else:
                if fragment_opcode is not None:
                    raise ProtocolError(1002)
                fragment_opcode = opcode
                fragments.extend(payload)
            if len(fragments) > PAYLOAD_LIMIT:
                raise ProtocolError(1009)
            if not final:
                continue
            if fragment_opcode == 1:
                fragments.decode("utf-8")
            frame(connection, fragment_opcode, bytes(fragments))
            fragments.clear()
            fragment_opcode = None
            messages += 1
            if messages == MESSAGE_LIMIT:
                break
        frame(connection, 8, struct.pack("!H", 1008))
    except UnicodeError:
        frame(connection, 8, struct.pack("!H", 1007))
    except ProtocolError as error:
        frame(connection, 8, struct.pack("!H", error.args[0]))


class Handler(socketserver.BaseRequestHandler):
    """One HTTP request or one bounded WebSocket session; never serve a directory."""

    def respond(self, status, body=b"", content_type="application/octet-stream", extra=b""):
        headers = (f"HTTP/1.1 {status} QA\r\nContent-Length: {len(body)}\r\n"
                   f"Content-Type: {content_type}\r\nCache-Control: no-store\r\n"
                   "X-Content-Type-Options: nosniff\r\nConnection: close\r\n").encode("ascii")
        self.request.sendall(headers + extra + b"\r\n" + body)

    def handle(self):
        reader = Reader(self.request, self.server.connection_seconds)
        try:
            try:
                method, path, headers, length = request_headers(reader.headers())
                if path == "/ws":
                    accept = websocket_key(method, headers, length, self.server.allowed_origin)
                    self.request.sendall(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n"
                                         b"Connection: Upgrade\r\nCache-Control: no-store\r\n"
                                         b"X-Content-Type-Options: nosniff\r\nSec-WebSocket-Accept: "
                                         + accept + b"\r\n\r\n")
                    websocket(self.request, reader)
                    return
                if "upgrade" in headers or length and method != "POST":
                    raise ProtocolError(400)
                if method == "GET" and path == "/health":
                    self.respond(200, HEALTH, "application/json")
                elif method == "POST" and path == "/echo":
                    if "content-length" not in headers:
                        raise ProtocolError(411)
                    self.respond(200, reader.read(length))
                else:
                    self.respond(404)
            except ProtocolError as error:
                self.respond(error.args[0])
        except (OSError, ConnectionError, TimeoutError):
            pass


class EchoServer(socketserver.ThreadingMixIn, socketserver.TCPServer):
    """Reject excess connections instead of creating an unbounded worker queue."""
    allow_reuse_address = True
    daemon_threads = True
    request_queue_size = 4

    def __init__(self, address, *, allowed_origin=None, connection_seconds=10):
        self.allowed_origin = allowed_origin
        self.connection_seconds = connection_seconds
        self.slots = threading.BoundedSemaphore(4)
        super().__init__(address, Handler)

    def process_request(self, request, client_address):
        if not self.slots.acquire(blocking=False):
            self.shutdown_request(request)
            return
        try:
            super().process_request(request, client_address)
        except Exception:
            self.slots.release()
            raise

    def process_request_thread(self, request, client_address):
        try:
            super().process_request_thread(request, client_address)
        finally:
            self.slots.release()

    def handle_error(self, request, client_address):
        # No request path, headers, payload, exception or environment enters logs.
        pass


def confined_identity():
    """Read only this process's privilege metadata, never guest account or key files."""
    status = dict(line.split(":", 1) for line in Path("/proc/self/status").read_text().splitlines()
                  if ":" in line)
    if (os.getuid() == 0 or os.geteuid() == 0 or int(status["NoNewPrivs"].strip()) != 1
            or int(status["CapEff"].strip(), 16)):
        raise ValueError("Run as a non-root UID with NoNewPrivs=1 and CapEff=0")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", choices=("127.0.0.1", "0.0.0.0"), default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8080)
    parser.add_argument("--origin", help="Exact browser Origin; absent Origin is allowed for native QA clients")
    parser.add_argument("--lifetime-seconds", type=int, default=300)
    args = parser.parse_args()
    if args.port != 0 and not 1024 <= args.port <= 65535 or not 1 <= args.lifetime_seconds <= 900:
        parser.error("Use port 0 or 1024..65535 and a lifetime of 1..900 seconds")
    if args.origin:
        try:
            origin = urlsplit(args.origin)
        except ValueError:
            parser.error("Origin must be a literal http(s) origin")
        if (origin.scheme not in ("http", "https") or not origin.hostname or origin.username
                or origin.password or origin.path or origin.query or origin.fragment
                or any(c.isspace() or ord(c) < 32 for c in args.origin)):
            parser.error("Origin must be a literal http(s) origin without credentials, path or query")
    try:
        confined_identity()
    except (OSError, ValueError, KeyError):
        parser.error("Run on Linux as a non-root UID with NoNewPrivs=1 and CapEff=0")
    for kind, maximum in ((resource.RLIMIT_NOFILE, 64), (resource.RLIMIT_AS, 128 * 1024 * 1024),
                          (resource.RLIMIT_CPU, 15), (resource.RLIMIT_FSIZE, 0), (resource.RLIMIT_CORE, 0)):
        resource.setrlimit(kind, (maximum, maximum))
    with EchoServer((args.host, args.port), allowed_origin=args.origin) as server:
        server.timeout = 0.2
        print(json.dumps({"fixture": "firecrab-414-http-ws", "host": args.host,
                          "port": server.server_address[1], "uid": os.getuid(), "noNewPrivs": True}), flush=True)
        deadline = time.monotonic() + args.lifetime_seconds
        while time.monotonic() < deadline:
            server.handle_request()


if __name__ == "__main__":
    main()
