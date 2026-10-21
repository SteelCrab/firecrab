import hashlib
import importlib.util
from pathlib import Path
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("prepare_cli_release", ROOT / "scripts/prepare-cli-release.py")
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


class PackagePins(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.manifest = self.root / "micromanager.toml"
        self.original = (ROOT / "packaging/micromanager.toml").read_text()
        self.manifest.write_bytes(self.original.replace("\n", "\r\n").encode())
        self.installer = self.root / "install.sh"
        self.installer.write_bytes(b"#!/bin/bash\n# exact published installer\n")
        for arch in ("aarch64", "x86_64"):
            (self.root / f"firecrab-host-{arch}-gnu.tar.gz").write_bytes(arch.encode())

    def test_exact_release_artifacts_are_embedded_and_other_pins_are_preserved(self):
        release.prepare(self.manifest, self.root, self.installer, "v0.3.2")
        result = tomllib.loads(self.manifest.read_text())
        original = tomllib.loads(self.original)
        self.assertEqual(result["firecrab"]["version"], "v0.3.2")
        for name, path, asset in [
            ("installer", self.installer, "install.sh"),
            *[(arch, self.root / f"firecrab-host-{arch}-gnu.tar.gz", f"firecrab-host-{arch}-gnu.tar.gz") for arch in ("aarch64", "x86_64")],
        ]:
            pin = result["firecrab"][name]
            self.assertEqual(pin["url"], f"https://github.com/SteelCrab/firecrab/releases/download/v0.3.2/{asset}")
            self.assertEqual(pin["digest"], hashlib.sha256(path.read_bytes()).hexdigest())
        self.assertEqual(result["firecrab"]["installer"]["filename"], "install-firecrab-v0.3.2.sh")
        for key in original.keys() - {"firecrab"}:
            self.assertEqual(result[key], original[key])
        self.assertNotIn(b"\r\n", self.manifest.read_bytes())

    def test_missing_host_bundle_does_not_publish_partial_pins(self):
        before = self.manifest.read_bytes()
        (self.root / "firecrab-host-x86_64-gnu.tar.gz").unlink()
        with self.assertRaisesRegex(ValueError, "missing or empty"):
            release.prepare(self.manifest, self.root, self.installer, "v0.3.2")
        self.assertEqual(self.manifest.read_bytes(), before)

    def test_missing_manifest_fields_and_invalid_tags_are_rejected(self):
        with self.assertRaisesRegex(ValueError, "release tag"):
            release.prepare(self.manifest, self.root, self.installer, "latest")
        self.manifest.write_text(self.original.replace('[firecrab.x86_64]', '[missing.x86_64]'))
        with self.assertRaisesRegex(ValueError, "missing release fields"):
            release.prepare(self.manifest, self.root, self.installer, "v0.3.2")


if __name__ == "__main__":
    unittest.main()
