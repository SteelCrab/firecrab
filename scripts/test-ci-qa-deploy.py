#!/usr/bin/env python3
"""Validate Deploy live-QA safety guards and OCI fixture content without KVM."""

import base64
import gzip
import http.client
import importlib.util
import io
import json
from pathlib import Path
import os
import select
import shlex
import tarfile
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import Mock, patch

import deploy_auto_qa as automatic
import deploy_fleet_qa as fleet
import deploy_recovery_qa as recovery
import deploy_observation_qa as observation
import deploy_database_security_qa as database_security

module = importlib.util.spec_from_file_location("deploy_qa", Path(__file__).with_name("ci-qa-deploy.py"))
qa = importlib.util.module_from_spec(module)
module.loader.exec_module(qa)


class DatabaseSecurityQaTests(unittest.TestCase):
    def saved_image(self, directory, corrupt=False, classic=False):
        raw = b"test OCI layer"
        compressed = gzip.compress(raw, mtime=0)
        arch = {"x86_64": "amd64", "amd64": "amd64", "aarch64": "arm64", "arm64": "arm64"}[__import__("platform").machine()]
        config = json.dumps({"os": "linux", "architecture": arch,
            "config": {"Entrypoint": ["/usr/local/bin/firecrab-postgres-qa"]},
            "rootfs": {"type": "layers", "diff_ids": [database_security.digest(b"wrong" if corrupt else raw)]}}).encode()
        config_path = database_security.digest(config).split(":")[1] + ".json" if classic else "blobs/sha256/" + database_security.digest(config).split(":")[1]
        layer_path = "legacy/layer.tar" if classic else "blobs/sha256/" + database_security.digest(compressed).split(":")[1]
        entries = {"manifest.json": json.dumps([{"Config": config_path, "Layers": [layer_path]}]).encode(),
                   config_path: config, layer_path: raw if classic else compressed}
        path = Path(directory) / "fixture.tar"
        with tarfile.open(path, "w") as archive:
            for name, data in entries.items():
                item = tarfile.TarInfo(name)
                item.size = len(data)
                archive.addfile(item, io.BytesIO(data))
        return path, raw

    def test_docker_archive_checks_layer_digests_before_serving_image(self):
        with tempfile.TemporaryDirectory() as directory:
            path, raw = self.saved_image(directory)
            image, manifest, blobs = database_security.image_fixture(path)
            self.assertEqual(image, database_security.digest(manifest))
            layer = json.loads(manifest)["layers"][0]
            self.assertEqual(gzip.decompress(blobs[layer["digest"]]), raw)
            classic, _ = self.saved_image(directory, classic=True)
            _, classic_manifest, classic_blobs = database_security.image_fixture(classic)
            self.assertEqual(gzip.decompress(classic_blobs[json.loads(classic_manifest)["layers"][0]["digest"]]), raw)
            path, _ = self.saved_image(directory, corrupt=True)
            with self.assertRaisesRegex(ValueError, "layer digest mismatch"):
                database_security.image_fixture(path)

    def test_database_acceptance_requires_real_scanner_and_report(self):
        result = subprocess.run([sys.executable, str(Path(__file__).with_name("ci-qa-deploy.py")),
            "--disposable-host", "--database-image-archive", "/tmp/image.tar"], capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn("requires --observability and real security scanner inputs", result.stderr)

    def test_database_security_links_scans_detects_and_cleans_up(self):
        class Api:
            def __init__(self):
                self.linked = False
                self.scanned = False
                self.stopped = False
                self.calls = []
            def call(self, method, path, body=None, allowed=None):
                self.calls.append((method, path, body))
                if (method, path) == ("GET", "/api/pipe/databases"): return []
                if (method, path) == ("POST", "/api/micro-services"): return {"id":"service"}
                if (method, path) == ("GET", "/api/micro-services/service/deployment"):
                    return {"id":"release", "phase":"stopped" if self.stopped else "running", "digest":"sha256:test", "instances":[{"vmId":"replica", "ipv4":"192.0.2.8"}]}
                if (method, path) == ("POST", "/api/pipe/databases"): return {"id":"database"}
                if path == "/api/service-security/databases/database/link":
                    self.linked = method == "PUT"
                    return None
                if (method, path) == ("POST", "/api/service-security/service/scan"):
                    self.scanned = True
                    return None
                if (method, path) == ("GET", "/api/service-security"):
                    return {"databases":[{"id":"database", "serviceId":"service" if self.linked else None}],
                        "unsupportedWorkloads":[] if self.linked else [{"id":"database", "reason":"database_connection_unlinked"}],
                        "services":[{"id":"service", "scan":{"status":"complete", "deploymentId":"release", "digest":"sha256:test", "replicaIds":["replica"]} if self.scanned else None,
                        "activityHistory":[{"source":database_security.SOURCE, "deploymentId":"release", "replicaIds":["replica"], "count":10}]}]}
                if (method, path) == ("POST", "/api/micro-services/service/stop"): self.stopped = True
                return None
        def wait(check, **_):
            result = check()
            if not result: raise AssertionError("condition did not pass")
            return result
        for bad_login in (True, False):
            api = Api()
            spec = {"port":8080, "healthPath":"/health", "env":{}, "secrets":{}}
            failed = Mock(returncode=1 if bad_login else 0, stderr="password authentication failed")
            evidence = {}
            with patch.object(database_security.subprocess, "run", return_value=failed) as psql, patch("builtins.print"):
                if bad_login:
                    database_security.run_database_security(api, wait, "network", spec, ("sha256:test",), evidence)
                    self.assertEqual(psql.call_count, 10)
                    self.assertEqual(psql.call_args.kwargs["env"]["PGPASSWORD"], database_security.BAD_PASSWORD)
                    self.assertEqual(len(evidence["databaseSecurity"]["stages"]), 4)
                else:
                    with self.assertRaisesRegex(AssertionError, "did not reject"):
                        database_security.run_database_security(api, wait, "network", spec, ("sha256:test",), evidence)
                self.assertIn(("DELETE", "/api/pipe/databases/database", None), api.calls)
                self.assertIn(("DELETE", "/api/micro-services/service", None), api.calls)
                self.assertEqual(evidence["databaseSecurity"]["cleanupErrors"], [])


class DeployQaTests(unittest.TestCase):
    def test_real_security_acceptance_requires_both_fixture_inputs(self):
        result = subprocess.run([sys.executable, str(Path(__file__).with_name("ci-qa-deploy.py")),
                                 "--disposable-host", "--security-base-layer", "/tmp/layer"], capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn("requires --observability", result.stderr)

    def test_ingress_denial_fixture_returns_http_403(self):
        fixture = qa.image_fixture(b"busybox-fixture", "A")
        manifest = json.loads(fixture[1])
        raw = gzip.decompress(fixture[2][manifest["layers"][0]["digest"]])
        with tarfile.open(fileobj=io.BytesIO(raw)) as archive:
            script = archive.extractfile("www/cgi-bin/security-denied").read()
        result = subprocess.run(["/bin/sh"], input=script, capture_output=True, check=True)
        self.assertEqual(result.stdout, b"Status: 403 Forbidden\r\nContent-Type: text/plain\r\n\r\ndenied\n")

    def test_security_image_contains_only_fixture_packages_and_their_libraries(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory) / "layer.tar.gz"
            files = {"etc/alpine-release":b"3.17.10\n", "etc/os-release":b"ID=alpine\n",
                     "lib/apk/db/installed":b"P:libcrypto3\nV:3.0.15-r0\n\nP:unrelated\nV:1\n\nP:libssl3\nV:3.0.15-r0\n\n",
                     "lib/libcrypto.so.3":b"crypto-fixture", "lib/libssl.so.3":b"ssl-fixture"}
            with tarfile.open(base, "w:gz") as archive:
                for path, data in files.items():
                    entry = tarfile.TarInfo(path)
                    entry.size = len(data)
                    archive.addfile(entry, io.BytesIO(data))
            fixture = qa.image_fixture(b"busybox-fixture", "A", security_layer=base)
            manifest = json.loads(fixture[1])
            raw = gzip.decompress(fixture[2][manifest["layers"][0]["digest"]])
            with tarfile.open(fileobj=io.BytesIO(raw)) as archive:
                installed = archive.extractfile("lib/apk/db/installed").read()
                self.assertIn(b"P:libcrypto3", installed)
                self.assertIn(b"P:libssl3", installed)
                self.assertNotIn(b"P:unrelated", installed)
                self.assertEqual(archive.extractfile("lib/libcrypto.so.3").read(), b"crypto-fixture")

    def test_security_scanner_fixture_requires_registry_credentials(self):
        scanner = Path(__file__).with_name("ci-qa-security-trivy.py")
        target = "ghcr.io/team/app@sha256:" + "a" * 64
        with tempfile.TemporaryDirectory() as root:
            env = {**os.environ, "DOCKER_CONFIG": root}
            config = Path(root) / "config.json"
            config.write_text(json.dumps({"auths": {"ghcr.io": {"auth": "fixture"}}}))
            result = subprocess.run([sys.executable, str(scanner), "image", target], env=env, capture_output=True, text=True, check=True)
            report = json.loads(result.stdout)
            self.assertEqual(report["Metadata"]["RepoDigests"], [target])
            self.assertEqual(report["Results"][0]["Vulnerabilities"][0]["FixedVersion"], "2.0")
            config.write_text(json.dumps({"auths": {"example.com": {}}}))
            rejected = subprocess.run([sys.executable, str(scanner), "image", target], env=env, capture_output=True)
            self.assertNotEqual(rejected.returncode, 0)

    def test_observation_acceptance_requires_report_before_runtime_access(self):
        result = subprocess.run([sys.executable, str(Path(__file__).with_name("ci-qa-deploy.py")),
                                 "--disposable-host", "--observability"], capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn("extended acceptance requires --report", result.stderr)

    def test_local_collector_validates_and_records_otlp_and_failure_response(self):
        collector = observation.Collector()
        self.addCleanup(collector.close)
        connection = http.client.HTTPConnection("127.0.0.1", collector.server.server_port)
        self.addCleanup(connection.close)
        payload = {"resourceLogs": [{"resource": {"attributes": [
            {"key": "firecrab.service.id", "value": {"stringValue": "service-a"}}]},
            "scopeLogs": [{"logRecords": [{"attributes": [{"key": "log.iostream", "value": {"stringValue": "stderr"}}],
                "body": {"stringValue": "line-a"}}]}]}]}
        for fail in [False, True]:
            collector.set_failure(fail)
            connection.request("POST", "/v1/logs", json.dumps(payload), {"Content-Type": "application/json"})
            response = connection.getresponse()
            self.assertEqual(response.status, 400 if fail else 200)
            self.assertEqual(response.read(), b"{}")
        records = collector.observations()
        self.assertEqual(len(records), 2)
        self.assertEqual(records[0], ({"firecrab.service.id": "service-a"}, {"log.iostream": "stderr"}, "line-a"))

    def test_automatic_acceptance_requires_report_before_runtime_access(self):
        result = subprocess.run([sys.executable, str(Path(__file__).with_name("ci-qa-deploy.py")), "--automatic-deploy", "--disposable-host"], capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn("extended acceptance requires --report", result.stderr)

    def test_registry_trust_refuses_non_root_before_reading_host_files(self):
        with patch.object(automatic.os, "geteuid", return_value=1000), patch.object(Path, "read_text") as read:
            with self.assertRaisesRegex(AssertionError, "disposable root"):
                with automatic.trusted_registry([]):
                    self.fail("must not enter fixture")
            read.assert_not_called()

    def test_authenticated_metadata_fixture_follows_mutable_tags_without_exposing_secrets(self):
        fixtures = [qa.image_fixture(b"static-toolbox", label) for label in ["A", "B", "bad", "C"]]
        registry = automatic.MetadataRegistry(fixtures, address=("127.0.0.1", 0))
        self.addCleanup(registry.close)
        connection = http.client.HTTPConnection("127.0.0.1", registry.server.server_port)
        self.addCleanup(connection.close)
        connection.request("GET", "/v2/team/deploy-qa/manifests/stable")
        response = connection.getresponse()
        self.assertEqual(response.status, 401)
        self.assertEqual(response.read(), b"")
        headers = {"Authorization": "Basic " + base64.b64encode(b"team:local-deploy-qa-only").decode()}
        for index in [0, 1, 3]:
            registry.tags["stable"] = fixtures[index][0]
            connection.request("GET", "/v2/team/deploy-qa/manifests/stable", headers=headers)
            response = connection.getresponse()
            self.assertEqual(response.status, 200)
            self.assertEqual(response.read(), fixtures[index][1])
        connection.request("GET", "/v2/team/deploy-qa/tags/list?n=100", headers=headers)
        response = connection.getresponse()
        self.assertEqual(json.loads(response.read())["tags"], sorted(registry.tags))
        connection.request("HEAD", "/v2/team/deploy-qa/manifests/" + fixtures[0][0], headers=headers)
        response = connection.getresponse()
        self.assertEqual(response.status, 200)
        self.assertEqual(response.getheader("Docker-Content-Digest"), fixtures[0][0])
        self.assertEqual(response.read(), b"")
        self.assertEqual(registry.authenticated, 5)

    def test_queue_acceptance_requires_a_report_before_runtime_access(self):
        result = subprocess.run([sys.executable, str(Path(__file__).with_name("ci-qa-deploy.py")),
                                 "--disposable-host", "--queue-controls"], capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn("extended acceptance requires --report", result.stderr)

    def test_ci_acceptance_requires_report_and_rejects_extra_tls_hosts(self):
        result = subprocess.run([sys.executable, str(Path(__file__).with_name("ci-qa-deploy.py")),
                                 "--disposable-host", "--ci-deploy"], capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn("extended acceptance requires --report", result.stderr)
        with patch.object(automatic.os, "geteuid", return_value=0):
            with self.assertRaisesRegex(AssertionError, "private CI QA hostname"):
                with automatic.trusted_registry([], proxy_hosts=("production.example.com",)):
                    self.fail("must refuse other hosts before access")

    def test_internal_call_commits_a_response_only_after_its_upstream_finishes(self):
        fixture = qa.image_fixture(b"static-toolbox-fixture", "A")
        manifest = json.loads(fixture[1])
        raw = gzip.decompress(fixture[2][manifest["layers"][0]["digest"]])
        with tarfile.open(fileobj=io.BytesIO(raw)) as archive:
            script = archive.extractfile("www/cgi-bin/internal-call").read().decode()
        for failed in [False, True]:
            with self.subTest(failed=failed), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                tool = root / "busybox"
                tool.write_text('#!/bin/sh\ntouch "$STARTED"\nwhile [ ! -e "$RELEASE" ]; do sleep .01; done\n'
                                'if [ "$FAILED" = 1 ]; then exit 1; fi\nprintf "old\\n"\n')
                tool.chmod(0o755)
                started, release = root / "started", root / "release"
                environment = dict(os.environ, STARTED=str(started), RELEASE=str(release), FAILED=str(int(failed)))
                process = subprocess.Popen(["/bin/sh", "-c", script.replace("/bin/busybox", shlex.quote(str(tool)))],
                                           stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=environment)
                try:
                    deadline = time.monotonic() + 3
                    while not started.exists() and time.monotonic() < deadline:
                        time.sleep(.01)
                    self.assertTrue(started.exists())
                    self.assertEqual(select.select([process.stdout], [], [], 0)[0], [])
                finally:
                    release.touch()
                    output, error = process.communicate(timeout=3)
                self.assertEqual(process.returncode, 0, error)
                expected = b'Status: 502 Bad Gateway\r\nContent-Type: text/plain\r\n\r\nunavailable\n' if failed else b'Content-Type: text/plain\r\n\r\nold\n'
                self.assertEqual(output, expected)

    def test_recovery_reads_the_network_detail_response_before_creating_resources(self):
        api = Mock()

        def call(method, path, body=None):
            if (method, path) == ("GET", "/api/micro-networks/network-id"):
                return {"subnet": {"cidr": "172.30.0.0/24", "gateway": "172.30.0.1"}}
            if (method, path) == ("POST", "/api/secrets"):
                raise RuntimeError("fixture stopped before resource creation")
            self.fail(f"unexpected request: {method} {path}")

        api.call.side_effect = call
        evidence = {}
        with self.assertRaisesRegex(RuntimeError, "fixture stopped before resource creation"):
            recovery.run_recovery(api, None, None, "network-id", {}, 8080, evidence)
        self.assertEqual(evidence["replicaRecovery"]["cleanupErrors"], [])
        self.assertEqual(api.call.call_count, 2)

    def test_rotation_fixture_reports_labels_and_rejects_unhealthy_private_inputs(self):
        fixture = qa.image_fixture(b"static-toolbox-fixture", "A")
        manifest = json.loads(fixture[1])
        raw = gzip.decompress(fixture[2][manifest["layers"][0]["digest"]])
        with tarfile.open(fileobj=io.BytesIO(raw)) as archive:
            application = archive.extractfile("www/cgi-bin/rotation").read().decode()
            health = archive.extractfile("www/cgi-bin/rotation-health").read().decode()
        for value, label in [("qa-secret-v0", "old"), ("qa-secret-v1", "next"), ("qa-secret-v2", "latest")]:
            env = {"PASSWORD": value}
            response = subprocess.check_output(["/bin/sh", "-c", application], env=env)
            header, body = response.split(b"\r\n\r\n", 1)
            self.assertEqual(body, (label + "\n").encode())
            self.assertIn(f"Content-Length: {len(body)}".encode(), header)
            self.assertNotIn(value.encode(), response)
            self.assertTrue(subprocess.check_output(["/bin/sh", "-c", health], env=env).endswith(b"healthy\n"))
        self.assertTrue(subprocess.check_output(["/bin/sh", "-c", health], env={"PASSWORD": "qa-secret-bad"}).startswith(b"Status: 503"))
        self.assertTrue(subprocess.check_output(["/bin/sh", "-c", application], env={"UNRELATED": "1"}).endswith(b"unrelated\n"))

    def test_fleet_refuses_cpu_oversubscription_before_reading_host_files(self):
        with patch.object(fleet.subprocess, "check_output", return_value="4\n"), patch.object(Path, "read_text") as read:
            with self.assertRaisesRegex(AssertionError, "at least 16 CPUs"):
                fleet.host_evidence("/unused")
            read.assert_not_called()

    def test_fleet_refuses_insufficient_memory_or_disk_before_reading_binaries(self):
        for memory, disk, message in [(6399, 32 * 1024**3, "6400 MiB"), (12000, 15 * 1024**3, "16 GiB")]:
            with self.subTest(message=message), patch.object(fleet.subprocess, "check_output", return_value="16\n"), \
                    patch.object(Path, "read_text", return_value=f"MemAvailable: {memory * 1024} kB\n"), \
                    patch.object(fleet.shutil, "disk_usage", return_value=Mock(free=disk)), \
                    patch.object(Path, "read_bytes") as read:
                with self.assertRaisesRegex(AssertionError, message):
                    fleet.host_evidence("/unused")
                read.assert_not_called()

    def test_image_descriptors_match_actual_layer_config_and_process(self):
        for healthy in [True, False]:
            identity, manifest, blobs = qa.image_fixture(b"static-toolbox-fixture", "A", healthy)
            self.assertEqual(identity, qa.digest(manifest))
            document = json.loads(manifest)
            config = json.loads(blobs[document["config"]["digest"]])
            layer = blobs[document["layers"][0]["digest"]]
            self.assertEqual(len(layer), document["layers"][0]["size"])
            self.assertEqual(qa.digest(layer), document["layers"][0]["digest"])
            raw = gzip.decompress(layer)
            self.assertEqual(qa.digest(raw), config["rootfs"]["diff_ids"][0])
            self.assertEqual(config["config"]["Entrypoint"], ["/bin/sh", "/bin/observed-init"])
            with tarfile.open(fileobj=io.BytesIO(raw)) as archive:
                self.assertEqual("www/health" in archive.getnames(), healthy)
                init = archive.extractfile("bin/observed-init").read().decode()
                self.assertIn('fixture-stdout-%s', init)
                self.assertIn('fixture-stderr-%s', init)
                self.assertIn('exec /bin/busybox httpd', init)
                security_cgi = archive.extractfile("www/cgi-bin/security-activity").read().decode()
                self.assertIn('"status":403', security_cgi)
                cgi = archive.extractfile("www/cgi-bin/observation").read().decode()
                result = subprocess.run(["/bin/sh", "-c", cgi], env={"OBS_ID": "alpha"}, capture_output=True, check=True)
                self.assertEqual(result.stderr, b"fixture-cgi-alpha\n")
                self.assertTrue(result.stdout.endswith(b"observed\n"))
                env_cgi = archive.extractfile("www/cgi-bin/otel-environment").read().decode()
                otel = {"OTEL_EXPORTER_OTLP_ENDPOINT": "http://collector:4318", "OTEL_EXPORTER_OTLP_PROTOCOL": "http/protobuf", "OTEL_SERVICE_NAME": "alpha"}
                output = subprocess.check_output(["/bin/sh", "-c", env_cgi], env=otel)
                self.assertTrue(output.endswith(b"http://collector:4318|http/protobuf|alpha\n"))
                self.assertEqual(archive.extractfile("www/index.html").read(), b"release-A\n")
                self.assertEqual(archive.getmember("bin/sh").linkname, "busybox")
                held = archive.extractfile("www/cgi-bin/hold").read()
                self.assertIn(b"/bin/busybox touch", held)
                self.assertIn(b"printf ' '", held)
                self.assertTrue(archive.getmember("www/cgi-bin/hold").mode & 0o111)
                header = archive.extractfile("www/cgi-bin/holding").read().splitlines()[1]
                self.assertEqual(subprocess.check_output(["/bin/sh", "-c", header.decode()]), b"Content-Type: text/plain\r\n\r\n")

    def test_registry_serves_the_exact_manifest_and_content_addressed_blob(self):
        fixture = qa.image_fixture(b"static-toolbox-fixture", "B")
        registry = qa.Registry([fixture])
        self.addCleanup(registry.close)
        connection = http.client.HTTPConnection(registry.address)
        self.addCleanup(connection.close)
        connection.request("GET", "/v2/team/deploy-qa/manifests/" + fixture[0])
        response = connection.getresponse()
        body = response.read()
        self.assertEqual(response.status, 200)
        self.assertEqual(response.getheader("Docker-Content-Digest"), fixture[0])
        self.assertEqual(body, fixture[1])
        key, value = next(iter(fixture[2].items()))
        connection.request("GET", "/v2/team/deploy-qa/blobs/" + key)
        response = connection.getresponse()
        self.assertEqual(response.read(), value)

    def test_existing_resources_are_rejected_before_system_or_file_mutations(self):
        for resource in ["vms", "micro-services", "pools", "pipe/auth"]:
            api = Mock()
            api.call.side_effect = lambda method, path: [{"id": "existing"}] if path == "/api/" + resource else []
            with patch.object(qa.subprocess, "check_output") as command:
                with self.assertRaisesRegex(AssertionError, "refusing existing " + resource):
                    qa.require_empty_host(api)
                command.assert_not_called()

    def test_existing_ingress_or_configuration_is_rejected(self):
        api = Mock()
        api.call.return_value = []
        with patch.object(qa.subprocess, "check_output", return_value="loaded\n"):
            with self.assertRaisesRegex(AssertionError, "existing ingress"):
                qa.require_empty_host(api)
        api.call.side_effect = lambda method, path: {"config": {"routes": []}} if path == "/api/pipe" else []
        with patch.object(qa.subprocess, "check_output", return_value="not-found\n"):
            with self.assertRaisesRegex(AssertionError, "existing Pipe configuration"):
                qa.require_empty_host(api)

    def test_remote_api_targets_are_rejected(self):
        for address in ["http://example.com", "https://127.0.0.1", "http://192.168.64.58"]:
            with self.assertRaisesRegex(AssertionError, "local disposable API required"):
                qa.Api(address)


if __name__ == "__main__":
    unittest.main()
