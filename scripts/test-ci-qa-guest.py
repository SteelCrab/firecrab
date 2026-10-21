#!/usr/bin/env python3
"""Exercise public-image QA failure aggregation and owned-image cleanup."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent
TRANSPORT = r'''#!/usr/bin/env python3
import json, os, pathlib, sys
args = sys.argv[1:]
state_path = pathlib.Path(os.environ['QA_TEST_STATE'])
state = json.loads(state_path.read_text())
def alias(reference):
    return reference.split(':')[0] + '-test'
def image_rows():
    return [{'alias': a, 'installed': True, 'minDiskGb': 1} for a in state['installed']]
def install(reference):
    value = alias(reference)
    state['installed'].append(value)
    return {'alias': value, 'status': 'succeeded'}
if pathlib.Path(sys.argv[0]).name == 'firecrab':
    args = args[2:]  # --api URL
    action = args[1]
    if action == 'list': body = image_rows()
    elif action == 'inspect': body = {'alias': alias(args[2])}
    elif action == 'import': body = install(args[2])
    elif action == 'import-status': body = {'status': 'succeeded'}
    else: raise SystemExit('unexpected CLI action ' + action)
    print(json.dumps(body))
else:
    method = args[args.index('-X') + 1] if '-X' in args else 'GET'
    url = next(a for a in args if a.startswith('http://'))
    route = url.split('/api/', 1)[1]
    code = 200
    if route == 'oci/inspect':
        reference = args[args.index('--data-urlencode') + 1].split('=', 1)[1]
        body = {'alias': alias(reference)}
    elif route == 'oci/import' and method == 'POST':
        body = install(json.loads(args[args.index('--data') + 1])['reference'])
        code = 202
    elif route.startswith('oci/import/'):
        body = {'status': 'succeeded'}
    elif route.startswith('images/'):
        value = route.split('/', 1)[1]
        if method == 'DELETE':
            state['deleted'].append(value)
            state['installed'].remove(value)
            body, code = None, 204
        else:
            body = {'installed': value in state['installed'], 'minDiskGb': 1}
    elif route == 'vms': body = []
    else: raise SystemExit('unexpected HTTP route ' + route)
    text = '' if body is None else json.dumps(body)
    if '-o' in args:
        destination = args[args.index('-o') + 1]
        if destination != '/dev/null': pathlib.Path(destination).write_text(text)
    else: print(text)
    if '-w' in args: print(code, end='')
state_path.write_text(json.dumps(state))
'''


class GuestQaTests(unittest.TestCase):
    def run_qa(self, failed_alias="", installed=()):
        with tempfile.TemporaryDirectory(prefix="firecrab-guest-qa-") as temporary:
            root = Path(temporary)
            scripts = root / "scripts"
            scripts.mkdir()
            shutil.copy(ROOT / "ci-qa-guest.sh", scripts)
            helper = scripts / "ci-m2-guest-boot.sh"
            helper.write_text('#!/bin/bash\nset -eu\nprintf "%s:%s\\n" "$1" "${FIRECRAB_QA_FIRST_REFERENCE:-0}" >> "$QA_TEST_BOOTS"\n[ "$1" != "$QA_TEST_FAILED_ALIAS" ] || exit 5\n')
            helper.chmod(0o755)
            commands = root / "bin"
            commands.mkdir()
            for name in ("curl", "firecrab"):
                command = commands / name
                command.write_text(TRANSPORT)
                command.chmod(0o755)
            # Isolate orchestration from host KVM/sudo; boot is a stub above.
            uname = commands / "uname"
            uname.write_text('#!/bin/sh\necho Darwin\n')
            uname.chmod(0o755)
            state_path = root / "state.json"
            state_path.write_text(json.dumps({"installed": list(installed), "deleted": []}))
            boots_path = root / "boots.txt"
            env = {
                **os.environ,
                "PATH": str(commands) + os.pathsep + os.environ["PATH"],
                "FIRECRAB_API": "http://qa.invalid:5523",
                "FIRECRAB_QA_CLI": str(commands / "firecrab"),
                "QA_TEST_STATE": str(state_path),
                "QA_TEST_BOOTS": str(boots_path),
                "QA_TEST_FAILED_ALIAS": failed_alias,
            }
            result = subprocess.run(["bash", str(scripts / "ci-qa-guest.sh")], env=env,
                                    text=True, capture_output=True, timeout=30)
            return result, json.loads(state_path.read_text()), boots_path.read_text().splitlines()

    def test_failed_first_boot_still_checks_later_images_and_cleans_its_import(self):
        result, state, boots = self.run_qa("alpine-test")
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertEqual(boots, ["alpine-test:1", "ubuntu-test:0", "fedora-test:0"])
        self.assertIn("FAILED alpine:3.21 exit=5", result.stdout)
        self.assertIn("PASS ubuntu:24.04 exit=0", result.stdout)
        self.assertIn("PASS fedora:42 exit=0", result.stdout)
        self.assertEqual(state["installed"], [])
        self.assertEqual(state["deleted"], ["alpine-test", "ubuntu-test", "fedora-test"])

    def test_success_requires_every_reference_to_pass_and_cleans_all_imports(self):
        result, state, boots = self.run_qa()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(len(boots), 3)
        self.assertNotIn("FAILED", result.stdout)
        self.assertEqual(state["installed"], [])

    def test_a_preexisting_image_is_warned_about_and_preserved(self):
        result, state, _ = self.run_qa(installed=("alpine-test",))
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("WARNING I6/alpine:3.21", result.stdout)
        self.assertEqual(state["installed"], ["alpine-test"])
        self.assertEqual(state["deleted"], ["ubuntu-test", "fedora-test"])


if __name__ == "__main__":
    unittest.main()
