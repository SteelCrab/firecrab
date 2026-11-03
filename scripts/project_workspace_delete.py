"""Offline, journaled deletion of one retained project while preserving borrowed resources."""
from contextlib import closing
import ctypes
import fcntl
import hashlib
import json
import os
import re
from pathlib import Path
import shutil
import sqlite3
import stat
import subprocess
import sys
import time
import uuid

from project_backup_import import encode, rename_new, rw_database, transaction


def open_owned_file(path):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    info = os.fstat(fd)
    if not stat.S_ISREG(info.st_mode) or info.st_uid != os.geteuid() or info.st_nlink != 1:
        os.close(fd)
        raise ValueError("Project file is not an owned single-link regular file")
    return os.fdopen(fd, "rb")


def file_proof(path):
    digest = hashlib.sha256()
    with open_owned_file(path) as file:
        before = os.fstat(file.fileno())
        while chunk := file.read(1024 * 1024):
            digest.update(chunk)
        after = os.fstat(file.fileno())
    if (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns) != (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns):
        raise ValueError("Project file changed during inventory")
    return dict(identity=[after.st_dev, after.st_ino, after.st_size], sha=digest.hexdigest())


def owner_directory(path):
    info = os.lstat(path)
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.geteuid() or info.st_mode & 0o022:
        raise ValueError("Project directory is not owner-controlled")
    return info


def pool_path(db, storage_id, requested):
    path = Path(requested)
    if not path.is_absolute():
        raise ValueError("The configured pool path must be absolute")
    owner_directory(path)
    path = path.resolve(strict=True)
    configured = os.environ.get("FIRECRAB_STORAGE_ROOTS", "").strip()
    roots = {}
    for part in configured.split(":") if configured else []:
        if not part.strip():
            continue
        key, value = part.strip().split("=", 1)
        if key.strip() in roots or not key.strip() or not value.strip():
            raise ValueError("Configured storage roots are invalid")
        roots[key.strip()] = Path(value.strip())
    if not roots:
        roots = {"default": Path("data")}
    selected = roots.get(storage_id)
    if selected is None:
        row = db.execute("SELECT path FROM micro_storages WHERE id=?", (storage_id,)).fetchone()
        selected = Path(row[0]) if row else None
    expected = selected.resolve(strict=True) if selected is not None else None
    if expected != path:
        raise ValueError("Pool path does not match the destination controller configuration")
    return path


def volume_identity(path, volume, size):
    with open_owned_file(path) as file:
        info = os.fstat(file.fileno())
        file.seek(1024)
        header = file.read(120)
    if info.st_size != size or len(header) != 120 or header[56:58] != b"\x53\xef" or header[104:120] != uuid.UUID(volume).bytes:
        raise ValueError("Volume size, ext4 filesystem or UUID differs from its registration")


def mountpoints():
    """Include bind mounts even when they share the same device number."""
    result = set()
    with open("/proc/self/mountinfo", "r", encoding="utf-8") as source:
        for line in source:
            fields = line.split()
            if len(fields) < 6:
                raise ValueError("Cannot inspect Linux mount ownership")
            path = re.sub(r"\\([0-7]{3})", lambda m: chr(int(m.group(1), 8)), fields[4])
            result.add(os.path.normpath(path))
    return result


def inventory(roots):
    files, directories = [], []
    mounted = mountpoints()
    for role, root in roots.items():
        if not os.path.lexists(root):
            continue
        root_stat = owner_directory(root)
        def visit(directory, relative):
            info = owner_directory(directory)
            if os.path.normpath(str(directory)) in mounted:
                raise ValueError("Project data contains a mount point")
            if info.st_dev != root_stat.st_dev:
                raise ValueError("Project data contains another mounted filesystem")
            directories.append(dict(role=role, path=relative, identity=[info.st_dev, info.st_ino]))
            for entry in os.scandir(directory):
                child = Path(entry.path)
                rel = str(Path(relative) / entry.name) if relative else entry.name
                info = entry.stat(follow_symlinks=False)
                if os.path.normpath(str(child)) in mounted:
                    raise ValueError("Project data contains a mount point")
                if stat.S_ISDIR(info.st_mode):
                    visit(child, rel)
                elif stat.S_ISREG(info.st_mode):
                    if info.st_dev != root_stat.st_dev:
                        raise ValueError("Project data contains a different filesystem")
                    proof = file_proof(child)
                    files.append(dict(role=role, path=rel, **proof))
                else:
                    raise ValueError("Project data contains a link or unsupported filesystem entry")
                if len(files) + len(directories) > 100000:
                    raise ValueError("Project deletion inventory exceeds its bound")
        visit(root, "")
    files.sort(key=lambda item: (item["role"], item["path"]))
    directories.sort(key=lambda item: (-len(Path(item["path"]).parts), item["role"], item["path"]))
    return files, directories


