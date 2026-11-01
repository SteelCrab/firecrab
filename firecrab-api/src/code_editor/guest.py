"""Install a pinned official editor and run it as the existing workspace user."""
import fcntl
import hashlib
import json
import os
import pathlib
import platform
import pwd
import shutil
import stat
import tarfile
import tempfile
import time
import urllib.request

# Unix account name, not an authentication credential.
ACCOUNT_NAME = 'fc-code'

VERSION = '4.144.0'
DIGESTS = {'x86_64': ('amd64', '710f749c40a0f3d0ab3be09c0de65314e5c2907cc50646f5fbc0f24c0e8fba2d'),
           'aarch64': ('arm64', '4404c784f541bd56a2a8d6e2a9474ed0b4e37bc8502aa3a247898ca933434482')}


def identity(payload):
    user = pwd.getpwnam(ACCOUNT_NAME)
    home = pathlib.Path('/home/fc-code')
    control = pathlib.Path('/var/lib/firecrab-code')
    marker = control / 'journal.json'
    for path in [control, marker]:
        info = path.lstat()
        if stat.S_ISLNK(info.st_mode) or info.st_uid != 0 or info.st_mode & 0o022:
            raise RuntimeError('Workspace marker ownership')
    saved = json.loads(marker.read_text())
    if (user.pw_uid == 0 or user.pw_gecos != 'firecrab-code-' + payload['workspace']
            or user.pw_dir != str(home) or home.is_symlink() or home.stat().st_uid != user.pw_uid
            or saved['workspace'] != payload['workspace'] or saved['phase'] != 'ready'
            or saved['initialCommit'] != payload['commit'] or saved.get('uid') != user.pw_uid):
        raise RuntimeError('Workspace ownership changed')
    project = home / 'project'
    if project.is_symlink() or not project.is_dir() or project.stat().st_uid != user.pw_uid:
        raise RuntimeError('Workspace folder changed')
    return user


def drop(user):
    os.setgroups([])
    os.setgid(user.pw_gid)
    os.setuid(user.pw_uid)


def extract(tar, destination, prefix):
    # Create regular files before links. Never traverse an archive-created link.
    destination.mkdir()
    destination = destination.resolve()
    links = []
    for member in tar.getmembers():
        parts = pathlib.PurePosixPath(member.name)
        if parts.is_absolute() or '..' in parts.parts or not parts.parts or parts.parts[0] != prefix:
            raise RuntimeError('Editor archive path')
        path = destination.joinpath(*parts.parts)
        parents = list(path.relative_to(destination).parents)[:-1]
        for parent in reversed(parents):
            directory = destination / parent
            directory.mkdir(exist_ok=True)
            if directory.is_symlink() or not directory.is_dir():
                raise RuntimeError('Editor archive directory')
        if member.isdir():
            path.mkdir(exist_ok=True)
            if path.is_symlink() or not path.is_dir():
                raise RuntimeError('Editor archive directory')
        elif member.isfile():
            with os.fdopen(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o644), 'wb') as output:
                shutil.copyfileobj(tar.extractfile(member), output)
            path.chmod(0o755 if member.mode & 0o111 else 0o644)
        elif member.issym() or member.islnk():
            links.append((member, path))
        else:
            raise RuntimeError('Editor archive special file')
    for member, path in links:
        parent = path.parent
        while parent != destination:
            if parent.is_symlink() or not parent.is_dir():
                raise RuntimeError('Editor archive link parent')
            parent = parent.parent
        if member.issym():
            if pathlib.PurePosixPath(member.linkname).is_absolute():
                raise RuntimeError('Editor archive absolute link')
            os.symlink(member.linkname, path)
        else:
            target = destination / member.linkname
            if not target.resolve().is_relative_to(destination / prefix) or not target.is_file() or target.is_symlink():
                raise RuntimeError('Editor archive hard link')
            os.link(target, path, follow_symlinks=False)
    for _, path in links:
        if not path.resolve().is_relative_to(destination / prefix):
            raise RuntimeError('Editor archive link escapes release')


def install():
    if platform.libc_ver()[0] != 'glibc' or platform.machine() not in DIGESTS:
        raise RuntimeError('Use a supported glibc Linux image (Ubuntu)')
    arch, digest = DIGESTS[platform.machine()]
    base = pathlib.Path('/opt/firecrab-code-editor')
    for path in [pathlib.Path('/opt'), base]:
        if not path.exists():
            path.mkdir(mode=0o755)
        info = path.lstat()
        if not stat.S_ISDIR(info.st_mode) or info.st_uid != 0 or info.st_mode & 0o022:
            raise RuntimeError('Editor install ownership')
    target = base / VERSION
    if not target.exists():
        with tempfile.TemporaryDirectory(dir=base) as temporary:
            temporary = pathlib.Path(temporary)
            archive = temporary / 'release.tar.gz'
            url = f'https://github.com/coder/code-server/releases/download/v{VERSION}/code-server-{VERSION}-linux-{arch}.tar.gz'
            checksum = hashlib.sha256()
            with urllib.request.urlopen(url, timeout=60) as response, archive.open('wb') as output:
                total = 0
                while chunk := response.read(1024 * 1024):
                    total += len(chunk)
                    if total > 384 * 1024 * 1024:
                        raise RuntimeError('Editor archive too large')
                    output.write(chunk)
                    checksum.update(chunk)
            if checksum.hexdigest() != digest:
                raise RuntimeError('Editor digest mismatch')
            prefix = f'code-server-{VERSION}-linux-{arch}'
            with tarfile.open(archive) as tar:
                members = tar.getmembers()
                if sum(m.size for m in members) > 1536 * 1024 * 1024 or len(members) > 100000:
                    raise RuntimeError('Editor extraction too large')
                for member in members:
                    if not (member.name == prefix or member.name.startswith(prefix + '/')):
                        raise RuntimeError('Editor archive path')
                extract(tar, temporary / 'extract', prefix)
            release = temporary / 'extract' / prefix
            # The verified release stays root-owned; the workspace cannot replace its executable.
            for path in [release, *release.rglob('*')]:
                if not path.is_symlink():
                    os.chmod(path, path.stat().st_mode & ~0o022)
            (release / '.firecrab-sha256').write_text(digest)
            os.rename(release, target)
    info = target.lstat()
    marker = target / '.firecrab-sha256'
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != 0 or info.st_mode & 0o022 or marker.is_symlink() or marker.read_text() != digest:
        raise RuntimeError('Editor release ownership')
    return target / 'bin/code-server'


