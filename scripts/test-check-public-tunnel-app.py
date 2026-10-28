#!/usr/bin/env python3
"""Exercise public application checks against an isolated local TLS server."""

import base64
import hashlib
import http.server
import importlib.util
from pathlib import Path
import socket
import ssl
import subprocess
import tempfile
import threading
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location("public_app", Path(__file__).with_name("check-public-tunnel-app.py"))
check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check)


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_GET(self):
        self.server.requests.append((self.path, dict(self.headers)))
        if self.path == "/app":
            body = b"fixture-a"
            self.send_response(self.server.http_status)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        key = self.headers["Sec-WebSocket-Key"]
        accept = base64.b64encode(hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest()).decode()
        if self.server.bad_accept:
            accept = "wrong"
        self.send_response(101)
        self.send_header("Connection", "Upgrade")
        self.send_header("Upgrade", "websocket")
        self.send_header("Sec-WebSocket-Accept", accept)
        self.end_headers()
        if self.server.bad_accept:
            return
        first, size = check.read_exact(self.rfile, 2)
        if first != 0x81 or not size & 0x80:
            return
        mask = check.read_exact(self.rfile, 4)
        payload = check.read_exact(self.rfile, size & 0x7f)
        message = bytes(byte ^ mask[index % 4] for index, byte in enumerate(payload))
        if self.server.bad_echo:
            message = b"x" * len(message)
        self.wfile.write(bytes((0x81, len(message))) + message)
        self.close_connection = True

    def log_message(self, *args):
        pass


class PublicAppCheck(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.directory = tempfile.TemporaryDirectory()
        cert = Path(cls.directory.name) / "cert.pem"
        key = Path(cls.directory.name) / "key.pem"
        subprocess.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
                        "-keyout", str(key), "-out", str(cert), "-days", "1",
                        "-subj", "/CN=qa-a.firecrab.dev", "-addext", "subjectAltName=DNS:qa-a.firecrab.dev"],
                       check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        cls.context = ssl.create_default_context(cafile=str(cert))
        server_context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        server_context.load_cert_chain(cert, key)
        cls.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        cls.server.socket = server_context.wrap_socket(cls.server.socket, server_side=True)
        cls.thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.thread.start()

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()
        cls.thread.join()
        cls.directory.cleanup()

    def setUp(self):
        self.server.requests = []
        self.server.http_status = 200
        self.server.bad_accept = False
        self.server.bad_echo = False
        original_connect = socket.create_connection

        def connect(address, *args, **kwargs):
            self.assertEqual(address[1], 443)
            return original_connect(("127.0.0.1", self.server.server_port), *args, **kwargs)

        self.connection_patch = patch.object(socket, "create_connection", side_effect=connect)
        self.connection_patch.start()
        self.addCleanup(self.connection_patch.stop)

    def test_http_and_websocket_round_trip_over_verified_tls(self):
        check.check_https("qa-a.firecrab.dev", "/app", b"fixture-a", self.context)
        check.check_websocket("qa-a.firecrab.dev", "/ws", self.context)
        self.assertEqual([item[0] for item in self.server.requests], ["/app", "/ws"])
        self.assertTrue(all("Authorization" not in headers for _, headers in self.server.requests))

    def test_unexpected_status_body_and_certificate_fail(self):
        self.server.http_status = 302
        with self.assertRaises(check.CheckError):
            check.check_https("qa-a.firecrab.dev", "/app", b"fixture-a", self.context)
        self.server.http_status = 200
        with self.assertRaises(check.CheckError):
            check.check_https("qa-a.firecrab.dev", "/app", b"fixture-b", self.context)
        with self.assertRaises(check.CheckError):
            check.check_https("qa-b.firecrab.dev", "/app", b"fixture-a", self.context)

    def test_invalid_upgrade_and_wrong_echo_fail(self):
        self.server.bad_accept = True
        with self.assertRaises(check.CheckError):
            check.check_websocket("qa-a.firecrab.dev", "/ws", self.context)
        self.server.bad_accept = False
        self.server.bad_echo = True
        with self.assertRaises(check.CheckError):
            check.check_websocket("qa-a.firecrab.dev", "/ws", self.context)

    def test_rejects_unowned_address_shapes_and_unsafe_paths(self):
        for label in ["console", "qa-", "qa-a.firecrab.dev", "qa-a\r\nX: b", "qa-A", "qa-" + "a" * 61]:
            with self.subTest(label=label), self.assertRaises(check.CheckError):
                check.validate_host(label)
        for path in ["//evil?token=a", "/ws#x", "/ws\r\nX: b", "ws"]:
            with self.subTest(path=path), self.assertRaises(check.CheckError):
                check.validate_path(path)
        self.assertEqual(check.validate_host("qa-test-1"), "qa-test-1.firecrab.dev")


if __name__ == "__main__":
    unittest.main()
