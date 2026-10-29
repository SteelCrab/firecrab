#!/usr/bin/env python3
"""Exercise managed updates without a VM, systemd, or modifying host files."""

import hashlib
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).parent / "micromanager/update-guest.sh"


class ManagedUpdateTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.units = self.root / "units"
        self.data = self.root / "data"
        self.data.mkdir()
        (self.data / "queue.db").write_bytes(b"queue data created by development API")
        self.override = self.units / "firecrab-api.service.d/90-firecrab-dev.conf"
        self.override.parent.mkdir(parents=True)
        self.override.write_text("[Service]\nExecStart=/development/firecrab-api\n")
        self.commands = self.root / "commands"
        self.commands.mkdir()
        self.environment = dict(os.environ, PATH=f"{self.commands}:{os.environ['PATH']}")
        self.stub("flock", "exit 0")
        self.stub("systemctl", f"echo \"$*\" >>'{self.root / 'systemctl.log'}'\nexit 0")
        self.stub("curl", "printf '%s' '{\"version\":\"0.4.1\"}'")
        self.stub("jq", "cat >/dev/null\nexit 0")

    def stub(self, name, body):
        path = self.commands / name
        path.write_text(f"#!/bin/sh\nset -eu\n{body}\n")
        path.chmod(0o755)

    def run_update(self, installer, digest=None):
        script = SCRIPT.read_text().replace("/var/lib/firecrab/dev", str(self.data / "dev"))
        script = script.replace("/etc/systemd/system", str(self.units))
        return subprocess.run(
            ["bash", "-c", script, "--", "v0.4.1", digest or hashlib.sha256(installer).hexdigest()],
            input=installer,
            capture_output=True,
            env=self.environment,
            timeout=10,
        )

    def test_updates_before_starting_release_api_and_keeps_queue_data(self):
        installer = (
            f"#!/bin/bash\nset -eu\ntest \"$*\" = '--version v0.4.1'\n"
            f"test ! -f '{self.override}'\n"
            f"echo installer >>'{self.root / 'systemctl.log'}'\n"
        ).encode()
        result = self.run_update(installer)
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        self.assertFalse(self.override.exists())
        events = (self.root / "systemctl.log").read_text()
        self.assertLess(events.index("installer"), events.index("restart firecrab-helper firecrab-api"))
        self.assertNotIn("stop firecrab-api", events)
        self.assertEqual((self.data / "queue.db").read_bytes(), b"queue data created by development API")

    def test_failed_install_restores_development_settings_and_keeps_diagnostics(self):
        before = self.override.read_bytes()
        result = self.run_update(b"#!/bin/bash\nexit 23\n")
        self.assertEqual(result.returncode, 23, result.stderr.decode())
        self.assertEqual(self.override.read_bytes(), before)
        self.assertTrue(list((self.data / "dev").glob("update.*/install.sh")))
        self.assertIn(b"[ROLLBACK]", result.stderr)

    def test_corrupt_installer_does_not_change_running_services(self):
        before = self.override.read_bytes()
        result = self.run_update(b"#!/bin/bash\nexit 0\n", "0" * 64)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.override.read_bytes(), before)
        self.assertFalse((self.root / "systemctl.log").exists())

    def test_active_deployment_is_not_interrupted(self):
        self.stub("flock", "exit 1")
        result = self.run_update(b"#!/bin/bash\nexit 0\n")
        self.assertNotEqual(result.returncode, 0)
        self.assertTrue(self.override.exists())
        self.assertIn(b"cancel it before updating", result.stderr)


if __name__ == "__main__":
    unittest.main()
