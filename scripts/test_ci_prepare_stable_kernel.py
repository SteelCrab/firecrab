"""Verify CI follows the stable API job and passes the verified image to VM QA."""
import contextlib
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import call, patch

spec = importlib.util.spec_from_file_location("prepare_kernel", Path(__file__).with_name("ci-prepare-stable-kernel.py"))
qa = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qa)


class StableKernelQaTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.images = self.root / "images"
        self.version = "8.1.4"
        self.image = self.images / ".oci/kernel/x86_64" / f"vmlinux-{self.version}-x86_64"
        self.image.parent.mkdir(parents=True)
        self.image.write_bytes(b"verified native kernel fixture")
        digest = hashlib.sha256(self.image.read_bytes()).hexdigest()
        self.entry = {"version": self.version, "architecture": "x86_64", "image": self.image.name,
                      "imageSha256": digest, "installed": True}
        self.receipt = {"version": self.version, "architecture": "x86_64", "image_sha256": digest,
                        "build_revision": "firecrab-host9"}
        self.receipt_path = Path(str(self.image) + ".json")
        self.receipt_path.write_text(json.dumps(self.receipt))
        self.api = "http://127.0.0.1:5523"

    def responses(self, *jobs):
        # Discovery and completion can straddle an upstream stable update.
        before = [dict(self.entry, installed=False)]
        after = [dict(self.entry, version="8.1.5", installed=False), self.entry]
        return [before, {"status": "running"}, *jobs, after]

    def test_current_stable_and_receipt_revision_reach_vm_environment(self):
        output = self.root / "qa/kernel.json"
        environment = self.root / "github-env"
        environment.write_text("EXISTING=value\n")
        jobs = [{"status": "running", "log": "building\n"}, {"status": "succeeded", "log": "building\nready\n"}]
        argv = ["prepare", "--api", self.api, "--images", str(self.images), "--output", str(output), "--github-env", str(environment)]
        with patch.object(qa, "request", side_effect=self.responses(*jobs)) as request, \
             patch.object(qa.time, "sleep"), patch("sys.argv", argv), contextlib.redirect_stdout(io.StringIO()) as log:
            qa.main()
        result = json.loads(output.read_text())
        self.assertEqual(result["kernel"], str(self.image))
        self.assertEqual(result["expectedRelease"], "8.1.4-firecrab-host9")
        self.assertIn(call(self.api, "/api/kernels/stable/install", "POST"), request.call_args_list)
        self.assertIn(f"FIRECRAB_CI_KERNEL_PATH={self.image}\n", environment.read_text())
        self.assertIn("FIRECRAB_CI_KERNEL_RELEASE=8.1.4-firecrab-host9\n", environment.read_text())
        self.assertTrue(environment.read_text().startswith("EXISTING=value\n"))
        self.assertEqual(log.getvalue().count("building"), 1)

    def test_failed_build_reports_the_actual_build_log(self):
        failed = {"status": "failed", "log": "Missing kernel build headers: libelf\n"}
        with patch.object(qa, "request", side_effect=self.responses(failed)), contextlib.redirect_stdout(io.StringIO()):
            with self.assertRaisesRegex(RuntimeError, "Missing kernel build headers: libelf"):
                qa.prepare(self.api, self.images, 60)

    def test_running_build_obeys_the_configured_deadline(self):
        clock = [0]
        def sleep(seconds):
            clock[0] += seconds
        with patch.object(qa, "request", side_effect=self.responses({"status": "running", "log": "building\n"})), \
             patch.object(qa.time, "monotonic", side_effect=lambda: clock[0]), patch.object(qa.time, "sleep", side_effect=sleep), \
             contextlib.redirect_stdout(io.StringIO()):
            with self.assertRaisesRegex(TimeoutError, "exceeded 1 seconds"):
                qa.prepare(self.api, self.images, 1)

    def test_corrupt_cached_image_cannot_reach_vm_qa(self):
        self.image.write_bytes(b"corrupt")
        with patch.object(qa, "request", side_effect=self.responses({"status": "succeeded"})), contextlib.redirect_stdout(io.StringIO()):
            with self.assertRaisesRegex(RuntimeError, "image digest does not match"):
                qa.prepare(self.api, self.images, 60)

    def test_legacy_catalog_is_not_mistaken_for_upstream_stable(self):
        with patch.object(qa, "request", return_value=[dict(self.entry, version="7.2.2-vsock1")]) as request:
            with self.assertRaisesRegex(RuntimeError, "upstream stable release"):
                qa.prepare(self.api, self.images, 60)
        request.assert_called_once()

    def test_foreign_receipt_is_rejected(self):
        self.receipt_path.write_text(json.dumps(dict(self.receipt, architecture="aarch64")))
        with patch.object(qa, "request", side_effect=self.responses({"status": "succeeded"})), contextlib.redirect_stdout(io.StringIO()):
            with self.assertRaisesRegex(RuntimeError, "receipt does not match"):
                qa.prepare(self.api, self.images, 60)


if __name__ == "__main__":
    unittest.main()