def check_inventory(roots, files, directories):
    mounted = mountpoints()
    file_keys = {(x["role"], x["path"]) for x in files}
    dir_keys = {(x["role"], x["path"]) for x in directories}
    if len(file_keys) != len(files) or len(dir_keys) != len(directories):
        raise ValueError("Deletion inventory has duplicate entries")
    for role, root in roots.items():
        if os.path.lexists(root) and (role, "") not in dir_keys:
            raise ValueError("Unexpected project root appeared during deletion")
    for entry in directories:
        role, relative = entry["role"], entry["path"]
        if role not in roots or (relative and (Path(relative).is_absolute() or ".." in Path(relative).parts)):
            raise ValueError("Deletion directory is outside the recorded project roots")
        path = roots[role] / relative
        if os.path.normpath(str(path)) in mounted:
            raise ValueError("Project data contains a mount point")
        if not os.path.lexists(path):
            continue
        info = owner_directory(path)
        if [info.st_dev, info.st_ino] != entry["identity"]:
            raise ValueError("Project directory changed during deletion")
        for child in os.scandir(path):
            name = str(Path(relative) / child.name) if relative else child.name
            if (role, name) not in file_keys and (role, name) not in dir_keys:
                raise ValueError("Unknown project data appeared during deletion")
    for entry in files:
        role, relative = entry["role"], entry["path"]
        if role not in roots or not relative or Path(relative).is_absolute() or ".." in Path(relative).parts:
            raise ValueError("Deletion file is outside the recorded project roots")
        parent = str(Path(relative).parent)
        parent = "" if parent == "." else parent
        if (role, parent) not in dir_keys:
            raise ValueError("Deletion file has no recorded parent directory")
        path = roots[role] / relative
        if os.path.normpath(str(path)) in mounted:
            raise ValueError("Project data contains a mount point")
        if os.path.lexists(path) and file_proof(path) != {"identity": entry["identity"], "sha": entry["sha"]}:
            raise ValueError("Project file changed during deletion")


def save(db, record):
    db.execute("UPDATE project_workspace_deletions SET phase=?,document=? WHERE id=?", (record["phase"], encode(record), record["id"]))


