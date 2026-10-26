#!/usr/bin/env python3
"""Offline plan/ownership/syntax checks. Never execute the guest probe or contact an API."""
import ast
import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import stat
import tempfile
import unittest
import uuid

PATH = Path(__file__).with_name("qa-sandbox-openrc-v1.py")
spec = importlib.util.spec_from_file_location("openrc_plan", PATH)
qa = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qa)
RUN = uuid.UUID("659958b5-b7a8-4e81-a5c0-b0f70585ca5e")
VM = "e5f36c4d-eb59-4c5d-8a0e-631335234f75"
RUNTIME = "4fe36daa-0f63-4b13-8b32-53bc5c9e6d90"
GENERATION = "399122a6-939a-4c2d-8751-d6df845b15af"


def exported_baseline():
    return ({"id": qa.PROJECT, "owner": "local", "spec": {"environmentId": qa.ENVIRONMENT,
             "vmId": None, "sourcePath": None, "selection": None, "instructions": "NoModel: retain strict seed baseline"}},
            {"id": qa.ENVIRONMENT, "projectId": qa.PROJECT, "owner": "local", "state": "ready",
             "generation": GENERATION, "dnsMode": "private_only", "roles": [{"role": "debug",
             "vmId": VM, "runtime": {"vmId": VM, "runtimeId": RUNTIME}, "agent": "ready", "network": "ready"}]})


