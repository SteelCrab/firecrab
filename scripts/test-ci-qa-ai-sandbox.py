#!/usr/bin/env python3
"""Reject false live-gate evidence and protect borrowed resources; no live calls."""

import copy
from contextlib import redirect_stderr, redirect_stdout
import importlib.util
import io
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
import uuid
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("sandbox_qa", Path(__file__).with_name("ci-qa-ai-sandbox.py"))
qa = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qa)

RUN = str(uuid.uuid4())
PROJECT = str(uuid.uuid4())
VM = str(uuid.uuid4())
CONNECTION = str(uuid.uuid4())
SELECTION = {"connectionId": CONNECTION, "model": "fixture-model"}
NONCE = "f0" * 16


def result():
    return {"projectId": PROJECT, "selection": SELECTION.copy(), "state": "result",
            "attemptId": str(uuid.uuid4()), "runtime": {"vmId": VM, "runtimeId": str(uuid.uuid4())},
            "result": f"Guest returned {NONCE}", "events": [{"name": "run_command", "input": json.dumps({"command": qa.guest_command(RUN), "timeout_seconds": 30}),
                "output": f"FIRECRAB414:{RUN}:stdout:{NONCE}\n", "stderr": f"FIRECRAB414:{RUN}:stderr\n", "exitCode": 7}],
            "changes": [{"path": "guest-result.txt", "before": "not executed\n", "after": NONCE + "\n", "applied": False}]}


class ReadOnlyApi:
    def __init__(self):
        self.calls = []
        self.connection = {"id": CONNECTION, "name": f"sandbox-foundation-{RUN}"}
        self.machine = {"id": VM, "name": f"sandbox-foundation-{RUN}", "state": "running", "purpose": "instance"}
        self.projects = []

    def request(self, method, path):
        self.calls.append((method, path))
        if method != "GET":
            raise AssertionError("Preflight must not mutate or contact the provider")
        return 200, {"/api/pipe/ai/connections": [self.connection],
                     f"/api/vms/{VM}": self.machine,
                     "/api/ai-sandbox/projects": self.projects}[path]


