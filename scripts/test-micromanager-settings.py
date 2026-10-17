#!/usr/bin/env python3
"""Exercise the actual crossterm editor in an isolated POSIX PTY."""
import argparse
import errno
import fcntl
import json
import os
import pty
import select
import struct
import subprocess
import tempfile
import termios
import time
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument('--test-binary', required=True, type=Path)
args = parser.parse_args()

with tempfile.TemporaryDirectory(prefix='firecrab-settings-') as directory:
    def session(keys, expected):
        master, slave = pty.openpty()
        original = termios.tcgetattr(slave)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 28, 80, 0, 0))
        env = dict(os.environ, FIRECRAB_TUI_TEST_HOME=directory)
        process = subprocess.Popen([
            str(args.test_binary.resolve()), '--exact',
            'micromanager::settings_tui::tests::interactive_terminal', '--ignored', '--nocapture'
        ], stdin=slave, stdout=slave, stderr=slave, env=env)
        output = bytearray()
        def until(needle):
            deadline = time.monotonic() + 10
            while needle not in output:
                assert time.monotonic() < deadline, output.decode(errors='replace')
                ready, _, _ = select.select([master], [], [], 0.1)
                if ready:
                    try:
                        chunk = os.read(master, 65536)
                    except OSError as error:
                        if error.errno == errno.EIO:
                            break
                        raise
                    if not chunk:
                        break
                    output.extend(chunk)
            assert needle in output, output.decode(errors='replace')
        try:
            until(b'microManager settings')
            # Resize uses the real event loop; the editor remains usable.
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 30, 96, 0, 0))
            os.write(master, keys)
            until(expected)
            if b'Unsaved' in expected:
                os.write(master, b'y')
            else:
                os.write(master, b'\x1b')
            assert process.wait(timeout=10) == 0
            assert termios.tcgetattr(slave) == original, 'raw terminal mode leaked after exit'
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
            os.close(master)
            os.close(slave)
    session(b'\r4\r\x13', b'Saved.')
    path = Path(directory) / 'settings.json'
    assert json.loads(path.read_text())['cpu'] == 4
    previous = path.read_bytes()
    session(b'\r8\r\x1b', b'Unsaved changes.')
    assert path.read_bytes() == previous, 'discard wrote an unsaved draft'
    print('PASS: keyboard edit/save, resize, discard, atomic persistence, terminal cleanup')
