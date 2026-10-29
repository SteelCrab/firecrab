#!/usr/bin/env python3
"""Loopback-only Linux regression for the standalone public-payload fixture."""
from contextlib import closing
import importlib.util
import json
import os
from pathlib import Path
import selectors
import socket
import struct
import subprocess
import sys
import threading
import time
import unittest

SCRIPT = Path(__file__).with_name("qa-sandbox-http-ws.py")
spec = importlib.util.spec_from_file_location("qa_echo", SCRIPT)
qa = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qa)
ORIGIN = "https://fixture.invalid"
HANDSHAKE = (b"GET /ws HTTP/1.1\r\nHost: fixture.invalid\r\nUpgrade: websocket\r\n"
             b"Connection: keep-alive, Upgrade\r\nSec-WebSocket-Version: 13\r\n"
             b"Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n")


def read_exact(connection, length):
    result = bytearray()
    while len(result) < length:
        part = connection.recv(length - len(result))
        if not part:
            raise AssertionError("Unexpected EOF")
        result.extend(part)
    return bytes(result)


def read_headers(connection):
    result = bytearray()
    while not result.endswith(b"\r\n\r\n"):
        result.extend(read_exact(connection, 1))
        if len(result) > 4096:
            raise AssertionError("Oversized response headers")
    return bytes(result)


def client_frame(opcode, payload=b"", *, final=True, reserved=0):
    # Fixed masks make malformed-frame regressions reproducible; public probes use fresh masks.
    mask = b"QA42"
    header = bytes([(0x80 if final else 0) | reserved | opcode])
    header += (bytes([0x80 | len(payload)]) if len(payload) < 126 else
               b"\xfe" + struct.pack("!H", len(payload)))
    return header + mask + bytes(value ^ mask[index % 4] for index, value in enumerate(payload))


def server_frame(connection):
    first, second = read_exact(connection, 2)
    if not first & 0x80 or first & 0x70 or second & 0x80:
        raise AssertionError("Server frame must be final, unmasked and have no RSV bits")
    length = second & 127
    if length == 126:
        length = struct.unpack("!H", read_exact(connection, 2))[0]
    if length > 4096:
        raise AssertionError("Oversized response frame")
    return first & 15, read_exact(connection, length)


