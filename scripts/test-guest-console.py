#!/usr/bin/env python3
"""Exercise the shipped resize implementation on real PTYs (Linux and macOS)."""
import errno
import fcntl
import os
from pathlib import Path
import pty
import select
import signal
import socket
import struct
import subprocess
import tempfile
import termios
import unittest

ROOT = Path(__file__).resolve().parents[1]


class GuestConsole(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory(prefix="firecrab-resize-")
        root = Path(cls.temp.name)
        driver = root / "driver.c"
        driver.write_text('#include <stdlib.h>\n#include "resize.h"\nint main(int argc, char **argv) { if (argc != 3) return 2; return firecrab_console_resize(atoi(argv[1]), atoi(argv[2])) != 0; }\n')
        cls.binary = root / "resize-test"
        subprocess.run([os.environ.get("CC", "cc"), "-std=c11", "-Wall", "-Wextra", "-Werror", "-I", str(ROOT / "packaging/guest-console"), str(ROOT / "packaging/guest-console/resize.c"), str(driver), "-o", str(cls.binary)], check=True)

    @classmethod
    def tearDownClass(cls):
        cls.temp.cleanup()

    def setUp(self):
        self.master, self.tty = pty.openpty()
        fcntl.ioctl(self.tty, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))

    def tearDown(self):
        os.close(self.tty)
        os.close(self.master)

    def request(self, payload, fragment=False):
        host, guest = socket.socketpair()
        host.settimeout(2)
        with host, guest:
            process = subprocess.Popen([str(self.binary), str(guest.fileno()), str(self.tty)], pass_fds=(guest.fileno(), self.tty))
            guest.close()
            try:
                if fragment:
                    host.sendall(payload[:3])
                    host.sendall(payload[3:])
                else:
                    host.sendall(payload)
                host.shutdown(socket.SHUT_WR)
                reply = b""
                while len(reply) < 8:
                    part = host.recv(8 - len(reply))
                    if not part:
                        break
                    reply += part
                process.wait(timeout=2)
                return reply
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait()

    def size(self):
        return struct.unpack("HHHH", fcntl.ioctl(self.tty, termios.TIOCGWINSZ, bytes(8)))[:2]

    def test_split_request_applies_actual_tty_dimensions(self):
        self.assertEqual(self.request(b"FCR1" + struct.pack(">HH", 42, 132), fragment=True), b"FCA1" + struct.pack(">HH", 42, 132))
        self.assertEqual(self.size(), (42, 132))

    def test_rejects_bad_versions_and_bounds_without_resizing(self):
        for magic, rows, cols in [(b"FCR2", 42, 132), (b"FCR1", 0, 80), (b"FCR1", 24, 1001)]:
            self.assertEqual(self.request(magic + struct.pack(">HH", rows, cols)), b"FCE1" + struct.pack(">I", errno.EINVAL))
            self.assertEqual(self.size(), (24, 80))

    def test_partial_request_does_not_modify_the_tty(self):
        self.assertEqual(self.request(b"FCR1\x00"), b"")
        self.assertEqual(self.size(), (24, 80))

    def test_foreground_process_receives_sigwinch(self):
        read_end, write_end = os.pipe()
        monitor = subprocess.Popen(["python3", "-c", '''import fcntl, os, signal, sys, termios
os.setsid()
fcntl.ioctl(int(sys.argv[1]), termios.TIOCSCTTY, 0)
signal.signal(signal.SIGWINCH, lambda *_: os.write(int(sys.argv[2]), b"W"))
os.write(int(sys.argv[2]), b"R")
while True: signal.pause()
''', str(self.tty), str(write_end)], pass_fds=(self.tty, write_end))
        try:
            self.assertTrue(select.select([read_end], [], [], 3)[0])
            self.assertEqual(os.read(read_end, 1), b"R")
            self.assertTrue(self.request(b"FCR1" + struct.pack(">HH", 40, 120)).startswith(b"FCA1"))
            self.assertTrue(select.select([read_end], [], [], 3)[0])
            self.assertEqual(os.read(read_end, 1), b"W")
        finally:
            monitor.terminate()
            monitor.wait(timeout=3)
            os.close(read_end)
            os.close(write_end)


if __name__ == "__main__":
    unittest.main()
