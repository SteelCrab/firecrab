#!/usr/bin/env python3
"""Exercise public Tunnel preflight safety and response validation offline."""

import importlib.util
from pathlib import Path
import ssl
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location("public_tunnel", Path(__file__).with_name("check-public-tunnel.py"))
check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check)


class Response:
    def __init__(self, status, body=b'{"error":"not found"}', content_type="application/json"):
        self.status, self.body, self.content_type = status, body, content_type

    def getheader(self, name, default=""):
        return self.content_type if name == "Content-Type" else default

    def read(self, limit):
        return self.body[:limit]


class Connection:
    def __init__(self, status, requests):
        self.response = Response(status)
        self.requests = requests

    def request(self, method, path, headers):
        self.requests.append((method, path, headers))

    def getresponse(self):
        return self.response

    def close(self):
        pass


class PublicTunnelPreflight(unittest.TestCase):
    def test_only_read_only_requests_are_sent_to_the_known_public_hosts(self):
        requests = []
        hosts = []
        dns = []

        def connect(host, timeout, context):
            self.assertEqual(timeout, 10)
            self.assertEqual(context.verify_mode, ssl.CERT_REQUIRED)
            self.assertTrue(context.check_hostname)
            hosts.append(host)
            status = 200 if host == check.CONSOLE else 405 if host == check.REGISTRATION else 404
            return Connection(status, requests)

        def resolve(host, port, type):
            dns.append(host)
            self.assertEqual(port, 443)
            return [(None, None, None, None, ("127.0.0.1", 443))]

        with patch.object(check.socket, "getaddrinfo", side_effect=resolve), \
                patch.object(check.http.client, "HTTPSConnection", side_effect=connect):
            result = check.preflight()
        self.assertEqual(result["publicApplicationAcceptance"], "not_run")
        self.assertEqual(hosts, dns)
        self.assertEqual(hosts[:2], [check.CONSOLE, check.REGISTRATION])
        self.assertRegex(hosts[2], r"^qa-[0-9a-f]{32}\.firecrab\.dev$")
        self.assertEqual([request[0] for request in requests], ["GET"] * 3)
        self.assertEqual(requests[1], ("GET", "/v1/register", {"X-Firecrab-Protocol": "2"}))
        self.assertTrue(all("Authorization" not in headers for _, _, headers in requests))

    def test_unexpected_status_and_non_json_error_are_rejected(self):
        for response in [Response(200), Response(404, b"not JSON"), Response(404, b"{}"),
                         Response(404, b'{"error":"x"}', "text/html")]:
            with self.subTest(status=response.status, body=response.body), \
                    patch.object(check.http.client, "HTTPSConnection") as cls:
                cls.return_value.getresponse.return_value = response
                with self.assertRaises(check.CheckError):
                    check.check_https("test.firecrab.dev", "/", 404, json_error=True)

    def test_dns_failure_does_not_issue_http_requests(self):
        with patch.object(check.socket, "getaddrinfo", side_effect=OSError("unavailable")), \
                patch.object(check.http.client, "HTTPSConnection") as connection:
            with self.assertRaises(check.CheckError):
                check.preflight()
            connection.assert_not_called()


if __name__ == "__main__":
    unittest.main()
