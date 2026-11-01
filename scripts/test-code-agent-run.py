#!/usr/bin/env python3
"""Local provider/CLI contracts. Never uses a real credential or model service."""
import http.server
import json
import os
from pathlib import Path
import subprocess
import struct
import selectors
import time
import sys
import tempfile
import threading
import types
import unittest

ROOT = Path(__file__).resolve().parents[1]
run = types.ModuleType('run')
for source in ['code_agent_auth/guest.py', 'code_editor/guest.py', 'code_agent_run/guest.py']:
    exec((ROOT / 'firecrab-api/src' / source).read_text(), run.__dict__)
PROFILES = json.loads((ROOT / 'firecrab-api/src/code_agent_auth/adapters.json').read_text())
TOKEN = 'fc-run-synthetic-not-a-provider-key'


def definitions(body):
    return body.get('tools', []) + [t for item in body.get('input', []) if isinstance(item,dict) and item.get('type')=='additional_tools' for t in item.get('tools',[])]


def responses(body, turn):
    tools = definitions(body)
    flat = [nested for tool in tools for nested in tool.get('tools', [tool])]
    names = [t.get('name') for t in flat]
    if turn == 0 and tools:
        name = next((n for n in ['exec_command', 'bash', 'shell_command', 'exec'] if n in names), None)
        if not name:
            raise AssertionError('No workspace shell tool: ' + repr(names))
        args = {'cmd' if name == 'exec_command' else 'command': "printf 'same-workspace' > agent-proof.txt"}
        if name == 'bash':
            args['description'] = 'Write workspace proof'
        item = {'id': 'fc_fixture_tool', 'type': 'function_call', 'call_id': 'call_fixture',
                'name': name, 'arguments': json.dumps(args), 'status': 'completed'}
        if name=='exec':
            item={'id':'ctc_fixture','type':'custom_tool_call','call_id':'call_fixture','namespace':'functions','name':'exec','input':'text(await tools.exec_command('+json.dumps({'cmd':"printf 'same-workspace' > agent-proof.txt"})+'));','status':'completed'}
    else:
        item = {'id': 'msg_fixture', 'type': 'message', 'role': 'assistant', 'status': 'completed',
                'content': [{'type': 'output_text', 'text': 'Workspace task completed.', 'annotations': []}]}
    response = {'id': 'resp_fixture_' + str(turn), 'object': 'response', 'status': 'completed',
                'model': body['model'], 'output': [item],
                'usage': {'input_tokens': 10, 'output_tokens': 10, 'total_tokens': 20}}
    initial = {**item, 'input':'', 'status':'in_progress'} if item['type']=='custom_tool_call' else ({**item, 'arguments': '', 'status': 'in_progress'} if item['type'] == 'function_call' else {**item, 'content': [], 'status': 'in_progress'})
    events = [('response.created', {'type': 'response.created', 'response': {**response, 'status': 'in_progress', 'output': []}}),
              ('response.output_item.added', {'type': 'response.output_item.added', 'output_index': 0, 'item': initial})]
    if item['type']=='custom_tool_call':
        events += [('response.custom_tool_call_input.delta',{'type':'response.custom_tool_call_input.delta','item_id':item['id'],'output_index':0,'delta':item['input']}),('response.custom_tool_call_input.done',{'type':'response.custom_tool_call_input.done','item_id':item['id'],'output_index':0,'input':item['input']})]
    elif item['type'] == 'function_call':
        events += [('response.function_call_arguments.delta', {'type': 'response.function_call_arguments.delta', 'item_id': item['id'], 'output_index': 0, 'delta': item['arguments']}),
                   ('response.function_call_arguments.done', {'type': 'response.function_call_arguments.done', 'item_id': item['id'], 'output_index': 0, 'arguments': item['arguments']})]
    else:
        events += [('response.content_part.added', {'type': 'response.content_part.added', 'item_id': item['id'], 'output_index': 0, 'content_index': 0, 'part': {'type': 'output_text', 'text': '', 'annotations': []}}),
                   ('response.output_text.delta', {'type': 'response.output_text.delta', 'item_id': item['id'], 'output_index': 0, 'content_index': 0, 'delta': item['content'][0]['text']})]
    events += [('response.output_item.done', {'type': 'response.output_item.done', 'output_index': 0, 'item': item}),
               ('response.completed', {'type': 'response.completed', 'response': response})]
    return events