class SandboxFoundationQaTests(unittest.TestCase):
    def test_complete_guest_evidence_includes_model_received_entropy(self):
        self.assertEqual(qa.validate_result(result(), PROJECT, VM, SELECTION, RUN), NONCE)

    def test_assistant_text_alone_cannot_claim_live_execution(self):
        task = result()
        task["events"] = []
        with self.assertRaises(qa.QaFailure):
            qa.validate_result(task, PROJECT, VM, SELECTION, RUN)

    def test_partial_or_misattributed_results_are_not_passes(self):
        mutations = [
            lambda t: t.update(state="failed"),
            lambda t: t.update(projectId=str(uuid.uuid4())),
            lambda t: t.update(selection={**SELECTION, "model": "silently-fallen-back"}),
            lambda t: t.update(attemptId=str(uuid.UUID(int=0))),
            lambda t: t["runtime"].update(vmId=str(uuid.uuid4())),
            lambda t: t["runtime"].update(runtimeId=str(uuid.UUID(int=0))),
            lambda t: t.update(result="No guest result reached the model"),
            lambda t: t["events"][0].update(input=json.dumps({"command": "true"})),
            lambda t: t["events"][0].update(exitCode=0),
            lambda t: t["events"][0].update(stderr="merged with stdout"),
            lambda t: t["events"][0].update(output="predetermined fake stdout\n"),
            lambda t: t["changes"][0].update(after="not the guest nonce\n"),
            lambda t: t["changes"][0].update(applied=True),
        ]
        for index, mutate in enumerate(mutations):
            with self.subTest(case=index):
                task = copy.deepcopy(result())
                mutate(task)
                with self.assertRaises(qa.QaFailure):
                    qa.validate_result(task, PROJECT, VM, SELECTION, RUN)

    def test_borrowed_account_is_rejected_before_any_other_request(self):
        api = ReadOnlyApi()
        api.connection["name"] = "User account"
        with self.assertRaises(qa.QaFailure):
            qa.owned_inputs(api, RUN, CONNECTION, VM)
        self.assertEqual(api.calls, [("GET", "/api/pipe/ai/connections")])

    def test_system_stopped_or_borrowed_vm_is_rejected(self):
        for change in ({"name": "unrelated-vm"}, {"purpose": "service"}, {"state": "stopped"}):
            with self.subTest(change=change):
                api = ReadOnlyApi()
                api.machine.update(change)
                with self.assertRaises(qa.QaFailure):
                    qa.owned_inputs(api, RUN, CONNECTION, VM)
                self.assertTrue(all(method == "GET" for method, _ in api.calls))

    def test_project_uses_nested_spec_and_prevents_resource_reuse(self):
        for settings in ({"vmId": VM}, {"selection": SELECTION}):
            with self.subTest(settings=settings):
                api = ReadOnlyApi()
                api.projects = [{"id": PROJECT, "spec": settings}]
                with self.assertRaises(qa.QaFailure):
                    qa.owned_inputs(api, RUN, CONNECTION, VM)

    def test_only_explicit_loopback_api_urls_are_allowed(self):
        for url in ("https://example.com", "http://127.0.0.1", "http://127.0.0.1:5523/api",
                    "http://user:password@127.0.0.1:5523", "http://127.0.0.1:5523?target=remote"):
            with self.subTest(url=url), self.assertRaises(qa.QaFailure):
                qa.Api(url)
        self.assertEqual(qa.Api("http://127.0.0.1:5523/").base, "http://127.0.0.1:5523")

    def test_reports_are_private_and_keep_the_latest_checkpoint(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "summary.json"
            qa.save_report(output, {"status": "RUNNING", "projectId": PROJECT})
            qa.save_report(output, {"status": "FAILED", "projectId": PROJECT})
            self.assertEqual(json.loads(output.read_text())["status"], "FAILED")
            self.assertEqual(output.stat().st_mode & 0o777, 0o600)
            self.assertEqual(list(Path(directory).iterdir()), [output])

    def test_live_and_disconnect_flags_are_required_before_contacting_api(self):
        args = ["ci-qa-ai-sandbox.py", "--api", "http://127.0.0.1:5523", "--run-id", RUN,
                "--connection", CONNECTION, "--vm", VM, "--default-model", "one", "--override-model", "two", "--results-dir", "/unused"]
        with redirect_stderr(io.StringIO()), patch.object(qa.sys, "argv", args), patch.object(qa.sys, "platform", "linux"), patch.object(qa, "Api") as api:
            with self.assertRaises(SystemExit) as error:
                qa.main()
            self.assertEqual(error.exception.code, 2)
            api.assert_not_called()

    def test_complete_flow_retains_evidence_and_preserves_borrowed_vm(self):
        with tempfile.TemporaryDirectory() as directory:
            api = LifecycleApi()
            args = SimpleNamespace(run_id=uuid.UUID(RUN), connection=uuid.UUID(CONNECTION), vm=uuid.UUID(VM),
                                   api="http://127.0.0.1:5523", default_model="default", override_model="override",
                                   results_dir=Path(directory) / "results")
            with redirect_stdout(io.StringIO()):
                qa.run_gate(args, api)
            report = json.loads((args.results_dir / "summary.json").read_text())
            self.assertEqual(report["status"], "PASS")
            self.assertEqual(len(report["tasks"]), 2)
            self.assertEqual(set(report["checks"]), {"nativeModelPermissions", "projectDefault", "taskOverride",
                "deniedModelVerification", "deniedModel", "afterDisconnect", "resultExport", "ownedCleanup"})
            self.assertEqual({t["selection"]["model"] for t in report["tasks"].values()}, {"default", "override"})
            self.assertTrue(api.disconnected)
            self.assertTrue(api.project_deleted)
            self.assertIs(report["connectionDisconnect"]["remoteRevocationConfirmed"], False)
            self.assertFalse((args.results_dir / "source").exists())
            self.assertFalse(any(method == "DELETE" and path.startswith("/api/vms/") for method, path in api.calls))

    def test_failed_guest_proof_retains_source_and_ids_for_reconciliation(self):
        with tempfile.TemporaryDirectory() as directory:
            api = LifecycleApi(bad_exit=True)
            args = SimpleNamespace(run_id=uuid.UUID(RUN), connection=uuid.UUID(CONNECTION), vm=uuid.UUID(VM),
                                   api="http://127.0.0.1:5523", default_model="default", override_model="override",
                                   results_dir=Path(directory) / "results")
            with redirect_stdout(io.StringIO()), self.assertRaises(qa.QaFailure):
                qa.run_gate(args, api)
            report = json.loads((args.results_dir / "summary.json").read_text())
            self.assertEqual(report["status"], "FAILED")
            self.assertTrue(report["retainedOnFailure"])
            self.assertEqual(len(report["tasks"]), 1)
            self.assertTrue((args.results_dir / "source").exists())
            self.assertFalse(api.disconnected or api.project_deleted)


class LifecycleApi(ReadOnlyApi):
    """REST schema fixture only; it is never live-model or real-VM evidence."""
    def __init__(self, bad_exit=False):
        super().__init__()
        self.connection.update(provider="openai", authentication="chatgpt")
        self.machine.update(microNetworkId=str(uuid.uuid4()), template="fixture", templateVersion="1")
        self.tasks = {}
        self.disconnected = False
        self.project_deleted = False
        self.project = None
        self.runtime = str(uuid.uuid4())
        self.bad_exit = bad_exit

    def request(self, method, path, body=None, allowed=(200,)):
        self.calls.append((method, path))
        status, doc = 200, {}
        if path == "/api/pipe/ai/connections":
            doc = [] if self.disconnected else [self.connection]
        elif path == f"/api/vms/{VM}":
            doc = self.machine
        elif path == "/api/ai-sandbox/projects":
            doc = []
        elif "/connections/" in path and "/verify" in path:
            if self.disconnected:
                status = 409
            elif body["model"].startswith("unverified-"):
                status = 404
            else:
                doc = {"verified": True, "toolsVerified": True}
        elif "/connections/" in path and "/models" in path:
            if self.disconnected:
                status = 409
            else:
                doc = {"models": [{"id": m, "verified": True} for m in ("default", "override")]}
        elif path == f"/api/pipe/ai/connections/{CONNECTION}" and method == "DELETE":
            self.disconnected = True
            doc = {"deleted": True, "remoteRevocationConfirmed": False}
        elif "/projects/" in path and "/tasks/" not in path:
            if method == "PUT":
                self.project = {"id": path.rsplit("/", 1)[1], "spec": body}
            elif method == "DELETE":
                self.project_deleted = True
            doc = self.project
        elif "/projects/" in path and "/tasks/" in path:
            key = path.rsplit("/", 1)[1]
            selected = body.get("selection", self.project["spec"]["selection"])
            if self.disconnected or selected["model"].startswith("unverified-"):
                status = 400
            else:
                task = result()
                task.update(id=key, projectId=self.project["id"], request=body, selection=selected,
                            runtime={"vmId": VM, "runtimeId": self.runtime})
                if self.tasks:
                    task = json.loads(json.dumps(task).replace(NONCE, "ab" * 16))
                if self.bad_exit:
                    task["events"][0]["exitCode"] = 0
                self.tasks[key] = task
                status, doc = 202, task
        elif "/tasks/" in path and method == "GET":
            key = path.rsplit("/", 1)[1]
            status, doc = (200, self.tasks[key]) if key in self.tasks else (404, {})
        else:
            raise AssertionError(f"Unexpected fixture request: {method} {path}")
        if status not in allowed:
            raise qa.QaFailure(f"Unexpected fixture status {status}")
        return status, copy.deepcopy(doc)


if __name__ == "__main__":
    unittest.main()
