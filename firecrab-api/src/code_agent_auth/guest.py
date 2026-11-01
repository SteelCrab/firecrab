"""Disposable CLI auth configuration probe. Never execute repository code or inference."""
import contextlib
import json
import os
import pathlib
import pwd
import re
import resource
import selectors
import signal
import stat
import subprocess
import tempfile
import time
import uuid


class Failure(Exception):
    pass


class System:
    ephemeral = '/run'

    def identity(self, workspace):
        user = pwd.getpwnam('fc-code')
        if user.pw_uid == 0 or user.pw_dir != '/home/fc-code' or user.pw_gecos != 'firecrab-code-' + workspace:
            raise Failure('runtime_unavailable')
        return user.pw_uid, user.pw_gid

    def executable(self, profile):
        path = pathlib.Path('/usr/local/bin') / profile['binary']
        try:
            for check in [path, path.resolve(strict=True)]:
                for part in [check, *check.parents]:
                    info = part.stat()
                    if info.st_uid != 0 or info.st_mode & 0o022:
                        raise Failure('cli_untrusted')
            if not path.is_file() or not os.access(path, os.X_OK):
                raise Failure('cli_unavailable')
        except FileNotFoundError:
            raise Failure('cli_unavailable') from None
        return str(path)

    def own(self, path, uid, gid):
        os.chown(path, uid, gid)
        os.chmod(path, 0o700)

    def run(self, args, env, directory, identity, data=b''):
        def limits():
            os.umask(0o077)
            resource.setrlimit(resource.RLIMIT_FSIZE, (16 * 1024 * 1024, 16 * 1024 * 1024))
        process = subprocess.Popen(args, cwd=directory, env=env, stdin=subprocess.PIPE,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True,
                                   user=identity[0], group=identity[1], extra_groups=[], preexec_fn=limits)
        output = [bytearray(), bytearray()]
        deadline = time.monotonic() + 8
        try:
            with selectors.DefaultSelector() as selector:
                for stream, index in [(process.stdout, 0), (process.stderr, 1)]:
                    os.set_blocking(stream.fileno(), False)
                    selector.register(stream, selectors.EVENT_READ, index)
                if data:
                    os.set_blocking(process.stdin.fileno(), False)
                    selector.register(process.stdin, selectors.EVENT_WRITE, 2)
                else:
                    process.stdin.close()
                while selector.get_map():
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        raise Failure('timed_out')
                    for item, _ in selector.select(remaining):
                        if item.data == 2:
                            data = data[os.write(item.fd, data[:4096]):]
                            if not data:
                                selector.unregister(item.fileobj); item.fileobj.close()
                        else:
                            chunk = os.read(item.fd, 4096)
                            if not chunk:
                                selector.unregister(item.fileobj); item.fileobj.close()
                            else:
                                output[item.data].extend(chunk)
                                if sum(map(len, output)) > 16384:
                                    raise Failure('cli_rejected')
            process.wait(timeout=max(0.01, deadline - time.monotonic()))
            return process.returncode, *(v.decode('utf-8', errors='replace') for v in output)
        except subprocess.TimeoutExpired:
            raise Failure('timed_out') from None
        finally:
            with contextlib.suppress(ProcessLookupError):
                os.killpg(process.pid, signal.SIGKILL)
            process.wait()
            for stream in [process.stdin, process.stdout, process.stderr]:
                stream.close()