def messages(body, turn):
    if turn == 0 and body.get('tools'):
        names = [t['name'] for t in body.get('tools', [])]
        name = next((n for n in ['Bash', 'bash'] if n in names), None)
        if not name:
            raise AssertionError('No workspace Bash tool: ' + repr(names))
        block = {'type': 'tool_use', 'id': 'tool_fixture', 'name': name,
                 'input': {'command': "printf 'same-workspace' > agent-proof.txt", 'description': 'Write workspace proof'}}
    else:
        block = {'type': 'text', 'text': 'Workspace task completed.'}
    message = {'id': 'msg_fixture_' + str(turn), 'type': 'message', 'role': 'assistant',
               'model': body['model'], 'content': [], 'stop_reason': None, 'stop_sequence': None,
               'usage': {'input_tokens': 10, 'output_tokens': 0}}
    initial = {**block, 'input': {}} if block['type'] == 'tool_use' else {**block, 'text': ''}
    delta = {'type': 'input_json_delta', 'partial_json': json.dumps(block['input'])} if block['type'] == 'tool_use' else {'type': 'text_delta', 'text': block['text']}
    return [('message_start', {'type': 'message_start', 'message': message}),
            ('content_block_start', {'type': 'content_block_start', 'index': 0, 'content_block': initial}),
            ('content_block_delta', {'type': 'content_block_delta', 'index': 0, 'delta': delta}),
            ('content_block_stop', {'type': 'content_block_stop', 'index': 0}),
            ('message_delta', {'type': 'message_delta', 'delta': {'stop_reason': 'tool_use' if block['type']=='tool_use' else 'end_turn', 'stop_sequence': None}, 'usage': {'output_tokens': 10}}),
            ('message_stop', {'type': 'message_stop'})]


def provider(records):
    tool_turn = [0]
    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_GET(self):
            self.send_response(200); self.send_header('Content-Type', 'application/json'); self.end_headers()
            self.wfile.write(b'{"object":"list","data":[]}')

        def do_POST(self):
            try:
                body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                records.append((self.path, dict(self.headers), body))
                if 'count_tokens' in self.path:
                    self.send_response(200); self.end_headers(); self.wfile.write(b'{"input_tokens":100}'); return
                turn = tool_turn[0]
                if len(records) > 8:
                    raise AssertionError('CLI did not terminate after synthetic completion')
                events = messages(body, turn) if '/messages' in self.path else responses(body, turn)
                if definitions(body):
                    tool_turn[0] += 1
                data = ''.join('event: ' + name + '\ndata: ' + json.dumps(event) + '\n\n' for name, event in events).encode()
                self.send_response(200); self.send_header('Content-Type', 'text/event-stream'); self.send_header('Content-Length', str(len(data))); self.end_headers()
                self.wfile.write(data)
            except Exception as error:
                records.append(('fixture-error', {}, str(error)))
                self.send_response(500); self.end_headers()
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


