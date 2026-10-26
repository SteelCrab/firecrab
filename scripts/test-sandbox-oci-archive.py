#!/usr/bin/env python3
"""Test the read-only same-host registry fixture with a tiny synthetic image."""
import hashlib
import gzip
import importlib.util
import io
import json
from pathlib import Path
import tarfile
import tempfile
import threading
import unittest
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("oci_fixture", ROOT / "scripts/serve-sandbox-oci-archive.py")
FIXTURE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(FIXTURE)


def image_archive(path, unsafe=False, compressed=False):
    layer = io.BytesIO()
    with tarfile.open(fileobj=layer, mode="w") as archive:
        entry = tarfile.TarInfo("seed.txt")
        entry.size = 4
        archive.addfile(entry, io.BytesIO(b"seed"))
    layer = layer.getvalue()
    config = FIXTURE.encoded({"os": "linux", "architecture": "arm64", "rootfs": {"type": "layers", "diff_ids": ["sha256:" + hashlib.sha256(layer).hexdigest()]}, "config": {"Cmd": ["/bin/true"]}})
    if compressed:layer=gzip.compress(layer,mtime=0)
    saved = FIXTURE.encoded([{"Config": "config.json", "Layers": ["../outside" if unsafe else "layer.tar"]}])
    with tarfile.open(path, mode="w") as archive:
        for name, content in {"manifest.json": saved, "config.json": config, "layer.tar": layer}.items():
            entry = tarfile.TarInfo(name)
            entry.size = len(content)
            archive.addfile(entry, io.BytesIO(content))


class OciArchive(unittest.TestCase):
    def test_modern_docker_save_gzip_layers_keep_verified_plain_diff_ids(self):
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/'image.tar';image_archive(path,compressed=True)
            image=FIXTURE.Image(path)
            self.assertEqual(json.loads(image.manifest)['layers'][0]['mediaType'],FIXTURE.LAYER_TYPE+'+gzip')
    def test_manifest_and_streamed_blob_match_their_content_digests(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "image.tar"
            image_archive(path)
            image = FIXTURE.Image(path)
            server = FIXTURE.Registry(image)
            threading.Thread(target=server.serve_forever, daemon=True).start()
            try:
                base = f"http://127.0.0.1:{server.server_port}/v2/{image.repository}"
                with urllib.request.urlopen(base + "/manifests/" + image.digest) as response:
                    data = response.read()
                    self.assertEqual(response.headers["Docker-Content-Digest"], "sha256:" + hashlib.sha256(data).hexdigest())
                    manifest = json.loads(data)
                for descriptor in [manifest["config"], *manifest["layers"]]:
                    with urllib.request.urlopen(base + "/blobs/" + descriptor["digest"]) as response:
                        data = response.read()
                        self.assertEqual(len(data), descriptor["size"])
                        self.assertEqual("sha256:" + hashlib.sha256(data).hexdigest(), descriptor["digest"])
                with self.assertRaises(urllib.error.HTTPError):
                    urllib.request.urlopen(base + "/blobs/../../config.json")
            finally:
                server.shutdown()
                server.server_close()
            self.assertEqual(sorted(Path(directory).iterdir()), [path])

    def test_archive_paths_are_not_extracted_or_followed(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "image.tar"
            image_archive(path, unsafe=True)
            with self.assertRaises(ValueError):
                FIXTURE.Image(path)
            self.assertEqual(sorted(Path(directory).iterdir()), [path])


if __name__ == "__main__":
    unittest.main()