def running(saved, user):
    try:
        root = pathlib.Path('/proc') / str(saved['pid'])
        fields = root.joinpath('stat').read_text().split(') ', 1)[1].split()
        return root.stat().st_uid == user.pw_uid and fields[0] != 'Z' and fields[19] == saved['started']
    except (KeyError, FileNotFoundError, ProcessLookupError):
        return False


def launch(args, env, user, marker, workspace):
    gate_read, gate_write = os.pipe()
    ack_read, ack_write = os.pipe()
    child = os.fork()
    if child == 0:
        try:
            os.close(gate_write)
            os.close(ack_read)
            os.setsid()
            drop(user)
            os.write(ack_write, b'r')
            os.close(ack_write)
            # Parent death before publication closes this pipe; an untracked editor never starts.
            if os.read(gate_read, 1) != b'r':
                os._exit(127)
            os.close(gate_read)
            null = os.open('/dev/null', os.O_RDWR)
            for fd in (0, 1, 2):
                os.dup2(null, fd)
            os.close(null)
            os.chdir('/')
            os.execve(args[0], args, env)
        finally:
            os._exit(127)
    os.close(gate_read)
    os.close(ack_write)
    try:
        if os.read(ack_read, 1) != b'r':
            raise RuntimeError('Editor user transition failed')
        started = pathlib.Path(f'/proc/{child}/stat').read_text().split(') ', 1)[1].split()[19]
        saved = {'workspace': workspace, 'version': VERSION, 'pid': child, 'started': started}
        fd, temporary = tempfile.mkstemp(prefix='editor-', dir=marker.parent)
        try:
            with os.fdopen(fd, 'w') as output:
                json.dump(saved, output)
                output.flush()
                os.fsync(output.fileno())
            os.replace(temporary, marker)
            directory = os.open(marker.parent, os.O_RDONLY | os.O_DIRECTORY)
            try:
                os.fsync(directory)
            finally:
                os.close(directory)
        finally:
            if os.path.exists(temporary):
                os.unlink(temporary)
        os.write(gate_write, b'r')
        return saved
    finally:
        os.close(gate_write)
        os.close(ack_read)


def main(payload):
    user = identity(payload)
    fd = os.open('/var/lib/firecrab-code/editor.lock', os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'r+') as lock:
        info = os.fstat(lock.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_uid != 0 or info.st_mode & 0o077:
            raise RuntimeError('Editor lock ownership')
        fcntl.flock(lock, fcntl.LOCK_EX)
        prepare_editor(payload, user)


def prepare_editor(payload, user):
    binary = install()
    marker = pathlib.Path('/var/lib/firecrab-code/editor.json')
    if marker.is_symlink():
        raise RuntimeError('Editor marker symlink')
    saved = json.loads(marker.read_text()) if marker.exists() else {}
    if not (saved.get('workspace') == payload['workspace'] and saved.get('version') == VERSION and running(saved, user)):
        if saved and running(saved, user):
            raise RuntimeError('Another editor process is retained')
        env = {'HOME': user.pw_dir, 'USER': user.pw_name, 'LOGNAME': user.pw_name, 'PATH': '/usr/local/bin:/usr/bin:/bin',
               'SHELL': '/bin/sh', 'LANG': 'C.UTF-8'}
        args = [str(binary), '--config', '/dev/null', '--bind-addr', '127.0.0.1:17777', '--auth', 'none', '--disable-telemetry',
                '--disable-update-check', '--disable-proxy', '--user-data-dir', user.pw_dir + '/.local/share/firecrab-editor',
                '--extensions-dir', user.pw_dir + '/.local/share/firecrab-editor/extensions', user.pw_dir + '/project']
        saved = launch(args, env, user, marker, payload['workspace'])
    for _ in range(120):
        if not running(saved, user):
            raise RuntimeError('Editor exited')
        try:
            with urllib.request.urlopen('http://127.0.0.1:17777/healthz', timeout=1) as response:
                if response.status == 200:
                    print('editor-ready')
                    return
        except OSError:
            pass
        time.sleep(0.5)
    raise RuntimeError('Editor readiness timeout')