def preflight(db, args, control, io):
    binding = db.execute("SELECT database_id,volume_id,vm_id,generation,provisioning FROM project_workspaces WHERE project_id=?", (args.project,)).fetchone()
    if not binding or binding[4] or binding[3] != args.generation:
        raise ValueError("Project binding is missing, provisioning, or has another generation")
    database, volume, vm, generation, _ = binding
    for value in (database, volume, vm):
        io.identifier(value)
    project = db.execute("SELECT vm_id FROM ai_sandbox_projects WHERE id=?", (args.project,)).fetchone()
    if project != (vm,):
        raise ValueError("Project and workspace VM registrations differ")
    machine = db.execute("SELECT state,purpose FROM vms WHERE id=?", (vm,)).fetchone()
    if not machine or machine[0] not in ("created", "stopped") or machine[1] != "instance":
        raise ValueError("Stop and reconcile the borrowed user VM before deleting project data")
    disk = db.execute("SELECT size_gib,storage_root,phase,attached_vm_id,mount_path FROM micro_block_storages WHERE id=?", (volume,)).fetchone()
    if not disk or disk[2] != "ready" or (disk[3], disk[4]) not in ((None, None), (vm, "/workspace/shared")):
        raise ValueError("Borrowed volume is missing, busy, or attached elsewhere")
    if db.execute("SELECT 1 FROM project_backup_operations WHERE project_id=? AND active=1", (args.project,)).fetchone() or db.execute("SELECT 1 FROM project_backup_cleanups WHERE project_id=? AND active=1", (args.project,)).fetchone():
        raise ValueError("Finish pending backup or cleanup before deletion")
    if db.execute("SELECT 1 FROM project_workspace_imports WHERE phase!='complete'").fetchone():
        raise ValueError("Finish pending imports before deletion")
    if db.execute("SELECT 1 FROM project_workspace_imports WHERE project_id=? AND json_extract(document,'$.outcome')='abandoned' AND COALESCE(json_extract(document,'$.quarantineCleanup.phase'),'')!='complete'", (args.project,)).fetchone():
        raise ValueError("Clean abandoned import quarantine before deleting this project")
    if db.execute("SELECT 1 FROM ai_sandbox_environments WHERE project_id=? AND state!='deleted'", (args.project,)).fetchone() or db.execute("SELECT 1 FROM ai_sandbox_tunnels WHERE project_id=? AND (subdomain IS NOT NULL OR json_extract(spec,'$.state') NOT IN ('deleted','superseded') OR (json_extract(spec,'$.successorBindingId') IS NOT NULL AND json_extract(spec,'$.handoffConfirmedAt') IS NULL))", (args.project,)).fetchone():
        raise ValueError("Retire project environments and preview bindings first")
    route = db.execute("SELECT phase,document FROM sandbox_task_routes WHERE project_id=?", (args.project,)).fetchone()
    if route and route[0] != "project":
        raise ValueError("Complete the Sandbox task migration before deletion")
    if db.execute("SELECT 1 FROM ai_sandbox_task_index WHERE project_id=? AND busy=1", (args.project,)).fetchone():
        raise ValueError("Reconcile active controller-backed tasks before deletion")
    for (raw,) in db.execute("SELECT spec FROM ai_sandbox_tasks WHERE project_id=?", (args.project,)):
        saved = json.loads(raw)
        if saved["task"]["state"] in ("waiting", "accepted", "in_progress") or saved.get("guest_pending") or saved.get("guest_staged"):
            raise ValueError("Reconcile unsettled controller-backed tasks before deletion")
    for (raw,) in db.execute("SELECT spec FROM ai_sandbox_access"):
        policy = json.loads(raw)
        if not isinstance(policy.get("grants"), list) or any(not isinstance(grant, dict) or not isinstance(grant.get("resource"), dict) for grant in policy["grants"]):
            raise ValueError("Repair invalid Sandbox access policy before deletion")
    pool = pool_path(db, disk[1], args.pool)
    disk_dir = pool / "volumes" / volume
    owner_directory(pool / "volumes")
    owner_directory(disk_dir)
    disk_path = disk_dir / "data.ext4"
    mounted = mountpoints()
    if os.path.normpath(str(disk_dir)) in mounted or os.path.normpath(str(disk_path)) in mounted:
        raise ValueError("Borrowed volume path is a mount point")
    volume_identity(disk_path, volume, disk[0] << 30)
    parent = control.with_suffix(".sandbox")
    owner_directory(parent)
    owner_directory(parent / "projects")
    root = parent / "projects" / args.project
    owner_directory(root)
    if os.path.lexists(parent / "project-backups"):
        owner_directory(parent / "project-backups")
    backups = parent / "project-backups" / args.project
    if os.path.lexists(backups / "fence.json"):
        raise ValueError("Finish project maintenance before deletion")
    with closing(io.connect(root / "project.db")) as project_db:
        if project_db.execute("PRAGMA application_id").fetchone()[0] != 0x46575031:
            raise ValueError("Private project database has an unexpected application identity")
        identity = project_db.execute("SELECT project_id,database_id,volume_id,initialized FROM identity WHERE singleton=1").fetchone()
        if identity != (args.project, database, volume, 1):
            raise ValueError("Private project database identity differs from its registration")
        if project_db.execute("PRAGMA integrity_check").fetchall() != [("ok",)] or project_db.execute("PRAGMA foreign_key_check").fetchone():
            raise ValueError("Repair the private project database before deletion")
        tables = {r[0] for r in project_db.execute("SELECT name FROM sqlite_master WHERE type='table'")}
        if "ai_sandbox_task_index" in tables and project_db.execute("SELECT 1 FROM ai_sandbox_task_index WHERE busy=1").fetchone():
            raise ValueError("Reconcile active private tasks before deletion")
        marker = project_db.execute("SELECT operation,source_sha FROM sandbox_migration WHERE singleton=1").fetchone() if "sandbox_migration" in tables else None
        if route:
            route_document = json.loads(route[1])
            if marker != (route_document["operation"], route_document["source_sha"]):
                raise ValueError("Private task authority differs from its migration route")
        elif marker:
            raise ValueError("Private task authority has no registered route")
        if "ai_sandbox_tasks" in tables:
            for (raw,) in project_db.execute("SELECT spec FROM ai_sandbox_tasks"):
                saved = json.loads(raw)
                if saved["task"]["state"] in ("waiting", "accepted", "in_progress") or saved.get("guest_pending") or saved.get("guest_staged"):
                    raise ValueError("Reconcile unsettled private tasks before deletion")
        if "file_operations" in tables and project_db.execute("SELECT 1 FROM file_operations WHERE state='pending'").fetchone():
            raise ValueError("Reconcile pending project file writes before deletion")
    backup_counts = db.execute("SELECT COUNT(*),COALESCE(SUM(CASE WHEN json_extract(document,'$.public.protected')=1 THEN 1 ELSE 0 END),0) FROM project_backup_operations WHERE project_id=? AND json_extract(document,'$.public.request.restoreCheckpoint') IS NULL", (args.project,)).fetchone()
    roots = dict(workspace=str(root), backups=str(backups), attempts=str(parent / args.project))
    return dict(project=args.project, database=database, volume=volume, vm=vm, generation=generation,
                size=disk[0] << 30, storageId=disk[1], pool=str(pool), disk=str(disk_path), roots=roots,
                backupReceipts=backup_counts[0], protectedBackupReceipts=backup_counts[1])


