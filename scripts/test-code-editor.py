#!/usr/bin/env python3
"""Exercise archive boundaries and a real local PTY without root users or network."""
import importlib.util
import io
import json
import os
from pathlib import Path
import select
import struct
import subprocess
import sys
import tarfile
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / 'firecrab-api/src/code_editor'
spec = importlib.util.spec_from_file_location('editor_guest', SOURCE / 'guest.py')
guest = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guest)


def archive(members):
    stream = io.BytesIO()
    with tarfile.open(fileobj=stream, mode='w') as tar:
        for name, kind, value in members:
            member = tarfile.TarInfo(name)
            if kind == 'file':
                data = value.encode()
                member.size = len(data)
                tar.addfile(member, io.BytesIO(data))
            else:
                member.type = {'link': tarfile.SYMTYPE, 'hard': tarfile.LNKTYPE, 'device': tarfile.CHRTYPE}[kind]
                member.linkname = value
                tar.addfile(member)
    stream.seek(0)
    return tarfile.open(fileobj=stream)


class Editor(unittest.TestCase):
    def test_verified_release_extraction_preserves_files_and_internal_links(self):
        with tempfile.TemporaryDirectory() as temporary:
            dest = Path(temporary) / 'out'
            with archive([('release/lib/entry', 'file', 'trusted'), ('release/bin/editor', 'link', '../lib/entry')]) as tar:
                guest.extract(tar, dest, 'release')
            self.assertEqual((dest / 'release/bin/editor').read_text(), 'trusted')

    def test_extraction_refuses_traversal_links_and_special_files(self):
        cases = [[('../escape', 'file', 'bad')], [('/absolute', 'file', 'bad')],
                 [('release/link', 'link', '/etc/passwd')], [('release/link', 'link', '../../escape')],
                 [('release/device', 'device', '')], [('release/a', 'file', 'one'), ('release/a', 'file', 'two')],
                 [('release/a', 'link', '.'), ('release/a/b', 'link', '..')]]
        for members in cases:
            with self.subTest(members=members), tempfile.TemporaryDirectory() as temporary:
                with archive(members) as tar, self.assertRaises((RuntimeError, OSError)):
                    guest.extract(tar, Path(temporary) / 'out', 'release')
                self.assertFalse((Path(temporary) / 'escape').exists())

    @unittest.skipIf(os.geteuid() == 0, 'run the host PTY check as an ordinary user')
    def test_real_pty_keeps_current_unprivileged_user_cwd_files_and_resize(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary)
            setup = f'''\nfrom types import SimpleNamespace
user=SimpleNamespace(pw_uid=os.getuid(),pw_gid=os.getgid(),pw_name='test',pw_dir={str(path)!r})
identity=lambda payload:user
drop=lambda user:None
original_chdir=os.chdir
os.chdir=lambda requested:original_chdir({str(path)!r})
terminal({{}})
'''
            source = (SOURCE / 'guest.py').read_text() + '\n' + (SOURCE / 'terminal.py').read_text() + setup
            process = subprocess.Popen([sys.executable, '-u', '-c', source], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                def send(value):
                    data = json.dumps(value).encode()
                    process.stdin.write(struct.pack('!I', len(data)) + data)
                    process.stdin.flush()
                import base64
                send({'rows': 31, 'cols': 93})
                command = 'pwd > proof; id -u >> proof; stty size >> proof; printf retained > edited.txt\n'
                send({'input': base64.b64encode(command.encode()).decode()})
                deadline = time.monotonic() + 10
                while not (path / 'edited.txt').exists() and time.monotonic() < deadline:
                    readable, _, _ = select.select([process.stdout], [], [], 0.05)
                    if readable:
                        os.read(process.stdout.fileno(), 8192)
                self.assertEqual((path / 'edited.txt').read_text(), 'retained')
                self.assertEqual((path / 'proof').read_text().splitlines(), [str(path.resolve()), str(os.geteuid()), '31 93'])
            finally:
                process.stdin.close()
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=3)
                process.stdout.close()
                process.stderr.close()


if __name__ == '__main__':
    unittest.main()
