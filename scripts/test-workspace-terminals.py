#!/usr/bin/env python3
"""Real PTYs in an owned temporary fixture. Run as root in an isolated Linux container."""
import os
from pathlib import Path
import signal
import struct
import subprocess
import sys
import tempfile
import time
import unittest
import uuid

REPO = Path(__file__).resolve().parent.parent


@unittest.skipUnless(sys.platform == "linux" and os.geteuid() == 0, "requires an isolated Linux root fixture")
class Terminals(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.build = tempfile.TemporaryDirectory(prefix="firecrab-pty-build-")
        cls.binary = Path(cls.build.name) / "terminals"
        driver = Path(cls.build.name) / "main.c"
        driver.write_text('#include "workspace.h"\nint main(int n,char **v){return firecrab_workspace_terminal(n,v);}\n')
        source = REPO / "packaging/guest-console"
        subprocess.run([os.environ.get("CC", "cc"), "-std=c11", "-Wall", "-Wextra", "-Werror",
            "-DFIRECRAB_WORKSPACE_TEST", "-I", str(source), str(source / "workspace.c"),
            str(source / "terminal.c"), str(driver), "-o", str(cls.binary)], check=True)

    @classmethod
    def tearDownClass(cls):
        cls.build.cleanup()

    def setUp(self):
        self.fixture = tempfile.TemporaryDirectory(prefix="firecrab-pty-")
        self.root = Path(self.fixture.name)
        self.project, self.database, self.volume = [str(uuid.uuid4()) for _ in range(3)]
        self.digest = "a" * 64
        marker = self.root / ".firecrab-workspace"
        marker.mkdir(mode=0o700)
        (marker / "ready").write_text(f"{self.project}:{self.database}:{self.digest}")
        (marker / "ready").chmod(0o600)
        self.sessions = []
        self.addCleanup(self.cleanup)

    def cleanup(self):
        for session in self.sessions:
            try:
                self.call(session, 5)
                self.until(lambda: self.call(session)["state"] in (2, 3))
            except (OSError, AssertionError, subprocess.SubprocessError):
                pass
        self.fixture.cleanup()

    def frame(self, session, action, data=b"", cursor=0, rows=24, cols=80):
        return b"FCT1" + struct.pack(">I", action) + session.bytes + struct.pack(">QHHI", cursor, rows, cols, len(data)) + data

    def call(self, session, action=2, data=b"", cursor=0, rows=24, cols=80, error=None):
        result = subprocess.run([str(self.binary), "--workspace-terminal", self.project, self.database,
            self.volume, self.digest, str(self.root)], input=self.frame(session, action, data, cursor, rows, cols),
            capture_output=True, timeout=8)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertGreaterEqual(len(result.stdout), 8)
        magic, size = result.stdout[:4], struct.unpack(">I", result.stdout[4:8])[0]
        if error is not None:
            self.assertEqual((magic, size), (b"FCTE", error))
            return
        self.assertEqual(magic, b"FCTR", (magic, size, result.stderr))
        self.assertEqual(size, len(result.stdout) - 8)
        body = result.stdout[8:]
        self.assertGreaterEqual(len(body), 96)
        self.assertEqual(body[:4], b"FCTS")
        return {"state": struct.unpack_from(">I", body, 4)[0],
            "uid": struct.unpack_from(">I", body, 8)[0], "gid": struct.unpack_from(">I", body, 12)[0],
            "supervisor": struct.unpack_from(">I", body, 16)[0], "child": struct.unpack_from(">I", body, 20)[0],
            "supervisorTicks": struct.unpack_from(">Q", body, 24)[0],
            "boot": uuid.UUID(bytes=body[40:56]),
            "begin": struct.unpack_from(">Q", body, 56)[0], "end": struct.unpack_from(">Q", body, 64)[0],
            "exit": struct.unpack_from(">I", body, 72)[0],
            "rows": struct.unpack_from(">I", body, 80)[0], "columns": struct.unpack_from(">I", body, 84)[0], "flags": struct.unpack_from(">I", body, 76)[0],
            "cursor": struct.unpack_from(">Q", body, 88)[0], "bytes": body[96:]}

    def until(self, probe, seconds=8):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            result = probe()
            if result:
                return result
            time.sleep(0.02)
        self.fail("owned terminal did not reach expected state")

    def create(self, session=None, cwd=b""):
        session = session or uuid.uuid4()
        self.sessions.append(session)
        item = self.call(session, 1, cwd)
        self.assertEqual(item["state"], 1)
        self.assertGreaterEqual(item["uid"], 100000)
        return session, item

    def output(self, session):
        cursor, result = 0, bytearray()
        for _ in range(256):
            item = self.call(session, cursor=cursor)
            result += item["bytes"]
            cursor = item["cursor"] + len(item["bytes"])
            if cursor >= item["end"]:
                return bytes(result)
        self.fail("output replay did not finish")

    def test_independent_ptys_and_detached_processes(self):
        first, a = self.create()
        second, b = self.create()
        self.assertNotEqual(a["uid"], b["uid"])
        self.assertNotEqual(a["child"], b["child"])
        self.assertEqual(a["gid"], b["gid"])
        self.call(first, 3, b"export PROOF=first; printf 'FIRST:%s\\n' $PROOF\n")
        self.call(second, 3, b"printf 'SECOND:%s\\n' ${PROOF-unset}\n")
        self.until(lambda: b"FIRST:first" in self.output(first))
        self.until(lambda: b"SECOND:unset" in self.output(second))
        self.assertNotIn(b"FIRST:first", self.output(second))
        time.sleep(0.1)
        self.assertEqual(self.call(first)["supervisorTicks"], a["supervisorTicks"])

    def test_create_is_idempotent_and_never_reexecutes_a_closed_session(self):
        session, original = self.create()
        self.assertEqual(self.call(session, 1)["child"], original["child"])
        self.call(session, 1, rows=25, error=3)
        self.call(session, 3, b"printf one >> starts; exit 7\n")
        self.until(lambda: self.call(session)["state"] == 2)
        repeated = self.call(session, 1)
        self.assertEqual(repeated["child"], original["child"])
        self.assertEqual(repeated["state"], 2)
        self.assertEqual(repeated["exit"], 7)
        self.assertEqual((self.root / "starts").read_bytes(), b"one")

    def test_identity_and_private_metadata(self):
        session, item = self.create()
        self.call(session, 3, b"id -u; id -g; grep NoNewPrivs /proc/self/status; cat .firecrab-workspace/ready; printf DONE\\n\n")
        self.until(lambda: b"Permission denied" in self.output(session))
        output = self.output(session)
        self.assertIn(str(item["uid"]).encode(), output)
        self.assertIn(str(item["gid"]).encode(), output)
        self.assertIn(b"NoNewPrivs:\t1", output)
        self.assertNotIn(self.digest.encode(), output)

    def test_resize_changes_only_the_selected_pty(self):
        first, _ = self.create()
        second, _ = self.create()
        resized = self.call(first, 4, rows=35, cols=101)
        self.assertEqual((resized["rows"], resized["columns"]), (35, 101))
        self.call(first, 3, b"stty size\n")
        self.call(second, 3, b"stty size\n")
        self.until(lambda: b"35 101" in self.output(first))
        self.until(lambda: b"24 80" in self.output(second))

    def test_interrupt_stops_foreground_job_and_keeps_the_shell(self):
        session, before = self.create()
        self.call(session, 3, b"sleep 100\n")
        time.sleep(0.1)
        self.call(session, 3, b"\x03")
        self.call(session, 3, b"printf done > after-interrupt\n")
        self.until(lambda: (self.root / "after-interrupt").exists())
        self.assertEqual(self.call(session)["child"], before["child"])
        self.assertEqual(self.call(session)["state"], 1)

    def test_stopped_uid_is_never_reused_for_another_session_with_the_same_seed(self):
        first, before = self.create()
        private = self.root / ".firecrab-workspace/terminals" / first.hex / "home/private"
        secret = os.urandom(32).hex().encode()
        private.write_bytes(secret)
        os.chown(private, before["uid"], before["gid"])
        private.chmod(0o600)
        self.call(first, 5)
        self.until(lambda: self.call(first)["state"] == 2)
        identifier = uuid.UUID(bytes=first.bytes[:4] + uuid.uuid4().bytes[4:])
        second, after = self.create(identifier)
        self.assertNotEqual(after["uid"], before["uid"])
        self.call(second, 3, f"cat .firecrab-workspace/terminals/{first.hex}/home/private\n".encode())
        self.until(lambda: b"Permission denied" in self.output(second))
        self.assertNotIn(secret, self.output(second))

    def test_non_dumpable_descendant_cannot_hide_from_termination(self):
        session, _ = self.create()
        command = ("python3 -c \"import ctypes,os,pathlib,signal,time; "
            "libc=ctypes.CDLL(None); assert libc.prctl(4,0,0,0,0)==0; pathlib.Path('private-dumpable').write_text(str(libc.prctl(3,0,0,0,0))); signal.signal(signal.SIGTERM,signal.SIG_IGN); "
            "signal.signal(signal.SIGHUP,signal.SIG_IGN); pathlib.Path('private-pid').write_text(str(os.getpid())); time.sleep(100)\" &\n")
        self.call(session, 3, command.encode())
        self.until(lambda: (self.root / "private-pid").exists())
        pid = int((self.root / "private-pid").read_text())
        self.assertEqual((self.root / "private-dumpable").read_text(), "0")
        self.call(session, 5)
        self.until(lambda: self.call(session)["state"] == 2)
        def gone():
            try:
                return Path(f"/proc/{pid}/stat").read_text().rsplit(") ", 1)[1][0] in ("Z", "X")
            except FileNotFoundError:
                return True
        self.until(gone)

    def test_stop_cleans_escaped_descendants_and_preserves_other_session(self):
        first, a = self.create()
        second, b = self.create()
        self.call(first, 3, b"setsid sh -c 'trap \"\" HUP TERM; sleep 100' >/dev/null 2>&1 &\n")
        self.call(first, 5)
        self.until(lambda: self.call(first)["state"] == 2)
        live = []
        for process in Path('/proc').iterdir():
            if not process.name.isdigit():
                continue
            try:
                state = (process / 'stat').read_text().rsplit(') ', 1)[1][0]
                if process.stat().st_uid == a["uid"] and state not in ('Z', 'X'):
                    live.append(process.name)
            except FileNotFoundError:
                pass
        self.assertEqual(live, [])
        self.assertEqual(self.call(second)["child"], b["child"])
        self.assertEqual(self.call(second)["state"], 1)

    def test_supervisor_crash_is_interrupted_without_duplicate_execution(self):
        session, item = self.create()
        self.call(session, 3, b"printf once >> crash-proof\n")
        self.until(lambda: (self.root / "crash-proof").exists())
        os.kill(item["supervisor"], signal.SIGKILL)
        def supervisor_gone():
            try:
                state = Path(f"/proc/{item['supervisor']}/stat").read_text().rsplit(") ", 1)[1][0]
                return state in ("Z", "X")
            except FileNotFoundError:
                return True
        self.until(supervisor_gone)
        self.until(lambda: self.call(session)["state"] == 3)
        self.assertEqual(self.call(session, 1)["state"], 3)
        self.assertEqual((self.root / "crash-proof").read_bytes(), b"once")

    def test_confined_working_folder_and_shared_writes(self):
        (self.root / "src").mkdir()
        session, _ = self.create(cwd=b"src")
        self.call(session, 3, b"printf shared > file; pwd\n")
        self.until(lambda: (self.root / "src/file").exists())
        self.assertEqual((self.root / "src/file").read_bytes(), b"shared")
        self.assertEqual((self.root / "src/file").stat().st_mode & 0o777, 0o660)
        for path in (b"../", b"/root", b".firecrab-workspace", b"src/../"):
            self.call(uuid.uuid4(), 1, path, error=1)

    def test_retained_output_has_a_bounded_cursor_and_truncation_notice(self):
        session, _ = self.create()
        self.call(session, 3, b"python3 -c \"import sys; sys.stdout.write('x'*140000)\"\n")
        self.until(lambda: self.call(session)["end"] > 130000)
        item = self.call(session)
        self.assertGreater(item["begin"], 0)
        self.assertEqual(item["cursor"], item["begin"])
        self.assertEqual(item["flags"] & 1, 1)
        self.assertLessEqual(item["end"] - item["begin"], 65536)
        self.assertLessEqual(len(self.output(session)), 65536)


if __name__ == "__main__":
    unittest.main()
