#!/usr/bin/env python3
"""Check Secrets QA reporting and build gates without launching services."""

import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("qa_secrets", Path(__file__).with_name("ci-qa-secrets.py"))
qa = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qa)


class SecretsQaTests(unittest.TestCase):
    def run_qa(self, failed_stage=None, missing_case=None):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            results = root / "report"
            calls = []

            def run(name, command, output, env):
                calls.append(name)
                if name in ("secrets-ui", "secrets-live"):
                    numbers = range(1, 9) if name == "secrets-ui" else range(9, 15)
                    specs = [{"title": f"SEC{number}: fixture", "tests": [{"results": [{"status": "passed"}]}]} for number in numbers if number != missing_case]
                    report = root / f"firecrab-e2e/test-results/{name}/results.json"
                    report.parent.mkdir(parents=True, exist_ok=True)
                    report.write_text(json.dumps({"suites": [{"suites": [{"specs": specs}]}]}))
                return {"name": name, "status": "FAILED" if name == failed_stage else "PASS", "exitCode": int(name == failed_stage)}

            with patch.object(qa, "ROOT", root), patch.object(qa, "git", return_value="fixture-revision"), patch.object(qa, "run_stage", side_effect=run), patch.object(qa.sys, "platform", "linux"), patch.object(qa.sys, "argv", ["ci-qa-secrets.py", "--results-dir", str(results)]):
                code = qa.main()
            return code, json.loads((results / "summary.json").read_text()), calls

    def test_manual_account_warnings_do_not_fail_automated_coverage(self):
        code, report, _ = self.run_qa()
        self.assertEqual(code, 0)
        self.assertEqual([report["cases"][f"SEC{number}"]["status"] for number in range(1, 15)], ["PASS"] * 14)
        self.assertNotIn("SEC15", report["cases"])
        self.assertEqual(report["cases"]["SEC16"]["status"], "WARNING")

    def test_failure_still_runs_later_checks_and_returns_nonzero(self):
        code, report, calls = self.run_qa(failed_stage="secrets-ui")
        self.assertEqual(code, 1)
        self.assertIn("secrets-live", calls)
        self.assertEqual(report["exitCode"], 1)

    def test_missing_browser_case_is_not_a_pass(self):
        code, report, _ = self.run_qa(missing_case=12)
        self.assertEqual(code, 1)
        self.assertEqual(report["cases"]["SEC12"]["status"], "WARNING")

    def test_failed_build_blocks_live_checks_against_stale_binaries(self):
        code, report, calls = self.run_qa(failed_stage="api-build")
        self.assertEqual(code, 1)
        self.assertNotIn("secrets-live", calls)
        self.assertEqual(report["cases"]["SEC9"]["status"], "WARNING")


if __name__ == "__main__":
    unittest.main()
