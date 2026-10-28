#!/usr/bin/env python3
"""Exercise ownership, partial failure and secret handling without production."""

import contextlib
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import MagicMock, patch

import public_tunnel_acceptance as acceptance
from public_tunnel_credentials import Credential


class Api:
    def __init__(self):
        self.residual = False

    def call(self, method, path):
        assert method == "GET" and path == "/api/pipe"
        return {"config": {"quickTunnels": [{"id": "orphan"}] if self.residual else []}}


class AcceptanceTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.store = Path(self.directory.name) / "addresses.json"
        self.credentials = [Credential(label="qa-test-a", token="a" * 64, path="/private/a"),
                            Credential(label="qa-test-b", token="b" * 64, path="/private/b")]
        self.services = [{"id": "a", "spec": {"domain": "a.example.test"}},
                         {"id": "b", "spec": {"domain": "b.example.test"}}]
        self.api = Api()
        self.calls = []
        self.failed_connect = None
        self.failed_delete = None
        self.revoked = []
        self.evidence = {}

        def run_cli(cli, *args, credential=None):
            self.calls.append((args, credential.label if credential else None))
            if credential:
                entries = self.read_bindings()
                entries = [entry for entry in entries if entry["subdomain"] != credential.label]
                entries.append({"subdomain": credential.label, "token": credential.token,
                                "ownedConnector": True, "connectorId": credential.label + "-connector"})
                self.write_bindings(entries)
                if self.failed_connect == credential.label:
                    raise acceptance.AcceptanceError("original connect failure")
            if args[:2] == ("address", "delete"):
                if self.failed_delete == args[2]:
                    raise acceptance.AcceptanceError("delete failed")
                self.write_bindings([item for item in self.read_bindings() if item["subdomain"] != args[2]])

        patches = [
            patch.object(acceptance, "STORE", self.store),
            patch.object(acceptance.shutil, "which", return_value="/test/firecrab"),
            patch.object(acceptance, "fixture_targets", return_value=["172.29.1.2:8080", "172.29.1.3:8080"]),
            patch.object(acceptance, "verify_registration", return_value=None),
            patch.object(acceptance, "verify_revoked", side_effect=lambda credential: self.revoked.append(credential.label)),
            patch.object(acceptance, "run_cli", side_effect=run_cli),
            patch.object(acceptance, "ready", return_value=True),
            patch.object(acceptance, "wait_offline", return_value=True),
            patch.object(acceptance, "restart_api", return_value=3),
            patch("builtins.input", return_value=""),
        ]
        for item in patches:
            item.start()
            self.addCleanup(item.stop)

    def read_bindings(self):
        if not self.store.exists():
            return []
        return json.loads(self.store.read_text())["bindings"]

    def write_bindings(self, bindings):
        self.store.write_text(json.dumps({"endpoint": acceptance.ENDPOINT, "protocol": "register_v1", "bindings": bindings}))

    def run_acceptance(self):
        with contextlib.redirect_stdout(io.StringIO()):
            acceptance.run_public(self.api, lambda *a, **kw: True,
                                  *self.services, self.credentials, self.evidence)

    def test_complete_sequence_checks_both_revocations_and_cleans_connections(self):
        self.run_acceptance()
        result = self.evidence["publicTunnel"]
        self.assertEqual(result["status"], "pending_operator_verification")
        self.assertEqual(result["connectionChecks"], "passed")
        self.assertEqual(result["dashboardRevocations"], "operator_verification_required")
        self.assertEqual(self.revoked, ["qa-test-a", "qa-test-b"])
        self.assertEqual(result["tokenAuthentication"], {"qa-test-a": "registration_rejected", "qa-test-b": "registration_rejected"})
        self.assertEqual(result["dashboardReservations"], "operator_cleanup_required")
        self.assertEqual(self.read_bindings(), [])
        self.assertIn((("address", "connect", "qa-test-a", "--nginx", "b.example.test"), None), self.calls)
        self.assertNotIn("a" * 64, json.dumps(self.evidence))

    def test_partial_connect_is_cleaned_even_when_command_fails(self):
        self.failed_connect = "qa-test-b"
        with self.assertRaisesRegex(acceptance.AcceptanceError, "original connect failure"):
            self.run_acceptance()
        self.assertEqual(self.read_bindings(), [])
        self.assertEqual(self.evidence["publicTunnel"]["status"], "failed")
        self.assertEqual(self.evidence["publicTunnel"]["tokenAuthentication"]["qa-test-a"], "accepted")

    def test_cleanup_failure_does_not_hide_original_failure(self):
        self.failed_connect = "qa-test-b"
        self.failed_delete = "qa-test-b"
        with self.assertRaisesRegex(acceptance.AcceptanceError, "original connect failure"):
            self.run_acceptance()
        self.assertEqual(self.evidence["publicTunnel"]["cleanupErrors"], ["local connection cleanup failed: qa-test-b"])

    def test_foreign_ownership_is_never_deleted(self):
        credential = self.credentials[0]
        self.write_bindings([{"subdomain": credential.label, "token": "f" * 64,
                              "ownedConnector": True, "connectorId": "foreign"}])
        with self.assertRaises(acceptance.AcceptanceError):
            acceptance.owned_binding(self.store, credential)
        self.assertEqual(len(self.read_bindings()), 1)

    def test_orphaned_connector_makes_cleanup_fail(self):
        self.api.residual = True
        with self.assertRaisesRegex(acceptance.AcceptanceError, "cleanup failed"):
            self.run_acceptance()
        self.assertEqual(self.evidence["publicTunnel"]["status"], "failed")
        self.assertIn("connector cleanup is incomplete or unverified", self.evidence["publicTunnel"]["cleanupErrors"])

    def test_failed_token_check_cannot_be_reported_as_revocation(self):
        with patch.object(acceptance, "verify_revoked", side_effect=acceptance.AcceptanceError("token still accepted")):
            with self.assertRaisesRegex(acceptance.AcceptanceError, "token still accepted"):
                self.run_acceptance()
        result = self.evidence["publicTunnel"]
        self.assertEqual(result["status"], "failed")
        self.assertEqual(result["connectionChecks"], "not_completed")
        self.assertEqual(result["tokenAuthentication"]["qa-test-a"], "accepted")
        self.assertEqual(self.read_bindings(), [])


