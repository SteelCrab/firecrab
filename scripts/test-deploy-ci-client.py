#!/usr/bin/env python3
"""Exercise the shipped Action client over HTTP without external credentials."""
import contextlib
import http.server
import importlib.util
import io
import json
import os
from pathlib import Path
import threading
import unittest
from unittest.mock import patch
import uuid

path = Path(__file__).resolve().parents[1] / ".github/actions/deploy/deploy.py"
spec = importlib.util.spec_from_file_location("deploy_ci_client", path)
client = importlib.util.module_from_spec(spec)
spec.loader.exec_module(client)
TOKEN = "fcd_" + uuid.uuid4().hex + "_" + "a" * 43
IMAGE = "ghcr.io/team/app@sha256:" + "b" * 64


@contextlib.contextmanager
def server(responses):
    calls = []
    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            self.answer()
        def do_POST(self):
            self.answer()
        def answer(self):
            raw = self.rfile.read(int(self.headers.get("content-length", "0")))
            calls.append((self.command, self.path, json.loads(raw) if raw else None, self.headers.get("Authorization")))
            status, value = responses.pop(0)
            if status == 0:
                self.close_connection = True
                return
            data = json.dumps(value).encode()
            self.send_response(status)
            if status == 302:
                self.send_header("Location", "https://other.invalid/__firecrab/deploy")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
        def log_message(self, *args):
            pass
    httpd = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=httpd.serve_forever, daemon=True)
    thread.start()
    try:
        yield "http://127.0.0.1:" + str(httpd.server_port) + "/__firecrab/deploy", calls
    finally:
        httpd.shutdown()
        httpd.server_close()
        thread.join()


def outcome(operation, release, phase="running", **fields):
    return {"operationId": operation, "serviceId": str(uuid.uuid4()), "expectedReleaseId": release,
            "image": IMAGE, "strategy": "rolling", "phase": phase, "completed": True,
            "succeeded": phase == "running", "digest": "sha256:" + "c" * 64, **fields}


class ActionClientTests(unittest.TestCase):
    def test_rejects_unsafe_endpoints_and_mutable_images(self):
        for url in ["http://example.test/__firecrab/deploy", "https://user:secret@example.test/__firecrab/deploy",
                    "https://example.test/api/vms", "https://example.test/__firecrab/deploy?token=secret",
                    "https://example.test/__firecrab/deploy#secret", "https://example.test/__firecrab/deploy/../api"]:
            with self.subTest(url=url), self.assertRaises(client.Failure):
                client.endpoint(url)
        self.assertEqual(client.endpoint("https://app.example.test/__firecrab/deploy/"), "https://app.example.test/__firecrab/deploy")
        with self.assertRaises(client.Failure):
            client.image_reference("ghcr.io/team/app:latest")

    def test_submission_and_exact_status_do_not_follow_the_restored_active_release(self):
        operation, release = str(uuid.uuid4()), str(uuid.uuid4())
        with server([(404, {}), (200, {"releaseId": release}), (202, outcome(operation, release, "failed"))]) as (url, calls):
            with self.assertRaisesRegex(client.Failure, "failed or was cancelled"):
                client.deploy(client.Client(url, TOKEN), IMAGE, operation)
            self.assertEqual(calls[-1][2], {"operationId": operation, "expectedReleaseId": release, "image": IMAGE, "strategy": "rolling"})
            self.assertTrue(all(call[3] == "Bearer " + TOKEN for call in calls))

    def test_rerun_reads_existing_attempt_without_submitting_and_rejects_changed_inputs(self):
        operation, release = str(uuid.uuid4()), str(uuid.uuid4())
        with server([(200, outcome(operation, release))]) as (url, calls):
            self.assertTrue(client.deploy(client.Client(url, TOKEN), IMAGE, operation)["succeeded"])
            self.assertEqual(len(calls), 1)
        with server([(200, outcome(operation, release))]) as (url, _):
            with self.assertRaisesRegex(client.Failure, "different Deploy inputs"):
                client.deploy(client.Client(url, TOKEN), IMAGE, operation, str(uuid.uuid4()))

    def test_lost_acknowledgement_and_restart_keep_one_operation(self):
        operation, release = str(uuid.uuid4()), str(uuid.uuid4())
        with server([(503, {}), (404, {}), (0, {}), (200, outcome(operation, release))]) as (url, calls), patch.object(client.time, "sleep"):
            self.assertTrue(client.deploy(client.Client(url, TOKEN), IMAGE, operation, release)["succeeded"])
            self.assertEqual(sum(call[0] == "POST" for call in calls), 1)
            self.assertTrue(all(call[1].endswith(operation) for call in calls if call[0] == "GET"))

    def test_redirects_never_forward_the_token_or_print_upstream_content(self):
        operation = str(uuid.uuid4())
        with server([(302, {"private": TOKEN})]) as (url, calls):
            with self.assertRaisesRegex(client.Failure, "HTTP 302"):
                client.deploy(client.Client(url, TOKEN), IMAGE, operation)
            self.assertEqual(len(calls), 1)

    def test_pipeline_exits_nonzero_for_failure_revocation_and_timeout_without_secret_output(self):
        operation, release = str(uuid.uuid4()), str(uuid.uuid4())
        for responses, timeout in [([(200, outcome(operation, release, "cancelled"))], "10"),
                                   ([(401, {"private": TOKEN})], "10"),
                                   ([(200, outcome(operation, release, "queued", completed=False, succeeded=False))], "1")]:
            with server(responses) as (url, _), patch.dict(os.environ, {"CI_DEPLOY_URL": url, "CI_DEPLOY_TOKEN": TOKEN,
                    "CI_DEPLOY_OPERATION": operation, "CI_DEPLOY_IMAGE": IMAGE, "CI_DEPLOY_TIMEOUT": timeout,
                    "GITHUB_OUTPUT": ""}), contextlib.redirect_stdout(io.StringIO()) as out, contextlib.redirect_stderr(io.StringIO()) as err:
                self.assertEqual(client.main(), 1)
                self.assertNotIn(TOKEN, out.getvalue() + err.getvalue())

    def test_action_run_identity_is_stable_across_reruns(self):
        operations = []
        class FixtureClient:
            def __init__(self, *args):
                pass
        def finish(_client, _image, operation, *_args):
            operations.append(operation)
            return {"digest": "sha256:" + "c" * 64}
        with patch.dict(os.environ, {"CI_DEPLOY_URL": "https://service.example.test/__firecrab/deploy", "CI_DEPLOY_OPERATION": "",
                "CI_DEPLOY_RUN_KEY": "123/job/step", "GITHUB_OUTPUT": ""}), patch.object(client, "Client", FixtureClient), patch.object(client, "deploy", finish):
            self.assertEqual(client.main(), 0)
            self.assertEqual(client.main(), 0)
        self.assertEqual(operations[0], operations[1])

    def test_loopback_plaintext_never_sends_credentials_to_an_environment_proxy(self):
        operation, release = str(uuid.uuid4()), str(uuid.uuid4())
        with server([(200, outcome(operation, release))]) as (url, direct), server([]) as (proxy, proxied), patch.dict(os.environ,
                {"HTTP_PROXY": proxy, "http_proxy": proxy, "ALL_PROXY": proxy, "all_proxy": proxy, "NO_PROXY": "", "no_proxy": ""}):
            self.assertTrue(client.deploy(client.Client(url, TOKEN), IMAGE, operation)["succeeded"])
            self.assertEqual(len(direct), 1)
            self.assertEqual(proxied, [])


if __name__ == "__main__":
    unittest.main()