class OfflineTests(unittest.TestCase):
    def test_preparer_has_no_network_process_or_ssh_execution_surface(self):
        tree = ast.parse(PATH.read_text())
        imported = {node.names[0].name.split(".")[0] if isinstance(node, ast.Import) else node.module.split(".")[0]
                    for node in ast.walk(tree) if isinstance(node, (ast.Import, ast.ImportFrom))}
        self.assertTrue(imported.isdisjoint({"urllib", "http", "socket", "subprocess", "requests", "asyncio"}))
        probe = PATH.parent / "fixtures/sandbox-openrc-v1/probe.py"
        ast.parse(probe.read_text())
        # Parse literal contracts without importing or running the guest payload.
        literals = {node.targets[0].id: ast.literal_eval(node.value) for node in ast.parse(probe.read_text()).body
                    if isinstance(node, ast.Assign) and isinstance(node.targets[0], ast.Name)
                    and node.targets[0].id in ("LITERAL_ARGS", "LITERAL_ENV", "STDIN")}
        self.assertEqual(literals["LITERAL_ARGS"], qa.LITERAL_ARGS)
        self.assertEqual(literals["LITERAL_ENV"], qa.LITERAL_ENV)
        self.assertEqual(literals["STDIN"], qa.STDIN.encode())

    def test_fixed_target_and_manual_gates_do_not_prepare_a_project_mutation(self):
        planned = qa.plan(RUN)
        self.assertTrue(planned["preparedOnly"])
        self.assertFalse(planned["liveQaAccepted"])
        self.assertFalse(planned["projectMutationPrepared"])
        self.assertTrue(all(value is False for value in planned["rootGates"].values()))
        self.assertEqual(planned["projectId"], qa.PROJECT)
        self.assertEqual(planned["environmentId"], qa.ENVIRONMENT)
        self.assertEqual(planned["role"], "debug")
        self.assertEqual(planned["selection"], qa.SELECTION)
        self.assertTrue(planned["remoteSourcePath"].startswith("/srv/firecrab-qa/openrc-v1-414-"))
        self.assertIsNone(planned["frozenBaseline"])

    def test_no_model_baseline_and_owned_strict_runtime_are_frozen_without_changes(self):
        project, environment = exported_baseline()
        original = copy.deepcopy((project, environment))
        frozen = qa.baseline(project, environment)
        self.assertEqual((project, environment), original)
        self.assertEqual(frozen["projectSpecSha256"], qa.digest(project["spec"]))
        for changed in ("project", "environment", "owner", "noModel", "binding", "dns", "state", "runtime", "agent"):
            p, e = copy.deepcopy(original)
            if changed == "project": p["id"] = str(uuid.uuid4())
            if changed == "environment": e["id"] = str(uuid.uuid4())
            if changed == "owner": e["owner"] = "foreign"
            if changed == "noModel": p["spec"]["instructions"] = "Run arbitrary tasks"
            if changed == "binding": p["spec"]["environmentId"] = str(uuid.uuid4())
            if changed == "dns": e["dnsMode"] = "legacy_forward"
            if changed == "state": e["state"] = "stopped"
            if changed == "runtime": e["roles"][0]["runtime"]["vmId"] = str(uuid.uuid4())
            if changed == "agent": e["roles"][0]["agent"] = "unsupported"
            with self.subTest(changed=changed), self.assertRaises(ValueError):
                qa.baseline(p, e)

    def test_native_tool_literal_nul_stdin_and_hard_limits_survive_json(self):
        planned = json.loads(json.dumps(qa.plan(RUN)))
        self.assertEqual(len({item["id"] for item in planned["tasks"]}), 5)
        for item in planned["tasks"]:
            request = item["request"]
            self.assertEqual(request["selection"], qa.SELECTION)
            self.assertEqual(request["environmentRole"], "debug")
            self.assertEqual(request["coordination"]["retryLimit"], 0)
            self.assertIsNone(request["retryOf"])
            for command in item["invocations"]:
                self.assertEqual(command["version"], 1)
                self.assertIsInstance(command["env"], list)
                self.assertEqual(command["cwd"], "qa cwd")
                self.assertFalse(any("\0" in value for value in command["argv"]))
                self.assertTrue(1 <= command["limits"]["wallSeconds"] <= 120)
                self.assertEqual(command["limits"]["cpuSeconds"], 3)
        literal = planned["tasks"][0]["invocations"][0]
        self.assertEqual(literal["argv"][4:], qa.LITERAL_ARGS)
        self.assertIn("\0", literal["stdin"])
        self.assertEqual(literal["stdin"].encode(), b"public-input\x00bytes\n")
        self.assertEqual(planned["tasks"][1]["invocations"][0]["limits"]["memoryMib"], 64)
        self.assertEqual(planned["tasks"][2]["invocations"][0]["limits"]["wallSeconds"], 2)
        self.assertEqual(planned["tasks"][3]["invocations"][0]["limits"]["outputBytes"], 256)
        cancel = planned["tasks"][4]
        self.assertEqual([c["argv"][2] for c in cancel["invocations"]], ["cancel-ready", "cancel-running"])
        self.assertIn("retain_artifact", cancel["request"]["input"])

    def test_admission_guards_keep_attempt_runtime_and_exact_request_identity(self):
        project, environment = exported_baseline()
        planned = qa.plan(RUN, qa.baseline(project, environment))
        item = planned["tasks"][0]
        saved = {"id": item["id"], "projectId": qa.PROJECT, "selection": qa.SELECTION,
                 "request": item["request"], "environment": planned["frozenBaseline"]["environment"],
                 "runtime": planned["frozenBaseline"]["runtime"], "attemptId": str(uuid.uuid4())}
        refs = qa.admission(saved, planned, item)
        self.assertEqual(refs, qa.admission(copy.deepcopy(saved), planned, item, refs))
        for field in ("id", "projectId", "selection", "request", "environment", "runtime", "attemptId"):
            changed = copy.deepcopy(saved)
            changed[field] = str(uuid.uuid4()) if field in ("id", "projectId", "attemptId") else {}
            with self.subTest(field=field), self.assertRaises(ValueError):
                qa.admission(changed, planned, item, refs)

    def test_prepare_writes_private_public_payload_files_once_and_never_runs_them(self):
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "prepared"
            qa.prepare(target, RUN)
            self.assertEqual(stat.S_IMODE(target.stat().st_mode), 0o700)
            for p in target.rglob("*"):
                if p.is_file():
                    self.assertEqual(stat.S_IMODE(p.stat().st_mode), 0o600)
            self.assertEqual((target / "source/probe.txt").read_text(), "not executed\n")
            self.assertEqual(len(list((target / "source/qa cwd").iterdir())), 1)
            self.assertFalse(list((target / "source").rglob("*-literal.json")))
            with self.assertRaises(FileExistsError):
                qa.prepare(target, RUN)

    def test_artifact_checks_bind_nonce_bytes_literal_contract_and_actual_exit(self):
        project, environment = exported_baseline()
        planned = qa.plan(RUN, qa.baseline(project, environment));item = planned["tasks"][0]
        nonce = "d" * 32
        value = {"runId": RUN.hex, "case": "literal", "nonce": nonce, "uid": 1000,
                 "euid": 1000, "groups": [], "noNewPrivs": 1, "capEff": "0000",
                 "pid": 123, "startTicks": 456, "bootId": str(uuid.uuid4()),
                 "cwdName": "qa cwd", "addressSpaceLimit": [96 * 1024 * 1024] * 2,
                 "cpuLimit": [3, 3], "nofileLimit": [1024, 1024], "argv": qa.LITERAL_ARGS,
                 "env": {"QA_V1_LITERAL": qa.LITERAL_ENV, "QA_V1_RUN": RUN.hex},
                 "stdinHex": qa.STDIN.encode().hex(), "stdinSha256": hashlib.sha256(qa.STDIN.encode()).hexdigest()}
        payload = (json.dumps(value) + "\n").encode()
        saved = {"id": item["id"], "projectId": qa.PROJECT, "attemptId": str(uuid.uuid4()),
                 "selection": qa.SELECTION, "request": item["request"], "state": "result",
                 "runtime": planned["frozenBaseline"]["runtime"], "environment": planned["frozenBaseline"]["environment"],
                 "events": [{"name": "run_argv", "sequence": 3, "input": json.dumps(item["invocations"][0]),
                             "output": f"OPENRCV1:{RUN.hex}:literal:stdout:{nonce}\n",
                             "stderr": f"OPENRCV1:{RUN.hex}:literal:stderr\n", "exitCode": 7}],
                 "tests": [{"eventSequence": 3, "passed": False}], "result": f"Actual guest nonce {nonce}"}
        metadata = {"id": str(uuid.uuid4()), "taskId": saved["id"], "attemptId": saved["attemptId"],
                    "path": item["artifactPath"], "size": len(payload), "sha256": hashlib.sha256(payload).hexdigest()}
        saved["artifacts"] = [metadata]
        report = qa.artifact(saved, planned, item, metadata, payload)
        self.assertEqual(report["rawExitCodes"], [7]);self.assertFalse(report["liveQaAccepted"])
        for changed in ("bytes", "owner", "nonce", "exit", "stderr", "passed", "replay", "edit", "source", "result"):
            task = copy.deepcopy(saved);meta = copy.deepcopy(metadata);body = payload
            if changed == "bytes": body += b" "
            if changed == "owner": meta["attemptId"] = str(uuid.uuid4())
            if changed == "nonce": task["events"][0]["output"] = task["events"][0]["output"].replace(nonce, "e" * 32)
            if changed == "exit": task["events"][0]["exitCode"] = 0
            if changed == "stderr": task["events"][0]["stderr"] = ""
            if changed == "passed": task["tests"][0]["passed"] = True
            if changed == "replay": task["events"].append(task["events"][0])
            if changed == "edit": task["events"].append({"name": "write_file"})
            if changed == "source": task["changes"] = [{"path": "qa cwd/probe.py", "after": "fake probe"}]
            if changed == "result": task["result"] = "No actual nonce"
            with self.subTest(changed=changed), self.assertRaises(ValueError):
                qa.artifact(task, planned, item, meta, body)


if __name__ == "__main__":
    unittest.main(verbosity=2)
