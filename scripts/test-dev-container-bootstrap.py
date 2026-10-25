#!/usr/bin/env python3
"""Exercise private bootstrap verification without network, root, or a VM."""

import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parent.parent
BOOTSTRAP = ROOT / "scripts/micromanager/dev-container-bootstrap.sh"
HELPERS = ROOT / "scripts/firecrab-release.sh"
INSTALLER = r'''#!/bin/bash
set -euo pipefail
[ "$#" -eq 2 ]
[ -f "$TEST_ROOT/dnsmasq-masked" ]
printf '{"command":"installer","args":["%s","%s"]}\n' "$1" "$2" >> "$TEST_ROOT/events"
[ "${INSTALL_FAIL:-0}" = 0 ] || exit 47
printf 'verified release installed\n' > "$TEST_ROOT/installed-api"
chmod 0755 "$TEST_ROOT/installed-api"
'''

COMMAND = r'''#!/usr/bin/env python3
import hashlib, json, os, pathlib, sys
root = pathlib.Path(os.environ['TEST_ROOT'])
name = pathlib.Path(sys.argv[0]).name
args = sys.argv[1:]
with (root / 'events').open('a') as events:
    events.write(json.dumps({'command': name, 'args': args}) + '\n')
if name == 'systemctl':
    if args == ['is-system-running']:
        state = os.environ.get('SYSTEM_STATE', 'running')
        print(state)
        sys.exit(0 if state == 'running' else 1)
    if args == ['mask', '--now', 'dnsmasq.service']:
        if os.environ.get('MASK_FAIL') == '1':
            sys.exit(1)
        (root / 'dnsmasq-masked').touch()
        sys.exit(0)
    if args == ['cat', 'firecrab-api']:
        assert (root / 'dnsmasq-masked').is_file(), 'service checked before dnsmasq mask'
        sys.exit(0 if os.environ.get('HAS_UNIT') == '1' else 1)
    if args == ['start', 'firecrab-dev-publish.service']:
        assert (root / 'installed-api').is_file(), 'relay started before installation'
        sys.exit(0)
    raise AssertionError(('unexpected systemctl', args))
assert name == 'curl', name
assert (root / 'dnsmasq-masked').is_file(), 'release downloaded before dnsmasq mask'
assert args[:4] == ['--proto', '=https', '--tlsv1.2', '-fsSL'], args
assert args[-2] == '-o', args
url, output = args[-3], pathlib.Path(args[-1])
assert output.is_relative_to(root), output
prefix = 'https://github.com/SteelCrab/firecrab/releases/latest/download/'
assert url.startswith(prefix), url
asset = url[len(prefix):]
if os.environ.get('DOWNLOAD_FAIL') == asset:
    sys.exit(22)
installer = (root / 'release-installer').read_bytes()
if asset == 'install.sh':
    output.write_bytes(installer)
elif asset == 'SHA256SUMS':
    digest = hashlib.sha256(installer).hexdigest()
    mode = os.environ.get('CHECKSUM_MODE', 'correct')
    if mode == 'mismatch':
        digest = '0' * 64
    filename = 'unrelated.sh' if mode == 'missing' else 'install.sh'
    output.write_text(f'{digest}  ./{filename}\n')
else:
    raise AssertionError(('unexpected asset', asset))
'''


class PrivateBootstrapTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="firecrab-bootstrap-test-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.commands = self.root / "commands"
        self.commands.mkdir()
        self.downloads = self.root / "downloads"
        self.downloads.mkdir()
        for name in ["curl", "systemctl"]:
            command = self.commands / name
            command.write_text(COMMAND)
            command.chmod(0o755)
        self.installed = self.root / "installed-api"
        self.helper = self.root / "release-helpers.sh"
        shutil.copyfile(HELPERS, self.helper)
        (self.root / "release-installer").write_text(INSTALLER)
        script = BOOTSTRAP.read_text()
        script = script.replace("/dev/kvm", "/dev/null")
        script = script.replace("/usr/local/lib/firecrab/firecrab-api", shlex.quote(str(self.installed)))
        script = script.replace("/opt/firecrab-bootstrap/scripts/firecrab-release.sh", shlex.quote(str(self.helper)))
        script = script.replace("seq 1 60", "seq 1 2").replace("sleep 1", "sleep 0.01")
        self.script = self.root / "bootstrap.sh"
        self.script.write_text(script)

    def bootstrap(self, **flags):
        (self.root / "events").unlink(missing_ok=True)
        (self.root / "dnsmasq-masked").unlink(missing_ok=True)
        environment = dict(os.environ)
        environment.update(
            TEST_ROOT=str(self.root),
            TMPDIR=str(self.downloads),
            PATH=str(self.commands) + os.pathsep + environment["PATH"],
            FIRECRAB_RELEASE_BASE="https://github.com/SteelCrab/firecrab/releases",
            CHECKSUM_MODE="correct",
            HAS_UNIT="0",
            SYSTEM_STATE="running",
            INSTALL_FAIL="0",
            DOWNLOAD_FAIL="",
            MASK_FAIL="0",
        )
        environment.update({name: str(value) for name, value in flags.items()})
        return subprocess.run(["bash", str(self.script)], env=environment,
                              text=True, capture_output=True, timeout=5)

    def events(self):
        path = self.root / "events"
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def mutations(self):
        return [event for event in self.events()
                if event["command"] == "installer"
                or (event["command"] == "systemctl" and event["args"][0] in ["mask", "start"])]

    def mask_event(self):
        return {"command": "systemctl", "args": ["mask", "--now", "dnsmasq.service"]}

    def assert_clean_downloads(self):
        self.assertEqual(list(self.downloads.iterdir()), [])

    def test_verified_release_installer_runs_before_its_relay(self):
        result = self.bootstrap()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.mutations(), [
            self.mask_event(),
            {"command": "installer", "args": ["--libc", "gnu"]},
            {"command": "systemctl", "args": ["start", "firecrab-dev-publish.service"]},
        ])
        self.assertEqual(len([event for event in self.events() if event["command"] == "curl"]), 2)
        self.assert_clean_downloads()

    def test_bad_or_missing_checksum_never_executes_the_installer(self):
        for mode in ["mismatch", "missing"]:
            with self.subTest(mode=mode):
                result = self.bootstrap(CHECKSUM_MODE=mode)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("checksum mismatch", result.stderr)
                self.assertEqual(self.mutations(), [self.mask_event()])
                self.assertFalse(self.installed.exists())
                self.assert_clean_downloads()

    def test_either_download_failure_stops_before_installation(self):
        for asset in ["install.sh", "SHA256SUMS"]:
            with self.subTest(asset=asset):
                result = self.bootstrap(DOWNLOAD_FAIL=asset)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(self.mutations(), [self.mask_event()])
                self.assertFalse(self.installed.exists())
                self.assert_clean_downloads()

    def test_existing_complete_installation_skips_downloads(self):
        self.installed.write_text("keep existing installed binary\n")
        self.installed.chmod(0o755)
        result = self.bootstrap(HAS_UNIT=1)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(any(event["command"] in ["curl", "installer"] for event in self.events()))
        self.assertEqual(self.installed.read_text(), "keep existing installed binary\n")
        self.assertEqual(self.mutations(), [
            self.mask_event(),
            {"command": "systemctl", "args": ["start", "firecrab-dev-publish.service"]},
        ])

    def test_missing_installed_binary_repairs_only_the_private_installation(self):
        result = self.bootstrap(HAS_UNIT=1)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.installed.read_text(), "verified release installed\n")
        self.assertEqual(len(self.mutations()), 3)

    def test_installer_failure_never_starts_the_relay(self):
        result = self.bootstrap(INSTALL_FAIL=1)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.mutations(), [
            self.mask_event(),
            {"command": "installer", "args": ["--libc", "gnu"]},
        ])
        self.assertFalse(self.installed.exists())
        self.assert_clean_downloads()

    def test_unready_systemd_stops_before_downloads_or_mutations(self):
        result = self.bootstrap(SYSTEM_STATE="starting")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("private systemd did not become ready", result.stderr)
        self.assertTrue(all(event["command"] == "systemctl"
                            and event["args"] == ["is-system-running"] for event in self.events()))
        self.assertEqual(self.mutations(), [])
        self.assert_clean_downloads()

    def test_degraded_systemd_is_a_usable_state(self):
        result = self.bootstrap(SYSTEM_STATE="degraded")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(len(self.mutations()), 3)

    def test_dnsmasq_mask_failure_stops_before_installation_or_relay(self):
        result = self.bootstrap(MASK_FAIL=1)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.mutations(), [self.mask_event()])
        self.assertFalse(any(event["command"] in ["curl", "installer"] for event in self.events()))
        self.assertFalse(self.installed.exists())
        self.assert_clean_downloads()


if __name__ == "__main__":
    unittest.main()
