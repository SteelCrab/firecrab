#!/usr/bin/env python3
"""Check guest QA's kernel identity against registry and native receipts."""
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location(
    "prepare_kernel", Path(__file__).with_name("ci-prepare-stable-kernel.py")
)
prepare_kernel = importlib.util.module_from_spec(spec)
spec.loader.exec_module(prepare_kernel)


class KernelIdentityTests(unittest.TestCase):
    def prepare(self, registry, *, catalog_digest=None, receipt_version="7.3.1"):
        with tempfile.TemporaryDirectory() as temporary:
            images = Path(temporary)
            directory = images / ".oci/kernel/x86_64"
            directory.mkdir(parents=True)
            kernel = directory / "vmlinux-7.3.1-x86_64"
            kernel.write_bytes(b"kernel identity fixture")
            digest = hashlib.sha256(kernel.read_bytes()).hexdigest()
            receipt = {
                "version": receipt_version,
                "architecture": "x86_64",
                "build_revision": "firecrab-host1",
                "image_sha256": digest,
            }
            if registry:
                receipt["package_url"] = "https://registry.firecrab.dev/kernel/7.3.1/r2/x86_64/vmlinux-7.3.1.tar.zst"
                receipt["package_sha256"] = "a" * 64
            Path(str(kernel) + ".json").write_text(json.dumps(receipt))
            entry = {
                "version": "7.3.1",
                "architecture": "x86_64",
                "image": kernel.name,
                "imageSha256": catalog_digest or digest,
                "installed": True,
            }
            with patch.object(prepare_kernel, "request", side_effect=[
                [entry], {}, {"status": "succeeded", "log": ""}, [entry]
            ]):
                return prepare_kernel.prepare("http://localhost", images, 1)

    def test_registry_cache_revision_is_not_a_kernel_release_suffix(self):
        result = self.prepare(True)
        self.assertEqual(result["expectedRelease"], "7.3.1")

    def test_native_receipts_keep_their_release_suffix(self):
        result = self.prepare(False)
        self.assertEqual(result["expectedRelease"], "7.3.1-firecrab-host1")

    def test_catalog_digest_mismatch_is_rejected(self):
        with self.assertRaisesRegex(RuntimeError, "digest does not match"):
            self.prepare(True, catalog_digest="0" * 64)

    def test_receipt_for_another_release_is_rejected(self):
        with self.assertRaisesRegex(RuntimeError, "does not match the selected release"):
            self.prepare(True, receipt_version="7.2.15")


if __name__ == "__main__":
    unittest.main()
