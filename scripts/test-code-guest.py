#!/usr/bin/env python3
"""Run real Git against disposable repositories; no root users, host packages or network."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SOURCE = Path(__file__).resolve().parents[1] / 'firecrab-api/src/code_runtime/guest.py'
spec = importlib.util.spec_from_file_location('code_guest', SOURCE)
guest = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guest)


def git(*args, cwd):
    return subprocess.run(['/usr/bin/git', *args], cwd=cwd, check=True, capture_output=True,
                          env={**os.environ, 'GIT_CONFIG_NOSYSTEM': '1', 'GIT_CONFIG_GLOBAL': '/dev/null',
                               'GIT_AUTHOR_NAME': 'Test', 'GIT_AUTHOR_EMAIL': 'test@example.invalid',
                               'GIT_COMMITTER_NAME': 'Test', 'GIT_COMMITTER_EMAIL': 'test@example.invalid'}).stdout.decode().strip()


class LocalSystem(guest.System):
    def __init__(self, root, source):
        self.home, self.control, self.ephemeral = root / 'home', root / 'control', root / 'run'
        self.ephemeral.mkdir()
        self.source = source
        self.uid, self.gid = os.getuid(), os.getgid()
        self.requests = []

    def account(self, workspace):
        self.home.mkdir(mode=0o700, exist_ok=True)

    def packages(self, runtime):
        pass

    def run_git(self, args, cwd, env):
        self.requests.append((args.copy(), 'FIRECRAB_CODE_TOKEN' in env))
        if 'FIRECRAB_CODE_TOKEN' in env:
            helper = Path(env['GIT_ASKPASS'])
            self.helper_source = helper.read_text()
            result = subprocess.run([str(helper), "Password for 'https://github.com':"], env=env,
                                    capture_output=True, check=True)
            assert result.stdout.decode().strip() == env['FIRECRAB_CODE_TOKEN']
        # Only the test transport substitutes a local repository. The saved origin remains GitHub.
        if args[0] == 'fetch':
            args[args.index('origin')] = str(self.source)
        return git('-c', 'core.hooksPath=/dev/null', '-c', 'credential.helper=',
                   '-c', 'protocol.file.allow=always', *args, cwd=cwd)


class GuestTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.source = self.root / 'source'
        self.source.mkdir()
        git('init', '-b', 'feature/test', cwd=self.source)
        (self.source / 'README').write_text('initial\n')
        git('add', '.', cwd=self.source)
        git('commit', '-m', 'initial', cwd=self.source)
        self.sha = git('rev-parse', 'HEAD', cwd=self.source)
        self.system = LocalSystem(self.root, self.source)
        self.payload = dict(workspace='11111111-1111-4111-8111-111111111111', owner='team',
                            repository='demo', branch='feature/test', runtime='python', sha=self.sha,
                            token="test-private-'$()\\token", account='octocat', resume=False)

    def prepare(self):
        with patch.object(guest.os, 'sync'):
            return guest.prepare(self.payload, self.system)

    def test_clone_has_git_metadata_without_credentials_and_resume_preserves_changes(self):
        report = self.prepare()
        self.assertEqual(report['initialCommit'], self.sha)
        project = self.system.home / 'project'
        self.assertTrue((project / '.git').is_dir())
        self.assertEqual(git('branch', '--show-current', cwd=project), 'feature/test')
        (project / 'README').write_text('local edit\n')
        git('add', '.', cwd=project)
        git('commit', '-m', 'local commit', cwd=project)
        local_head = git('rev-parse', 'HEAD', cwd=project)
        (project / 'untracked').write_text('keep me')
        self.payload.update(resume=True, token='')
        self.prepare()
        self.assertEqual(git('rev-parse', 'HEAD', cwd=project), local_head)
        self.assertEqual((project / 'untracked').read_text(), 'keep me')
        self.assertEqual(sum(args[0] == 'fetch' for args, _ in self.system.requests), 1)
        self.assertTrue(all(has_token == (args[0] == 'fetch') for args, has_token in self.system.requests))
        self.assertNotIn(self.payload['token'] or 'test-private-', (project / '.git/config').read_text())
        self.assertNotIn('test-private-', self.system.helper_source)
        self.assertEqual(list(self.system.ephemeral.iterdir()), [])

    def test_existing_unowned_home_or_project_is_not_overwritten(self):
        self.system.home.mkdir()
        (self.system.home / 'important').write_text('retain')
        with self.assertRaises(RuntimeError):
            self.prepare()
        self.assertEqual((self.system.home / 'important').read_text(), 'retain')

    def test_toolchain_stage_needs_no_token_and_checkout_auth_is_supplied_later(self):
        self.payload.update(stageOnly=True, token='')
        self.assertTrue(self.prepare()['needsCredentials'])
        self.assertFalse((self.system.home / 'project').exists())
        self.assertEqual(self.system.requests, [])
        self.payload.update(stageOnly=False, token='test-private-fresh')
        self.assertEqual(self.prepare()['initialCommit'], self.sha)

    def test_completed_guest_checkout_is_recovered_without_reconnecting_github(self):
        self.prepare()
        # API crashed after publication but before it stored initialCommit.
        self.payload.update(stageOnly=True, resume=False, token='', sha='')
        self.assertEqual(self.prepare()['initialCommit'], self.sha)
        self.assertEqual(sum(args[0] == 'fetch' for args, _ in self.system.requests), 1)

    def test_branch_movement_does_not_publish_a_different_commit(self):
        (self.source / 'README').write_text('advanced')
        git('add', '.', cwd=self.source)
        git('commit', '-m', 'advance', cwd=self.source)
        with self.assertRaises(RuntimeError):
            self.prepare()
        self.assertFalse((self.system.home / 'project').exists())
        self.payload['sha'] = git('rev-parse', 'HEAD', cwd=self.source)
        self.assertEqual(self.prepare()['initialCommit'], self.payload['sha'])

    def test_recovery_after_publish_does_not_clone_over_an_existing_checkout(self):
        self.prepare()
        marker_path = self.system.control / self.payload['workspace'] / 'journal.json'
        marker = json.loads(marker_path.read_text())
        marker['phase'] = 'prepared'
        marker_path.write_text(json.dumps(marker))
        self.payload['sha'] = 'a' * 40  # Remote branch may have moved after the original publication.
        self.assertEqual(self.prepare()['initialCommit'], self.sha)
        self.assertEqual(sum(args[0] == 'fetch' for args, _ in self.system.requests), 1)

    def test_recovery_of_prepared_staging_publishes_the_same_checkout(self):
        self.prepare()
        marker_path = self.system.control / self.payload['workspace'] / 'journal.json'
        marker = json.loads(marker_path.read_text())
        marker['staging'] = '.clone-recovered'
        stage = self.system.home / marker['staging']
        (self.system.home / 'project').rename(stage)
        marker['phase'] = 'prepared'
        marker_path.write_text(json.dumps(marker))
        self.assertEqual(self.prepare()['initialCommit'], self.sha)
        self.assertTrue((self.system.home / 'project/README').is_file())

    def test_missing_or_changed_retained_repository_fails_without_reset(self):
        self.prepare()
        self.payload.update(resume=True, token='')
        project = self.system.home / 'project'
        git('remote', 'set-url', 'origin', 'https://untrusted.invalid/repo', cwd=project)
        with self.assertRaises(RuntimeError):
            self.prepare()
        self.assertEqual(git('remote', 'get-url', 'origin', cwd=project), 'https://untrusted.invalid/repo')

    def test_project_symlink_and_source_rebinding_are_rejected(self):
        self.prepare()
        project = self.system.home / 'project'
        retained = self.system.home / 'retained'
        project.rename(retained)
        project.symlink_to(retained)
        with self.assertRaises(RuntimeError):
            self.prepare()
        self.assertTrue((retained / 'README').exists())
        project.unlink()
        retained.rename(project)
        self.payload['owner'] = 'different-owner'
        with self.assertRaises(RuntimeError):
            self.prepare()

    def test_branch_names_are_literal_arguments(self):
        branch = 'feature/$(touch${IFS}UNEXPECTED)'
        git('branch', branch, cwd=self.source)
        self.payload['branch'] = branch
        self.prepare()
        project = self.system.home / 'project'
        self.assertEqual(git('branch', '--show-current', cwd=project), branch)
        self.assertFalse((project / 'UNEXPECTED').exists())

    @unittest.skipIf(os.getuid() == 0, 'Uses an unprivileged temporary owner')
    def test_account_creation_uses_a_valid_owned_gecos_tag_without_touching_host_users(self):
        from types import SimpleNamespace
        system = guest.System()
        system.home = self.root / 'account-home'
        user = SimpleNamespace(pw_uid=os.getuid(), pw_gid=os.getgid(),
                               pw_gecos='firecrab-code-' + self.payload['workspace'], pw_dir=str(system.home))

        def create_account(argv, **kwargs):
            self.assertIn(user.pw_gecos, argv)
            self.assertNotIn(':', user.pw_gecos)
            system.home.mkdir(mode=0o700)

        with patch.object(guest.pwd, 'getpwnam', side_effect=[KeyError(), user]), \
                patch.object(guest.shutil, 'which', return_value='/usr/sbin/useradd'), \
                patch.object(guest.subprocess, 'run', side_effect=create_account):
            system.account(self.payload['workspace'])
        self.assertEqual(system.uid, os.getuid())

    def test_native_git_uses_an_unprivileged_child_and_never_places_a_token_in_arguments(self):
        from types import SimpleNamespace
        system = guest.System()
        system.home = self.root
        system.ephemeral = self.root
        system.uid, system.gid = 1000, 1000
        calls = []

        def capture(argv, **kwargs):
            self.assertNotIn(self.payload['token'], ' '.join(argv))
            self.assertNotEqual(kwargs['user'], 0)
            self.assertEqual(kwargs['extra_groups'], [])
            self.assertEqual(kwargs['env']['FIRECRAB_CODE_TOKEN'], self.payload['token'])
            calls.append(argv)
            return SimpleNamespace(stdout=b'')

        with patch.object(guest.subprocess, 'run', side_effect=capture):
            system.git(['fetch', 'origin', 'refs/heads/main'], self.root, self.payload['token'], 'octocat')
        self.assertEqual(len(calls), 1)


if __name__ == '__main__':
    unittest.main()
