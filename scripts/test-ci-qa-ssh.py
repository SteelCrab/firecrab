#!/usr/bin/env python3
"""Check SSH QA failure diagnostics with a fake API and no guest or network."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent
TRANSPORT = r'''#!/usr/bin/env python3
import json, os, pathlib, sys
args = sys.argv[1:]
url = next(arg for arg in args if arg.startswith('http://'))
route = url.split('/api/', 1)[1]
root = pathlib.Path(os.environ['SSH_QA_TEST_ROOT'])
if route.endswith('/ssh-key'):
    body = '-----BEGIN OPENSSH PRIVATE KEY-----\nprivate-key-must-not-be-logged\n'
elif route.endswith('/ssh-host-key/check'):
    count_file = root / 'checks'
    count = int(count_file.read_text()) + 1 if count_file.exists() else 1
    count_file.write_text(str(count))
    ready = os.environ['SSH_QA_TEST_READY'] == '1' or count > 1
    body = json.dumps({'status': 'match' if ready else 'unreachable'})
elif route.endswith('/ssh-host-key'):
    body = json.dumps({'fingerprint': 'SHA256:fixture', 'publicKey': 'ssh-ed25519 fixture'})
elif route.endswith('/log'):
    body = json.dumps({'consoleLog': (root / 'console').read_text()})
else:
    raise SystemExit('unexpected API route ' + route)
if '-o' in args:
    pathlib.Path(args[args.index('-o') + 1]).write_text(body)
else:
    print(body)
if '-w' in args:
    print(200, end='')
'''


class SshQaTests(unittest.TestCase):
    def run_qa(self, console, ready=False):
        with tempfile.TemporaryDirectory(prefix="firecrab-ssh-qa-") as temporary:
            root = Path(temporary)
            (root / "console").write_text(console)
            for name, content in {
                "curl": TRANSPORT,
                "uname": "#!/bin/sh\necho Darwin\n",
                "sleep": "#!/bin/sh\nexit 0\n",
            }.items():
                tool = root / name
                tool.write_text(content)
                tool.chmod(0o755)
            env = {
                key: value for key, value in os.environ.items()
                if not key.startswith("FIRECRAB_QA_")
            }
            env.update({
                "PATH": str(root) + os.pathsep + os.environ["PATH"],
                "FIRECRAB_API": "http://qa.invalid:5523",
                "SSH_QA_TEST_ROOT": str(root),
                "SSH_QA_TEST_READY": "1" if ready else "0",
            })
            result = subprocess.run(
                ["bash", str(ROOT / "ci-qa-ssh.sh"), "fixture", "192.0.2.1"],
                env=env, text=True, capture_output=True, timeout=15,
            )
            return result, int((root / "checks").read_text())

    def test_package_failure_preserves_bounded_console_without_private_key(self):
        console = "discarded-old-line\n"
        console += "\n".join(f"old log {number}" for number in range(100))
        console += "\n" + "x" * 20000 + "\n"
        console += "\nWARNING: fetching APKINDEX: temporary error\n"
        console += "FIRECRAB_PACKAGES_FAILED base-packages\n"
        console += "FIRECRAB_SSHD skipped: no /usr/sbin/sshd\n"
        result, checks = self.run_qa(console)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertEqual(checks, 1)
        self.assertIn("FAILED V8c", result.stderr)
        self.assertIn("fetching APKINDEX: temporary error", result.stderr)
        self.assertNotIn("discarded-old-line", result.stderr)
        self.assertLess(len(result.stderr), 17000)
        self.assertNotIn("private-key-must-not-be-logged", result.stdout + result.stderr)

    def test_incomplete_boot_can_recover_without_package_failure(self):
        result, checks = self.run_qa("FIRECRAB_NETWORK_READY 192.0.2.1\n")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(checks, 2)
        self.assertIn("PASS V8c", result.stdout)
        self.assertNotIn("Guest console", result.stderr)

    def test_ready_ssh_does_not_report_old_package_failure(self):
        result, checks = self.run_qa(
            "FIRECRAB_PACKAGES_FAILED\nFIRECRAB_SSHD skipped: no /usr/sbin/sshd\n",
            ready=True,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(checks, 1)
        self.assertNotIn("Guest console", result.stderr)


if __name__ == "__main__":
    unittest.main()
