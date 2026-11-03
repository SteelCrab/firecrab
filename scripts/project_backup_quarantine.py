"""Explicit, journaled cleanup of abandoned import quarantine files."""
import fcntl
import json
import os
from pathlib import Path
import stat
import sys
import time

from project_backup_import import check_abandon_contents, encode, rw_database, save, transaction

ROLES = ("project", "volume", "backup", "projectStage", "volumeStage", "archiveStage")


def quarantine_roots(db, control, record, io):
    """Derive quarantine paths from the retained identity, never from a caller path."""
    c = record["manifest"]["checkpoint"]
    for key in ("id", "projectId", "databaseId", "volumeId"):
        io.identifier(c[key])
    io.identifier(record["id"])
    if c["projectId"] != record["project"] or c["id"] != record["checkpoint"]:
        raise ValueError("Abandoned import identity differs from its checkpoint")
    if record["request"]["control"] != str(control):
        raise ValueError("Abandoned import belongs to another controller")
    row = db.execute("SELECT path FROM micro_storages WHERE id=?", (record["request"]["storage"],)).fetchone()
    if not row or not Path(row[0]).is_absolute():
        raise ValueError("Registered destination storage is unavailable")
    pool = Path(row[0])
    info = os.lstat(pool)
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.geteuid() or info.st_mode & 0o022:
        raise ValueError("Registered destination storage is not owner-controlled")
    pool = pool.resolve(strict=True)
    info = os.stat(pool)
    if record["poolIdentity"] != [info.st_dev, info.st_ino]:
        raise ValueError("Destination storage identity changed")
    parent = control.with_suffix(".sandbox")
    projects, backups, volumes = parent / "projects", parent / "project-backups", pool / "volumes"
    expected = dict(project=str(projects / c["projectId"]), volume=str(volumes / c["volumeId"]), backup=str(backups / c["projectId"]), archive=str(backups / c["projectId"] / c["id"]), projectStage=str(projects / (".project-import-" + record["id"])), volumeStage=str(volumes / (".project-import-" + record["id"])), archiveStage=str(backups / (".project-import-" + record["id"])))
    if record["folders"] != expected or set(record["abandonPlan"]) != set(ROLES):
        raise ValueError("Abandoned import paths or directory plan changed")
    for directory in (parent, projects, backups):
        if os.path.lexists(directory):
            io.private_dir(directory)
    if os.path.lexists(volumes):
        info = os.lstat(volumes)
        if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.geteuid() or info.st_mode & 0o022:
            raise ValueError("Volume namespace is not owner-controlled")
    roots = {}
    for role in ROLES:
        source = Path(expected[role])
        roots[role] = source.parent / (".abandoned-import-" + record["id"] + "-" + role)
    return roots, pool


def file_proof(path, role, relative, io):
    with io.private_file(path) as file:
        info = os.fstat(file.fileno())
        identity = [info.st_dev, info.st_ino, info.st_size]
    return dict(role=role, relative=str(relative), identity=identity, sha=io.digest(path))


def build_plan(record, roots, io):
    files, directories = [], []
    prepared = bool(record.get("files"))
    for role in ROLES:
        root, expected = roots[role], record["abandonPlan"][role]
        if expected is None:
            if os.path.lexists(root):
                raise ValueError("Unexpected quarantine directory")
            continue
        io.private_dir(root)
        info = os.lstat(root)
        if [info.st_dev, info.st_ino] != expected:
            raise ValueError("Quarantine directory identity changed")
        check_abandon_contents(role, root, record["checkpoint"], record["id"], io, prepared)
        directories.append(dict(role=role, relative="", identity=expected))
        for entry in root.iterdir():
            if role == "backup":
                nested = os.lstat(entry)
                directories.append(dict(role=role, relative=entry.name, identity=[nested.st_dev, nested.st_ino]))
                for child in entry.iterdir():
                    files.append(file_proof(child, role, Path(entry.name) / child.name, io))
            else:
                files.append(file_proof(entry, role, Path(entry.name), io))
            if len(files) > 1024:
                raise ValueError("Too many quarantine files for one cleanup operation")
    files.sort(key=lambda item: (item["role"], item["relative"]))
    directories.sort(key=lambda item: (item["relative"] == "", item["role"], item["relative"]))
    return dict(phase="pending", files=files, directories=directories)


def allowed_relative(role, relative, record, io, directory=False):
    path = Path(relative)
    if directory:
        return relative == "" or (role == "backup" and relative == record["checkpoint"])
    parts = path.parts
    if role == "backup":
        return len(parts) == 2 and parts[0] == record["checkpoint"] and parts[1] in io.FILES
    if len(parts) != 1 or parts[0] in (".", ".."):
        return False
    if role == "project":
        return relative == "project.db"
    if role == "volume":
        return relative == "data.ext4"
    return role.endswith("Stage") and relative.startswith(".import-" + record["id"] + "-")


