#!/usr/bin/env python3
"""Validate Deploy live-QA safety guards and OCI fixture content without KVM."""

import gzip
import http.client
import importlib.util
import io
import json
from pathlib import Path
import tarfile
import subprocess
import unittest
from unittest.mock import Mock, patch

module = importlib.util.spec_from_file_location("deploy_qa", Path(__file__).with_name("ci-qa-deploy.py"))
qa = importlib.util.module_from_spec(module)
module.loader.exec_module(qa)


class DeployQaTests(unittest.TestCase):
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
            self.assertEqual(config["config"]["Entrypoint"][0:2], ["/bin/busybox", "httpd"])
            with tarfile.open(fileobj=io.BytesIO(raw)) as archive:
                self.assertEqual("www/health" in archive.getnames(), healthy)
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
