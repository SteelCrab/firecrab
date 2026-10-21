#!/usr/bin/env python3
"""Create tiny isolated GPT/ext4 fixtures; never use an installed management disk."""
import argparse
import json
from pathlib import Path
import shutil
import struct
import subprocess
import uuid
import zlib


def run(arguments):
    return subprocess.run(arguments, check=True, capture_output=True, text=True)


def disk(destination, filesystem):
    sectors = 262144
    entries = bytearray(128 * 128)
    struct.pack_into("<16s16sQQQ72s", entries, 0,
                     uuid.UUID("0fc63daf-8483-4772-8e79-3d69d8477de4").bytes_le,
                     uuid.uuid4().bytes_le, 2048, 2048 + filesystem.stat().st_size // 512 - 1,
                     0, "root".encode("utf-16le").ljust(72, b"\0"))
    disk_id = uuid.uuid4().bytes_le

    def header(current, alternate, entry_lba):
        result = bytearray(512)
        struct.pack_into("<8sIIIIQQQQ16sQIII", result, 0, b"EFI PART", 0x10000, 92,
                         0, 0, current, alternate, 34, sectors - 34, disk_id,
                         entry_lba, 128, 128, zlib.crc32(entries))
        struct.pack_into("<I", result, 16, zlib.crc32(result[:92]))
        return result

    with destination.open("wb") as output:
        output.truncate(sectors * 512)
        mbr = bytearray(512)
        struct.pack_into("<B3sB3sII", mbr, 446, 0, b"\0\x02\0", 0xEE,
                         b"\xff\xff\xff", 1, sectors - 1)
        mbr[510:512] = b"\x55\xaa"
        output.write(mbr)
        output.write(header(1, sectors - 1, 2))
        output.write(entries)
        output.seek(2048 * 512)
        with filesystem.open("rb") as source:
            shutil.copyfileobj(source, output)
        output.seek((sectors - 33) * 512)
        output.write(entries)
        output.write(header(sectors - 1, 1, sectors - 33))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--kernel", type=Path, required=True)
    parser.add_argument("--initrd", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--e2fsprogs", type=Path, default=Path("/opt/homebrew/opt/e2fsprogs/sbin"))
    args = parser.parse_args()
    output = args.output.absolute()
    if output.exists():
        parser.error("output must be a new fixture directory")
    # Test artifacts belong in temporary storage, not a managed installation.
    temporary = Path("/private/tmp").resolve()
    if not output.parent.resolve().is_relative_to(temporary):
        parser.error("output must be under /private/tmp")
    output.mkdir(mode=0o700)
    filesystem = output / "clean.ext4"
    with filesystem.open("wb") as file:
        file.truncate(64 * 1024 * 1024)
    run([str(args.e2fsprogs / "mkfs.ext4"), "-F", "-q", str(filesystem)])
    corrupt = output / "corrupt.ext4"
    shutil.copyfile(filesystem, corrupt)
    payload = output / "payload"
    payload.write_text("isolated orphan fixture\n")
    run([str(args.e2fsprogs / "debugfs"), "-w", "-R", f"write {json.dumps(str(payload))} /orphan", str(corrupt)])
    run([str(args.e2fsprogs / "debugfs"), "-w", "-R", "unlink /orphan", str(corrupt)])
    check = subprocess.run([str(args.e2fsprogs / "e2fsck"), "-f", "-n", str(corrupt)],
                           capture_output=True, text=True)
    (output / "corruption-check.log").write_text(check.stdout + check.stderr)
    if check.returncode != 4 or "Unattached inode" not in check.stdout + check.stderr:
        raise RuntimeError("fixture did not produce the expected unattached-inode failure")
    directory_corrupt = output / "directory-corrupt.ext4"
    shutil.copyfile(filesystem, directory_corrupt)
    run([str(args.e2fsprogs / "debugfs"), "-w", "-R", "mkdir /broken", str(directory_corrupt)])
    # Zero the first directory entry's record length so a read-only check aborts.
    run([str(args.e2fsprogs / "debugfs"), "-w", "-R",
         "zap_block -f /broken -o 4 -l 2 -p 0 0", str(directory_corrupt)])
    check = subprocess.run([str(args.e2fsprogs / "e2fsck"), "-f", "-n", str(directory_corrupt)],
                           capture_output=True, text=True)
    (output / "directory-corruption-check.log").write_text(check.stdout + check.stderr)
    if check.returncode != 12 or "directory corrupted" not in check.stdout + check.stderr:
        raise RuntimeError("fixture did not produce the expected aborted directory check")
    for name, source in [("clean", filesystem), ("corrupt", corrupt),
                         ("directory-corrupt", directory_corrupt)]:
        home = output / name
        for directory in ["system", "data", "runtime/repairs/test"]:
            (home / directory).mkdir(parents=True, mode=0o700)
        for source_file, target in [(args.kernel, "system/Image"), (args.initrd, "system/initrd.img")]:
            shutil.copyfile(source_file, home / target)
        disk(home / "system/debian-system.raw", filesystem)
        disk(home / "runtime/repairs/test/debian-system.repaired.raw", source)
        (home / "data/firecrab-data.raw").write_text("persistent data must stay unchanged\n")
        (home / "runtime/manager_ed25519").write_text("fixture key must stay unchanged\n")
        (home / ".firecrab-os-repair-fixture").write_text("isolated OS repair fixture\n")
    print(output)


if __name__ == "__main__":
    main()
