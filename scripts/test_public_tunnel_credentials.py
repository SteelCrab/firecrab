#!/usr/bin/env python3
"""Check credential safety and registration over an isolated TLS server."""

import http.server
import json
import os
from pathlib import Path
import socket
import ssl
import subprocess
import tempfile
import threading
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import public_tunnel_credentials as credentials


class Handler(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
        self.server.requests.append((self.path, dict(self.headers), body))
        response = self.server.response
        self.send_response(self.server.status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(response)))
        self.end_headers()
        self.wfile.write(response)

    def log_message(self, *_):
        pass


class CredentialsTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.directory = tempfile.TemporaryDirectory()
        cls.root = Path(os.path.realpath(cls.directory.name))
        cls.cert = cls.root / "cert.pem"
        key = cls.root / "key.pem"
        subprocess.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
                        "-keyout", str(key), "-out", str(cls.cert), "-days", "1",
                        "-subj", "/CN=tunnel.firecrab.dev", "-addext", "subjectAltName=DNS:tunnel.firecrab.dev"],
                       check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(cls.cert, key)
        cls.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        cls.server.socket = context.wrap_socket(cls.server.socket, server_side=True)
        cls.thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.thread.start()

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()
        cls.thread.join()
        cls.directory.cleanup()

    def setUp(self):
        self.first = self.root / "first.token"
        self.second = self.root / "second.token"
        self.first.unlink(missing_ok=True)
        self.second.unlink(missing_ok=True)
        self.first.write_text("a" * 64 + "\n")
        self.second.write_text("b" * 64)
        self.first.chmod(0o600)
        self.second.chmod(0o400)
        self.server.requests = []
        self.server.status = 200
        self.server.response = json.dumps({
            "name": "qa-first", "url": "https://qa-first.firecrab.dev",
            "heartbeat_url": "https://qa-first.firecrab.dev/__firecrab/heartbeat", "suspended": False,
        }).encode()
        original_connect = socket.create_connection
        default_context = ssl.create_default_context
        self.default_context = default_context

        def connect(address, *args, **kwargs):
            self.assertEqual(address, (credentials.HOST, 443))
            return original_connect(("127.0.0.1", self.server.server_port), *args, **kwargs)

        connection_patch = patch.object(socket, "create_connection", side_effect=connect)
        context_patch = patch.object(credentials.ssl, "create_default_context", side_effect=lambda: default_context(cafile=str(self.cert)))
        connection_patch.start()
        context_patch.start()
        self.addCleanup(connection_patch.stop)
        self.addCleanup(context_patch.stop)

    def pair(self):
        return credentials.load_credentials([("qa-first", self.first), ("qa-second", self.second)])

    def test_protected_pair_and_empty_authenticated_registration(self):
        first, second = self.pair()
        self.assertNotIn(first.token, repr(first))
        self.assertNotIn(second.token, repr(second))
        result = credentials.verify_registration(first)
        self.assertEqual(result, {"label": "qa-first", "url": "https://qa-first.firecrab.dev", "status": 200})
        path, headers, body = self.server.requests[0]
        self.assertEqual(path, "/v1/register")
        self.assertEqual(headers["X-Firecrab-Protocol"], "2")
        self.assertEqual(headers["Authorization"], "Bearer " + first.token)
        self.assertEqual(body, b"{}")

    def test_rejects_shared_tokens_and_invalid_credentials(self):
        self.second.chmod(0o600)
        for entries in [
            [("qa-first", self.first)],
            [("qa-first", self.first), ("qa-first", self.second)],
            [("qa-first", self.first), ("qa-second", self.first)],
            [("console", self.first), ("qa-second", self.second)],
        ]:
            with self.subTest(entries=entries), self.assertRaises(credentials.CredentialError):
                credentials.load_credentials(entries)
        self.second.write_text("a" * 64)
        with self.assertRaises(credentials.CredentialError):
            self.pair()
        self.second.write_text("x" * 64)
        with self.assertRaises(credentials.CredentialError):
            self.pair()
        self.second.write_text("b" * 64 + "\nextra")
        with self.assertRaises(credentials.CredentialError):
            self.pair()

    def test_rejects_unsafe_file_permissions_and_links(self):
        self.first.chmod(0o644)
        with self.assertRaises(credentials.CredentialError):
            self.pair()
        self.first.chmod(0o600)
        link = self.root / "token-link"
        link.symlink_to(self.first)
        with self.assertRaises(credentials.CredentialError):
            credentials.load_credentials([("qa-first", link), ("qa-second", self.second)])
        link.unlink()
        link.symlink_to(self.root, target_is_directory=True)
        with self.assertRaises(credentials.CredentialError):
            credentials.load_credentials([("qa-first", link / self.first.name), ("qa-second", self.second)])
        link.unlink()
        os.link(self.first, link)
        with self.assertRaises(credentials.CredentialError):
            self.pair()
        link.unlink()
        fifo = self.root / "fifo"
        os.mkfifo(fifo, 0o600)
        with self.assertRaises(credentials.CredentialError):
            credentials.load_credentials([("qa-first", fifo), ("qa-second", self.second)])
        fifo.unlink()

    def test_registration_checks_exact_identity_and_never_echoes_body(self):
        first, _ = self.pair()
        for response in [
            {"name": "qa-second", "url": "https://qa-first.firecrab.dev", "heartbeat_url": "https://qa-first.firecrab.dev/__firecrab/heartbeat", "suspended": False},
            {"name": "qa-first", "url": "https://qa-first.firecrab.dev", "heartbeat_url": "https://evil.invalid/", "suspended": False},
            {"name": "qa-first", "url": "https://qa-first.firecrab.dev", "heartbeat_url": "https://qa-first.firecrab.dev/__firecrab/heartbeat", "suspended": True},
            {"token": first.token},
        ]:
            self.server.response = json.dumps(response).encode()
            with self.subTest(response=list(response)), self.assertRaises(credentials.CredentialError) as failure:
                credentials.verify_registration(first)
            self.assertNotIn(first.token, str(failure.exception))
        self.server.response = b"x" * (credentials.MAX_RESPONSE + 1)
        with self.assertRaises(credentials.CredentialError):
            credentials.verify_registration(first)

    def test_rejects_token_file_owned_by_another_user(self):
        original = os.fstat

        def wrong_owner(fd):
            info = original(fd)
            return SimpleNamespace(st_mode=info.st_mode, st_nlink=info.st_nlink, st_uid=-1)

        with patch.object(credentials.os, "fstat", side_effect=wrong_owner):
            with self.assertRaises(credentials.CredentialError):
                self.pair()

    def test_revocation_requires_authentication_rejection(self):
        first, _ = self.pair()
        for status in (401, 410):
            self.server.status = status
            self.server.response = b'{}'
            self.assertEqual(credentials.verify_revoked(first)["status"], status)
        for status in (200, 403, 404, 429, 503):
            self.server.status = status
            with self.subTest(status=status), self.assertRaises(credentials.CredentialError):
                credentials.verify_revoked(first)

    def test_registration_refuses_untrusted_tls(self):
        first, _ = self.pair()
        with patch.object(credentials.ssl, "create_default_context", side_effect=self.default_context):
            with self.assertRaises(credentials.CredentialError) as failure:
                credentials.verify_registration(first)
        self.assertNotIn(first.token, str(failure.exception))
        self.assertEqual(self.server.requests, [])


if __name__ == "__main__":
    unittest.main()
