#!/usr/bin/env python3
"""Exercise the guest app supervisor with a real local stream socket."""
import os
from pathlib import Path
import signal
import socket
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
KEY = b"0123456789abcdef0123456789abcdef"


class GuestAppLog(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory(prefix="firecrab-applog-", dir="/tmp")
        root = Path(cls.temp.name)
        driver = root / "driver.c"
        driver.write_text('''#define _POSIX_C_SOURCE 200809L
#include "app_log.h"
#include <errno.h>
#include <signal.h>
#include <stdlib.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <sys/uio.h>
#include <unistd.h>
ssize_t test_sendmsg(int fd, const struct msghdr *msg, int flags) {
    static int injected;
    const unsigned char *header = msg->msg_iov[0].iov_base;
    if (!injected && getenv("TEST_EAGAIN") && header[0] == 1) {
        injected = 1; errno = EAGAIN; return -1;
    }
    return sendmsg(fd, msg, flags);
}
pid_t test_fork(void) {
    pid_t child = fork();
    if (child > 0 && getenv("TEST_FORK_TERM")) kill(getpid(), SIGTERM);
    return child;
}
int main(int argc, char **argv) { return firecrab_app_log_run(argc - 1, argv + 1); }
''')
        cls.binary = root / "app-log-test"
        object_file = root / "app-log.o"
        subprocess.run([
            os.environ.get("CC", "cc"), "-std=c11", "-Wall", "-Wextra", "-Werror",
            "-DFIRECRAB_APP_LOG_TEST", "-D_DARWIN_C_SOURCE", "-I", str(ROOT / "packaging/guest-console"),
            "-Dsendmsg=test_sendmsg", "-Dfork=test_fork", "-c", str(ROOT / "packaging/guest-console/app_log.c"), "-o", str(object_file),
        ], check=True)
        subprocess.run([
            os.environ.get("CC", "cc"), "-std=c11", "-Wall", "-Wextra", "-Werror",
            "-I", str(ROOT / "packaging/guest-console"), str(object_file), str(driver), "-o", str(cls.binary),
        ], check=True)

    @classmethod
    def tearDownClass(cls):
        cls.temp.cleanup()

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(dir=self.temp.name)
        self.root = Path(self.directory.name)
        self.key = self.root / "key"
        self.key.write_bytes(KEY)
        self.socket = self.root / "socket"
        self.env = {**os.environ, "FIRECRAB_APP_LOG_TEST_KEY": str(self.key),
                    "FIRECRAB_APP_LOG_TEST_SOCKET": str(self.socket)}

    def tearDown(self):
        self.directory.cleanup()

    @staticmethod
    def exact(stream, length):
        result = bytearray()
        while len(result) < length:
            chunk = stream.recv(length - len(result))
            if not chunk:
                raise EOFError("incomplete frame")
            result.extend(chunk)
        return bytes(result)

    def test_separate_output_is_framed_and_child_exit_is_preserved(self):
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as listener:
            listener.bind(str(self.socket))
            listener.listen()
            listener.settimeout(5)
            process = subprocess.Popen([str(self.binary), "/bin/sh", "-c",
                                        "printf 'app-out'; printf 'app-err' >&2; exit 7"],
                                       env=self.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                with listener.accept()[0] as stream:
                    stream.settimeout(5)
                    frames = []
                    while True:
                        try:
                            header = self.exact(stream, 5)
                            frames.append((header[0], self.exact(stream, int.from_bytes(header[1:], "big"))))
                        except EOFError:
                            break
                stdout, stderr = process.communicate(timeout=5)
                self.assertEqual(process.returncode, 7)
                self.assertEqual((stdout, stderr), (b"", b""))
                self.assertEqual(frames[0], (0, KEY))
                self.assertEqual(b"".join(body for tag, body in frames if tag == 1), b"app-out")
                self.assertEqual(b"".join(body for tag, body in frames if tag == 2), b"app-err")
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait()

    def test_missing_key_keeps_original_console_and_exit_status(self):
        self.key.unlink()
        result = subprocess.run([str(self.binary), "/bin/sh", "-c", "printf fallback; exit 9"],
                                env=self.env, capture_output=True, timeout=5)
        self.assertEqual(result.returncode, 9)
        self.assertEqual(result.stdout, b"fallback")

    def test_unavailable_sink_does_not_block_a_noisy_app(self):
        result = subprocess.run([str(self.binary), "/bin/sh", "-c",
                                 "i=0; while [ $i -lt 5000 ]; do printf '01234567890123456789\\n'; i=$((i+1)); done"],
                                env=self.env, capture_output=True, timeout=8)
        self.assertEqual(result.returncode, 0)
        self.assertEqual((result.stdout, result.stderr), (b"", b""))

    def test_eagain_retries_without_losing_the_connection(self):
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as listener:
            listener.bind(str(self.socket))
            listener.listen()
            listener.settimeout(5)
            process = subprocess.Popen([str(self.binary), "/bin/sh", "-c", "printf 'retry-me'"],
                                       env={**self.env, "TEST_EAGAIN": "1"}, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                with listener.accept()[0] as stream:
                    stream.settimeout(5)
                    header = self.exact(stream, 5)
                    self.assertEqual((header[0], self.exact(stream, int.from_bytes(header[1:], "big"))), (0, KEY))
                    header = self.exact(stream, 5)
                    self.assertEqual((header[0], self.exact(stream, int.from_bytes(header[1:], "big"))), (1, b"retry-me"))
                stdout, stderr = process.communicate(timeout=5)
                self.assertEqual(process.returncode, 0)
                self.assertEqual((stdout, stderr), (b"", b""))
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait()

    def test_termination_immediately_after_fork_reaches_child(self):
        result = subprocess.run([str(self.binary), "/bin/sleep", "1"],
                                env={**self.env, "TEST_FORK_TERM": "1"}, capture_output=True, timeout=5)
        self.assertEqual(result.returncode, 128 + signal.SIGTERM)


if __name__ == "__main__":
    unittest.main()
