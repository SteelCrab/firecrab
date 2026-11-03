#!/usr/bin/env python3
"""Export, inspect or import a private, matched AI Canvas project backup for host transfer.

Export and inspection stage bytes without touching a live project. The Linux import
command registers a fresh destination while its controller is stopped. Existing
project or volume paths are never overwritten.
"""
import argparse
from contextlib import closing
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


def manifest_at(directory, manifest_path=None):
    private_dir(directory)
    with private_file(manifest_path or directory / "manifest.json") as f:
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


def inspect_files(files):
    m = manifest_at(files["manifest.json"].parent, files["manifest.json"])
    c = m["checkpoint"]
    for name in ("project.db", "control.db"):
        for suffix in ("-wal", "-shm", "-journal"):
            if os.path.lexists(str(files[name]) + suffix):
                raise ValueError("SQLite sidecars are not part of the matched checkpoint")
    expected = {"project.db": c["databaseSha256"], "data.ext4": c["volumeSha256"], "control.db": m["controlDatabaseSha256"]}
    for name, sha in expected.items():
        if digest(files[name]) != sha:
            raise ValueError("Backup file integrity mismatch: " + name)
    with private_file(files["data.ext4"]) as volume:
        if os.fstat(volume.fileno()).st_size != m["volumeBytes"]:
            raise ValueError("Backup volume length mismatch")
        volume.seek(1024)
        header = volume.read(120)
        if len(header) != 120 or header[56:58] != b"\x53\xef" or header[104:120] != uuid.UUID(c["volumeId"]).bytes:
            raise ValueError("Backup ext4 identity mismatch")
    with closing(connect(files["project.db"])) as db:
        if db.execute("PRAGMA application_id").fetchone()[0] != 0x46575031:
            raise ValueError("Unexpected project DB application ID")
        if db.execute("PRAGMA integrity_check").fetchall() != [("ok",)] or db.execute("PRAGMA foreign_key_check").fetchone():
            raise ValueError("Project DB integrity check failed")
        version = db.execute("PRAGMA user_version").fetchone()[0]
        row = db.execute("SELECT project_id,database_id,volume_id,revision,initialized FROM identity WHERE singleton=1").fetchone()
        if not 1 <= version <= 9 or version != c["schemaVersion"] or row != (c["projectId"], c["databaseId"], c["volumeId"], c["revision"], 1):
            raise ValueError("Backup DB identity or revision mismatch")
    with closing(connect(files["control.db"])) as control:
        if control.execute("PRAGMA integrity_check").fetchall() != [("ok",)]:
            raise ValueError("Control DB integrity check failed")
        row = control.execute("SELECT database_id,volume_id,generation,provisioning FROM project_workspaces WHERE project_id=?", (c["projectId"],)).fetchone()
        if row != (c["databaseId"], c["volumeId"], c["generation"], 0):
            raise ValueError("Control snapshot binding differs from the checkpoint")
    return m


def inspect(directory):
    private_dir(directory)
    return inspect_files({name: directory / name for name in FILES})


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


def destination_plan(args):
    """Inspect a destination without registering or modifying its resources."""
    directory = Path(args.directory).absolute()
    if (directory / "INCOMPLETE").exists():
        raise ValueError("Transfer is incomplete; retain the directory for inspection")
    manifest = inspect(directory)
    checkpoint = manifest["checkpoint"]
    project = checkpoint["projectId"]
    database = checkpoint["databaseId"]
    volume = checkpoint["volumeId"]
    with connect(directory / "control.db") as source:
        project_row = source.execute("SELECT spec FROM ai_sandbox_projects WHERE id=?", (project,)).fetchone()
        if not project_row:
            raise ValueError("Control snapshot has no matching project registration")
        source_project = json.loads(project_row[0])
        if source_project.get("id") != project or not isinstance(source_project.get("spec"), dict):
            raise ValueError("Source project registration is invalid")
        route = source.execute("SELECT phase FROM sandbox_task_routes WHERE project_id=?", (project,)).fetchone()
        task_count = source.execute("SELECT count(*) FROM sandbox_task_catalog WHERE project_id=?", (project,)).fetchone()[0]
    with connect(directory / "project.db") as project_db:
        tables = {row[0] for row in project_db.execute("SELECT name FROM sqlite_master WHERE type='table'")}
        counts = {}
        for table in ("ai_sandbox_tasks", "terminal_sessions", "private_editors", "private_views", "private_drafts"):
            counts[table] = project_db.execute("SELECT count(*) FROM " + table).fetchone()[0] if table in tables else 0
        pending_files = project_db.execute("SELECT count(*) FROM file_operations WHERE state='pending'").fetchone()[0] if "file_operations" in tables else 0
        busy_tasks = project_db.execute("SELECT count(*) FROM ai_sandbox_task_index WHERE busy=1").fetchone()[0] if "ai_sandbox_task_index" in tables else 0
    with connect(Path(args.destination_control)) as target:
        vm = target.execute("SELECT state,purpose,last_runtime_id FROM vms WHERE id=?", (args.vm,)).fetchone()
        if not vm:
            raise ValueError("Destination VM does not exist")
        conflicts = []
        if vm != ("created", "instance", None):
            conflicts.append("Destination VM must be a never-started Created user VM")
        if target.execute("SELECT EXISTS(SELECT 1 FROM ai_sandbox_projects WHERE id=? OR vm_id=?)", (project, args.vm)).fetchone()[0]:
            conflicts.append("Project ID or VM is already assigned")
        if target.execute("SELECT EXISTS(SELECT 1 FROM project_workspaces WHERE project_id=? OR database_id=? OR volume_id=? OR vm_id=?)", (project, database, volume, args.vm)).fetchone()[0]:
            conflicts.append("Project, database, volume or VM binding already exists")
        if target.execute("SELECT EXISTS(SELECT 1 FROM micro_block_storages WHERE id=? OR name=?)", (volume, args.volume_name)).fetchone()[0]:
            conflicts.append("Volume ID or name already exists")
        if target.execute("SELECT EXISTS(SELECT 1 FROM sandbox_task_catalog WHERE project_id=?)", (project,)).fetchone()[0]:
            conflicts.append("Destination already has task reservations for this project")
        if pending_files or busy_tasks:
            conflicts.append("Source contains pending file operations or active tasks")
        if route and route[0] != "project":
            conflicts.append("Source Sandbox migration is incomplete")
    print(json.dumps({
        "projectId": project, "databaseId": database, "volumeId": volume,
        "checkpointId": checkpoint["id"], "destinationVmId": args.vm,
        "sourceGeneration": checkpoint["generation"], "sourceSandboxRoute": route[0] if route else "controller",
        "sourceTaskReservations": task_count, "sourceProjectRecords": counts,
        "conflicts": conflicts,
        "status": "inspection_only",
        "remaining": ["Import verifies registered storage and exclusive controller ownership", "Operator must quiesce the source and stop the destination controller", "Import validates task identities and installs the matched pair"],
    }))


