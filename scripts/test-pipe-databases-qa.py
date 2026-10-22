#!/usr/bin/env python3
"""Guard and failure-cleanup contracts for disposable database QA; no real Docker."""

import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("pipe_database_qa", ROOT / "scripts/test-pipe-databases.py")
qa = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qa)


class DatabaseQaContracts(unittest.TestCase):
    def test_exported_sources_can_report_revision_without_git(self):
        environment = {"FIRECRAB_QA_SOURCE_REVISION": "0123456789abcdef", "FIRECRAB_QA_SOURCE_DIRTY": "1"}
        with patch.dict(os.environ, environment), patch.object(qa.shutil, "which", return_value=None):
            self.assertEqual(qa.source_metadata(), ("0123456789abcdef", True))

    def test_refuses_remote_database_writes(self):
        for uri in ["postgresql://user:password@db.rds.amazonaws.com/app", "https://user:password@localhost/app", "postgresql://user@localhost/app"]:
            with self.subTest(uri=uri), patch.dict(os.environ, {"FIRECRAB_TEST_POSTGRES_URI": uri}):
                with self.assertRaises(ValueError):
                    qa.Fixture("postgres", sys.executable)

    def run_wrapper(self, failure, browser=False):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binaries = root / "bin"
            temporary = root / "tmp"
            binaries.mkdir()
            temporary.mkdir()
            log = root / "calls.jsonl"
            fake = "#!" + sys.executable + "\n" + r'''
import json, os, pathlib, sys
name = pathlib.Path(sys.argv[0]).name
args = sys.argv[1:]
with open(os.environ["QA_CALLS"], "a") as stream:
    stream.write(json.dumps([name, *args]) + "\n")
if name == "uname":
    print("Linux")
elif name == "openssl":
    for flag in ["-keyout", "-out"]:
        if flag in args: pathlib.Path(args[args.index(flag) + 1]).write_text("test fixture")
elif name == "python3":
    if args[0] == "-c": print("contract-fixture")
    else: sys.exit(31 if os.environ["QA_FAILURE"] == "qa" else 0)
elif name == "docker":
    stage = os.environ["QA_FAILURE"]
    if args[0] == "inspect" and len(args) == 2: sys.exit(0 if stage == "cleanup" else 1)
    if args[0] == "run" and "mysql:8.4" in args and stage == "mysql-start": sys.exit(23)
    if args[0] == "exec" and "ALTER INSTANCE RELOAD TLS" in args and stage == "tls": sys.exit(29)
    if args[0] == "inspect": print("true" if ".State.Running" in args[2] else "15432")
'''
            for name in ["docker", "openssl", "python3", "uname", "psql", "mysql"]:
                path = binaries / name
                path.write_text(fake)
                path.chmod(0o755)
            api = root / "api"
            api.write_text("#!/bin/sh\nexit 0\n")
            api.chmod(0o755)
            environment = dict(os.environ, PATH=str(binaries) + os.pathsep + os.environ["PATH"],
                               TMPDIR=str(temporary), QA_CALLS=str(log), QA_FAILURE=failure)
            command = ["bash", str(ROOT / "scripts/ci-qa-pipe-databases.sh"), "--api", str(api), "--output", str(root / "report.json")]
            if browser:
                command.append("--browser")
            result = subprocess.run(command, env=environment, capture_output=True, text=True, timeout=10)
            calls = [json.loads(line) for line in log.read_text().splitlines()]
            cleanup = [call for call in calls if call[:3] == ["docker", "rm", "-fv"]]
            self.assertEqual(len(cleanup), 1)
            self.assertEqual(set(cleanup[0][3:]), {"qa-pipe-postgres-contract-fixture", "qa-pipe-mysql-contract-fixture"})
            self.assertEqual(list(temporary.iterdir()), [])
            return result.returncode, calls

    def test_cleans_up_after_database_start_and_tls_failures(self):
        for failure, expected in [("mysql-start", 23), ("tls", 29)]:
            with self.subTest(failure=failure):
                code, calls = self.run_wrapper(failure)
                self.assertEqual(code, expected)
                self.assertFalse(any(call[0] == "python3" and "--require-tls" in call for call in calls))

    def test_propagates_qa_failure_and_runs_strict_tls_browser_checks(self):
        code, calls = self.run_wrapper("qa", browser=True)
        self.assertEqual(code, 31)
        qa_calls = [call for call in calls if call[0] == "python3" and "--require-tls" in call]
        self.assertEqual(len(qa_calls), 1)
        self.assertIn("--browser", qa_calls[0])
        self.assertIn("--frontend-dist", qa_calls[0])

    def test_leftover_containers_fail_an_otherwise_successful_run(self):
        code, _ = self.run_wrapper("cleanup")
        self.assertEqual(code, 1)


if __name__ == "__main__":
    unittest.main()
