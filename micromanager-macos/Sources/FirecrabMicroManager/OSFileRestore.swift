import Foundation

// Runs inside the detached OS copy with the cached image mounted read-only.
let osFileRestoreScript = #"""
import hashlib
import os
from pathlib import Path
import shutil
import stat
import sys

# Restore Debian-owned OS files in binary/library trees, including unit files
# and generators. /etc, application data, existing files, and unknown orphans
# stay intact.
PREFIXES = ("/usr/bin/", "/usr/sbin/", "/usr/lib/", "/usr/libexec/", "/bin/", "/sbin/", "/lib/")


def package_versions(root):
    versions = {}
    text = (root / "var/lib/dpkg/status").read_text()
    for paragraph in text.split("\n\n"):
        fields = {}
        for line in paragraph.splitlines():
            if line and not line[0].isspace() and ": " in line:
                key, value = line.split(": ", 1)
                fields[key] = value
        if fields.get("Status") != "install ok installed":
            continue
        name = fields["Package"]
        architecture = fields["Architecture"]
        versions[name] = fields["Version"]
        versions[name + ":" + architecture] = fields["Version"]
    return versions


def rooted_path(root, name, follow_final=False):
    # Absolute symlinks in the reference image must resolve inside that image.
    pending = name.split("/")
    resolved = []
    links = 0
    while pending:
        part = pending.pop(0)
        if part in ("", "."):
            continue
        if part == "..":
            if not resolved:
                raise RuntimeError("path escapes OS root: " + name)
            resolved.pop()
            continue
        candidate = root.joinpath(*resolved, part)
        if candidate.is_symlink() and (pending or follow_final):
            links += 1
            if links > 40:
                raise RuntimeError("symlink loop: " + name)
            target = os.readlink(candidate)
            if target.startswith("/"):
                resolved = []
            pending = target.split("/") + pending
        else:
            resolved.append(part)
    return root.joinpath(*resolved)


def checksum(path):
    digest = hashlib.md5(usedforsecurity=False)
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1048576), b""):
            digest.update(block)
    return digest.hexdigest()


def restore_missing(root, reference):
    installed = package_versions(root)
    cached = package_versions(reference)
    restored = 0
    info = root / "var/lib/dpkg/info"
    orphans = {}
    lost = root / "lost+found"
    if lost.is_dir():
        for path in lost.iterdir():
            if stat.S_ISREG(path.lstat().st_mode):
                orphans.setdefault(checksum(path), path)
    reference_info = reference / "var/lib/dpkg/info"
    names = {path.name for directory in [info, reference_info] for path in directory.glob("*.list")}
    for filename in sorted(names):
        package = filename[:-5]
        if package not in installed:
            continue
        manifest = info / filename
        # fsck can also lose package manifests. Their exact package version
        # is still recorded in status, so use the authenticated matching cache.
        for suffix in [".list", ".md5sums"]:
            destination = info / (package + suffix)
            source = reference_info / destination.name
            if os.path.lexists(destination) or not source.is_file():
                continue
            if installed[package] != cached.get(package):
                raise RuntimeError("cached package version does not match missing manifest: " + str(destination))
            with destination.open("xb") as output, source.open("rb") as input_file:
                shutil.copyfileobj(input_file, output)
            metadata = source.stat()
            os.chown(destination, metadata.st_uid, metadata.st_gid)
            shutil.copystat(source, destination)
            restored += 1
            print("[RESTORE] /var/lib/dpkg/info/" + destination.name, file=sys.stderr)
        checksums = {}
        sums = manifest.with_suffix(".md5sums")
        if sums.is_file():
            for line in sums.read_text().splitlines():
                digest, name = line.split(None, 1)
                checksums["/" + name.lstrip("/")] = digest
        for name in sorted(manifest.read_text().splitlines(), key=lambda x: (x.count("/"), x)):
            if not name.startswith(PREFIXES):
                continue
            if ".." in name.split("/") or "\n" in name or "\x00" in name:
                raise RuntimeError("invalid package path: " + repr(name))
            destination = rooted_path(root, name)
            relative = "/" + destination.relative_to(root).as_posix()
            if not relative.startswith(PREFIXES):
                raise RuntimeError("package path leaves OS binaries: " + name)
            if os.path.lexists(destination):
                continue
            expected = checksums.get(name)
            source = orphans.get(expected, rooted_path(reference, name))
            if not os.path.lexists(source):
                raise RuntimeError("cached image cannot restore missing file: " + name)
            metadata = source.lstat()
            if stat.S_ISREG(metadata.st_mode):
                if not expected or checksum(source) != expected:
                    raise RuntimeError("cached file does not match installed package checksum: " + name)
            elif stat.S_ISDIR(metadata.st_mode) or stat.S_ISLNK(metadata.st_mode):
                if installed[package] != cached.get(package):
                    raise RuntimeError("cached package version does not match missing file: " + name)
            else:
                raise RuntimeError("unsupported cached file type: " + name)
            # Parents are copied individually, never recursively. An existing
            # parent, including a symlink, is never replaced.
            missing_parents = []
            parent = destination.parent
            while not parent.exists():
                missing_parents.append(parent)
                parent = parent.parent
            for parent in reversed(missing_parents):
                template = rooted_path(reference, "/" + parent.relative_to(root).as_posix(), True)
                parent_metadata = template.stat()
                if not stat.S_ISDIR(parent_metadata.st_mode):
                    raise RuntimeError("cached parent is not a directory: " + str(template))
                parent.mkdir(mode=stat.S_IMODE(parent_metadata.st_mode))
                os.chown(parent, parent_metadata.st_uid, parent_metadata.st_gid)
                os.chmod(parent, stat.S_IMODE(parent_metadata.st_mode))
            if stat.S_ISDIR(metadata.st_mode):
                destination.mkdir(mode=stat.S_IMODE(metadata.st_mode))
            elif stat.S_ISLNK(metadata.st_mode):
                destination.symlink_to(os.readlink(source))
            else:
                with destination.open("xb") as output, source.open("rb") as input_file:
                    shutil.copyfileobj(input_file, output)
                shutil.copystat(source, destination, follow_symlinks=False)
                if checksum(destination) != expected:
                    raise RuntimeError("restored file checksum failed: " + name)
            os.chown(destination, metadata.st_uid, metadata.st_gid, follow_symlinks=False)
            if not stat.S_ISLNK(metadata.st_mode):
                # chown clears Linux setuid/setgid bits even when the owner
                # does not change. Restore the package mode after ownership.
                os.chmod(destination, stat.S_IMODE(metadata.st_mode))
            restored += 1
            print("[RESTORE] " + name, file=sys.stderr)
    return restored


if __name__ == "__main__":
    print(restore_missing(Path(sys.argv[1]), Path(sys.argv[2])))
"""#
