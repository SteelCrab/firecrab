#!/usr/bin/env python3
"""Check a test address's HTTPS response and WebSocket echo without a token."""

import argparse
import base64
import hashlib
import http.client
import json
import os
import re
import socket
import ssl
import sys
import uuid


class CheckError(Exception):
    """The address did not serve the expected test application."""


def validate_host(label):
    if not re.fullmatch(r"qa-[a-z0-9](?:[a-z0-9-]{0,56}[a-z0-9])?", label):
        raise CheckError("the test address label must start with qa- and contain only lowercase letters, digits, and hyphens")
    return label + ".firecrab.dev"


def validate_path(path):
    if not re.fullmatch(r"/[A-Za-z0-9._~/-]*", path):
        raise CheckError("paths must be absolute and contain no query, fragment, or escapes")
    return path


def check_https(host, path, expected, context):
    try:
        connection = http.client.HTTPSConnection(host, timeout=15, context=context)
        try:
            connection.request("GET", path, headers={"Cache-Control": "no-cache"})
            response = connection.getresponse()
            body = response.read(4097)
            if response.status != 200 or body != expected:
                raise CheckError("HTTPS status or response did not match the test fixture")
        finally:
            connection.close()
    except (OSError, http.client.HTTPException) as error:
        raise CheckError("HTTPS request failed") from error


def read_exact(stream, count):
    result = bytearray()
    while len(result) < count:
        data = stream.read(count - len(result))
        if not data:
            raise CheckError("WebSocket closed before the echo was received")
        result.extend(data)
    return bytes(result)


def read_headers(stream):
    data = bytearray()
    while not data.endswith(b"\r\n\r\n"):
        if len(data) >= 16384:
            raise CheckError("WebSocket handshake headers are too large")
        data.extend(read_exact(stream, 1))
    try:
        lines = data.decode("ascii").split("\r\n")
        status = lines.pop(0).split(" ", 2)
        if len(status) < 2 or status[:2] != ["HTTP/1.1", "101"]:
            raise CheckError("WebSocket upgrade was rejected")
        headers = {}
        for line in lines:
            if line:
                name, value = line.split(":", 1)
                headers.setdefault(name.lower(), []).append(value.strip())
        return headers
    except (UnicodeError, ValueError) as error:
        raise CheckError("invalid WebSocket handshake") from error


def check_websocket(host, path, context):
    key = base64.b64encode(os.urandom(16)).decode("ascii")
    accept = base64.b64encode(hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest()).decode("ascii")
    nonce = ("firecrab-qa-" + uuid.uuid4().hex).encode("ascii")
    try:
        with socket.create_connection((host, 443), timeout=15) as raw:
            with context.wrap_socket(raw, server_hostname=host) as tls:
                request = (f"GET {path} HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\n"
                           f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\n"
                           "Sec-WebSocket-Version: 13\r\n\r\n")
                tls.sendall(request.encode("ascii"))
                with tls.makefile("rb", buffering=0) as stream:
                    headers = read_headers(stream)
                    connection = ",".join(headers.get("connection", [])).lower().split(",")
                    if (headers.get("sec-websocket-accept") != [accept]
                            or headers.get("upgrade", [""])[0].lower() != "websocket"
                            or "upgrade" not in [part.strip() for part in connection]):
                        raise CheckError("invalid WebSocket upgrade response")
                    mask = os.urandom(4)
                    masked = bytes(value ^ mask[index % 4] for index, value in enumerate(nonce))
                    tls.sendall(bytes((0x81, 0x80 | len(nonce))) + mask + masked)
                    first, length = read_exact(stream, 2)
                    if first != 0x81 or length != len(nonce):
                        raise CheckError("invalid WebSocket echo frame")
                    if read_exact(stream, length) != nonce:
                        raise CheckError("WebSocket echo did not match")
    except (OSError, ssl.SSLError) as error:
        raise CheckError("WebSocket request failed") from error


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("label", help="owned, disposable qa- address label")
    parser.add_argument("--expect", required=True, help="exact non-secret HTTPS fixture body")
    parser.add_argument("--http-path", default="/")
    parser.add_argument("--ws-path", default="/ws")
    args = parser.parse_args()
    try:
        host = validate_host(args.label)
        http_path, ws_path = validate_path(args.http_path), validate_path(args.ws_path)
        expected = args.expect.encode("utf-8")
        if not expected or len(expected) > 4096:
            raise CheckError("expected fixture body must be 1–4096 bytes")
        context = ssl.create_default_context()
        check_https(host, http_path, expected, context)
        check_websocket(host, ws_path, context)
    except CheckError as error:
        print(f"Public Tunnel app check failed: {error}", file=sys.stderr)
        return 1
    print(json.dumps({"https": "passed", "websocketEcho": "passed", "host": host}, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
