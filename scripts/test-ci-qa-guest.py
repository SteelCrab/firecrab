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
def create_pool(request):
    pool = dict(request, id='pool-test', templateVersion='v1', deleting=False, members=[])
    if pool['minReady']:
        pool['members'] = [{'vmId': 'vm-pool-test', 'state': 'ready'}]
    state['pool'] = pool
    return pool
if pathlib.Path(sys.argv[0]).name == 'firecrab':
    args = args[2:]  # --api URL
    action = args[1]
    if args[0] == 'pool':
        body = state['pool']
        if action == 'update': body['leaseTtlSeconds'] = int(args[args.index('--lease-ttl') + 1])
    elif action == 'list': body = image_rows()
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
    request = json.loads(args[args.index('--data') + 1]) if '--data' in args else None
    if route == 'micro-networks':
        state['network'] = dict(request, id='net-pool-test')
        body, code = state['network'], 201
    elif route == 'micro-networks/net-pool-test' and method == 'DELETE':
        state['network'] = None
        body, code = None, 204
    elif route == 'pools':
        if method == 'POST': body, code = create_pool(request), 201
        else: body = [state['pool']] if state.get('pool') else []
    elif route == 'pools/pool-test':
        body = state.get('pool')
        if method == 'PATCH': body.update(request)
        elif method == 'DELETE':
            body = dict(body, deleting=True) if body else {}
            state['pool'] = None
            code = 202
        elif body is None: code = 404
    elif route == 'pools/pool-test/acquire':
        pool = state['pool']
        if not pool['members']:
            body, code = {'error': {'code': 'pool_exhausted'}}, 409
        elif state.get('lease'):
            body, code = state['lease'], 200
        else:
            member = pool['members'][0]
            member['state'] = 'leased'
            body, code = {'id': 'lease-pool-test', 'vmId': member['vmId'], 'state': 'active'}, 201
            state['lease'] = body
            pool['members'].append({'vmId': 'vm-warm-test', 'state': 'ready'})
    elif route == 'pools/pool-test/leases/lease-pool-test' and method == 'DELETE':
        state['pool']['members'] = [{'vmId': 'vm-replacement-test', 'state': 'ready'}]
        body = {'state': 'released'}
    elif route == 'vms/vm-pool-test':
        if method == 'DELETE':
            body, code = {'error': {'code': 'pool_owned'}}, 409
            if os.environ.get('QA_TEST_POOL_FAILURE') == '1': code = 500
        else:
            present = any(m['vmId'] == 'vm-pool-test' for m in (state.get('pool') or {}).get('members', []))
            body, code = ({'state': 'running'}, 200) if present else (None, 404)
    elif route.startswith('vms/'):
        body = {'state': 'running'}
    elif route == 'host': body = {}
    elif route == 'oci/inspect':
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
    elif route == 'vms':
        body = [{'id': m['vmId'], 'purpose': 'pool'} for m in (state.get('pool') or {}).get('members', [])]
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
    def run_qa(self, failed_alias="", installed=(), restart=False, pool_failure=False):
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
            for name, body in {
                "sudo": '#!/bin/sh\nexec "$@"\n',
                "pgrep": '#!/bin/sh\nprintf "123\\n124\\n"\n',
                "systemctl": '#!/usr/bin/env python3\nimport json, os, pathlib, sys\nassert sys.argv[1:] == ["restart", "firecrab-api"]\np = pathlib.Path(os.environ["QA_TEST_STATE"])\ns = json.loads(p.read_text()); s["restarts"] = s.get("restarts", 0) + 1; p.write_text(json.dumps(s))\n',
            }.items():
                command = commands / name
                command.write_text(body)
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
                "QA_TEST_POOL_FAILURE": str(int(pool_failure)),
                "FIRECRAB_QA_POOL_RESTART": str(int(restart)),
                "FIRECRAB_QA_MANAGER_HOST": "",
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

    def test_pool_failure_releases_its_lease_and_cleans_network_and_image(self):
        result, state, boots = self.run_qa(pool_failure=True)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertEqual(len(boots), 3)
        self.assertIn("FAILED P6", result.stderr)
        self.assertIsNone(state["pool"])
        self.assertIsNone(state["network"])
        self.assertEqual(state["installed"], [])

    def test_pool_restart_recovers_the_same_lease(self):
        result, state, _ = self.run_qa(restart=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(state["restarts"], 1)
        self.assertIn("PASS P7 API restart preserved", result.stdout)


if __name__ == "__main__":
    unittest.main()
