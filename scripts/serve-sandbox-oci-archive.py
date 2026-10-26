#!/usr/bin/env python3
"""Serve one owned Docker-save archive as a read-only loopback OCI registry.

No archive member is extracted to the host. The existing Firecrab OCI API still
downloads and verifies normal manifest/config/layer digests. This development
fixture has no remote registry connection, upload, authentication or deletion.
"""
import argparse
import hashlib
import gzip
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path, PurePosixPath
import re
import tarfile
from urllib.parse import unquote, urlsplit

MANIFEST_TYPE = "application/vnd.oci.image.manifest.v1+json"
CONFIG_TYPE = "application/vnd.oci.image.config.v1+json"
LAYER_TYPE = "application/vnd.oci.image.layer.v1.tar"


def encoded(value):
    return json.dumps(value, separators=(",", ":"), sort_keys=True).encode()


def member(archive, name, limit):
    if not isinstance(name, str) or name.startswith("/") or ".." in PurePosixPath(name).parts:
        raise ValueError("unsafe archive member name")
    entry = archive.getmember(name)
    if not entry.isfile() or entry.size > limit:
        raise ValueError("archive member must be a bounded regular file")
    return entry


class Image:
    def __init__(self, path, repository="firecrab/sandbox-seed"):
        if not re.fullmatch(r"[a-z0-9]+(?:[._/-][a-z0-9]+)*", repository) or len(repository) > 128:
            raise ValueError("invalid owned fixture repository")
        self.path = Path(path).resolve(strict=True)
        self.identity = self.signature()
        self.repository = repository
        self.blobs = {}
        with tarfile.open(self.path, mode="r:") as archive:
            saved = json.load(archive.extractfile(member(archive, "manifest.json", 4 * 1024 * 1024)))
            if not isinstance(saved, list) or len(saved) != 1:
                raise ValueError("docker save must contain exactly one selected image")
            image = saved[0]
            config_entry = member(archive, image["Config"], 4 * 1024 * 1024)
            config_bytes = archive.extractfile(config_entry).read()
            config = json.loads(config_bytes)
            self.architecture = config.get("architecture")
            if config.get("os") != "linux" or self.architecture not in ("arm64", "amd64"):
                raise ValueError("fixture requires a Linux amd64/arm64 image")
            config_digest = "sha256:" + hashlib.sha256(config_bytes).hexdigest()
            self.blobs[config_digest] = config_entry.name, config_entry.size
            layers = []
            diff_ids = []
            for name in image["Layers"]:
                entry = member(archive, name, 1024 * 1024 * 1024)
                digest = hashlib.sha256()
                with archive.extractfile(entry) as stream:
                    while chunk := stream.read(1024 * 1024):
                        digest.update(chunk)
                identity = "sha256:" + digest.hexdigest()
                self.blobs[identity] = entry.name, entry.size
                with archive.extractfile(entry) as stream:
                    magic=stream.read(4)
                media_type=LAYER_TYPE
                diff_id=identity
                if magic.startswith(b"\x1f\x8b"):
                    media_type += "+gzip"
                    decoded=hashlib.sha256();total=0
                    with archive.extractfile(entry) as stream:
                        with gzip.GzipFile(fileobj=stream) as plain:
                            while chunk:=plain.read(1024*1024):
                                total+=len(chunk)
                                if total>1024*1024*1024:raise ValueError("decoded fixture layer exceeds 1 GiB")
                                decoded.update(chunk)
                    diff_id="sha256:"+decoded.hexdigest()
                elif magic==b"\x28\xb5\x2f\xfd":
                    raise ValueError("zstd Docker-save layers are unsupported by this stdlib fixture; use gzip/plain layers")
                diff_ids.append(diff_id)
                layers.append({"mediaType": media_type, "size": entry.size, "digest": identity})
            if config.get("rootfs", {}).get("diff_ids") != diff_ids:
                raise ValueError("saved layer bytes do not match the image's uncompressed diff IDs")
            self.manifest = encoded({"schemaVersion": 2, "mediaType": MANIFEST_TYPE,
                                     "config": {"mediaType": CONFIG_TYPE, "size": config_entry.size, "digest": config_digest},
                                     "layers": layers})
            self.digest = "sha256:" + hashlib.sha256(self.manifest).hexdigest()

    def signature(self):
        status = self.path.stat()
        return status.st_dev, status.st_ino, status.st_size, status.st_mtime_ns

    def unchanged(self):
        return self.signature() == self.identity


class RegistryHandler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_GET(self):
        self.respond(True)

    def do_HEAD(self):
        self.respond(False)

    def respond(self, body):
        image = self.server.image
        if not image.unchanged():
            self.send_error(503, "owned archive changed")
            return
        request = urlsplit(self.path)
        path = unquote(request.path)
        prefix = f"/v2/{image.repository}/"
        data, content_type, digest = None, "application/json", None
        blob = None
        if request.query:
            self.send_error(404)
            return
        if path in ("/v2/", "/v2"):
            data = b"{}"
        elif path in (prefix + "manifests/qa", prefix + "manifests/" + image.digest):
            data, content_type, digest = image.manifest, MANIFEST_TYPE, image.digest
        elif path == prefix + "tags/list":
            data = encoded({"name": image.repository, "tags": ["qa"]})
        elif path.startswith(prefix + "blobs/"):
            digest = path.removeprefix(prefix + "blobs/")
            blob = image.blobs.get(digest)
            content_type = "application/octet-stream"
        if data is None and blob is None:
            self.send_error(404)
            return
        size = len(data) if data is not None else blob[1]
        self.send_response(200)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(size))
        self.send_header("Docker-Distribution-API-Version", "registry/2.0")
        if digest:
            self.send_header("Docker-Content-Digest", digest)
        self.end_headers()
        if not body:
            return
        try:
            if data is not None:
                self.wfile.write(data)
            else:
                with tarfile.open(image.path, mode="r:") as archive:
                    with archive.extractfile(blob[0]) as stream:
                        while chunk := stream.read(1024 * 1024):
                            self.wfile.write(chunk)
        except (BrokenPipeError, ConnectionResetError):
            return


class Registry(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, image, port=0):
        self.image = image
        super().__init__(("127.0.0.1", port), RegistryHandler)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("archive", type=Path, help="owned uncompressed docker save archive")
    parser.add_argument("--repository", default="firecrab/sandbox-seed")
    parser.add_argument("--port", type=int, default=0)
    args = parser.parse_args()
    image = Image(args.archive, args.repository)
    with Registry(image, args.port) as registry:
        reference = f"127.0.0.1:{registry.server_port}/{image.repository}@{image.digest}"
        print(json.dumps({"kind": "owned_loopback_oci_fixture", "reference": reference,
                          "digest": image.digest, "architecture": image.architecture,
                          "archiveBytes": image.identity[2], "manifestBytes": len(image.manifest)}, separators=(",", ":")), flush=True)
        registry.serve_forever()


if __name__ == "__main__":
    main()
