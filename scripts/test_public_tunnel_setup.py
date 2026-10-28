#!/usr/bin/env python3
"""Test fixture ownership and cleanup after partial setup failures."""

import unittest
from pathlib import Path
import subprocess
import sys

from public_tunnel_setup import run_fixture_services


class Api:
    def __init__(self, fail_deploy=None, fail_stop=None):
        self.states = {}
        self.deleted = []
        self.fail_deploy = fail_deploy
        self.fail_stop = fail_stop

    def call(self, method, path, body=None):
        if (method, path) == ("POST", "/api/micro-services"):
            identity = str(len(self.states) + 1)
            self.states[identity] = None
            return {"id": identity}
        identity = path.split("/")[3]
        if path.endswith("/deployment") and method == "POST":
            if identity == self.fail_deploy:
                raise RuntimeError("fixture deployment failed")
            self.states[identity] = "running"
        elif path.endswith("/deployment") and method == "GET":
            return {"phase": self.states[identity]} if self.states[identity] else None
        elif path.endswith("/stop"):
            if identity == self.fail_stop:
                raise RuntimeError("stop failed")
            self.states[identity] = "stopped"
        elif method == "DELETE":
            self.assert_stopped(identity)
            self.deleted.append(identity)

    def assert_stopped(self, identity):
        if self.states[identity] not in (None, "stopped"):
            raise AssertionError("refusing to remove a running fixture")


def wait_for(check, **_):
    result = check()
    if not result:
        raise RuntimeError("not ready")
    return result


class SetupTests(unittest.TestCase):
    def test_public_mode_requires_approval_and_default_host_paths_before_runtime(self):
        runner = str(Path(__file__).with_name("ci-qa-deploy.py"))
        cases = [
            (["--public-tunnel", "--report", "/unused"], "explicit target approval"),
            (["--public-tunnel", "--public-target-approved", "--public-fixture-binary", "/unused",
              "--public-token", "qa-a", "/unused-a", "--public-token", "qa-b", "/unused-b",
              "--api", "http://127.0.0.1:9999", "--report", "/unused"], "default disposable host paths"),
            (["--public-target-approved"], "options require --public-tunnel"),
        ]
        for arguments, message in cases:
            with self.subTest(arguments=arguments):
                result = subprocess.run([sys.executable, runner, "--disposable-host", *arguments],
                                        capture_output=True, text=True)
                self.assertEqual(result.returncode, 2)
                self.assertIn(message, result.stderr)

    def test_partial_deployment_failure_cleans_only_created_services(self):
        api = Api(fail_deploy="2")
        evidence = {}
        with self.assertRaisesRegex(RuntimeError, "fixture deployment failed"):
            run_fixture_services(api, wait_for, "network", {}, [("a",), ("b",)], (), evidence,
                                 lambda *_: self.fail("acceptance must not run"))
        self.assertEqual(api.deleted, ["2", "1"])
        self.assertEqual(evidence["publicFixtureCleanupErrors"], [])

    def test_cleanup_continues_after_a_stop_failure_and_reports_it(self):
        api = Api(fail_stop="2")
        evidence = {}
        with self.assertRaisesRegex(RuntimeError, "cleanup failed"):
            run_fixture_services(api, wait_for, "network", {}, [("a",), ("b",)], (), evidence,
                                 lambda *_: None)
        self.assertEqual(api.deleted, ["1"])
        self.assertEqual(len(evidence["publicFixtureCleanupErrors"]), 1)

    def test_cleanup_failure_preserves_the_original_error(self):
        api = Api(fail_deploy="2", fail_stop="1")
        evidence = {}
        with self.assertRaisesRegex(RuntimeError, "fixture deployment failed"):
            run_fixture_services(api, wait_for, "network", {}, [("a",), ("b",)], (), evidence,
                                 lambda *_: self.fail("acceptance must not run"))
        self.assertEqual(api.deleted, ["2"])
        self.assertEqual(len(evidence["publicFixtureCleanupErrors"]), 1)


if __name__ == "__main__":
    unittest.main()
