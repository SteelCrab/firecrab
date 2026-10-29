#!/usr/bin/env python3
"""Exercise the disposable Tunnel app and its OCI image without a public service."""

import base64
import contextlib
import gzip
import hashlib
import http.client
import io
import json
from pathlib import Path
import socket
import struct
import subprocess
import tarfile
import tempfile
import time
import unittest

import public_tunnel_fixture as fixture


def fake_elf(machine=62, dynamic=False):
    data = bytearray(120)
    data[:6] = b"\x7fELF\x02\x01"
    struct.pack_into("<HH", data, 16, 2, machine)
    struct.pack_into("<Q", data, 32, 64)
    struct.pack_into("<HH", data, 54, 56, 1)
    struct.pack_into("<I", data, 64, 3 if dynamic else 1)
    return bytes(data)


def read_exact(sock, size):
    data = b""
    while len(data) < size:
        received = sock.recv(size - len(data))
        if not received:
            raise EOFError("fixture closed the socket")
        data += received
    return data


class FixtureTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory()
        cls.binary = Path(cls.temp.name) / "fixture"
        subprocess.run(["rustc", "--edition=2021", "-O", str(Path(__file__).with_name("public_tunnel_fixture.rs")),
                        "-o", str(cls.binary)], check=True, capture_output=True)

    @classmethod
    def tearDownClass(cls):
        cls.temp.cleanup()

    def launch(self, label):
        with socket.socket() as reserve:
            reserve.bind(("127.0.0.1", 0))
            port = reserve.getsockname()[1]
        process = subprocess.Popen([str(self.binary), label, f"127.0.0.1:{port}"],
                                   stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        self.addCleanup(self.stop, process)
        deadline = time.monotonic() + 5
        while True:
            try:
                with socket.create_connection(("127.0.0.1", port), timeout=.2):
                    return port
            except OSError:
                if process.poll() is not None or time.monotonic() >= deadline:
                    self.fail("fixture did not start")
                time.sleep(.02)

    @staticmethod
    def stop(process):
        process.terminate()
        try:
            process.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.communicate(timeout=5)

    def test_distinct_http_targets_and_websocket_echo(self):
        for label in ("A", "B"):
            with self.subTest(label=label):
                port = self.launch(label)
                conn = http.client.HTTPConnection("127.0.0.1", port, timeout=3)
                self.addCleanup(conn.close)
                for path, expected in (("/app", f"fixture-{label}".encode()), ("/health", b"healthy")):
                    conn.request("GET", path)
                    response = conn.getresponse()
                    self.assertEqual((response.status, response.read()), (200, expected))
                key = base64.b64encode(b"0123456789abcdef").decode()
                with socket.create_connection(("127.0.0.1", port), timeout=3) as stream:
                    stream.sendall(("GET /ws HTTP/1.1\r\nHost: test\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n"
                                    f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n").encode())
                    headers = b""
                    while not headers.endswith(b"\r\n\r\n"):
                        headers += read_exact(stream, 1)
                    accept = base64.b64encode(hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest())
                    self.assertTrue(headers.startswith(b"HTTP/1.1 101 "))
                    self.assertIn(b"Sec-WebSocket-Accept: " + accept, headers)
                    message, mask = b"qa-echo-123", b"\0\xff\1\x80"
                    masked = bytes(byte ^ mask[i % 4] for i, byte in enumerate(message))
                    stream.sendall(bytes((0x81, 0x80 | len(message))) + mask + masked)
                    self.assertEqual(read_exact(stream, 2), bytes((0x81, len(message))))
                    self.assertEqual(read_exact(stream, len(message)), message)

    def test_image_contains_only_the_static_fixture_and_known_label(self):
        image = fake_elf()
        first = fixture.image_fixture(image, "A")
        self.assertEqual(first, fixture.image_fixture(image, "A"))
        digest, raw_manifest, blobs = first
        self.assertEqual(digest, fixture.digest(raw_manifest))
        manifest = json.loads(raw_manifest)
        config = json.loads(blobs[manifest["config"]["digest"]])
        self.assertEqual(config["architecture"], "amd64")
        self.assertEqual(config["config"]["Entrypoint"], ["/bin/public-tunnel-fixture", "A"])
        raw_layer = gzip.decompress(blobs[manifest["layers"][0]["digest"]])
        self.assertEqual(config["rootfs"]["diff_ids"], [fixture.digest(raw_layer)])
        with tarfile.open(fileobj=io.BytesIO(raw_layer)) as archive:
            self.assertEqual(archive.getnames(), ["bin", "bin/public-tunnel-fixture"])
            self.assertEqual(archive.extractfile("bin/public-tunnel-fixture").read(), image)
        self.assertNotEqual(digest, fixture.image_fixture(image, "B")[0])

    def test_connection_limit_preserves_service_after_slow_clients_disconnect(self):
        port = self.launch("A")
        clients = []
        self.addCleanup(lambda: [client.close() for client in clients])
        for _ in range(16):
            client = socket.create_connection(("127.0.0.1", port), timeout=3)
            client.sendall(b"G")
            clients.append(client)
        deadline = time.monotonic() + 3
        while True:
            with socket.create_connection(("127.0.0.1", port), timeout=3) as extra:
                extra.sendall(b"GET /health HTTP/1.1\r\nHost: test\r\n\r\n")
                try:
                    refused = extra.recv(1) == b""
                except ConnectionResetError:
                    refused = True
            if refused:
                break
            if time.monotonic() >= deadline:
                self.fail("fixture did not enforce its connection limit")
            time.sleep(.02)
        for client in clients:
            client.close()
        clients.clear()
        deadline = time.monotonic() + 5
        while True:
            try:
                with contextlib.closing(http.client.HTTPConnection("127.0.0.1", port, timeout=1)) as conn:
                    conn.request("GET", "/health")
                    response = conn.getresponse()
                    self.assertEqual((response.status, response.read()), (200, b"healthy"))
                    break
            except (OSError, http.client.HTTPException):
                if time.monotonic() >= deadline:
                    raise
                time.sleep(.02)

    def test_rejects_unsafe_or_unusable_images(self):
        for binary in [b"not elf", fake_elf(dynamic=True), fake_elf(machine=40), fake_elf()[:80]]:
            with self.subTest(binary=binary[:20]), self.assertRaises(ValueError):
                fixture.image_fixture(binary, "A")
        with self.assertRaises(ValueError):
            fixture.image_fixture(fake_elf(), "other")


if __name__ == "__main__":
    unittest.main()
