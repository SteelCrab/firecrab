#!/usr/bin/env python3
"""Queue QA target guards and failure handling; no VM or network changes."""
import importlib.util
import json
import sqlite3
import subprocess
import tempfile
import uuid
from pathlib import Path
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("queue_qa", Path(__file__).with_name("ci-qa-queue.py"))
qa = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qa)


class QueueQaContracts(unittest.TestCase):
    def test_only_explicit_local_http_origins_are_accepted(self):
        for value in ["https://127.0.0.1:5523", "http://remote:5523", "http://user:pass@localhost:5523",
                      "http://localhost", "http://localhost:5523/api", "http://localhost:5523/?token=x"]:
            with self.subTest(value=value), self.assertRaises(ValueError):
                qa.local_origin(value)
        self.assertEqual(qa.local_origin("http://127.0.0.1:15523/"), "http://127.0.0.1:15523")

    def test_existing_engine_is_refused_before_any_mutation(self):
        with patch.object(qa, "api", return_value={"vmId": "existing"}) as api:
            with self.assertRaisesRegex(RuntimeError, "no existing engine"):
                qa.run(None, {"checks": []})
        api.assert_called_once_with("/api/queue/engine")

    def test_permanent_engine_failure_does_not_wait_twenty_minutes(self):
        with patch.object(qa, "api", return_value={"phase": "failed", "error": "Image import failed"}):
            with self.assertRaisesRegex(qa.EngineFailed, "Image import failed"):
                qa.wait("engine boot", lambda: qa.engine_phase("running"))

    def test_disk_probe_reads_identity_without_returning_private_columns(self):
        with tempfile.TemporaryDirectory() as root:
            database = Path(root) / "state.db"
            vm_id, generation = uuid.uuid4(), uuid.uuid4()
            with sqlite3.connect(database) as db:
                db.execute("CREATE TABLE vms(id TEXT, storage_root TEXT, disk_generation TEXT, purpose TEXT, env TEXT)")
                db.execute("INSERT INTO vms VALUES(?,?,?,?,?)", (str(vm_id), "default", str(generation), "queue", "private-value"))
            disk = database.parent / "vms" / vm_id.hex / "d" / (generation.hex + ".ext4")
            disk.parent.mkdir(parents=True)
            disk.write_bytes(b"persistent-data")
            run = subprocess.run
            with patch.object(qa.subprocess, "run", side_effect=lambda command, **kw: run(command[2:], **kw)):
                identity = qa.disk_identity(database, str(vm_id))
                self.assertEqual(identity, {"generation": str(generation), "bytes": 15})
                self.assertNotIn("private", json.dumps(identity))
                disk.unlink()
                with self.assertRaisesRegex(RuntimeError, "disk identity"):
                    qa.disk_identity(database, str(vm_id))

    def test_http_redirects_are_not_followed(self):
        self.assertIsNone(qa.NoRedirect().redirect_request(None, None, 302, "redirect", {}, "http://remote"))


if __name__ == "__main__":
    unittest.main()
