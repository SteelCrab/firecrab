#!/usr/bin/env python3
"""Isolated loopback OCI registry for import tests and the browser E2E.

Serves one tiny Linux image for this host (amd64 or arm64) on 127.0.0.1.
Nothing is pulled from Docker Hub. Stop the process (SIGINT/SIGTERM) or
leave the context manager to close the listener and delete scratch blobs.

Usage:
    python3 scripts/oci-e2e-registry.py
    python3 scripts/oci-e2e-registry.py --port 15555

The first stdout line is JSON with the reference testers type:

    127.0.0.1:<port>/firecrab/e2e:ready
"""

from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import json
import os
import re
import signal
import subprocess
import sys
import tarfile
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Optional
from urllib.parse import unquote

READY_SENTINEL = "FIRECRAB_OCI_E2E_READY"
REPOSITORY = "firecrab/e2e"
TAG = "ready"
ENTRYPOINT = ["/etc/firecrab/busybox", "sh", "-c"]
MANIFEST_TYPE = "application/vnd.oci.image.manifest.v1+json"
CONFIG_TYPE = "application/vnd.oci.image.config.v1+json"
LAYER_TYPE = "application/vnd.oci.image.layer.v1.tar+gzip"
READY_COMMAND = (
    f"while true; do echo {READY_SENTINEL}; /etc/firecrab/busybox sleep 2; done"
)
OPENSSH_PROGRAMS = (Path("/usr/sbin/sshd"), Path("/usr/bin/ssh-keygen"))
OPENSSH_HELPER_DIRS = (Path("/usr/lib/openssh"), Path("/usr/libexec/openssh"))


def host_oci_arch() -> str:
    machine = os.uname().machine.lower()
    if machine in ("x86_64", "amd64"):
        return "amd64"
    if machine in ("aarch64", "arm64"):
        return "arm64"
    raise RuntimeError(f"unsupported OCI SSH E2E host architecture: {machine}")


def sha256_digest(data: bytes) -> str:
    return "sha256:" + hashlib.sha256(data).hexdigest()


def descriptor(media_type: str, digest: str, size: int) -> dict:
    return {"mediaType": media_type, "digest": digest, "size": size}


def dynamic_runtime_files(program: Path) -> set[Path]:
    if not program.is_file():
        raise RuntimeError(f"OCI SSH E2E requires {program}")
    completed = subprocess.run(
        ["ldd", str(program)],
        check=True,
        capture_output=True,
        text=True,
    )
    files = {program}
    for line in completed.stdout.splitlines():
        match = re.search(r"(?:=>\s*)?(/[^\s(]+)", line)
        if match:
            files.add(Path(match.group(1)))
    return files


def openssh_programs() -> set[Path]:
    programs = set(OPENSSH_PROGRAMS)
    for directory in OPENSSH_HELPER_DIRS:
        if directory.is_dir():
            programs.update(
                path
                for path in directory.glob("sshd-*")
                if path.is_file() and os.access(path, os.X_OK)
            )
    return programs


def add_directory(archive: tarfile.TarFile, guest_path: Path) -> None:
    member = tarfile.TarInfo(str(guest_path).lstrip("/") + "/")
    member.type = tarfile.DIRTYPE
    member.mode = 0o755
    member.uid = 0
    member.gid = 0
    archive.addfile(member)


def add_regular_file(
    archive: tarfile.TarFile,
    guest_path: Path,
    payload: bytes,
    mode: int,
) -> None:
    member = tarfile.TarInfo(str(guest_path).lstrip("/"))
    member.size = len(payload)
    member.mode = mode
    member.uid = 0
    member.gid = 0
    archive.addfile(member, io.BytesIO(payload))


def build_layer_tar() -> bytes:
    runtime_files: set[Path] = set()
    for program in openssh_programs():
        runtime_files.update(dynamic_runtime_files(program))
    directories = {Path("/etc")}
    for guest_path in runtime_files:
        directories.update(guest_path.parents)
    directories.discard(Path("/"))

    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w") as archive:
        for directory in sorted(directories, key=lambda path: (len(path.parts), str(path))):
            add_directory(archive, directory)

        for guest_path in sorted(runtime_files, key=str):
            add_regular_file(
                archive,
                guest_path,
                guest_path.read_bytes(),
                guest_path.stat().st_mode & 0o777,
            )

        add_regular_file(
            archive,
            Path("/etc/passwd"),
            b"root:x:0:0:root:/root:/usr/bin/sh\n"
            b"sshd:x:74:74:Privilege-separated SSH:/run/sshd:/usr/sbin/nologin\n",
            0o644,
        )
        add_regular_file(
            archive,
            Path("/etc/group"),
            b"root:x:0:\nsshd:x:74:\n",
            0o644,
        )
        payload = f"{READY_SENTINEL}\n".encode()
        add_regular_file(archive, Path("/etc/firecrab-e2e"), payload, 0o644)
    return buffer.getvalue()