def import_status(args):
    """Read durable import phases without opening any project or changing the journal."""
    control = connect(Path(args.destination_control).absolute())
    try:
        if args.operation:
            rows = control.execute("SELECT id,project_id,phase,document FROM project_workspace_imports WHERE id=?", (args.operation,)).fetchall()
            if not rows:
                raise ValueError("Import operation was not found")
        else:
            rows = control.execute("SELECT id,project_id,phase,document FROM project_workspace_imports WHERE phase!='complete' ORDER BY rowid DESC LIMIT 101").fetchall()
            if len(rows) > 100:
                raise ValueError("Too many incomplete imports; inspect the controller journal")
        result = []
        for operation, project, phase, raw in rows:
            record = json.loads(raw)
            if record.get("id") != operation or record.get("project") != project or record.get("phase") != phase or phase not in ("staging", "prepared", "complete"):
                raise ValueError("Import journal identity or phase is inconsistent")
            identifier(operation)
            identifier(project)
            checkpoint = identifier(record["checkpoint"])
            result.append(dict(operationId=operation, projectId=project, checkpointId=checkpoint,
                               phase=phase, generation=record["generation"],
                               destinationVmId=record["request"]["vm"],
                               destinationStorageId=record["request"]["storage"],
                               volumeName=record["request"]["volumeName"],
                               completedAt=record.get("completedAt")))
        print(json.dumps({"imports": result, "inspectionOnly": True}))
    finally:
        control.close()


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
    plan = commands.add_parser("plan-import", help="Inspect destination registration conflicts without writing")
    plan.add_argument("directory", help="Verified transferred checkpoint directory")
    plan.add_argument("--destination-control", required=True, help="Existing destination controller SQLite path")
    plan.add_argument("--vm", required=True, type=identifier, help="Existing destination user VM UUID")
    plan.add_argument("--volume-name", required=True, help="Proposed destination volume name")
    status = commands.add_parser("import-status", help="Read incomplete imports or one saved import receipt")
    status.add_argument("--destination-control", required=True, help="Existing destination controller SQLite path")
    status.add_argument("--operation", type=identifier, help="Exact import UUID; omit to list incomplete imports")
    receive = commands.add_parser("import", help="Install and register a fresh project while the destination controller is stopped")
    receive.add_argument("directory", help="Transferred matched checkpoint directory")
    receive.add_argument("--destination-control", required=True, help="Existing destination controller SQLite path")
    receive.add_argument("--operation", required=True, type=identifier, help="Stable import UUID; reuse it after interruption")
    receive.add_argument("--vm", required=True, type=identifier, help="Destination user VM that has never been started")
    receive.add_argument("--storage", required=True, type=identifier, help="Registered destination MicroStorage UUID")
    receive.add_argument("--volume-name", required=True)
    receive.add_argument("--confirm-source-quiesced", action="store_true", help="Acknowledge source workload quiescence before cutover")
    args = parser.parse_args()
    if args.command == "export":
        export(args)
    elif args.command == "plan-import":
        destination_plan(args)
    elif args.command == "import-status":
        import_status(args)
    elif args.command == "import":
        from project_backup_import import run_import
        run_import(args, sys.modules[__name__])
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