class SecretTransportTest(unittest.TestCase):
    @unittest.skipUnless(hasattr(os, "memfd_create"), "Linux memfd required")
    def test_token_is_in_inherited_file_not_arguments_or_errors(self):
        with tempfile.TemporaryDirectory() as directory:
            cli = Path(directory) / "firecrab"
            cli.write_text("""#!/usr/bin/env python3
import sys
value = 'a' * 64
assert not any(value in arg for arg in sys.argv)
assert open(sys.argv[sys.argv.index('--token-file') + 1]).read() == value
print(value, file=sys.stderr)
sys.exit(9)
""")
            cli.chmod(0o755)
            credential = Credential(label="qa-test-a", token="a" * 64, path="/private/a")
            with self.assertRaises(acceptance.AcceptanceError) as error:
                acceptance.run_cli(str(cli), "address", "connect", "--app", "127.0.0.1:8080", credential=credential)
            self.assertNotIn(credential.token, str(error.exception))

    def test_public_polling_is_limited(self):
        calls = []
        poll = acceptance.limited(lambda: calls.append(1) or False)
        with patch.object(acceptance.time, "monotonic", side_effect=[100, 101, 104, 105]):
            for _ in range(4):
                poll()
        self.assertEqual(len(calls), 2)


class OfflineResponseTest(unittest.TestCase):
    def test_only_cloudflare_1033_is_accepted_for_http_530(self):
        credential = Credential(label="qa-test-a", token="a" * 64, path="/private/a")
        cases = [
            (530, "cloudflare", b"error code: 1033\n", True),
            (530, "CloudFlare", b"error code: 1033\n", True),
            (530, "cloudflare", b"error code: 1000\n", False),
            (530, "cloudflare", b"error code: 1033", False),
            (530, "cloudflare", b"error code: 1033\nextra", False),
            (530, "other", b"error code: 1033\n", False),
            (200, "cloudflare", b"error code: 1033\n", False),
            (404, "cloudflare", b"not found", True),
        ]
        for status, server, body, expected in cases:
            with self.subTest(status=status, server=server, body=body):
                response = MagicMock(status=status)
                response.getheader.return_value = server
                response.read.return_value = body
                connection = MagicMock()
                connection.getresponse.return_value = response
                with patch.object(acceptance.http.client, "HTTPSConnection", return_value=connection):
                    self.assertEqual(acceptance.public_offline(credential), expected)
                connection.request.assert_called_once_with("GET", "/app", headers={"Cache-Control": "no-cache"})


class HostGuardTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.store = Path(self.directory.name) / "addresses.json"
        self.services = [{"id": "1", "spec": {"name": "tunnel-qa-a", "domain": "a.example.test", "port": 8080}},
                         {"id": "2", "spec": {"name": "tunnel-qa-b", "domain": "b.example.test", "port": 8080}}]
        self.values = {
            "/api/micro-services": [{"id": service["id"], **service["spec"]} for service in self.services],
            "/api/vms": [{"id": "vm1"}, {"id": "vm2"}],
            "/api/micro-services/1/deployment": {"id": "d1", "phase": "running", "instances": [{"vmId": "vm1", "ipv4": "172.29.1.2"}]},
            "/api/micro-services/2/deployment": {"id": "d2", "phase": "running", "instances": [{"vmId": "vm2", "ipv4": "172.29.1.3"}]},
            "/api/pipe": {"config": {"routes": [
                {"host": "a.example.test", "owner": {"serviceId": "1", "deploymentId": "d1"}},
                {"host": "tunnel-qa-a.internal", "owner": {"serviceId": "1", "deploymentId": "d1"}},
                {"host": "b.example.test", "owner": {"serviceId": "2", "deploymentId": "d2"}},
                {"host": "tunnel-qa-b.internal", "owner": {"serviceId": "2", "deploymentId": "d2"}},
            ]}},
        }
        class FakeApi:
            def call(_, method, path):
                return self.values[path]
        self.api = FakeApi()
        original_exists = Path.exists
        patches = [patch.object(acceptance, "STORE", self.store),
                   patch.object(acceptance.os, "geteuid", return_value=0),
                   patch.object(Path, "exists", lambda path: True if str(path) == "/dev/kvm" else original_exists(path)),
                   patch.object(acceptance, "expect_fixture")]
        for item in patches:
            item.start()
            self.addCleanup(item.stop)

    def test_accepts_only_the_two_declared_fixture_services_and_vms(self):
        self.assertEqual(acceptance.fixture_targets(self.api, self.services), ["172.29.1.2:8080", "172.29.1.3:8080"])
        self.values["/api/vms"].append({"id": "foreign"})
        with self.assertRaises(acceptance.AcceptanceError):
            acceptance.fixture_targets(self.api, self.services)

    def test_existing_tunnel_and_unrelated_route_are_rejected(self):
        self.values["/api/pipe"]["firecrabTunnels"] = [{"subdomain": "existing"}]
        with self.assertRaises(acceptance.AcceptanceError):
            acceptance.fixture_targets(self.api, self.services)
        del self.values["/api/pipe"]["firecrabTunnels"]
        self.values["/api/pipe"]["config"]["routes"].append({"host": "other.example.test"})
        with self.assertRaises(acceptance.AcceptanceError):
            acceptance.fixture_targets(self.api, self.services)

    def test_internal_route_with_another_owner_is_rejected(self):
        self.values["/api/pipe"]["config"]["routes"][1]["owner"]["serviceId"] = "foreign"
        with self.assertRaises(acceptance.AcceptanceError):
            acceptance.fixture_targets(self.api, self.services)


if __name__ == "__main__":
    unittest.main()