class EchoTests(unittest.TestCase):
    def setUp(self):
        self.server = qa.EchoServer(("127.0.0.1", 0), allowed_origin=ORIGIN,
                                    connection_seconds=1)
        self.worker = threading.Thread(target=self.server.serve_forever,
                                       kwargs={"poll_interval": .01}, daemon=True)
        self.worker.start()
        self.address = self.server.server_address

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.worker.join(2)

    def connect(self):
        return socket.create_connection(self.address, timeout=2)

    def http(self, request):
        with closing(self.connect()) as connection:
            connection.sendall(request)
            headers = read_headers(connection)
            status = int(headers.split(b" ")[1])
            body = bytearray()
            try:
                while part := connection.recv(4096):
                    body.extend(part)
                    self.assertLessEqual(len(body), 4096)
            except ConnectionResetError:
                if status < 400:
                    raise
            self.assertIn(b"Cache-Control: no-store\r\n", headers)
            self.assertIn(b"X-Content-Type-Options: nosniff\r\n", headers)
            self.assertIn(f"Content-Length: {len(body)}\r\n".encode(), headers)
            return status, bytes(body)

    def ws(self):
        connection = self.connect()
        connection.sendall(HANDSHAKE)
        headers = read_headers(connection)
        self.assertTrue(headers.startswith(b"HTTP/1.1 101 "))
        self.assertIn(b"Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n", headers)
        self.assertIn(b"Cache-Control: no-store\r\n", headers)
        return connection

    def assert_close(self, packet, code):
        with closing(self.ws()) as connection:
            connection.sendall(packet)
            self.assertEqual(server_frame(connection), (8, struct.pack("!H", code)))
            try:
                self.assertEqual(connection.recv(1), b"")
            except ConnectionResetError:
                # Rejecting before reading an invalid payload can reset TCP after its close frame.
                pass

    def test_fixed_health_and_exact_http_payload_limit(self):
        self.assertEqual(self.http(b"GET /health HTTP/1.1\r\nHost: fixture\r\n\r\n"),
                         (200, qa.HEALTH))
        for body in (b"", b"public-qa-only", bytes(range(256)) * 16):
            with self.subTest(length=len(body)):
                request = (f"POST /echo HTTP/1.1\r\nHost: fixture\r\nContent-Length: {len(body)}\r\n\r\n".encode()
                           + body)
                self.assertEqual(self.http(request), (200, body))
        self.assertEqual(self.http(b"POST /echo HTTP/1.1\r\nHost: fixture\r\nContent-Length: 4097\r\n\r\n"), (413, b""))

    def test_no_file_environment_account_or_query_routes(self):
        for path in ("/", "/.env", "/../../etc/passwd", "/proc/self/environ",
                     "/source/probe.txt", "/health?file=/etc/passwd"):
            with self.subTest(path=path):
                request = f"GET {path} HTTP/1.1\r\nHost: fixture\r\nAuthorization: public-test-canary\r\n\r\n".encode()
                self.assertEqual(self.http(request), (404, b""))

    def test_ambiguous_http_framing_and_oversized_headers_fail_closed(self):
        cases = [
            (b"POST /echo HTTP/1.1\r\nHost: fixture\r\n\r\n", 411),
            (b"POST /echo HTTP/1.1\r\nHost: fixture\r\nContent-Length: -1\r\n\r\n", 400),
            (b"POST /echo HTTP/1.1\r\nHost: fixture\r\nContent-Length: 0\r\nContent-Length: 0\r\n\r\n", 400),
            (b"POST /echo HTTP/1.1\r\nHost: fixture\r\nTransfer-Encoding: chunked\r\n\r\n", 400),
            (b"GET /health HTTP/1.1\r\nHost: fixture\r\n Folded: header\r\n\r\n", 400),
            (b"GET /health HTTP/1.1\r\n\r\n", 400),
            (b"GET /health HTTP/1.0\r\nHost: fixture\r\n\r\n", 505),
            (b"GET /health HTTP/1.1\r\nHost: fixture\r\nX-Pad: " + b"x" * 4096, 431),
        ]
        for request, status in cases:
            with self.subTest(status=status, prefix=request[:40]):
                self.assertEqual(self.http(request), (status, b""))

    def test_bad_handshakes_never_upgrade(self):
        for request, status in [
            (HANDSHAKE.replace(b"GET ", b"POST ", 1), 400),
            (HANDSHAKE.replace(b"Version: 13", b"Version: 12"), 400),
            (HANDSHAKE.replace(b"websocket", b"http"), 400),
            (HANDSHAKE.replace(b"keep-alive, Upgrade", b"keep-alive"), 400),
            (HANDSHAKE.replace(b"dGhlIHNhbXBsZSBub25jZQ==", b"not-base64"), 400),
            (HANDSHAKE.replace(b"dGhlIHNhbXBsZSBub25jZQ==", b"YQ=="), 400),
            (HANDSHAKE.replace(b"\r\n\r\n", b"\r\nHost: duplicate\r\n\r\n"), 400),
            (HANDSHAKE.replace(b"\r\n\r\n", b"\r\nContent-Length: 1\r\n\r\n"), 400),
            (HANDSHAKE.replace(b"\r\n\r\n", b"\r\nOrigin: https://wrong.invalid\r\n\r\n"), 403),
        ]:
            with self.subTest(status=status, request=request):
                self.assertEqual(self.http(request), (status, b""))

    def test_exact_browser_origin_and_unnegotiated_compression(self):
        request = HANDSHAKE.replace(b"\r\n\r\n", b"\r\nOrigin: https://fixture.invalid\r\n"
                                     b"Sec-WebSocket-Extensions: permessage-deflate\r\n\r\n")
        with closing(self.connect()) as connection:
            connection.sendall(request)
            headers = read_headers(connection)
            self.assertTrue(headers.startswith(b"HTTP/1.1 101 "))
            self.assertNotIn(b"Sec-WebSocket-Extensions:", headers)
            connection.sendall(client_frame(8))
            self.assertEqual(server_frame(connection), (8, b""))
        self.server.allowed_origin = None
        self.assertEqual(self.http(request), (403, b""))

    def test_text_binary_fragments_ping_pong_and_close(self):
        with closing(self.ws()) as connection:
            for opcode, payload in ((1, "검증용 echo".encode()), (2, bytes(range(256)) * 16)):
                connection.sendall(client_frame(opcode, payload))
                self.assertEqual(server_frame(connection), (opcode, payload))
            connection.sendall(client_frame(1, b"\xe2", final=False)
                               + client_frame(9, b"public-ping")
                               + client_frame(10, b"unsolicited-pong")
                               + client_frame(0, b"\x82\xac"))
            self.assertEqual(server_frame(connection), (10, b"public-ping"))
            self.assertEqual(server_frame(connection), (1, "€".encode()))
            close = struct.pack("!H", 1000) + b"qa-complete"
            connection.sendall(client_frame(8, close))
            self.assertEqual(server_frame(connection), (8, close))
            self.assertEqual(connection.recv(1), b"")

    def test_protocol_mask_reserved_opcode_control_and_length_errors(self):
        packets = [b"\x81\x01x", client_frame(1, b"x", reserved=0x40),
                   client_frame(3), client_frame(9, b"x", final=False),
                   client_frame(9, b"x" * 126), b"\x81\xfe\x00\x01",
                   b"\x81\xff" + struct.pack("!Q", 65535),
                   b"\x81\xff" + struct.pack("!Q", 1 << 63),
                   client_frame(0, b"orphan"),
                   client_frame(1, b"first", final=False) + client_frame(2, b"second")]
        for packet in packets:
            with self.subTest(packet=packet[:16]):
                self.assert_close(packet, 1002)

    def test_oversized_single_and_fragmented_messages_close_1009(self):
        self.assert_close(b"\x82\xfe" + struct.pack("!H", 4097), 1009)
        self.assert_close(b"\x82\xff" + struct.pack("!Q", 65536), 1009)
        self.assert_close(client_frame(2, b"x" * 3000, final=False)
                          + client_frame(0, b"x" * 1097), 1009)

    def test_invalid_text_and_close_payloads_fail_closed(self):
        for packet, code in [(client_frame(1, b"\xff"), 1007),
                             (client_frame(8, b"x"), 1002),
                             (client_frame(8, struct.pack("!H", 1005)), 1002),
                             (client_frame(8, struct.pack("!H", 1000) + b"\xff"), 1007)]:
            with self.subTest(packet=packet):
                self.assert_close(packet, code)

    def test_frame_and_message_budgets(self):
        for opcode, count, response_opcode in ((1, 16, 1), (9, 64, 10)):
            with self.subTest(opcode=opcode), closing(self.ws()) as connection:
                connection.sendall(client_frame(opcode, b"public") * count)
                for _ in range(count):
                    self.assertEqual(server_frame(connection), (response_opcode, b"public"))
                self.assertEqual(server_frame(connection), (8, struct.pack("!H", 1008)))

    def test_only_four_connections_can_hold_workers(self):
        connections = [self.ws() for _ in range(4)]
        try:
            with closing(self.connect()) as fifth:
                fifth.sendall(b"GET /health HTTP/1.1\r\nHost: fixture\r\n\r\n")
                try:
                    self.assertEqual(fifth.recv(1), b"")
                except ConnectionResetError:
                    pass
        finally:
            for connection in connections:
                connection.close()

    def test_slow_drip_headers_cannot_extend_absolute_deadline(self):
        self.server.connection_seconds = .25
        with closing(self.connect()) as connection:
            started = time.monotonic()
            def drip():
                try:
                    while True:
                        connection.sendall(b"G")
                        time.sleep(.025)
                except OSError:
                    pass
            sender = threading.Thread(target=drip, daemon=True)
            sender.start()
            self.assertEqual(connection.recv(1), b"")
            self.assertLess(time.monotonic() - started, .9)
        sender.join(1)


