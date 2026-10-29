#!/usr/bin/env python3
"""Verify identical seeds and independent fixture writes without creating a VM."""
import importlib.util
import hashlib
import json
from pathlib import Path
import tempfile
import threading
import unittest
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("seed_fixture", ROOT / "scripts/fixtures/sandbox-environment-seed.py")
FIXTURE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(FIXTURE)


class EnvironmentSeed(unittest.TestCase):
    def test_manifest_matches_script_and_canonical_seed(self):
        manifest=json.loads((ROOT/'scripts/fixtures/sandbox-environment-seed.json').read_text())
        self.assertEqual(manifest['scriptSha256'],hashlib.sha256((ROOT/'scripts/fixtures/sandbox-environment-seed.py').read_bytes()).hexdigest())
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/'seed.sqlite';FIXTURE.initialize(path)
            seed=FIXTURE.database_request(path,{'operation':'read'})
            self.assertEqual(manifest['seedSha256'],hashlib.sha256(json.dumps(seed,sort_keys=True,separators=(',',':')).encode()).hexdigest())

    def test_same_seed_different_databases_have_independent_writes(self):
        with tempfile.TemporaryDirectory() as directory:
            left, right = [Path(directory) / name for name in ("left.sqlite", "right.sqlite")]
            for path in (left, right):
                FIXTURE.initialize(path)
            initial = FIXTURE.database_request(left, {"operation": "read"})
            self.assertEqual(initial, FIXTURE.database_request(right, {"operation": "read"}))
            FIXTURE.database_request(left, {"operation": "write", "key": "deployment", "value": "left"})
            FIXTURE.database_request(right, {"operation": "write", "key": "deployment", "value": "right"})
            self.assertEqual(FIXTURE.database_request(left, {"operation": "read"})["values"], {"deployment": "left"})
            self.assertEqual(FIXTURE.database_request(right, {"operation": "read"})["values"], {"deployment": "right"})

    def test_tcp_database_and_http_backend_retain_actual_write(self):
        with tempfile.TemporaryDirectory() as directory:
            database = FIXTURE.DatabaseServer(("127.0.0.1", 0), Path(directory) / "state.sqlite")
            backend = FIXTURE.BackendServer(("127.0.0.1", 0), "127.0.0.1", database.server_address[1])
            for server in (database, backend):
                threading.Thread(target=server.serve_forever, daemon=True).start()
            try:
                base = f"http://127.0.0.1:{backend.server_port}"
                with urllib.request.urlopen(base + "/seed", timeout=3) as response:
                    self.assertEqual(json.load(response)["values"], {})
                request = urllib.request.Request(base + "/write", data=b'{"key":"proof","value":"retained"}', headers={"Content-Type": "application/json"}, method="POST")
                with urllib.request.urlopen(request, timeout=3) as response:
                    self.assertEqual(json.load(response)["values"], {"proof": "retained"})
                self.assertEqual(FIXTURE.remote_database("127.0.0.1", database.server_address[1], {"operation": "read"})["values"], {"proof": "retained"})
            finally:
                for server in (backend, database):
                    server.shutdown()
                    server.server_close()


if __name__ == "__main__":
    unittest.main()