class Contracts(unittest.TestCase):
    def test_commands_have_isolated_auth_and_exact_model(self):
        for p in PROFILES:
            if not p['supported']:
                continue
            command, env = run.agent_command(p, 'fixture-model', 'http://127.0.0.1:1234', TOKEN, Path('/run/disposable'))
            self.assertNotIn(TOKEN, ' '.join(command))
            self.assertEqual(env['HOME'], '/run/disposable')
            self.assertNotIn('SSH_AUTH_SOCK', env)
            self.assertNotIn('GH_TOKEN', env)
            self.assertIn(TOKEN, json.dumps(env))
            self.assertNotIn('bypassPermissions', command)
            if p['agent'] == 'codex':
                self.assertIn('--ignore-user-config', command); self.assertIn('workspace-write', command)
            if p['agent'] == 'claude_code':
                self.assertIn('--bare', command); self.assertIn('--permission-prompts', command)
            if p['agent'] == 'opencode':
                self.assertIn('--pure', command); self.assertEqual(env['OPENCODE_DISABLE_PROJECT_CONFIG'], 'true')

    @unittest.skipUnless(os.getenv('FIRECRAB_CODE_AUTH_CLI_ROOT'), 'set FIRECRAB_CODE_AUTH_CLI_ROOT for official CLI contracts')
    def test_pinned_clis_use_assigned_token_and_execute_in_the_workspace(self):
        packages = Path(os.environ['FIRECRAB_CODE_AUTH_CLI_ROOT']) / 'node_modules'
        patterns = {'codex': '@openai/codex-*/vendor/*/bin/codex',
                    'claude': '@anthropic-ai/claude-code-*/claude', 'opencode': 'opencode-*/bin/opencode'}
        for p, use_default in ((p,d) for p in PROFILES for d in [False,True]):
            if not p['supported']:
                continue
            with self.subTest(adapter=p['id'], default=use_default), tempfile.TemporaryDirectory(prefix='fc-run-contract-') as raw:
                project = Path(raw) / 'project'; project.mkdir()
                home = Path(raw) / 'home'; home.mkdir(mode=0o700)
                subprocess.run(['/usr/bin/git', 'init', '-q', str(project)], env={'HOME': str(home), 'PATH': '/usr/bin:/bin', 'GIT_CONFIG_NOSYSTEM': '1'}, check=True)
                records = []; server = provider(records)
                try:
                    model = '' if use_default else ('gpt-5.4' if p['provider'] == 'openai' else 'claude-sonnet-4-6')
                    command, env = run.agent_command(p, model, 'http://127.0.0.1:' + str(server.server_port), TOKEN, home)
                    binaries = list(packages.glob(patterns[p['binary']]))
                    self.assertEqual(len(binaries), 1)
                    command[0] = str(binaries[0])
                    run.agent_directories(env)
                    result = subprocess.run(command, cwd=project, env=env,
                                            input=b'Create agent-proof.txt with same-workspace in the current project.',
                                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=45)
                    diagnostic = p['id'] + '\n' + result.stdout.decode(errors='replace')[-5000:] + result.stderr.decode(errors='replace')[-1500:] + '\n' + repr([(r[0], r[2] if r[0]=='fixture-error' else '') for r in records])
                    self.assertEqual(result.returncode, 0, diagnostic)
                    self.assertTrue((project / 'agent-proof.txt').is_file(), diagnostic)
                    self.assertEqual((project / 'agent-proof.txt').read_text(), 'same-workspace')
                    self.assertGreaterEqual(len(records), 2)
                    self.assertFalse(any(r[0] == 'fixture-error' for r in records), diagnostic)
                    for _, headers, body in records:
                        if model: self.assertEqual(body['model'], model)
                        else: self.assertTrue(isinstance(body['model'],str) and body['model'])
                        lowered = {k.lower(): v for k, v in headers.items()}
                        self.assertTrue(lowered.get('authorization') == 'Bearer ' + TOKEN or lowered.get('x-api-key') == TOKEN)
                    print('verified:', p['id'], 'default' if use_default else 'explicit', [r[0] for r in records], flush=True)
                finally:
                    Path('/tmp/firecrab-560-cli-' + p['id'] + '.json').write_text(json.dumps(records, indent=2))
                    server.shutdown(); server.server_close()

    def guest(self, root, slow=False):
        project = root / 'project'; project.mkdir()
        ephemeral = root / 'ephemeral'; ephemeral.mkdir()
        binary = root / 'fake-codex'
        binary.write_text('#!' + sys.executable + '\n' + '''import json,os,pathlib,sys,time,re,urllib.request
if '--version' in sys.argv: print('codex-cli 0.166.0')
elif '--with-api-key' in sys.argv:
 p=pathlib.Path(os.environ['CODEX_HOME'])/'auth.json';p.write_text(json.dumps({'OPENAI_API_KEY':sys.stdin.read()}));p.chmod(0o600)
elif 'status' in sys.argv: print('API key configured - fixture\\nAuthentication is checked when you send a request.')
else:
 sys.stdin.read()
 base=re.search(r'base_url="([^"]+)"',' '.join(sys.argv)).group(1)
 request=urllib.request.Request(base+'/responses',data=json.dumps({'model':'fixture-model','input':'fixture'}).encode(),headers={'Content-Type':'application/json','Authorization':'Bearer '+os.environ['FIRECRAB_CODE_TOKEN']})
 urllib.request.urlopen(request,timeout=60).read()
 if SLOW: time.sleep(60)
 pathlib.Path('agent-proof.txt').write_text('same-workspace')
 print(json.dumps({'type':'item.completed','item':{'type':'agent_message','text':'done'}}))
'''.replace('SLOW', repr(slow)))
        binary.chmod(0o755)
        source = '\n'.join((ROOT / 'firecrab-api/src' / p).read_text() for p in ['code_agent_auth/guest.py', 'code_editor/guest.py', 'code_agent_run/guest.py'])
        overrides = f'''
import types
System.ephemeral={str(ephemeral)!r}
System.identity=lambda self,_:(os.getuid(),os.getgid())
System.executable=lambda self,_:{str(binary)!r}
System.own=lambda self,path,*_:os.chmod(path,0o700)
identity=lambda _:types.SimpleNamespace(pw_uid=os.getuid(),pw_gid=os.getgid())
agent_install=lambda *_:None
agent_claim=lambda:open({str(root / 'claim')!r},'a+b')
original_temp=tempfile.TemporaryDirectory
def local_temp(*args,**kwargs):
 if kwargs.get('dir')=='/run':kwargs['dir']={str(ephemeral)!r}
 return original_temp(*args,**kwargs)
tempfile.TemporaryDirectory=local_temp
original_popen=subprocess.Popen
def local_popen(*args,**kwargs):
 for key in ['user','group','extra_groups']:kwargs.pop(key,None)
 workspace=kwargs.get('cwd')=='/home/fc-code/project'
 if workspace:kwargs['cwd']={str(project)!r}
 child=original_popen(*args,**kwargs)
 if workspace:pathlib.Path({str(root / 'pid')!r}).write_text(str(child.pid))
 return child
subprocess.Popen=local_popen
agent_main()
'''
        child = subprocess.Popen([sys.executable, '-u', '-c', source + overrides], env={'PATH': '/usr/bin:/bin', 'HOME': str(root)}, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0)
        payload = {'workspace': '11111111-1111-4111-8111-111111111111', 'run': '22222222-2222-4222-8222-222222222222', 'commit': 'a' * 40, 'adapter': PROFILES[0]['id'], 'profiles': PROFILES, 'packages': {}, 'model': '', 'prompt': 'Write the proof'}
        data = json.dumps(payload).encode(); child.stdin.write(struct.pack('!I', len(data)) + data); child.stdin.flush()
        return child, project, ephemeral

    def frame(self, child):
        def read(size):
            output = bytearray(); deadline = time.monotonic() + 5
            with selectors.DefaultSelector() as selector:
                selector.register(child.stdout, selectors.EVENT_READ)
                while len(output) < size:
                    self.assertTrue(selector.select(max(0, deadline-time.monotonic())), 'guest frame deadline')
                    chunk = os.read(child.stdout.fileno(), size-len(output))
                    self.assertTrue(chunk, 'guest disconnected before receipt')
                    output.extend(chunk)
            return output
        size = struct.unpack('!I', read(4))[0]
        self.assertLessEqual(size, 12*1024*1024)
        return json.loads(read(size))

    def reply(self, child, request):
        import base64
        for frame in [{'kind':'start','id':request['id'],'status':200,'contentType':'application/json'},
                      {'kind':'data','id':request['id'],'data':base64.b64encode(b'{"reply":"ok"}').decode()},
                      {'kind':'end','id':request['id']}]:
            data=json.dumps(frame).encode(); child.stdin.write(struct.pack('!I',len(data))+data)
        child.stdin.flush()

    def test_supervisor_uses_same_folder_and_cleans_configuration_before_exit_receipt(self):
        with tempfile.TemporaryDirectory() as raw:
            child, project, ephemeral = self.guest(Path(raw))
            try:
                frames = []
                while True:
                    frame = self.frame(child); frames.append(frame)
                    if frame['kind']=='request':
                        self.assertEqual(frame['path'],'/v1/responses')
                        self.assertNotIn('fc-run-',json.dumps(frame))
                        self.reply(child,frame)
                    if frame['kind'] in ['error', 'exit']:
                        break
                self.assertEqual(frames[-1], {'kind': 'exit', 'code': 0}, frames)
                child.wait(timeout=5)
                self.assertEqual((project / 'agent-proof.txt').read_text(), 'same-workspace')
                self.assertEqual(list(ephemeral.iterdir()), [])
                self.assertTrue(any(f['kind'] == 'output' for f in frames))
            finally:
                if child.poll() is None: child.terminate()
                child.communicate(timeout=5)

    def test_controller_eof_kills_process_group_and_removes_configuration(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw); child, project, ephemeral = self.guest(root, slow=True)
            try:
                self.assertEqual(self.frame(child)['kind'], 'ready')
                self.assertEqual(self.frame(child)['kind'],'request')
                pid = int((root / 'pid').read_text())
                child.stdin.close(); child.stdin = None
                child.communicate(timeout=5)
                with self.assertRaises(ProcessLookupError): os.kill(pid, 0)
                self.assertFalse((project / 'agent-proof.txt').exists())
                self.assertEqual(list(ephemeral.iterdir()), [])
            finally:
                if child.poll() is None: child.terminate()
                child.communicate(timeout=5)


if __name__ == '__main__':
    unittest.main()
