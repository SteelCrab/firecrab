#!/usr/bin/env python3
"""Offline evidence guards. These do not establish a real model/VM run."""
import copy
import importlib.util
import json
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("coordination", Path(__file__).with_name("ci-qa-ai-sandbox-coordination.py"))
qa = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qa)


def task():
    return {"state": "result", "result": "Guest nonce " + "a" * 32,
            "events": [{"name": "run_argv", "sequence": 2, "input": json.dumps(qa.invocation("test-independent")),
                        "output": "FIRECRAB414:test-independent:stdout:" + "a" * 32 + "\n",
                        "stderr": "FIRECRAB414:test-independent:exit:0\n", "exitCode": 0}],
            "tests": [{"eventSequence": 2, "passed": True}], "artifacts": [{}, {}]}


class EvidenceGuards(unittest.TestCase):
    def test_results_require_completed_guest_commands_and_test_provenance(self):
        qa.validate(task(), "Test", ["test-independent"], [0])
        for field in ["exit", "stderr", "nonce", "modelReceipt", "provenance", "passed", "artifact", "state"]:
            value = task()
            if field == "exit": value["events"][0]["exitCode"] = None
            if field == "stderr": value["events"][0]["stderr"] = ""
            if field == "nonce": value["events"][0]["output"] = "model says success"
            if field == "modelReceipt": value["result"] = "done"
            if field == "provenance": value["tests"][0]["eventSequence"] = 1
            if field == "passed": value["tests"][0]["passed"] = False
            if field == "artifact": value["artifacts"] = []
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

    def test_dependency_admission_uses_distinct_task_and_agent_identity(self):
        value = qa.input_for("Test", "debug-task-id")
        self.assertEqual(value["coordination"]["dependencies"], ["debug-task-id"])
        self.assertEqual(value["coordination"]["assignedAgent"], "Test")
        self.assertEqual(value["coordination"]["retryLimit"], 1)
        self.assertIsNone(value["retryOf"])
        command = qa.invocation("test-independent")
        self.assertIsInstance(command["env"], list)
        self.assertIn("mvn -o -q", command["argv"][-1])


if __name__ == "__main__":
    unittest.main()