class EntrypointTests(unittest.TestCase):
    def test_confined_process_lifetime_and_health(self):
        qa.confined_identity()
        with subprocess.Popen([sys.executable, "-B", str(SCRIPT), "--port", "0",
                               "--lifetime-seconds", "1"], stdout=subprocess.PIPE,
                              stderr=subprocess.PIPE) as child:
            with selectors.DefaultSelector() as selector:
                selector.register(child.stdout, selectors.EVENT_READ)
                self.assertTrue(selector.select(3), "Startup must be bounded")
            line = child.stdout.readline(4096)
            status = json.loads(line)
            self.assertEqual(status["host"], "127.0.0.1")
            self.assertEqual(status["uid"], os.getuid())
            self.assertTrue(status["noNewPrivs"])
            with closing(socket.create_connection(("127.0.0.1", status["port"]), timeout=2)) as connection:
                connection.sendall(b"GET /health HTTP/1.1\r\nHost: fixture\r\n\r\n")
                headers = read_headers(connection)
                self.assertTrue(headers.startswith(b"HTTP/1.1 200 "))
                self.assertEqual(read_exact(connection, len(qa.HEALTH)), qa.HEALTH)
            self.assertEqual(child.wait(timeout=4), 0)
            self.assertEqual(child.stderr.read(4096), b"")

    def test_invalid_listener_lifetime_and_origin_arguments(self):
        for arguments in (("--host", "192.0.2.10"), ("--port", "80"),
                          ("--lifetime-seconds", "901"), ("--origin", "https://x.invalid/path"),
                          ("--origin", "https://user:password@x.invalid"), ("--origin", "https://[")):
            with self.subTest(arguments=arguments):
                result = subprocess.run([sys.executable, "-B", str(SCRIPT), *arguments],
                                        capture_output=True, timeout=3)
                self.assertEqual(result.returncode, 2)
                self.assertEqual(result.stdout, b"")
                self.assertNotIn(b"Traceback", result.stderr)


if __name__ == "__main__":
    unittest.main(verbosity=2)