def validate_plan(record, roots, plan, io):
    """Reject changed/recreated entries before deleting another planned file."""
    files, directories = plan["files"], plan["directories"]
    if plan["phase"] != "pending" or len(files) > 1024 or len(directories) > 7:
        raise ValueError("Quarantine cleanup plan is invalid")
    file_keys = {(item["role"], item["relative"]) for item in files}
    dir_keys = {(item["role"], item["relative"]) for item in directories}
    if len(file_keys) != len(files) or len(dir_keys) != len(directories):
        raise ValueError("Quarantine cleanup plan contains duplicate entries")
    expected_roots = {(role, "") for role in ROLES if record["abandonPlan"][role] is not None}
    if {key for key in dir_keys if key[1] == ""} != expected_roots:
        raise ValueError("Quarantine cleanup roots differ from the abandonment receipt")
    for item in directories:
        if item["relative"] == "" and item["identity"] != record["abandonPlan"].get(item["role"]):
            raise ValueError("Quarantine cleanup root identity differs from its receipt")
    for role, relative in file_keys:
        if (role, "") not in dir_keys or (role == "backup" and (role, record["checkpoint"]) not in dir_keys):
            raise ValueError("Quarantine cleanup file has no journaled parent")
    for item in directories:
        role, relative = item["role"], item["relative"]
        if role not in roots or not allowed_relative(role, relative, record, io, True):
            raise ValueError("Quarantine cleanup directory is invalid")
        path = roots[role] / relative
        if os.path.lexists(path):
            io.private_dir(path)
            info = os.lstat(path)
            if [info.st_dev, info.st_ino] != item["identity"]:
                raise ValueError("Quarantine directory changed")
            for entry in path.iterdir():
                child = str(Path(relative) / entry.name) if relative else entry.name
                if (role, child) not in file_keys and (role, child) not in dir_keys:
                    raise ValueError("Unknown quarantine entry prevents cleanup")
    for item in files:
        role, relative = item["role"], item["relative"]
        if role not in roots or not allowed_relative(role, relative, record, io):
            raise ValueError("Quarantine cleanup file is invalid")
        path = roots[role] / relative
        if os.path.lexists(path):
            with io.private_file(path) as file:
                info = os.fstat(file.fileno())
                if [info.st_dev, info.st_ino, info.st_size] != item["identity"]:
                    raise ValueError("Quarantine file identity changed")
            if io.digest(path) != item["sha"]:
                raise ValueError("Quarantine file contents changed")


def purge_abandoned(args, io):
    if not sys.platform.startswith("linux"):
        raise ValueError("Quarantine cleanup requires Linux")
    control = Path(args.destination_control).absolute()
    with io.private_file(control):
        pass
    control = control.resolve(strict=True)
    lock = os.open(control, os.O_RDWR | os.O_NOFOLLOW)
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        db = rw_database(control)
        try:
            row = db.execute("SELECT project_id,phase,document FROM project_workspace_imports WHERE id=?", (args.operation,)).fetchone()
            if not row:
                raise ValueError("Import operation was not found")
            record = json.loads(row[2])
            if (record.get("project"), record.get("phase"), record.get("id"), record.get("outcome")) != (row[0], "complete", args.operation, "abandoned") or row[1] != "complete":
                raise ValueError("Only a completed abandoned import can be cleaned")
            plan = record.get("quarantineCleanup")
            if plan and plan.get("phase") == "complete":
                print(encode(dict(status="complete", operationId=args.operation, fileCount=plan["fileCount"], bytes=plan["bytes"])))
                return
            roots, pool = quarantine_roots(db, control, record, io)
            if not plan:
                plan = build_plan(record, roots, io)
            validate_plan(record, roots, plan, io)
            count = len(plan["files"])
            size = sum(item["identity"][2] for item in plan["files"])
            if not args.confirm_delete:
                entries = [dict(role=item["role"], path=item["relative"], bytes=item["identity"][2]) for item in plan["files"]]
                print(encode(dict(status="preview", operationId=args.operation, fileCount=count, bytes=size, files=entries, pending=bool(record.get("quarantineCleanup")))))
                return
            if not record.get("quarantineCleanup"):
                if len(encode(plan).encode()) > 1024 * 1024:
                    raise ValueError("Quarantine cleanup plan is too large")
                record["quarantineCleanup"] = plan
                transaction(db, lambda: save(db, record))
            for item in plan["files"]:
                path = roots[item["role"]] / item["relative"]
                if not os.path.lexists(path):
                    continue
                with io.private_file(path) as file:
                    info = os.fstat(file.fileno())
                    if [info.st_dev, info.st_ino, info.st_size] != item["identity"]:
                        raise ValueError("Quarantine file identity changed during cleanup")
                if io.digest(path) != item["sha"]:
                    raise ValueError("Quarantine file changed during cleanup")
                os.unlink(path)
                io.sync_dir(path.parent)
            for item in plan["directories"]:
                path = roots[item["role"]] / item["relative"]
                if not os.path.lexists(path):
                    continue
                io.private_dir(path)
                info = os.lstat(path)
                if [info.st_dev, info.st_ino] != item["identity"]:
                    raise ValueError("Quarantine directory changed during cleanup")
                os.rmdir(path)
                io.sync_dir(path.parent)
            info = os.stat(pool)
            if record["poolIdentity"] != [info.st_dev, info.st_ino]:
                raise ValueError("Destination storage changed during cleanup")
            plan.update(phase="complete", fileCount=count, bytes=size, completedAt=time.time_ns() // 1_000_000)
            record["quarantineCleanup"] = plan
            transaction(db, lambda: save(db, record))
            print(encode(dict(status="complete", operationId=args.operation, fileCount=count, bytes=size)))
        finally:
            db.close()
    finally:
        os.close(lock)
