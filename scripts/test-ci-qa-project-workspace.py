#!/usr/bin/env python3
"""Ownership and signal guards for the real workspace acceptance harness."""
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
import uuid


SCRIPT = Path(__file__).with_name("ci-qa-project-workspace.py")
spec = importlib.util.spec_from_file_location("workspace_qa", SCRIPT)
qa = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qa)


class Ownership(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.run_id = uuid.uuid4()
        (self.root / "owner").write_text(str(self.run_id))
        (self.root / "owner").chmod(0o600)
        (self.root / "bin").mkdir()
        for name in ("firecrab-api", "firecrab-helper"):
            (self.root / "bin" / name).write_text("fixture")

    def test_owned_root_is_accepted(self):
        qa.validate_owner(self.root, self.run_id)

    def test_another_run_cannot_claim_the_root(self):
        with self.assertRaises(ValueError):
            qa.validate_owner(self.root, uuid.uuid4())

    def test_shared_root_is_rejected(self):
        self.root.chmod(0o755)
        with self.assertRaises(ValueError):
            qa.validate_owner(self.root, self.run_id)

    def test_shared_marker_is_rejected(self):
        (self.root / "owner").chmod(0o644)
        with self.assertRaises(ValueError):
            qa.validate_owner(self.root, self.run_id)

    def test_symlink_marker_is_rejected(self):
        target = self.root / "retained-marker"
        (self.root / "owner").rename(target)
        (self.root / "owner").symlink_to(target)
        with self.assertRaises(ValueError):
            qa.validate_owner(self.root, self.run_id)

    def test_binary_path_cannot_escape_the_owned_root(self):
        binary = self.root / "bin/firecrab-api"
        target = self.root / "outside-api"
        binary.rename(target)
        binary.symlink_to(target)
        with self.assertRaises(ValueError):
            qa.validate_owner(self.root, self.run_id)

    @unittest.skipUnless(sys.platform == "linux", "requires Linux process identity")
    def test_wrong_process_is_not_signalled(self):
        process = subprocess.Popen(["sleep", "30"], cwd=self.root)
        try:
            with self.assertRaises(RuntimeError):
                qa.stop_owned(process, self.root / "bin/firecrab-api", self.root)
            self.assertIsNone(process.poll())
        finally:
            process.terminate()
            process.wait(timeout=5)

    def test_live_mode_is_explicit(self):
        result = subprocess.run([sys.executable, str(SCRIPT), "--root", str(self.root),
            "--run-id", str(self.run_id)], capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("requires --live", result.stderr)


if __name__ == "__main__":
    unittest.main()