def check_volume_directory(disk, operation):
    for entry in disk.parent.iterdir():
        if entry.name != "data.ext4" and not entry.name.startswith(".project-delete-" + operation + "-"):
            raise ValueError("Unknown volume artifact requires operator inspection")
        if entry.name != "data.ext4":
            with open_owned_file(entry):
                pass


def take_locks(control, project):
    parent = control.with_suffix(".sandbox")
    candidates = (parent / "projects" / project / "owner.lock", parent / "project-backups" / project / "service.lock", parent / "project-backups" / project / "operation.lock")
    locks = []
    try:
        for path in candidates:
            if not os.path.lexists(path):
                continue
            file = open_owned_file(path)
            fcntl.flock(file.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
            locks.append(file)
        return locks
    except BaseException:
        for file in locks:
            file.close()
        raise


def reset_volume(record, io):
    disk = Path(record["disk"])
    old = disk.parent / (".project-delete-" + record["id"] + "-old")
    stage = Path(record["stage"])
    if stage.parent != disk.parent or not stage.name.startswith(".project-delete-" + record["id"] + "-new-"):
        raise ValueError("Deletion volume staging path changed")
    def matches(path, proof):
        return os.path.lexists(path) and file_proof(path) == proof
    if os.path.lexists(old) and not matches(old, record["oldDisk"]):
        raise ValueError("Preserved original volume changed")
    if os.path.lexists(disk) and matches(disk, record["oldDisk"]):
        if os.path.lexists(old) or not matches(stage, record["blankDisk"]):
            raise ValueError("Volume reset staging differs from its saved proof")
        rename_new(disk, old)
        io.sync_dir(disk.parent)
    if not os.path.lexists(disk):
        if not matches(old, record["oldDisk"]) or not matches(stage, record["blankDisk"]):
            raise ValueError("Volume reset cannot locate its saved pair")
        rename_new(stage, disk)
        io.sync_dir(disk.parent)
    if not matches(disk, record["blankDisk"]) or os.path.lexists(stage):
        raise ValueError("Reset volume differs from its saved proof")
    volume_identity(disk, record["volume"], record["size"])
    return old


def prepare_blank(db, record, io):
    disk = Path(record["disk"])
    stage = disk.parent / (".project-delete-" + record["id"] + "-new-" + str(uuid.uuid4()))
    candidates = record.setdefault("blankCandidates", [])
    if len(candidates) >= 32:
        raise ValueError("Too many interrupted blank-volume stages require inspection")
    fd = os.open(stage, os.O_RDWR | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    created = os.fstat(fd)
    candidates.append(dict(path=str(stage), identity=[created.st_dev, created.st_ino]))
    try:
        transaction(db, lambda: save(db, record))
    except BaseException:
        os.close(fd)
        raise
    with os.fdopen(fd, "w+b") as file:
        file.truncate(record["size"])
        file.flush()
        os.fsync(file.fileno())
    io.sync_dir(disk.parent)
    result = subprocess.run(["mkfs.ext4", "-F", "-q", "-U", record["volume"], "-E", "lazy_itable_init=0,lazy_journal_init=0", str(stage)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
    if result.returncode != 0:
        raise ValueError("Could not create the blank replacement volume")
    volume_identity(stage, record["volume"], record["size"])
    with open_owned_file(stage) as file:
        os.fsync(file.fileno())
    record["stage"] = str(stage)
    record["blankDisk"] = file_proof(stage)
    record["phase"] = "prepared"
    transaction(db, lambda: save(db, record))


def metadata_commit(db, record):
    project, database, volume, vm, generation = (record[x] for x in ("project", "database", "volume", "vm", "generation"))
    def apply():
        row = db.execute("SELECT database_id,volume_id,vm_id,generation,provisioning FROM project_workspaces WHERE project_id=?", (project,)).fetchone()
        if row != (database, volume, vm, generation, 0):
            raise ValueError("Project binding changed during deletion")
        disk = db.execute("SELECT phase,attached_vm_id,mount_path FROM micro_block_storages WHERE id=?", (volume,)).fetchone()
        if not disk or disk[0] != "ready" or (disk[1], disk[2]) not in ((None, None), (vm, "/workspace/shared")):
            raise ValueError("Borrowed volume binding changed during deletion")
        db.execute("UPDATE micro_block_storages SET attached_vm_id=NULL,mount_path=NULL WHERE id=?", (volume,))
        db.execute("DELETE FROM sandbox_task_routes WHERE project_id=?", (project,))
        db.execute("DELETE FROM ai_sandbox_admissions WHERE task_id IN (SELECT id FROM ai_sandbox_tasks WHERE project_id=?)", (project,))
        db.execute("DELETE FROM ai_sandbox_attempts WHERE project_id=?", (project,))
        db.execute("DELETE FROM ai_sandbox_task_index WHERE project_id=?", (project,))
        db.execute("DELETE FROM ai_sandbox_tasks WHERE project_id=?", (project,))
        db.execute("DELETE FROM project_workspace_import_tasks WHERE operation_id IN (SELECT id FROM project_workspace_imports WHERE project_id=?)", (project,))
        db.execute("DELETE FROM project_workspace_imports WHERE project_id=?", (project,))
        db.execute("DELETE FROM sandbox_task_catalog WHERE project_id=?", (project,))
        db.execute("DELETE FROM ai_sandbox_environment_resources WHERE environment_id IN (SELECT id FROM ai_sandbox_environments WHERE project_id=?)", (project,))
        db.execute("DELETE FROM ai_sandbox_environments WHERE project_id=?", (project,))
        db.execute("DELETE FROM ai_sandbox_tunnels WHERE project_id=?", (project,))
        for table in ("project_backup_operations", "project_backup_cleanups", "project_backup_policies", "project_backup_retention"):
            db.execute("DELETE FROM " + table + " WHERE project_id=?", (project,))
        for principal, raw in db.execute("SELECT principal,spec FROM ai_sandbox_access").fetchall():
            policy = json.loads(raw)
            grants = policy["grants"]
            policy["grants"] = [g for g in grants if g.get("resource") != {"kind": "project", "id": project}]
            if len(grants) != len(policy["grants"]):
                db.execute("UPDATE ai_sandbox_access SET spec=? WHERE principal=?", (encode(policy), principal))
        db.execute("DELETE FROM project_workspaces WHERE project_id=?", (project,))
        db.execute("DELETE FROM ai_sandbox_projects WHERE id=?", (project,))
        record["metadataDone"] = True
        save(db, record)
    db.execute("PRAGMA secure_delete=ON")
    transaction(db, apply)


def validate_saved_paths(record, control, pool, io):
    project = io.identifier(record["project"])
    volume = io.identifier(record["volume"])
    io.identifier(record["database"])
    io.identifier(record["vm"])
    parent = control.with_suffix(".sandbox")
    expected = dict(workspace=str(parent / "projects" / project), backups=str(parent / "project-backups" / project), attempts=str(parent / project))
    if record["roots"] != expected or Path(record["disk"]) != pool / "volumes" / volume / "data.ext4":
        raise ValueError("Saved deletion paths differ from the resource identities")
    if record["pool"] != str(pool):
        raise ValueError("Saved storage root changed")
    info = os.stat(pool)
    if record["poolIdentity"] != [info.st_dev, info.st_ino]:
        raise ValueError("Storage root identity changed")
    owner_directory(parent)
    owner_directory(parent / "projects")
    if os.path.lexists(parent / "project-backups"):
        owner_directory(parent / "project-backups")
    owner_directory(pool / "volumes")
    owner_directory(Path(record["disk"]).parent)
    return {name: Path(path) for name, path in expected.items()}


def cleanup_candidates(record, io):
    disk = Path(record["disk"])
    for candidate in record.get("blankCandidates", []):
        path = Path(candidate["path"])
        if path.parent != disk.parent or not path.name.startswith(".project-delete-" + record["id"] + "-new-"):
            raise ValueError("Blank-volume staging path changed")
        if not os.path.lexists(path):
            continue
        with open_owned_file(path) as file:
            info = os.fstat(file.fileno())
            if [info.st_dev, info.st_ino] != candidate["identity"]:
                raise ValueError("Blank-volume staging identity changed")
        os.unlink(path)
        io.sync_dir(disk.parent)


def delete_project(args, io):
    """Remove project-owned data, retain the VM and replace the borrowed disk with a blank filesystem."""
    if not sys.platform.startswith("linux"):
        raise ValueError("Project deletion requires Linux")
    control = Path(args.control).absolute()
    with io.private_file(control):
        pass
    control = control.resolve(strict=True)
    lock = os.open(control, os.O_RDWR | os.O_NOFOLLOW)
    locks = []
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        db = rw_database(control)
        try:
            exists = db.execute("SELECT 1 FROM sqlite_master WHERE type='table' AND name='project_workspace_deletions'").fetchone()
            if not exists:
                raise ValueError("Initialize the controller with deletion support, then stop it")
            previous = db.execute("SELECT project_id,phase,document FROM project_workspace_deletions WHERE id=?", (args.operation,)).fetchone()
            if previous:
                record = json.loads(previous[2])
                if (record.get("project"), record.get("phase"), record.get("id")) != (previous[0], previous[1], args.operation):
                    raise ValueError("Deletion journal identity or phase changed")
                if record["project"] != args.project or record["generation"] != args.generation or record["control"] != str(control) or record["pool"] != str(Path(args.pool).resolve(strict=True)):
                    raise ValueError("Deletion retry differs from its original request")
                if record["phase"] == "complete":
                    print(encode(dict(status="complete", operationId=args.operation, projectId=args.project, volumeId=record["volume"], vmId=record["vm"])))
                    return
                if args.confirm_delete and args.confirm_plan != record["previewDigest"]:
                    raise ValueError("Deletion retry must acknowledge the saved preview digest")
                if not args.confirm_delete:
                    print(encode(dict(status=record["phase"], operationId=args.operation, projectId=args.project)))
                    return
            if not shutil.which("mkfs.ext4") or not getattr(ctypes.CDLL(None), "renameat2", None):
                raise ValueError("Project deletion requires mkfs.ext4 and Linux renameat2 support")
            if not args.confirm_vm_stopped:
                raise ValueError("Confirm that the borrowed VM and its processes are stopped")
            locks = take_locks(control, args.project)
            if not previous:
                if db.execute("SELECT 1 FROM project_workspace_deletions WHERE project_id=? OR phase!='complete'", (args.project,)).fetchone():
                    raise ValueError("Another project deletion is retained or in progress")
                info = preflight(db, args, control, io)
                pool = Path(info["pool"])
                disk = Path(info["disk"])
                if any(disk.parent.iterdir()):
                    for entry in disk.parent.iterdir():
                        if entry.name != "data.ext4":
                            raise ValueError("Volume directory has unknown artifacts; inspect them first")
                files, directories = inventory({name: Path(path) for name, path in info["roots"].items()})
                old_disk = file_proof(disk)
                count = len(files)
                size = sum(item["identity"][2] for item in files) + old_disk["identity"][2]
                pool_info = os.stat(pool)
                pool_identity = [pool_info.st_dev, pool_info.st_ino]
                preview = dict(operation=args.operation, control=str(control), project=info, poolIdentity=pool_identity, oldDisk=old_disk, files=files, directories=directories)
                preview_digest = hashlib.sha256(encode(preview).encode("utf-8")).hexdigest()
                if not args.confirm_delete:
                    sample = [dict(area=item["role"], path=item["path"], bytes=item["identity"][2]) for item in files[:100]]
                    print(encode(dict(status="preview", projectId=args.project, databaseId=info["database"], volumeId=info["volume"], vmId=info["vm"], generation=info["generation"], projectFileCount=count, logicalBytes=size, backupReceipts=info["backupReceipts"], protectedBackupReceipts=info["protectedBackupReceipts"], previewDigest=preview_digest, files=sample, filesTruncated=count > len(sample), vmPreserved=True, volumePreserved=True, volumeContentsReset=True)))
                    return
                if args.confirm_plan != preview_digest:
                    raise ValueError("Deletion plan changed or the preview digest was not confirmed; preview again")
                record = dict(id=args.operation, phase="staging", control=str(control), poolIdentity=pool_identity, previewDigest=preview_digest, oldDisk=old_disk, files=files, directories=directories, fileCount=count, logicalBytes=size, **info)
                if len(encode(record).encode()) > 32 * 1024 * 1024:
                    raise ValueError("Deletion inventory exceeds its storage bound")
                old = disk.parent / (".project-delete-" + args.operation + "-old")
                if os.path.lexists(old):
                    raise ValueError("Deletion original-volume path already exists")
                transaction(db, lambda: db.execute("INSERT INTO project_workspace_deletions VALUES(?,?,'staging',?)", (args.operation, args.project, encode(record))))
            pool = pool_path(db, record["storageId"], args.pool)
            roots = validate_saved_paths(record, control, pool, io)
            disk = Path(record["disk"])
            mounted = mountpoints()
            if os.path.normpath(str(disk.parent)) in mounted or os.path.normpath(str(disk)) in mounted:
                raise ValueError("Borrowed volume path became a mount point")
            allowed = {"data.ext4", ".project-delete-" + record["id"] + "-old"}
            for candidate in record.get("blankCandidates", []):
                path = Path(candidate["path"])
                if path.parent != disk.parent or not path.name.startswith(".project-delete-" + record["id"] + "-new-"):
                    raise ValueError("Saved blank-volume path changed")
                allowed.add(path.name)
            for entry in disk.parent.iterdir():
                if entry.name not in allowed:
                    raise ValueError("Unknown volume artifact prevents deletion")
            check_inventory(roots, record["files"], record["directories"])
            if record["phase"] == "staging":
                if file_proof(disk) != record["oldDisk"]:
                    raise ValueError("Original volume changed before deletion")
                prepare_blank(db, record, io)
            if record["phase"] == "prepared":
                record["phase"] = "erasing"
                transaction(db, lambda: save(db, record))
            if record["phase"] == "erasing":
                old = reset_volume(record, io)
                check_inventory(roots, record["files"], record["directories"])
                for item in record["files"]:
                    path = roots[item["role"]] / item["path"]
                    if not os.path.lexists(path):
                        continue
                    if file_proof(path) != {"identity": item["identity"], "sha": item["sha"]}:
                        raise ValueError("Project file changed during deletion")
                    os.unlink(path)
                    io.sync_dir(path.parent)
                for item in record["directories"]:
                    path = roots[item["role"]] / item["path"]
                    if not os.path.lexists(path):
                        continue
                    info = owner_directory(path)
                    if [info.st_dev, info.st_ino] != item["identity"]:
                        raise ValueError("Project directory changed during deletion")
                    os.rmdir(path)
                    io.sync_dir(path.parent)
                for root in roots.values():
                    if os.path.lexists(root):
                        raise ValueError("Project directory still exists after deletion")
                if os.path.lexists(old):
                    if file_proof(old) != record["oldDisk"]:
                        raise ValueError("Original volume changed during deletion")
                    os.unlink(old)
                    io.sync_dir(old.parent)
                cleanup_candidates(record, io)
                record["phase"] = "finishing"
                transaction(db, lambda: save(db, record))
            if record["phase"] == "finishing":
                if any(os.path.lexists(root) for root in roots.values()):
                    raise ValueError("Project data reappeared before metadata commit")
                if {entry.name for entry in disk.parent.iterdir()} != {"data.ext4"}:
                    raise ValueError("Unexpected volume artifact remains before metadata commit")
                if file_proof(disk) != record["blankDisk"]:
                    raise ValueError("Blank borrowed volume changed before metadata commit")
                if not record.get("metadataDone"):
                    metadata_commit(db, record)
                checkpoint = db.execute("PRAGMA wal_checkpoint(TRUNCATE)").fetchone()
                if checkpoint and checkpoint[0] != 0:
                    raise ValueError("Controller database WAL could not be checkpointed; retry after readers stop")
                record["phase"] = "complete"
                record["completedAt"] = time.time_ns() // 1_000_000
                # The tombstone keeps resource IDs, not deleted filenames or file hashes.
                record.pop("files", None)
                record.pop("directories", None)
                record.pop("blankCandidates", None)
                record.pop("oldDisk", None)
                record.pop("blankDisk", None)
                record.pop("stage", None)
                transaction(db, lambda: save(db, record))
            if record["phase"] != "complete":
                raise ValueError("Unknown deletion phase")
            print(encode(dict(status="complete", operationId=args.operation, projectId=args.project, volumeId=record["volume"], vmId=record["vm"])))
        finally:
            for file in locks:
                file.close()
            db.close()
    finally:
        os.close(lock)


def deletion_status(args, io):
    """Read receipts without exposing private inventory or modifying the controller."""
    db = io.connect(Path(args.control).absolute())
    try:
        if args.operation:
            rows = db.execute("SELECT id,project_id,phase,document FROM project_workspace_deletions WHERE id=?", (args.operation,)).fetchall()
            if not rows:
                raise ValueError("Deletion operation was not found")
        else:
            rows = db.execute("SELECT id,project_id,phase,document FROM project_workspace_deletions WHERE phase!='complete' ORDER BY rowid LIMIT 101").fetchall()
            if len(rows) > 100:
                raise ValueError("Too many incomplete deletions")
        receipts = []
        for operation, project, phase, raw in rows:
            record = json.loads(raw)
            if (record.get("id"), record.get("project"), record.get("phase")) != (operation, project, phase):
                raise ValueError("Deletion receipt identity changed")
            receipts.append(dict(operationId=operation, projectId=project, phase=phase, volumeId=record["volume"], vmId=record["vm"], generation=record["generation"], fileCount=record["fileCount"], logicalBytes=record["logicalBytes"], previewDigest=record["previewDigest"], completedAt=record.get("completedAt")))
        print(encode(dict(deletions=receipts, inspectionOnly=True)))
    finally:
        db.close()
