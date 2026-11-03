#!/usr/bin/env python3
"""Export or inspect a private, matched AI Canvas project backup for host transfer.

This tool stages bytes only. It never registers resources, overwrites a destination,
starts a VM, or mutates a running controller. A separate import workflow is needed.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import stat
import sys
import uuid
from urllib.parse import quote

FILES = ("project.db", "data.ext4", "control.db", "manifest.json")
BLOCK = 1024 * 1024


def identifier(value):
    result = uuid.UUID(value)
    if result.int == 0 or str(result) != value:
        raise ValueError("IDs must be canonical nonzero UUIDs")
    return value


def private_file(path):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    st = os.fstat(fd)
    if not stat.S_ISREG(st.st_mode) or st.st_uid != os.geteuid() or st.st_nlink != 1 or st.st_mode & 0o077:
        os.close(fd)
        raise ValueError("Expected an owner-controlled, single-link regular file")
    return os.fdopen(fd, "rb")


def private_dir(path):
    st = os.lstat(path)
    if not stat.S_ISDIR(st.st_mode) or st.st_uid != os.geteuid() or st.st_mode & 0o077:
        raise ValueError("Expected a private owner-controlled directory")


def connect(path):
    with private_file(path):
        pass
    uri = "file:" + quote(str(path.resolve()), safe="/") + "?mode=ro"
    connection = sqlite3.connect(uri, uri=True)
    connection.execute("PRAGMA query_only=ON")
    return connection


def digest(path):
    h = hashlib.sha256()
    with private_file(path) as source:
        while chunk := source.read(BLOCK):
            h.update(chunk)
    return h.hexdigest()


def manifest_at(directory):
    private_dir(directory)
    with private_file(directory / "manifest.json") as f:
        if os.fstat(f.fileno()).st_size > 16384:
            raise ValueError("Manifest is too large")
        manifest = json.load(f)
    c = manifest["checkpoint"]
    for key in ("id", "projectId", "databaseId", "volumeId"):
        identifier(c[key])
    if c["generation"] < 1 or c["schemaVersion"] < 1 or manifest["volumeBytes"] < 1:
        raise ValueError("Invalid backup metadata")
    for value in (c["databaseSha256"], c["volumeSha256"], manifest["controlDatabaseSha256"]):
        if len(value) != 64 or any(ch not in "0123456789abcdef" for ch in value):
            raise ValueError("Invalid SHA-256 in manifest")
    return manifest


def inspect(directory):
    m = manifest_at(directory)
    c = m["checkpoint"]
    expected = {"project.db": c["databaseSha256"], "data.ext4": c["volumeSha256"], "control.db": m["controlDatabaseSha256"]}
    for name, sha in expected.items():
        if digest(directory / name) != sha:
            raise ValueError("Backup file integrity mismatch: " + name)
    with private_file(directory / "data.ext4") as volume:
        if os.fstat(volume.fileno()).st_size != m["volumeBytes"]:
            raise ValueError("Backup volume length mismatch")
        volume.seek(1024)
        header = volume.read(120)
        if len(header) != 120 or header[56:58] != b"\x53\xef" or header[104:120] != uuid.UUID(c["volumeId"]).bytes:
            raise ValueError("Backup ext4 identity mismatch")
    with connect(directory / "project.db") as db:
        if db.execute("PRAGMA application_id").fetchone()[0] != 0x46575031:
            raise ValueError("Unexpected project DB application ID")
        if db.execute("PRAGMA integrity_check").fetchall() != [("ok",)] or db.execute("PRAGMA foreign_key_check").fetchone():
            raise ValueError("Project DB integrity check failed")
        version = db.execute("PRAGMA user_version").fetchone()[0]
        row = db.execute("SELECT project_id,database_id,volume_id,revision,initialized FROM identity WHERE singleton=1").fetchone()
        if not 1 <= version <= 9 or version != c["schemaVersion"] or row != (c["projectId"], c["databaseId"], c["volumeId"], c["revision"], 1):
            raise ValueError("Backup DB identity or revision mismatch")
    with connect(directory / "control.db") as control:
        if control.execute("PRAGMA integrity_check").fetchall() != [("ok",)]:
            raise ValueError("Control DB integrity check failed")
        row = control.execute("SELECT database_id,volume_id,generation,provisioning FROM project_workspaces WHERE project_id=?", (c["projectId"],)).fetchone()
        if row != (c["databaseId"], c["volumeId"], c["generation"], 0):
            raise ValueError("Control snapshot binding differs from the checkpoint")
    return m


def sync_dir(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def copy_file(source, destination):
    with private_file(source) as src:
        fd = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        with os.fdopen(fd, "wb") as out:
            while chunk := src.read(BLOCK):
                if not any(chunk):
                    out.seek(len(chunk), os.SEEK_CUR)
                else:
                    out.write(chunk)
            out.truncate(src.tell())
            out.flush()
            os.fsync(out.fileno())


def binding(connection, project):
    return connection.execute("SELECT database_id,volume_id,vm_id,generation,provisioning,verified_runtime_id FROM project_workspaces WHERE project_id=?", (project,)).fetchone()


def export(args):
    parent = Path(args.workspace_parent).resolve(strict=True)
    private_dir(parent)
    root = parent / "project-backups"
    project_root = root / args.project
    private_dir(root)
    private_dir(project_root)
    lock = os.open(project_root / "operation.lock", os.O_RDWR | os.O_NOFOLLOW)
    try:
        lock_stat = os.fstat(lock)
        if not stat.S_ISREG(lock_stat.st_mode) or lock_stat.st_uid != os.geteuid() or lock_stat.st_nlink != 1 or lock_stat.st_mode & 0o077:
            raise ValueError("Backup operation lock is not a private single-link file")
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        if (project_root / "fence.json").exists():
            raise ValueError("Project maintenance is pending")
        source = project_root / args.checkpoint
        with connect(Path(args.control)) as control:
            control.execute("BEGIN")
            before = binding(control, args.project)
            row = control.execute("SELECT active,document FROM project_backup_operations WHERE id=? AND project_id=?", (args.checkpoint, args.project)).fetchone()
            if not row:
                raise ValueError("No retained checkpoint receipt")
            record = json.loads(row[1])
            public = record["public"]
            if row[0] or record["active"] or public["phase"] != "complete" or public["request"].get("restoreCheckpoint") is not None or public.get("cleanupId") is not None:
                raise ValueError("Checkpoint is not complete and retained")
            m = inspect(source)
            c = m["checkpoint"]
            if c["id"] != args.checkpoint or c["projectId"] != args.project or public.get("manifest") != m:
                raise ValueError("Receipt and archive differ")
            if not before or before[0:2] != (c["databaseId"], c["volumeId"]) or before[3] < c["generation"] or before[4]:
                raise ValueError("Current resource binding differs from the checkpoint")
        destination = Path(args.destination).absolute()
        private_dir(destination.parent)
        os.mkdir(destination, 0o700)
        sync_dir(destination.parent)
        marker = destination / "INCOMPLETE"
        fd = os.open(marker, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            f.write("Incomplete transfer: retain this directory for inspection.\n")
            f.flush()
            os.fsync(f.fileno())
        sync_dir(destination)
        for name in FILES:
            copy_file(source / name, destination / name)
        if inspect(destination) != m:
            raise ValueError("Copied backup differs from source")
        with connect(Path(args.control)) as control:
            if binding(control, args.project) != before:
                raise ValueError("Project binding changed during export")
        os.unlink(marker)
        sync_dir(destination)
        print(json.dumps({"projectId": args.project, "checkpointId": args.checkpoint, "destination": str(destination), "status": "staged"}))
    finally:
        os.close(lock)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    send = commands.add_parser("export", help="Copy a completed checkpoint into a new private directory")
    send.add_argument("--control", required=True, help="Live control SQLite path")
    send.add_argument("--workspace-parent", required=True, help="Private parent containing project-backups")
    send.add_argument("--project", required=True, type=identifier)
    send.add_argument("--checkpoint", required=True, type=identifier)
    send.add_argument("--destination", required=True, help="New directory in an existing private parent")
    check = commands.add_parser("inspect", help="Verify a transferred private directory without registering it")
    check.add_argument("directory")
    args = parser.parse_args()
    if args.command == "export":
        export(args)
    else:
        directory = Path(args.directory).absolute()
        if (directory / "INCOMPLETE").exists():
            raise ValueError("Transfer is incomplete; retain the directory for inspection")
        m = inspect(directory)
        print(json.dumps({"projectId": m["checkpoint"]["projectId"], "checkpointId": m["checkpoint"]["id"], "status": "verified"}))


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, TypeError, sqlite3.Error, json.JSONDecodeError) as error:
        print("Backup transfer failed:", error, file=sys.stderr)
        sys.exit(1)
