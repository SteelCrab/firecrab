#!/usr/bin/env python3
"""Prepare a private, read-only OCI registry tree for the S3 MicroVM QA app.

Layers come from the official amd64 Python Alpine image. No image is published.
Install boto3 into --vendor first. Run --serve inside the isolated QA network.
"""
import argparse
import gzip
import hashlib
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
import io
import json
from pathlib import Path
import tarfile
from urllib.request import Request, urlopen


MANIFEST = "application/vnd.oci.image.manifest.v1+json"
ACCEPT = ", ".join([MANIFEST, "application/vnd.oci.image.index.v1+json",
    "application/vnd.docker.distribution.manifest.v2+json", "application/vnd.docker.distribution.manifest.list.v2+json"])


def prepare(output, vendor):
    token = json.load(urlopen("https://auth.docker.io/token?service=registry.docker.io&scope=repository:library/python:pull", timeout=30))["token"]
    def fetch(path):
        request = Request("https://registry-1.docker.io/v2/library/python/" + path,
            headers={"Authorization": "Bearer " + token, "Accept": ACCEPT})
        with urlopen(request, timeout=120) as response:
            return response.read()
    index = json.loads(fetch("manifests/3.13-alpine"))
    digest = next(item["digest"] for item in index["manifests"]
        if item["platform"].get("os") == "linux" and item["platform"].get("architecture") == "amd64")
    manifest = json.loads(fetch("manifests/" + digest))
    base = output / "v2/qa/s3"
    blobs = base / "blobs"
    blobs.mkdir(parents=True, exist_ok=True)
    for descriptor in [manifest["config"], *manifest["layers"]]:
        data = fetch("blobs/" + descriptor["digest"])
        assert "sha256:" + hashlib.sha256(data).hexdigest() == descriptor["digest"]
        (blobs / descriptor["digest"]).write_bytes(data)
    config = json.loads((blobs / manifest["config"]["digest"]).read_bytes())
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w") as archive:
        app = Path(__file__).with_name("fixtures") / "pipe-storage-guest.py"
        archive.add(app, arcname="opt/qa/app.py")
        for path in sorted(vendor.rglob("*")):
            if path.is_file() and "__pycache__" not in path.parts:
                archive.add(path, arcname="opt/qa/vendor/" + str(path.relative_to(vendor)))
    uncompressed = buffer.getvalue()
    layer = gzip.compress(uncompressed, mtime=0)
    def save(data, media_type):
        digest = "sha256:" + hashlib.sha256(data).hexdigest()
        (blobs / digest).write_bytes(data)
        return {"mediaType": media_type, "digest": digest, "size": len(data)}
    manifest["layers"].append(save(layer, "application/vnd.oci.image.layer.v1.tar+gzip"))
    config["rootfs"]["diff_ids"].append("sha256:" + hashlib.sha256(uncompressed).hexdigest())
    config["config"]["Entrypoint"] = ["/usr/local/bin/python3", "/opt/qa/app.py"]
    config["config"]["Cmd"] = []
    config["config"]["Env"].append("PYTHONPATH=/opt/qa/vendor")
    manifest["config"] = save(json.dumps(config).encode(), "application/vnd.oci.image.config.v1+json")
    manifest["mediaType"] = MANIFEST
    data = json.dumps(manifest).encode()
    digest = "sha256:" + hashlib.sha256(data).hexdigest()
    directory = base / "manifests"
    directory.mkdir(exist_ok=True)
    (directory / "latest").write_bytes(data)
    (directory / digest).write_bytes(data)
    print("Prepared private QA image", digest, flush=True)


class Registry(SimpleHTTPRequestHandler):
    def guess_type(self, path):
        return MANIFEST if "/manifests/" in path else "application/octet-stream"

    def log_message(self, *_args):
        pass


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--vendor", type=Path)
    parser.add_argument("--serve", action="store_true")
    args = parser.parse_args()
    if args.serve:
        import os
        os.chdir(args.output)
        ThreadingHTTPServer(("127.0.0.1", 15439), Registry).serve_forever()
    else:
        prepare(args.output, args.vendor)
