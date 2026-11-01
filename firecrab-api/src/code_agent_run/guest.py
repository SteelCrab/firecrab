"""Supervise a workspace CLI; the provider credential never enters the guest."""
import base64
import http.server
import queue
import secrets
import selectors
import signal
import struct
import sys
import subprocess
import threading

FRAME_LIMIT = 12 * 1024 * 1024


def agent_claim():
    claim = open('/var/lib/firecrab-code/agent.lock', 'a+b', opener=lambda path, flags: os.open(path, flags | os.O_NOFOLLOW, 0o600))
    info = os.fstat(claim.fileno())
    if not stat.S_ISREG(info.st_mode) or info.st_uid != 0 or info.st_mode & 0o077:
        claim.close()
        raise Failure('cli_untrusted')
    try:
        fcntl.flock(claim, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        claim.close()
        raise Failure('agent_busy') from None
    return claim


def agent_frame(value):
    data = json.dumps(value, separators=(',', ':')).encode()
    if len(data) > FRAME_LIMIT:
        raise Failure('protocol_error')
    with AGENT_WRITE_LOCK:
        sys.stdout.buffer.write(struct.pack('!I', len(data)) + data)
        sys.stdout.buffer.flush()


def agent_install(profile, packages):
    binary = pathlib.Path('/usr/local/bin') / profile['binary']
    if binary.exists() or binary.is_symlink():
        return
    if platform.libc_ver()[0] != 'glibc':
        raise Failure('cli_platform_unsupported')
    package = packages.get(profile['binary'], {}).get(platform.machine())
    if not package:
        raise Failure('cli_platform_unsupported')
    base = pathlib.Path('/opt/firecrab-code-agents')
    for path in [pathlib.Path('/opt'), base, binary.parent]:
        path.mkdir(mode=0o755, exist_ok=True)
        info = path.lstat()
        if not stat.S_ISDIR(info.st_mode) or info.st_uid != 0 or info.st_mode & 0o022:
            raise Failure('cli_untrusted')
    target = base / (profile['binary'] + '-' + profile['cliVersion'])
    if target.exists():
        agent_publish(profile, target, binary)
        return
    with tempfile.TemporaryDirectory(dir=base) as temporary:
        temporary = pathlib.Path(temporary)
        archive = temporary / 'release.tgz'
        digest = hashlib.sha512()
        size = 0
        # The URL and integrity are controller-owned constants, never workspace input.
        with urllib.request.urlopen(package['url'], timeout=30) as response, archive.open('wb') as output:
            while chunk := response.read(1024 * 1024):
                size += len(chunk)
                if size > 384 * 1024 * 1024:
                    raise Failure('cli_install_failed')
                digest.update(chunk)
                output.write(chunk)
        expected = 'sha512-' + base64.b64encode(digest.digest()).decode()
        if not secrets.compare_digest(expected, package['integrity']):
            raise Failure('cli_install_failed')
        unpacked = temporary / 'unpacked'
        with tarfile.open(archive, 'r:gz') as tar:
            members = tar.getmembers()
            if len(members) > 1000 or sum(m.size for m in members) > 1024 ** 3:
                raise Failure('cli_install_failed')
            extract(tar, unpacked, 'package')
        os.rename(unpacked, target)
        target.chmod(0o755)
        agent_publish(profile, target, binary)


def agent_publish(profile, target, binary):
    if target.is_symlink():
        raise Failure('cli_untrusted')
    relative = {'codex': 'package/vendor/*/bin/codex',
                'claude': 'package/claude', 'opencode': 'package/bin/opencode'}[profile['binary']]
    candidates = list(target.glob(relative))
    if len(candidates) != 1 or not candidates[0].is_file() or candidates[0].is_symlink():
        raise Failure('cli_install_failed')
    # Reuse a complete, trusted release after interruption between rename and link publication.
    executable = System().executable({'binary': str(candidates[0])})
    os.symlink(executable, binary)


def agent_command(profile, model, endpoint, token, directory):
    """Only a disposable broker token is present in this environment."""
    home = str(directory)
    env = {'HOME': home, 'PATH': '/usr/local/bin:/usr/bin:/bin', 'LANG': 'C.UTF-8',
           'XDG_CONFIG_HOME': home + '/config', 'XDG_DATA_HOME': home + '/data',
           'XDG_CACHE_HOME': home + '/cache', 'XDG_STATE_HOME': home + '/state',
           'TMPDIR': home, 'NO_PROXY': '127.0.0.1,localhost'}
    binary = str(pathlib.Path('/usr/local/bin') / profile['binary'])
    if profile['agent'] == 'codex':
        env.update(CODEX_HOME=home + '/codex', FIRECRAB_CODE_TOKEN=token)
        # TOML inline tables use '='. Values are JSON strings and have no workspace input.
        provider = '{name="Assigned AI AUTH",base_url=' + json.dumps(endpoint + '/v1') + ',env_key="FIRECRAB_CODE_TOKEN",wire_api="responses"}'
        command = [binary, 'exec', '--json', '--ephemeral', '--ignore-user-config',
                   '--ignore-rules', '--sandbox', 'workspace-write', '-c', 'approval_policy="never"',
                   '-c', 'model_provider="firecrab"', '-c', 'model_providers.firecrab=' + provider,
                   '-c', 'mcp_servers={}', '-c', 'cli_auth_credentials_store="file"']
        if model:
            command += ['--model', model]
        return command + ['-'], env
    if profile['agent'] == 'claude_code':
        env.update(ANTHROPIC_API_KEY=token, ANTHROPIC_BASE_URL=endpoint,
                   CLAUDE_CONFIG_DIR=home + '/claude', DISABLE_AUTOUPDATER='1',
                   CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC='1')
        command = [binary, '--bare', '--print', '--output-format', 'stream-json', '--verbose',
                   '--no-session-persistence', '--permission-mode', 'dontAsk',
                   '--allowedTools', 'Read,Edit,Write,Bash,Glob,Grep', '--permission-prompts', 'none']
        if model:
            command += ['--model', model]
        return command, env
    provider = profile['provider']
    config = {'enabled_providers': [provider], 'provider': {provider: {
        'options': {'baseURL': endpoint + '/v1', 'apiKey': token}}}, 'mcp': {}, 'plugin': [],
        'permission': {'*': 'deny', 'read': 'allow', 'edit': 'allow', 'bash': 'allow',
                       'glob': 'allow', 'grep': 'allow'}}
    if model:
        config['model'] = config['small_model'] = provider + '/' + model
    env.update(OPENCODE_CONFIG_CONTENT=json.dumps(config), OPENCODE_DISABLE_PROJECT_CONFIG='true',
               OPENCODE_DISABLE_EXTERNAL_SKILLS='true', OPENCODE_DISABLE_MODELS_FETCH='true',
               OPENCODE_DISABLE_AUTOUPDATE='true', OPENCODE_DISABLE_DEFAULT_PLUGINS='true')
    env['OPENAI_API_KEY' if provider == 'openai' else 'ANTHROPIC_API_KEY'] = token
    return [binary, 'run', '--pure', '--format', 'json', '--agent', 'build'], env


def agent_directories(env, owner=None):
    for name in ['XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_CACHE_HOME', 'XDG_STATE_HOME', 'CODEX_HOME', 'CLAUDE_CONFIG_DIR']:
        if name in env:
            path = pathlib.Path(env[name])
            path.mkdir(mode=0o700, parents=True, exist_ok=True)
            if owner:
                System().own(path, *owner)


def agent_proxy(token, replies):
    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_GET(self):
            self.forward()

        def do_POST(self):
            self.forward()

        def forward(self):
            self.close_connection = True
            if (self.headers.get('Authorization') != 'Bearer ' + token
                    and self.headers.get('x-api-key') != token):
                self.send_error(403)
                return
            length = int(self.headers.get('Content-Length', '0'))
            if length < 0 or length > 8 * 1024 * 1024:
                self.send_error(413)
                return
            body = json.loads(self.rfile.read(length)) if length else None
            headers = {k.lower(): v for k, v in self.headers.items()
                       if k.lower() in ['openai-beta', 'anthropic-beta', 'x-codex-beta-features']}
            request_id = secrets.randbits(52)
            agent_frame({'kind': 'request', 'id': request_id, 'method': self.command,
                         'path': self.path, 'headers': headers, 'body': body})
            while True:
                frame = replies.get(timeout=600)
                if frame.get('id') != request_id:
                    raise Failure('protocol_error')
                kind = frame.get('kind')
                if kind == 'start':
                    self.send_response(frame['status'])
                    self.send_header('Content-Type', frame['contentType'])
                    self.send_header('Connection', 'close')
                    self.end_headers()
                elif kind == 'data':
                    self.wfile.write(base64.b64decode(frame['data'], validate=True))
                    self.wfile.flush()
                elif kind == 'end':
                    return
                else:
                    raise Failure('protocol_error')
    server = http.server.HTTPServer(('127.0.0.1', 0), Handler)
    # A blocked provider handler must never delay supervisor cleanup.
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


def agent_supervise(child, replies):
    selector = selectors.DefaultSelector()
    for stream in [sys.stdin.buffer, child.stdout, child.stderr]:
        os.set_blocking(stream.fileno(), False)
        selector.register(stream, selectors.EVENT_READ)
    buffers = {sys.stdin.buffer: b'', child.stdout: b''}
    total = 0
    deadline = time.monotonic() + 600
    while True:
        if time.monotonic() >= deadline:
            raise Failure('timed_out')
        for key, _ in selector.select(0.2):
            stream = key.fileobj
            data = os.read(stream.fileno(), 32768)
            if not data:
                if stream is sys.stdin.buffer:
                    raise Failure('controller_disconnected')
                selector.unregister(stream)
                continue
            if stream is child.stderr:
                continue
            buffers[stream] += data
            if stream is sys.stdin.buffer:
                while len(buffers[stream]) >= 4:
                    length = struct.unpack('!I', buffers[stream][:4])[0]
                    if length > FRAME_LIMIT:
                        raise Failure('protocol_error')
                    if len(buffers[stream]) < length + 4:
                        break
                    frame = json.loads(buffers[stream][4:length + 4])
                    buffers[stream] = buffers[stream][length + 4:]
                    replies.put_nowait(frame)
            else:
                total += len(data)
                if total > 16 * 1024 * 1024 or len(buffers[stream]) > 2 * 1024 * 1024:
                    raise Failure('output_limit')
                while b'\n' in buffers[stream]:
                    line, buffers[stream] = buffers[stream].split(b'\n', 1)
                    agent_frame({'kind': 'output', 'line': line.decode('utf-8', 'replace')})
        code = child.poll()
        if code is not None and child.stdout not in selector.get_map():
            return code


def agent_main():
    global AGENT_WRITE_LOCK
    AGENT_WRITE_LOCK = threading.Lock()
    child = server = temporary = claim = None
    outcome = {'kind': 'error', 'reason': 'guest_agent_rejected'}
    def stop(*_):
        raise Failure('controller_disconnected')
    signal.signal(signal.SIGHUP, stop)
    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGALRM, stop)
    signal.alarm(600)
    try:
        length = struct.unpack('!I', sys.stdin.buffer.read(4))[0]
        if length > 65536:
            raise Failure('protocol_error')
        payload = json.loads(sys.stdin.buffer.read(length))
        user = identity(payload)
        claim = agent_claim()
        profile = next(p for p in payload['profiles'] if p['id'] == payload['adapter'] and p['supported'])
        try:
            agent_install(profile, payload['packages'])
        except Failure:
            raise
        except Exception:
            raise Failure('cli_install_failed') from None
        token = 'fc-run-' + secrets.token_hex(32)
        check({'workspace': payload['workspace'], 'nonce': payload['run'],
               'adapter': profile['id'], 'key': token}, payload['profiles'])
        executable = System().executable(profile)
        temporary = tempfile.TemporaryDirectory(prefix='firecrab-code-run-', dir='/run')
        directory = pathlib.Path(temporary.name)
        System().own(directory, user.pw_uid, user.pw_gid)
        replies = queue.Queue(maxsize=4096)
        server = agent_proxy(token, replies)
        command, env = agent_command(profile, payload['model'],
                                     'http://127.0.0.1:' + str(server.server_port), token, directory)
        agent_directories(env, (user.pw_uid, user.pw_gid))
        command[0] = executable
        child = subprocess.Popen(command, cwd='/home/fc-code/project', env=env,
                                 user=user.pw_uid, group=user.pw_gid, extra_groups=[],
                                 start_new_session=True, stdin=subprocess.PIPE,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        child.stdin.write(payload['prompt'].encode())
        child.stdin.close()
        agent_frame({'kind': 'ready'})
        code = agent_supervise(child, replies)
        outcome = {'kind': 'exit', 'code': code}
    except Failure as error:
        outcome = {'kind': 'error', 'reason': str(error)}
    except Exception:
        outcome = {'kind': 'error', 'reason': 'guest_agent_rejected'}
    finally:
        signal.alarm(0)
        signal.signal(signal.SIGHUP, signal.SIG_IGN)
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        if child:
            try:
                os.killpg(child.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            child.wait()
        if server:
            server.server_close()
        if temporary:
            temporary.cleanup()
        if claim:
            claim.close()
    with contextlib.suppress(BrokenPipeError):
        agent_frame(outcome)
