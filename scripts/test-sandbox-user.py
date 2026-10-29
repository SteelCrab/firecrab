#!/usr/bin/env python3
"""Exercise the real trusted UID/lock helper on Linux. No VM or daemon is created."""
import fcntl
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[1]


@unittest.skipUnless(sys.platform == "linux", "Linux prctl/setresuid/flock contract")
class SandboxUser(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.directory = tempfile.TemporaryDirectory(prefix="firecrab-sandbox-user-")
        root = Path(cls.directory.name)
        root.chmod(0o755)
        driver = root / "driver.c"
        driver.write_text('#include "user.h"\nint main(int argc, char **argv) { return firecrab_sandbox_user(argc, argv); }\n')
        cls.binary = root / "sandbox-user"
        subprocess.run([os.environ.get("CC", "cc"), "-std=c11", "-Wall", "-Wextra", "-Werror", "-I", str(ROOT / "packaging/guest-console"), str(ROOT / "packaging/guest-console/user.c"), str(driver), "-o", str(cls.binary)], check=True)

    @classmethod
    def tearDownClass(cls):
        cls.directory.cleanup()

    def root(self):
        if os.geteuid() != 0:
            self.skipTest("run with sudo to verify real UID dropping")

    def run_user(self, *argv):
        return subprocess.run([str(self.binary), "--reuid", "65533", "--regid", "65533", "--clear-groups", "--no-new-privs", *argv], capture_output=True, check=False, timeout=5)

    def test_malformed_and_root_targets_are_rejected(self):
        self.root()
        for uid in ["0", "-1", "+1", "1x", "4294967295"]:
            result = subprocess.run([str(self.binary), "--reuid", uid, "--regid", "65533", "--clear-groups", "--no-new-privs", "/bin/true"], capture_output=True, check=False)
            self.assertEqual(result.returncode, 77)

    def test_exact_identity_no_new_privileges_and_no_supplementary_groups(self):
        self.root()
        result = self.run_user(sys.executable, "-c", 'import json,os; print(json.dumps([os.getuid(),os.geteuid(),os.getgid(),os.getgroups()])); print(open("/proc/self/status").read().split("NoNewPrivs:")[1].splitlines()[0].strip())')
        self.assertEqual(result.returncode, 0, result.stderr)
        lines = result.stdout.decode().splitlines()
        self.assertEqual(json.loads(lines[0]), [65533, 65533, 65533, []])
        self.assertEqual(lines[1], "1")

    def test_stdio_exit_and_argv_are_preserved(self):
        self.root()
        result = self.run_user("/bin/sh", "-c", 'printf "%s" "$1"; printf diagnostics >&2; exit 7', "sh", "$(echo escaped)'; literal")
        self.assertEqual(result.returncode, 7)
        self.assertEqual(result.stdout, b"$(echo escaped)'; literal")
        self.assertEqual(result.stderr, b"diagnostics")

    def test_unprivileged_caller_cannot_use_helper(self):
        self.root()
        def drop():
            os.setgroups([])
            os.setgid(65533)
            os.setuid(65533)
        result = subprocess.run([str(self.binary), "--sandbox-probe"], preexec_fn=drop, capture_output=True, check=False)
        self.assertEqual(result.returncode, 77)

    def test_lock_is_retained_by_the_parent_open_description_and_wait_is_bounded(self):
        self.root()
        path = Path(self.directory.name) / "owned.lock"
        with path.open("w+") as owned:
            result = subprocess.run([str(self.binary), "--sandbox-lock", str(owned.fileno()), "1"], pass_fds=(owned.fileno(),), check=False)
            self.assertEqual(result.returncode, 0)
            with path.open("r+") as contender:
                start = time.monotonic()
                result = subprocess.run([str(self.binary), "--sandbox-lock", str(contender.fileno()), "0"], pass_fds=(contender.fileno(),), check=False)
                self.assertEqual(result.returncode, 81)
                self.assertLess(time.monotonic() - start, 1)
                with self.assertRaises(BlockingIOError):
                    fcntl.flock(contender, fcntl.LOCK_EX | fcntl.LOCK_NB)
        with path.open("r+") as unlocked:
            fcntl.flock(unlocked, fcntl.LOCK_EX | fcntl.LOCK_NB)


if __name__ == "__main__":
    unittest.main()
