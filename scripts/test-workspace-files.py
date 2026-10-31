#!/usr/bin/env python3
"""Exercise the real descriptor-based guest file protocol on an owned fixture."""
import os
import fcntl
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
import struct
import subprocess
import tempfile
import unittest
import uuid

REPO = Path(__file__).resolve().parent.parent
LIMIT = 1024 * 1024


class WorkspaceFiles(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.build = tempfile.TemporaryDirectory(prefix="firecrab-workspace-files-")
        root = Path(cls.build.name)
        cls.binary = root / "workspace-files"
        driver = root / "main.c"
        driver.write_text('#include "workspace.h"\nint main(int n,char **v){return firecrab_workspace_files(n,v);}\n')
        source = REPO / "packaging/guest-console"
        subprocess.run([os.environ.get("CC", "cc"), "-std=c11", "-Wall", "-Wextra", "-Werror",
            "-DFIRECRAB_WORKSPACE_TEST", "-I", str(source), str(source / "workspace.c"),
            str(driver), "-o", str(cls.binary)], check=True)

    @classmethod
    def tearDownClass(cls):
        cls.build.cleanup()

    def setUp(self):
        self.fixture = tempfile.TemporaryDirectory()
        self.addCleanup(self.fixture.cleanup)
        self.root = Path(self.fixture.name)
        self.project, self.database, self.volume = [str(uuid.uuid4()) for _ in range(3)]
        self.digest = "a" * 64
        marker = self.root / ".firecrab-workspace"
        marker.mkdir(mode=0o700)
        (marker / "ready").write_text(f"{self.project}:{self.database}:{self.digest}")
        (marker / "ready").chmod(0o600)

    def request(self, action, path="", base=b"", content=b"", operation=None):
        name = path.encode()
        identifier = operation.bytes if operation else bytes(16)
        length = 0xffffffff if base is None else len(base)
        frame = b"FCW1" + struct.pack(">IIII", action, len(name), length, len(content)) + identifier
        frame += name + (base or b"") + content
        return frame

    def exchange(self, frame):
        result = subprocess.run([str(self.binary), "--workspace-files", self.project, self.database,
            self.volume, self.digest, str(self.root)], input=frame, capture_output=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertGreaterEqual(len(result.stdout), 8)
        magic = result.stdout[:4]
        size = struct.unpack(">I", result.stdout[4:8])[0]
        if magic == b"FCWE":
            self.assertEqual(len(result.stdout), 8)
            return size, b""
        self.assertEqual(magic, b"FCWR")
        self.assertEqual(size, len(result.stdout) - 8)
        return 0, result.stdout[8:]

    def test_create_read_edit_and_receipt(self):
        operation = uuid.uuid4()
        frame = self.request(3, "src/hello.txt", None, b"hello\x00world", operation)
        self.assertEqual(self.exchange(frame), (0, b""))
        self.assertEqual(self.exchange(self.request(2, "src/hello.txt")), (0, b"hello\x00world"))
        self.assertEqual(self.exchange(self.request(4, operation=operation)), (0, frame))
        edit = self.request(3, "src/hello.txt", b"hello\x00world", b"changed", uuid.uuid4())
        self.assertEqual(self.exchange(edit), (0, b""))
        self.assertEqual((self.root / "src/hello.txt").read_bytes(), b"changed")

    def test_retry_after_later_edit_does_not_replay_old_content(self):
        operation = uuid.uuid4()
        frame = self.request(3, "file", None, b"first", operation)
        self.assertEqual(self.exchange(frame)[0], 0)
        (self.root / "file").write_bytes(b"external later change")
        self.assertEqual(self.exchange(frame)[0], 0)
        self.assertEqual((self.root / "file").read_bytes(), b"external later change")

    def test_operation_id_cannot_be_reused_for_different_content(self):
        operation = uuid.uuid4()
        self.assertEqual(self.exchange(self.request(3, "file", None, b"first", operation))[0], 0)
        self.assertEqual(self.exchange(self.request(3, "file", None, b"other", operation))[0], 9)
        self.assertEqual((self.root / "file").read_bytes(), b"first")

    def test_stale_base_does_not_replace_file(self):
        (self.root / "file").write_bytes(b"current")
        frame = self.request(3, "file", b"stale", b"wrong", uuid.uuid4())
        self.assertEqual(self.exchange(frame)[0], 3)
        self.assertEqual((self.root / "file").read_bytes(), b"current")

    def test_paths_are_confined_and_metadata_is_private(self):
        for path in ("../escape", "/etc/passwd", "a//b", "./a", "a/../b", "a\\b",
                     ".firecrab-workspace/ready", "nested/.firecrab-state", "nul\x00name", "line\nname"):
            with self.subTest(path=path):
                self.assertNotEqual(self.exchange(self.request(2, path))[0], 0)

    def test_symlinks_hardlinks_and_special_files_are_not_read_or_written(self):
        with tempfile.TemporaryDirectory() as other:
            outside = Path(other) / "private"
            outside.write_bytes(b"private")
            (self.root / "link").symlink_to(outside)
            (self.root / "directory-link").symlink_to(other, target_is_directory=True)
            os.link(outside, self.root / "hardlink")
            os.mkfifo(self.root / "fifo")
            for path in ("link", "directory-link/private", "hardlink", "fifo"):
                with self.subTest(path=path):
                    self.assertNotEqual(self.exchange(self.request(2, path))[0], 0)
                    self.assertNotEqual(self.exchange(self.request(3, path, None, b"bad", uuid.uuid4()))[0], 0)
            self.assertEqual(outside.read_bytes(), b"private")

    def test_git_and_project_instructions_are_regular_project_files(self):
        for path in (".git/config", "AGENTS.md", "src/한글.txt", "$(touch pwned)", "`touch pwned`"):
            with self.subTest(path=path):
                self.assertEqual(self.exchange(self.request(3, path, None, b"content", uuid.uuid4()))[0], 0)
                self.assertEqual(self.exchange(self.request(2, path)), (0, b"content"))

    def test_listing_hides_internal_and_linked_entries(self):
        (self.root / "visible").write_bytes(b"ok")
        (self.root / "directory").mkdir()
        (self.root / "link").symlink_to("visible")
        status, payload = self.exchange(self.request(1))
        self.assertEqual(status, 0)
        entries = []
        while payload:
            kind, size = payload[0:1], struct.unpack(">I", payload[1:5])[0]
            entries.append((kind, payload[5:5+size].decode()))
            payload = payload[5+size:]
        self.assertEqual(sorted(entries), [(b"d", "directory"), (b"f", "visible")])

    def test_two_writers_cannot_both_replace_the_same_base(self):
        (self.root / "file").write_bytes(b"base")
        requests = [self.request(3, "file", b"base", value, uuid.uuid4()) for value in (b"one", b"two")]
        with ThreadPoolExecutor(max_workers=2) as pool:
            replies = list(pool.map(self.exchange, requests))
        self.assertEqual(sorted(code for code, _ in replies), [0, 3])
        winner = b"one" if replies[0][0] == 0 else b"two"
        self.assertEqual((self.root / "file").read_bytes(), winner)

    def test_shared_file_lock_wait_is_bounded(self):
        (self.root / "file").write_bytes(b"saved")
        lock = self.root / ".firecrab-workspace/files.lock"
        fd = os.open(lock, os.O_WRONLY | os.O_CREAT, 0o600)
        try:
            fcntl.flock(fd, fcntl.LOCK_EX)
            self.assertEqual(self.exchange(self.request(2, "file"))[0], 4)
        finally:
            os.close(fd)
        self.assertEqual(self.exchange(self.request(2, "file")), (0, b"saved"))

    def test_limits_and_trailing_frames_are_rejected(self):
        large = self.request(3, "file", None, b"x" * (LIMIT + 1), uuid.uuid4())
        self.assertNotEqual(self.exchange(large)[0], 0)
        self.assertNotEqual(self.exchange(self.request(2, "file") + b"another command")[0], 0)
        (self.root / "large").write_bytes(b"x" * (LIMIT + 1))
        self.assertEqual(self.exchange(self.request(2, "large"))[0], 6)

    def test_wrong_project_marker_cannot_access_files(self):
        (self.root / "file").write_bytes(b"private")
        self.database = str(uuid.uuid4())
        self.assertEqual(self.exchange(self.request(2, "file"))[0], 8)

    def test_completed_write_can_recover_pending_receipt(self):
        operation = uuid.uuid4()
        frame = self.request(3, "file", None, b"saved", operation)
        self.assertEqual(self.exchange(frame)[0], 0)
        journal = self.root / ".firecrab-workspace/writes"
        (journal / f"{operation.hex}.done").rename(journal / f"{operation.hex}.pending")
        self.assertEqual(self.exchange(self.request(4, operation=operation))[0], 2)
        self.assertEqual(self.exchange(frame)[0], 0)
        self.assertEqual(self.exchange(self.request(4, operation=operation)), (0, frame))

    def test_unapplied_pending_write_checks_its_original_base(self):
        operation = uuid.uuid4()
        frame = self.request(3, "file", None, b"saved", operation)
        self.assertEqual(self.exchange(frame)[0], 0)
        journal = self.root / ".firecrab-workspace/writes"
        (journal / f"{operation.hex}.done").rename(journal / f"{operation.hex}.pending")
        (self.root / "file").write_bytes(b"different")
        self.assertEqual(self.exchange(frame)[0], 9)
        self.assertEqual((self.root / "file").read_bytes(), b"different")


if __name__ == "__main__":
    unittest.main()