def check(payload, profiles, system=None):
    system = system or System()
    if set(payload) != {'workspace', 'nonce', 'adapter', 'key'}:
        raise Failure('cli_rejected')
    uuid.UUID(payload['workspace']); uuid.UUID(payload['nonce'])
    profile = next(p for p in profiles if p['id'] == payload['adapter'])
    if not profile['supported'] or not isinstance(payload['key'], str) or not 1 <= len(payload['key']) <= 16384 or not payload['key'].isascii() or not all(33 <= ord(c) <= 126 for c in payload['key']):
        raise Failure('adapter_unavailable')
    identity = system.identity(payload['workspace'])
    executable = system.executable(profile)
    with tempfile.TemporaryDirectory(prefix='firecrab-code-auth-', dir=system.ephemeral) as raw:
        directory = pathlib.Path(raw)
        system.own(directory, *identity)
        env = {'PATH': '/usr/local/bin:/usr/bin:/bin', 'HOME': raw, 'LANG': 'C.UTF-8', 'NO_COLOR': '1',
               'XDG_CONFIG_HOME': str(directory / 'config'), 'XDG_DATA_HOME': str(directory / 'data'),
               'XDG_CACHE_HOME': str(directory / 'cache'), 'XDG_STATE_HOME': str(directory / 'state')}
        code, out, err = system.run([executable, '--version'], env, raw, identity)
        version = profile['cliVersion']
        expected = {'codex': 'codex-cli ' + version, 'claude_code': version + ' (Claude Code)', 'opencode': version}[profile['agent']]
        if code or out.strip() != expected:
            raise Failure('version_mismatch')
        agent = profile['agent']
        if agent == 'codex':
            auth = directory / 'codex'; auth.mkdir(mode=0o700); system.own(auth, *identity)
            env['CODEX_HOME'] = str(auth)
            command = [executable, '-c', 'cli_auth_credentials_store="file"']
            if system.run([*command, 'login', '--with-api-key'], env, raw, identity, payload['key'].encode())[0]:
                raise Failure('cli_rejected')
            code, out, err = system.run([*command, 'login', 'status'], env, raw, identity)
            fd = os.open(auth / 'auth.json', os.O_RDONLY | os.O_NOFOLLOW)
            with os.fdopen(fd, 'rb') as stream:
                info = os.fstat(stream.fileno())
                if not stat.S_ISREG(info.st_mode) or info.st_uid != identity[0] or info.st_mode & 0o077:
                    raise Failure('cli_rejected')
                saved = json.loads(stream.read(32768))
            accepted = code == 0 and 'API key configured - ' in out + err and 'Authentication is checked when you send a request.' in out + err and saved.get('OPENAI_API_KEY') == payload['key']
        elif agent == 'claude_code':
            env.update({'ANTHROPIC_API_KEY': payload['key'], 'CLAUDE_CONFIG_DIR': str(directory / 'claude'), 'DISABLE_AUTOUPDATER': '1', 'DISABLE_TELEMETRY': '1'})
            code, out, _ = system.run([executable, 'auth', 'status'], env, raw, identity)
            status = json.loads(out)
            accepted = code == 0 and status.get('loggedIn') is True and status.get('authMethod') == 'api_key' and status.get('apiKeySource') == 'ANTHROPIC_API_KEY' and status.get('apiProvider') == 'firstParty' and status.get('configDirectory') == env['CLAUDE_CONFIG_DIR']
        elif agent == 'opencode':
            variable = 'OPENAI_API_KEY' if profile['provider'] == 'openai' else 'ANTHROPIC_API_KEY'
            env.update({variable: payload['key'], 'OPENCODE_DISABLE_AUTOUPDATE': 'true', 'OPENCODE_DISABLE_MODELS_FETCH': 'true', 'OPENCODE_DISABLE_PROJECT_CONFIG': 'true', 'OPENCODE_DISABLE_EXTERNAL_SKILLS': 'true'})
            code, out, err = system.run([executable, 'auth', 'list'], env, raw, identity)
            clean = re.sub(r'\x1b\[[0-9;?]*[A-Za-z]', '', out + err)
            accepted = code == 0 and variable in clean and '1 environment variable' in clean and '0 credentials' in clean
        else:
            raise Failure('adapter_unavailable')
        if not accepted:
            raise Failure('cli_rejected')
    return {'nonce': payload['nonce'], 'adapter': profile['id'], 'cliVersion': profile['cliVersion'], 'status': 'configuration_verified'}


def main(payload, profiles):
    def interrupted(*_):
        raise Failure('timed_out')
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGHUP, interrupted)
    try:
        result = check(json.loads(payload), profiles)
    except Failure as error:
        result = {'status': str(error)}
    except Exception:
        result = {'status': 'cli_rejected'}
    print(json.dumps(result))
