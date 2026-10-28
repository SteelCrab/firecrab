#!/usr/bin/env python3
"""Exercise the guest app supervisor with a real local stream socket."""
import os
from pathlib import Path
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
        driver.write_text('#include "app_log.h"\nint main(int argc, char **argv) { return firecrab_app_log_run(argc - 1, argv + 1); }\n')
        cls.binary = root / "app-log-test"
        subprocess.run([
            os.environ.get("CC", "cc"), "-std=c11", "-Wall", "-Wextra", "-Werror",
            "-DFIRECRAB_APP_LOG_TEST", "-D_DARWIN_C_SOURCE", "-I", str(ROOT / "packaging/guest-console"),
            str(ROOT / "packaging/guest-console/app_log.c"), str(driver), "-o", str(cls.binary),
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


if __name__ == "__main__":
    unittest.main()
