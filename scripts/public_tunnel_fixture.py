"""Build offline OCI fixtures for the disposable public Tunnel QA app."""

import gzip
import hashlib
import io
import json
import struct
import tarfile


def digest(data):
    return "sha256:" + hashlib.sha256(data).hexdigest()


def static_elf_arch(binary):
    """Refuse non-Linux-format, unsupported, or dynamically linked executables."""
    if len(binary) < 64 or binary[:6] != b"\x7fELF\x02\x01":
        raise ValueError("fixture must be a 64-bit little-endian ELF executable")
    kind, machine = struct.unpack_from("<HH", binary, 16)
    architecture = {62: "amd64", 183: "arm64"}.get(machine)
    if kind not in (2, 3) or not architecture:
        raise ValueError("fixture must be an x86_64 or aarch64 executable")
    offset = struct.unpack_from("<Q", binary, 32)[0]
    entry_size, count = struct.unpack_from("<HH", binary, 54)
    if entry_size < 56 or not count or offset + entry_size * count > len(binary):
        raise ValueError("invalid ELF program headers")
    segments = [struct.unpack_from("<I", binary, offset + entry_size * i)[0] for i in range(count)]
    if 3 in segments or 1 not in segments:
        raise ValueError("fixture must be statically linked")
    return architecture


def image_fixture(binary, label):
    """Return the same (digest, manifest, blobs) tuple as ci-qa-deploy.py."""
    if label not in ("A", "B"):
        raise ValueError("fixture label must be A or B")
    architecture = static_elf_arch(binary)
    layer = io.BytesIO()
    with tarfile.open(fileobj=layer, mode="w") as archive:
        directory = tarfile.TarInfo("bin")
        directory.type = tarfile.DIRTYPE
        directory.mode = 0o755
        archive.addfile(directory)
        executable = tarfile.TarInfo("bin/public-tunnel-fixture")
        executable.mode = 0o755
        executable.size = len(binary)
        archive.addfile(executable, io.BytesIO(binary))
    raw = layer.getvalue()
    compressed = gzip.compress(raw, mtime=0)
    config = json.dumps({"architecture": architecture, "os": "linux",
                         "rootfs": {"type": "layers", "diff_ids": [digest(raw)]},
                         "config": {"Entrypoint": ["/bin/public-tunnel-fixture", label]}},
                        separators=(",", ":")).encode()
    manifest = json.dumps({"schemaVersion": 2, "mediaType": "application/vnd.oci.image.manifest.v1+json",
                           "config": {"mediaType": "application/vnd.oci.image.config.v1+json",
                                      "digest": digest(config), "size": len(config)},
                           "layers": [{"mediaType": "application/vnd.oci.image.layer.v1.tar+gzip",
                                       "digest": digest(compressed), "size": len(compressed)}]},
                          separators=(",", ":")).encode()
    return digest(manifest), manifest, {digest(config): config, digest(compressed): compressed}
