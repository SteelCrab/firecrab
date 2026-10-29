#!/usr/bin/env python3
"""Offline evidence guards. These do not establish a real model/VM run."""
import copy
import importlib.util
import hashlib
import io
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest

spec = importlib.util.spec_from_file_location("coordination", Path(__file__).with_name("ci-qa-ai-sandbox-coordination.py"))
qa = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qa)


def task():
    return {"state": "result", "result": "Guest nonce " + "a" * 32,
            "events": [{"name": "run_argv", "sequence": 2, "input": json.dumps(qa.invocation("test-independent")),
                        "output": "FIRECRAB414:test-independent:stdout:" + "a" * 32 + "\n",
                        "stderr": "FIRECRAB414:test-independent:exit:0\n", "exitCode": 0}],
            "tests": [{"eventSequence": 2, "passed": True}],
            "changes": [{"path": qa.APPLICATION, "before": qa.application(), "after": qa.fixed_application(), "applied": False}],
            "artifacts": [{"path": "qa/test-independent.xml"}, {"path": "qa/test-independent.executed"}]}


class EvidenceGuards(unittest.TestCase):
    def test_results_require_completed_guest_commands_and_test_provenance(self):
        qa.validate(task(), "Test", ["test-independent"], [0])
        for field in ["exit", "stderr", "nonce", "modelReceipt", "provenance", "passed", "artifact", "wrongArtifacts", "testTampering", "changedFix", "testWrites", "state"]:
            value = task()
            if field == "exit": value["events"][0]["exitCode"] = None
            if field == "stderr": value["events"][0]["stderr"] = ""
            if field == "nonce": value["events"][0]["output"] = "model says success"
            if field == "modelReceipt": value["result"] = "done"
            if field == "provenance": value["tests"][0]["eventSequence"] = 1
            if field == "passed": value["tests"][0]["passed"] = False
            if field == "artifact": value["artifacts"] = []
            if field == "wrongArtifacts": value["artifacts"] = [{"path": "a.txt"}, {"path": "b.txt"}]
            if field == "testTampering": value["changes"].append({"path": "pom.xml"})
            if field == "changedFix": value["changes"][0]["after"] += "// extra mutation\n"
            if field == "testWrites": value["events"].append({"name": "write_file", "input": json.dumps({"path": qa.APPLICATION, "content": qa.fixed_application()})})
            if field == "state": value["state"] = "failed"
            with self.subTest(field=field), self.assertRaises(qa.qa.QaFailure):
                qa.validate(value, "Test", ["test-independent"], [0])

    def test_changed_limits_and_duplicate_commands_cannot_pass(self):
        for changed in ["limits", "command", "duplicate"]:
            value = task()
            command = json.loads(value["events"][0]["input"])
            if changed == "limits": command["limits"]["wallSeconds"] = 3600
            if changed == "command": command["argv"] = ["/bin/true"]
            value["events"][0]["input"] = json.dumps(command)
            if changed == "duplicate": value["events"].append(copy.deepcopy(value["events"][0]))
            with self.subTest(changed=changed), self.assertRaises(qa.qa.QaFailure):
                qa.validate(value, "Test", ["test-independent"], [0])

    def test_extra_diagnostics_preserve_required_artifacts_and_reject_duplicates(self):
        value = task()
        value["artifacts"].append({"path": "qa/test-independent.log"})
        qa.validate(value, "Test", ["test-independent"], [0])
        for artifacts in [value["artifacts"][1:], value["artifacts"] + [value["artifacts"][0]],
                          value["artifacts"] + [{"path": "unrelated.txt"}]]:
            rejected = copy.deepcopy(value)
            rejected["artifacts"] = artifacts
            with self.assertRaises(qa.qa.QaFailure):
                qa.validate(rejected, "Test", ["test-independent"], [0])

    def test_dependency_admission_uses_distinct_task_and_agent_identity(self):
        value = qa.input_for("Test", "debug-task-id")
        self.assertEqual(value["coordination"]["dependencies"], ["debug-task-id"])
        self.assertEqual(value["coordination"]["assignedAgent"], "Test")
        self.assertEqual(value["coordination"]["retryLimit"], 1)
        self.assertIsNone(value["retryOf"])
        command = qa.invocation("test-independent")
        self.assertIsInstance(command["env"], list)
        self.assertIn("mvn -o -q", command["argv"][-1])

    def test_download_requires_the_actual_junit_case_and_matching_guest_nonce(self):
        valid_xml = b'<testsuite tests="1" errors="0" failures="0"><testcase classname="io.firecrab.qa.GreetingTest" name="servesConfiguredGreeting"/></testsuite>'
        for changed in [None, "case", "nonce", "hash", "owner", "diagnostic"]:
            value = task()
            value.update({"id": "task", "attemptId": "attempt"})
            xml = valid_xml.replace(b"servesConfiguredGreeting", b"differentTest") if changed == "case" else valid_xml
            nonce = ("b" if changed == "nonce" else "a") * 32 + "\n"
            blobs = {"xml": xml, "nonce": nonce.encode(), "log": b"actual offline build log\n"}
            value["changes"].append({"path": "qa/test-independent.log", "after": "actual offline build log\n"})
            value["artifacts"] = [{"id": key, "path": path, "taskId": "task", "attemptId": "attempt", "size": len(blobs[key]), "sha256": hashlib.sha256(blobs[key]).hexdigest()}
                                  for key, path in [("xml", "qa/test-independent.xml"), ("nonce", "qa/test-independent.executed"), ("log", "qa/test-independent.log")]]
            if changed == "hash": value["artifacts"][0]["sha256"] = "0" * 64
            if changed == "owner": value["artifacts"][0]["taskId"] = "another-task"
            if changed == "diagnostic": value["changes"][-1]["after"] = "different build log\n"
            api = SimpleNamespace(base="http://127.0.0.1:5523", http=SimpleNamespace(open=lambda request, timeout: io.BytesIO(blobs[request.full_url.rsplit("/", 1)[1]])))
            with self.subTest(changed=changed), tempfile.TemporaryDirectory() as directory:
                if changed is None:
                    qa.download(api, value, Path(directory), "test-independent")
                else:
                    with self.assertRaises(qa.qa.QaFailure):
                        qa.download(api, value, Path(directory), "test-independent")


if __name__ == "__main__":
    unittest.main()
