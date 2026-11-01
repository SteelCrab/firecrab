#!/usr/bin/env python3
"""Disposable credential recipe tests; optional official CLI contracts use synthetic keys only."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('agent_auth', ROOT / 'firecrab-api/src/code_agent_auth/guest.py')
auth = importlib.util.module_from_spec(spec); spec.loader.exec_module(auth)
PROFILES = json.loads((ROOT / 'firecrab-api/src/code_agent_auth/adapters.json').read_text())
KEY = 'firecrab-fixture-invalid-key'
PAYLOAD = {'workspace': '11111111-1111-4111-8111-111111111111', 'nonce': '22222222-2222-4222-8222-222222222222', 'adapter': '', 'key': KEY}


class FixtureSystem(auth.System):
    def __init__(self, root, wrong_version=False, rejected=False):
        self.ephemeral = root; self.calls = []; self.wrong_version = wrong_version; self.rejected = rejected
    def identity(self, workspace): return os.getuid(), os.getgid()
    def own(self, path, *_): os.chmod(path, 0o700)
    def executable(self, p): self.profile = p; return '/fixture/' + p['binary']
    def run(self, args, env, directory, identity, data=b''):
        self.calls.append((args, env.copy(), data, directory))
        assert KEY not in ' '.join(args)
        assert env['HOME'] == directory and Path(directory).stat().st_mode & 0o077 == 0
        if '--version' in args:
            version = self.profile['cliVersion']
            expected = {'codex': 'codex-cli ' + version, 'claude_code': version + ' (Claude Code)', 'opencode': version}[self.profile['agent']]
            return 0, 'wrong' if self.wrong_version else expected, ''
        if '--with-api-key' in args:
            assert data.decode() == KEY
            (Path(env['CODEX_HOME']) / 'auth.json').write_text(json.dumps({'OPENAI_API_KEY': KEY}))
            os.chmod(Path(env['CODEX_HOME']) / 'auth.json', 0o600)
            return 0, '', ''
        if self.rejected: return 1, KEY, KEY
        if self.profile['agent'] == 'codex': return 0, 'API key configured - fixture\nAuthentication is checked when you send a request.', ''
        if self.profile['agent'] == 'claude_code':
            assert env['ANTHROPIC_API_KEY'] == KEY and 'OPENAI_API_KEY' not in env
            return 0, json.dumps({'loggedIn': True, 'authMethod': 'api_key', 'apiKeySource': 'ANTHROPIC_API_KEY', 'apiProvider': 'firstParty', 'configDirectory': env['CLAUDE_CONFIG_DIR']}), ''
        variable = 'OPENAI_API_KEY' if self.profile['provider'] == 'openai' else 'ANTHROPIC_API_KEY'
        assert env[variable] == KEY
        assert ('ANTHROPIC_API_KEY' if variable == 'OPENAI_API_KEY' else 'OPENAI_API_KEY') not in env
        return 0, '0 credentials\nEnvironment\n' + variable + '\n1 environment variable', ''


class Recipes(unittest.TestCase):
    def test_output_and_deadline_are_bounded_and_process_groups_are_killed(self):
        real_popen = subprocess.Popen
        def local_process(*args, **kwargs):
            for key in ['user', 'group', 'extra_groups']: kwargs.pop(key, None)
            return real_popen(*args, **kwargs)
        with tempfile.TemporaryDirectory() as root:
            env = {'PATH': '/usr/bin:/bin', 'HOME': root}
            for code, reason in [("print('x'*20000)", 'cli_rejected'), ("import time;time.sleep(60)", 'timed_out')]:
                with self.subTest(reason=reason), patch.object(auth.subprocess, 'Popen', local_process), patch.object(auth.os, 'killpg', wraps=os.killpg) as kill:
                    with self.assertRaisesRegex(auth.Failure, reason):
                        auth.System().run([sys.executable, '-c', code], env, root, (os.getuid(), os.getgid()))
                    kill.assert_called_once()

    def test_all_supported_recipes_isolate_credentials_and_remove_files(self):
        for p in PROFILES:
            if not p['supported']: continue
            with self.subTest(p=p['id']), tempfile.TemporaryDirectory() as root:
                system = FixtureSystem(root)
                with patch.dict(os.environ, {'OPENAI_API_KEY': 'ambient-secret', 'NODE_OPTIONS': '--require=untrusted'}):
                    result = auth.check({**PAYLOAD, 'adapter': p['id']}, PROFILES, system)
                self.assertEqual(result['status'], 'configuration_verified')
                self.assertNotIn(KEY, json.dumps(result)); self.assertEqual(list(Path(root).iterdir()), [])
                self.assertTrue(all('NODE_OPTIONS' not in c[1] for c in system.calls))

    def test_unknown_versions_and_failed_cli_never_leave_credentials(self):
        for wrong in [True, False]:
            with tempfile.TemporaryDirectory() as root:
                system = FixtureSystem(root, wrong_version=wrong, rejected=not wrong)
                with self.assertRaisesRegex(auth.Failure, 'version_mismatch' if wrong else 'cli_rejected'):
                    auth.check({**PAYLOAD, 'adapter': PROFILES[0]['id']}, PROFILES, system)
                self.assertEqual(list(Path(root).iterdir()), [])
                if wrong: self.assertEqual(len(system.calls), 1)

    def test_chatgpt_and_caller_commands_cannot_enter_a_recipe(self):
        with tempfile.TemporaryDirectory() as root:
            system = FixtureSystem(root)
            for payload in [{**PAYLOAD, 'adapter': 'codex-chatgpt-v1'}, {**PAYLOAD, 'adapter': PROFILES[0]['id'], 'command': 'untrusted'}]:
                with self.assertRaises(auth.Failure): auth.check(payload, PROFILES, system)
            self.assertEqual(system.calls, [])

    @unittest.skipUnless(os.getenv('FIRECRAB_CODE_AUTH_CLI_ROOT'), 'set FIRECRAB_CODE_AUTH_CLI_ROOT for official pinned CLI contracts')
    def test_official_pinned_cli_contracts_without_real_credentials(self):
        packages = Path(os.environ['FIRECRAB_CODE_AUTH_CLI_ROOT']) / 'node_modules'
        # Direct native binaries avoid package installation scripts and their wrapper stubs.
        def binary(p):
            patterns = {'codex': '@openai/codex-*/vendor/*/bin/codex', 'claude': '@anthropic-ai/claude-code-*/claude', 'opencode': 'opencode-*/bin/opencode'}
            matches = list(packages.glob(patterns[p['binary']]))
            self.assertEqual(len(matches), 1, p['binary'])
            return str(matches[0])
        real_popen = subprocess.Popen
        def unprivileged(*args, **kwargs):
            for key in ['user', 'group', 'extra_groups']: kwargs.pop(key, None)
            return real_popen(*args, **kwargs)
        for p in PROFILES:
            if not p['supported']: continue
            with self.subTest(p=p['id']), tempfile.TemporaryDirectory() as root:
                system = auth.System(); system.ephemeral = root
                system.identity = lambda _: (os.getuid(), os.getgid())
                system.executable = binary
                system.own = lambda path, *_: os.chmod(path, 0o700)
                with patch.object(auth.subprocess, 'Popen', unprivileged):
                    result = auth.check({**PAYLOAD, 'adapter': p['id']}, PROFILES, system)
                self.assertEqual(result['status'], 'configuration_verified'); self.assertEqual(list(Path(root).iterdir()), [])


if __name__ == '__main__': unittest.main()
