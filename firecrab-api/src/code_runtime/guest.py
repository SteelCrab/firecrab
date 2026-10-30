"""Server-authored guest bootstrap. Repository Git operations run without root privileges."""
import contextlib
import fcntl
import json
import os
import pathlib
import pwd
import shutil
import stat
import subprocess
import sys
import tempfile


class System:
    home = pathlib.Path('/home/fc-code')
    control = pathlib.Path('/var/lib/firecrab-code')
    ephemeral = pathlib.Path('/run')

    def account(self, workspace):
        tag = 'firecrab-code-' + workspace
        try:
            user = pwd.getpwnam('fc-code')
        except KeyError:
            if self.home.exists() or self.home.is_symlink():
                raise RuntimeError('Existing home')
            if shutil.which('useradd'):
                subprocess.run(['useradd', '--create-home', '--home-dir', str(self.home),
                                '--shell', '/bin/sh', '--comment', tag, '--user-group', 'fc-code'],
                               check=True, timeout=30, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            else:
                subprocess.run(['adduser', '-D', '-h', str(self.home), '-s', '/bin/sh',
                                '-g', tag, 'fc-code'], check=True, timeout=30,
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            user = pwd.getpwnam('fc-code')
        if user.pw_uid == 0 or user.pw_gecos != tag or user.pw_dir != str(self.home):
            raise RuntimeError('Account ownership')
        if self.home.is_symlink() or not self.home.is_dir() or self.home.stat().st_uid != user.pw_uid:
            raise RuntimeError('Home ownership')
        self.uid, self.gid = user.pw_uid, user.pw_gid
        os.chmod(self.home, 0o700)

    def packages(self, runtime):
        commands = {'node_js': ['node', 'npm'], 'python': ['python3'], 'rust': ['cargo', 'rustc']}
        packages = {'node_js': ['nodejs', 'npm'], 'python': ['python3', 'py3-pip'], 'rust': ['cargo', 'rust']}
        if all(shutil.which(cmd) for cmd in ['git'] + commands[runtime]):
            return
        env = {'PATH': '/usr/sbin:/usr/bin:/sbin:/bin', 'DEBIAN_FRONTEND': 'noninteractive', 'LC_ALL': 'C'}
        if shutil.which('apt-get'):
            subprocess.run(['apt-get', '-o', 'DPkg::Lock::Timeout=60', 'update'], env=env,
                           check=True, timeout=180, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            extra = {'node_js': ['nodejs', 'npm'], 'python': ['python3', 'python3-venv', 'python3-pip'],
                     'rust': ['cargo', 'rustc']}[runtime]
            command = ['apt-get', '-o', 'DPkg::Lock::Timeout=60', 'install', '-y', 'git', 'ca-certificates'] + extra
        elif shutil.which('apk'):
            command = ['apk', 'add', '--no-cache', 'git', 'ca-certificates'] + packages[runtime]
        else:
            raise RuntimeError('Unsupported image')
        subprocess.run(command, env=env, check=True, timeout=240,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        if not all(shutil.which(cmd) for cmd in ['git'] + commands[runtime]):
            raise RuntimeError('Missing runtime')

    def git(self, arguments, cwd, token=None, username='x-access-token'):
        env = {'HOME': str(self.home), 'PATH': '/usr/local/bin:/usr/bin:/bin', 'LC_ALL': 'C',
               'GIT_CONFIG_NOSYSTEM': '1', 'GIT_CONFIG_GLOBAL': '/dev/null', 'GIT_TERMINAL_PROMPT': '0'}
        with contextlib.ExitStack() as stack:
            if token:
                helper_dir = pathlib.Path(stack.enter_context(tempfile.TemporaryDirectory(prefix='fc-code-', dir=self.ephemeral)))
                os.chmod(helper_dir, 0o755)
                helper = helper_dir / 'askpass'
                helper.write_text('#!/usr/bin/python3\nimport os,sys\np=sys.argv[1]\n'
                                  'print(os.environ["FIRECRAB_CODE_USERNAME"] if p.startswith("Username") else '
                                  'os.environ["FIRECRAB_CODE_TOKEN"] if p.startswith("Password") else "")\n')
                helper.chmod(0o555)
                env.update(GIT_ASKPASS=str(helper), FIRECRAB_CODE_TOKEN=token, FIRECRAB_CODE_USERNAME=username)
            return self.run_git(arguments, cwd, env)

    def run_git(self, arguments, cwd, env):
        result = subprocess.run(['/usr/bin/git', '-c', 'credential.helper=', '-c', 'core.hooksPath=/dev/null',
                                 '-c', 'gc.auto=0', '-c', 'maintenance.auto=false', '-c', 'core.fsmonitor=false',
                                 '-c', 'http.followRedirects=false', '-c', 'http.sslVerify=true',
                                 '-c', 'protocol.allow=never', '-c', 'protocol.https.allow=always',
                                 '-c', 'submodule.recurse=false'] + arguments, cwd=cwd, env=env,
                                user=self.uid, group=self.gid, extra_groups=[], umask=0o077,
                                check=True, timeout=180, capture_output=True)
        return result.stdout.decode('utf-8').strip()


def atomic_json(path, value):
    fd, temporary = tempfile.mkstemp(prefix='.journal-', dir=path.parent)
    try:
        with os.fdopen(fd, 'w') as output:
            json.dump(value, output)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
        directory = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def prepare(payload, system):
    workspace = payload['workspace']
    source = {key: payload[key] for key in ['owner', 'repository', 'branch', 'runtime']}
    for directory in [system.control, system.control / workspace]:
        if directory.is_symlink():
            raise RuntimeError('Control symlink')
        directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        directory.chmod(0o700)
    control = system.control / workspace
    fd = os.open(control / 'lock', os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    if not stat.S_ISREG(os.fstat(fd).st_mode):
        os.close(fd)
        raise RuntimeError('Invalid control lock')
    with os.fdopen(fd, 'r+') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        marker_path = control / 'journal.json'
        if marker_path.is_symlink():
            raise RuntimeError('Marker symlink')
        if marker_path.exists():
            marker = json.loads(marker_path.read_text())
            if marker['workspace'] != workspace or marker['source'] != source:
                raise RuntimeError('Source ownership')
        else:
            if payload['resume'] or system.home.exists() or system.home.is_symlink():
                raise RuntimeError('Unowned workspace')
            marker = {'workspace': workspace, 'source': source, 'phase': 'reserved'}
            atomic_json(marker_path, marker)
        system.packages(payload['runtime'])
        system.account(workspace)
        if marker.get('uid', system.uid) != system.uid:
            raise RuntimeError('User identity changed')
        marker['uid'] = system.uid
        project = system.home / 'project'
        origin = 'https://github.com/' + payload['owner'] + '/' + payload['repository'] + '.git'
        if project.is_symlink() or (project.exists() and marker['phase'] not in ['prepared', 'ready']):
            raise RuntimeError('Unowned project')
        old_stage = marker.get('staging')
        if old_stage is not None:
            if '/' in old_stage or not old_stage.startswith('.clone-'):
                raise RuntimeError('Invalid staging identity')
            old_stage = system.home / old_stage
            if old_stage.is_symlink():
                raise RuntimeError('Staging symlink')
            if marker['phase'] == 'prepared' and not project.exists() and old_stage.is_dir():
                os.rename(old_stage, project)
            elif old_stage.exists() and old_stage != project:
                shutil.rmtree(old_stage)
        if marker['phase'] in ['prepared', 'ready'] and project.exists():
            if (project / '.git').is_symlink() or not (project / '.git').is_dir():
                raise RuntimeError('Repository identity')
            if system.git(['remote', 'get-url', 'origin'], project) != origin:
                raise RuntimeError('Remote identity')
            if payload['resume'] and marker['initialCommit'] != payload['sha']:
                raise RuntimeError('Initial commit changed')
            marker['phase'] = 'ready'
            marker.pop('staging', None)
            atomic_json(marker_path, marker)
        else:
            if payload.get('stageOnly', False):
                atomic_json(marker_path, marker)
                os.sync()
                return {'workspace': workspace, 'needsCredentials': True}
            if payload['resume'] or not payload['token']:
                raise RuntimeError('Missing retained repository')
            staging = pathlib.Path(tempfile.mkdtemp(prefix='.clone-', dir=system.home))
            try:
                os.chown(staging, system.uid, system.gid)
                marker.update(phase='cloning', staging=staging.name)
                atomic_json(marker_path, marker)
                system.git(['init', '--quiet'], staging)
                system.git(['remote', 'add', 'origin', origin], staging)
                system.git(['fetch', '--quiet', '--no-tags', '--depth=1', 'origin',
                            'refs/heads/' + payload['branch']], staging, payload['token'], payload['account'])
                if system.git(['rev-parse', '--verify', 'FETCH_HEAD'], staging) != payload['sha']:
                    raise RuntimeError('Branch advanced during clone')
                system.git(['checkout', '--quiet', '-b', payload['branch'], payload['sha'], '--'], staging)
                marker.update(phase='prepared', initialCommit=payload['sha'])
                atomic_json(marker_path, marker)
                os.rename(staging, project)
                marker['phase'] = 'ready'
                marker.pop('staging', None)
                atomic_json(marker_path, marker)
            finally:
                if staging.exists():
                    shutil.rmtree(staging)
        head = system.git(['rev-parse', '--verify', 'HEAD'], project)
        if len(head) != 40 or any(c not in '0123456789abcdefABCDEF' for c in head):
            raise RuntimeError('Invalid HEAD')
        os.sync()
        return {'workspace': workspace, 'initialCommit': marker['initialCommit']}


def main(data):
    try:
        if os.geteuid() != 0:
            raise RuntimeError('Guest operator required')
        result = prepare(json.loads(data), System())
        print(json.dumps(result))
    except Exception:
        print('CODE preparation failed', file=sys.stderr)
        sys.exit(1)
