#!/usr/bin/env python3
"""Build tiny QA engine images from a SHA-verified public busybox-static package.

Operator preparation only. This script never installs packages, creates VMs,
starts Docker, changes networking, or admits model tasks. `serve` binds loopback
and implements read-only Registry V2 routes for an explicit operator pull.
"""
import argparse
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import tarfile
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlsplit


def digest(data):
    return hashlib.sha256(data).hexdigest()


def encoded(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode()


def build(args):
    manifest = json.loads(args.bundle.read_text())
    package = next(p for p in manifest["packages"] if p["package"] == "busybox-static")
    if package["architecture"] != "arm64" or digest(args.deb.read_bytes()) != package["sha256"]:
        raise ValueError("The exact arm64 busybox-static package must match the verified bundle")
    content = subprocess.check_output(["dpkg-deb", "--fsys-tarfile", str(args.deb)])
    with tarfile.open(fileobj=io.BytesIO(content), mode="r:") as archive:
        member = next(m for m in archive if m.name.removeprefix("./") in ("bin/busybox", "usr/bin/busybox"))
        if not member.isfile() or not 100_000 < member.size < 10_000_000:
            raise ValueError("Unexpected busybox payload")
        binary = archive.extractfile(member).read()
    if binary[:6] != b"\x7fELF\x02\x01" or binary[18:20] != b"\xb7\x00":
        raise ValueError("The rootfs requires a Linux AArch64 ELF executable")
    args.output.mkdir(mode=0o700)
    rootfs = io.BytesIO()
    with tarfile.open(fileobj=rootfs, mode="w:", format=tarfile.USTAR_FORMAT) as archive:
        def add(name, payload=b"", mode=0o644, kind=tarfile.REGTYPE, link=""):
            entry = tarfile.TarInfo(name)
            entry.mode, entry.uid, entry.gid, entry.mtime = mode, 0, 0, 0
            entry.uname, entry.gname, entry.type, entry.linkname = "root", "root", kind, link
            entry.size = len(payload) if kind == tarfile.REGTYPE else 0
            archive.addfile(entry, io.BytesIO(payload) if entry.size else None)
        for name in ["bin", "dev", "etc", "firecrab", "proc", "run", "sys", "usr", "usr/bin", "workspace"]:
            add(name, mode=0o755, kind=tarfile.DIRTYPE)
        add("tmp", mode=0o1777, kind=tarfile.DIRTYPE)
        add("bin/busybox", binary, 0o755)
        applets = "sh awk base64 cat chmod chown cut date env false find grep head hostname id kill ls mkdir printf ps pwd readlink sed sha256sum sleep stat tee test timeout touch tr true uname wc"
        for name in applets.split():
            add(f"bin/{name}", mode=0o777, kind=tarfile.SYMTYPE, link="busybox")
        add("etc/passwd", b"root:x:0:0:root:/root:/bin/sh\n")
        add("etc/group", b"root:x:0:\n")
        add("etc/os-release", b'ID=firecrab-qa\nPRETTY_NAME="Firecrab QA BusyBox"\nVERSION_ID=1\n')
        add("etc/machine-id")
        add("etc/resolv.conf")
    layer = rootfs.getvalue()
    root_digest = digest(layer)
    root_path = args.output / f"{root_digest}.tar"
    root_path.write_bytes(layer)
    compressed = io.BytesIO()
    with gzip.GzipFile(fileobj=compressed, filename="", mode="wb", mtime=0) as stream:
        stream.write(layer)
    layer_gzip = compressed.getvalue()
    config = encoded({
        "architecture": "arm64", "os": "linux", "created": "1970-01-01T00:00:00Z",
        "config": {"Entrypoint": ["/bin/sh"], "User": "65532:65532", "WorkingDir": "/workspace",
                   "Env": ["PATH=/usr/local/bin:/usr/bin:/bin"],
                   "Labels": {"io.firecrab.qa.package": package["version"], "io.firecrab.qa.package-sha256": package["sha256"]}},
        "rootfs": {"type": "layers", "diff_ids": [f"sha256:{root_digest}"]},
    })
    config_digest, layer_digest = digest(config), digest(layer_gzip)
    image = encoded({
        "schemaVersion": 2, "mediaType": "application/vnd.docker.distribution.manifest.v2+json",
        "config": {"mediaType": "application/vnd.docker.container.image.v1+json", "size": len(config), "digest": f"sha256:{config_digest}"},
        "layers": [{"mediaType": "application/vnd.docker.image.rootfs.diff.tar.gzip", "size": len(layer_gzip), "digest": f"sha256:{layer_digest}"}],
    })
    image_digest = digest(image)
    routes = {}
    for suffix, data, mime in [
        (f"blobs/sha256:{config_digest}", config, "application/octet-stream"),
        (f"blobs/sha256:{layer_digest}", layer_gzip, "application/octet-stream"),
        (f"manifests/sha256:{image_digest}", image, "application/vnd.docker.distribution.manifest.v2+json"),
    ]:
        name = f"{digest(data)}.blob"
        (args.output / name).write_bytes(data)
        routes[f"/v2/sandbox414/busybox/{suffix}"] = {"file": name, "sha256": digest(data), "mime": mime}
    result = {
        "package": package, "archiveSigningFingerprint": manifest["signingFingerprint"],
        "nspawnImage": f"sha256:{root_digest}", "nspawnArchive": root_path.name,
        "dockerImage": f"127.0.0.1:{args.port}/sandbox414/busybox@sha256:{image_digest}",
        "dockerManifestSha256": image_digest, "routes": routes,
    }
    (args.output / "images.json").write_bytes(encoded(result) + b"\n")
    print(json.dumps({k: v for k, v in result.items() if k != "routes"}, indent=2))


def serve(args):
    root = args.root.resolve(strict=True)
    metadata = json.loads((root / "images.json").read_text())
    routes = metadata["routes"]
    if not metadata["dockerImage"].startswith(f"127.0.0.1:{args.port}/"):
        raise ValueError("Serve the exact loopback origin pinned in images.json")

    class Registry(BaseHTTPRequestHandler):
        def do_HEAD(self):
            self.respond(False)

        def do_GET(self):
            self.respond(True)

        def respond(self, body):
            path = urlsplit(self.path).path
            if path == "/v2/":
                data, mime = b"{}", "application/json"
            elif path in routes:
                route = routes[path]
                file = root / route["file"]
                if file.parent != root or file.is_symlink():
                    self.send_error(500)
                    return
                data, mime = file.read_bytes(), route["mime"]
                if digest(data) != route["sha256"]:
                    self.send_error(500)
                    return
            else:
                self.send_error(404)
                return
            self.send_response(200)
            self.send_header("Docker-Distribution-Api-Version", "registry/2.0")
            self.send_header("Content-Type", mime)
            self.send_header("Content-Length", str(len(data)))
            self.send_header("Docker-Content-Digest", f"sha256:{digest(data)}")
            self.end_headers()
            if body:
                self.wfile.write(data)

    print(f"Read-only QA registry on 127.0.0.1:{args.port}; stop it after the operator pull", flush=True)
    ThreadingHTTPServer(("127.0.0.1", args.port), Registry).serve_forever()


def main():
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    builder = sub.add_parser("build")
    builder.add_argument("--bundle", type=Path, required=True)
    builder.add_argument("--deb", type=Path, required=True)
    builder.add_argument("--output", type=Path, required=True)
    builder.add_argument("--port", type=int, default=24144)
    server = sub.add_parser("serve")
    server.add_argument("--root", type=Path, required=True)
    server.add_argument("--port", type=int, default=24144)
    args = parser.parse_args()
    if not 1024 <= args.port <= 65535:
        parser.error("Use an unprivileged loopback port")
    build(args) if args.command == "build" else serve(args)


if __name__ == "__main__":
    main()