class LocalOciRegistry:
    """In-process distribution v2 server. Safe to use as a context manager."""

    def __init__(self, port: int = 0) -> None:
        self._port = port
        self._httpd: Optional[ThreadingHTTPServer] = None
        self._thread: Optional[threading.Thread] = None
        self._scratch: Optional[tempfile.TemporaryDirectory[str]] = None
        self.reference = ""
        self.alias = ""
        self.architecture = host_oci_arch()

    def start(self) -> "LocalOciRegistry":
        scratch = tempfile.TemporaryDirectory(prefix="firecrab-oci-e2e-")
        image = _Image.materialize(Path(scratch.name), self.architecture)
        handler = _handler_for(image)
        httpd = _LoopbackServer(("127.0.0.1", self._port), handler)
        thread = threading.Thread(target=httpd.serve_forever, daemon=True)
        thread.start()
        port = httpd.server_address[1]
        self._scratch = scratch
        self._httpd = httpd
        self._thread = thread
        self.reference = f"127.0.0.1:{port}/{REPOSITORY}:{TAG}"
        self.alias = f"127.0.0.1-{port}-firecrab-e2e-ready"
        return self

    def stop(self) -> None:
        if self._httpd is not None:
            self._httpd.shutdown()
            self._httpd.server_close()
            self._httpd = None
        if self._thread is not None:
            self._thread.join(timeout=2)
            self._thread = None
        if self._scratch is not None:
            self._scratch.cleanup()
            self._scratch = None

    def __enter__(self) -> "LocalOciRegistry":
        return self.start()

    def __exit__(self, exc_type, exc, tb) -> None:
        self.stop()

    def announcement(self) -> dict[str, str]:
        return {
            "reference": self.reference,
            "ready": READY_SENTINEL,
            "alias": self.alias,
            "architecture": self.architecture,
        }


class _Image:
    def __init__(
        self,
        manifest: bytes,
        manifest_digest: str,
        blobs: dict[str, bytes],
    ) -> None:
        self.manifest = manifest
        self.manifest_digest = manifest_digest
        self.blobs = blobs

    @classmethod
    def materialize(cls, scratch: Path, architecture: str) -> "_Image":
        layer_tar = build_layer_tar()
        (scratch / "layer.tar").write_bytes(layer_tar)
        compressed = gzip.compress(layer_tar)
        (scratch / "layer.tar.gz").write_bytes(compressed)
        config = json.dumps(
            {
                "architecture": architecture,
                "os": "linux",
                "config": {"Entrypoint": ENTRYPOINT, "Cmd": [READY_COMMAND]},
                "rootfs": {
                    "type": "layers",
                    "diff_ids": [sha256_digest(layer_tar)],
                },
            },
            separators=(",", ":"),
        ).encode()
        (scratch / "config.json").write_bytes(config)
        config_digest = sha256_digest(config)
        layer_digest = sha256_digest(compressed)
        manifest = json.dumps(
            {
                "schemaVersion": 2,
                "mediaType": MANIFEST_TYPE,
                "config": descriptor(CONFIG_TYPE, config_digest, len(config)),
                "layers": [descriptor(LAYER_TYPE, layer_digest, len(compressed))],
            },
            separators=(",", ":"),
        ).encode()
        (scratch / "manifest.json").write_bytes(manifest)
        return cls(
            manifest,
            sha256_digest(manifest),
            {config_digest: config, layer_digest: compressed},
        )


class _LoopbackServer(ThreadingHTTPServer):
    allow_reuse_address = True
    daemon_threads = True


def _handler_for(image: _Image):
    prefix = f"/v2/{REPOSITORY}"

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, format: str, *args) -> None:
            return

        def do_GET(self) -> None:
            path = unquote(self.path.split("?", 1)[0])
            if path in ("/v2", "/v2/"):
                self._send(200, b"{}", "application/json", extra={
                    "Docker-Distribution-API-Version": "registry/2.0",
                })
                return
            if path.startswith(f"{prefix}/manifests/"):
                selector = path.rsplit("/", 1)[-1]
                if selector in (TAG, image.manifest_digest):
                    self._send(
                        200,
                        image.manifest,
                        MANIFEST_TYPE,
                        extra={"Docker-Content-Digest": image.manifest_digest},
                    )
                    return
            if path.startswith(f"{prefix}/blobs/"):
                digest = path.rsplit("/", 1)[-1]
                blob = image.blobs.get(digest)
                if blob is not None:
                    self._send(
                        200,
                        blob,
                        "application/octet-stream",
                        extra={"Docker-Content-Digest": digest},
                    )
                    return
            self.send_error(404, "not found")

        def _send(
            self,
            status: int,
            body: bytes,
            content_type: str,
            extra: Optional[dict[str, str]] = None,
        ) -> None:
            self.send_response(status)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(body)))
            for key, value in (extra or {}).items():
                self.send_header(key, value)
            self.end_headers()
            self.wfile.write(body)

    return Handler


def main(argv: Optional[list[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--port",
        type=int,
        default=int(os.environ.get("FIRECRAB_OCI_E2E_PORT", "0")),
        help="loopback port (0 = ephemeral, also FIRECRAB_OCI_E2E_PORT)",
    )
    args = parser.parse_args(argv)
    registry = LocalOciRegistry(port=args.port)
    registry.start()
    stop = threading.Event()

    def request_stop(_signum=None, _frame=None) -> None:
        stop.set()

    signal.signal(signal.SIGINT, request_stop)
    signal.signal(signal.SIGTERM, request_stop)
    try:
        print(json.dumps(registry.announcement()), flush=True)
        print(
            f"type this reference: {registry.reference}",
            file=sys.stderr,
            flush=True,
        )
        stop.wait()
    finally:
        registry.stop()
    return 0


if __name__ == "__main__":
    sys.exit(main())
