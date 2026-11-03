"""Offline, resumable installation into a fresh destination project and registered storage pool.

Only the command-line entry point invokes this module. No import is performed while
editing or building Firecrab. The destination controller must be stopped throughout.
"""
import copy
import ctypes
import fcntl
import json
import os
from pathlib import Path
import shutil
import sqlite3
import stat
import subprocess
import sys
import time
import uuid
from urllib.parse import quote

TASK_TABLES = ("ai_sandbox_tasks", "ai_sandbox_attempts", "ai_sandbox_admissions", "ai_sandbox_task_index")
TERMINAL_STATES = {"result", "merged", "failed", "cancelled", "interrupted"}


def encode(value):
    return json.dumps(value, separators=(",", ":"), ensure_ascii=False)


def rw_database(path):
    connection = sqlite3.connect("file:" + quote(str(path), safe="/") + "?mode=rw", uri=True, isolation_level=None)
    connection.execute("PRAGMA synchronous=FULL")
    connection.execute("PRAGMA foreign_keys=ON")
    connection.execute("PRAGMA busy_timeout=10000")
    return connection


def transaction(db, work):
    db.execute("BEGIN IMMEDIATE")
    try:
        result = work()
        db.execute("COMMIT")
        return result
    except BaseException:
        db.execute("ROLLBACK")
        raise


def rename_new(source, destination):
    """Linux publishes without ever replacing an existing filesystem entry."""
    libc = ctypes.CDLL(None, use_errno=True)
    rename = getattr(libc, "renameat2", None)
    if rename is None:
        raise ValueError("This importer requires Linux renameat2 support")
    rename.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
    rename.restype = ctypes.c_int
    if rename(-100, os.fsencode(source), -100, os.fsencode(destination), 1) != 0:
        code = ctypes.get_errno()
        raise OSError(code, os.strerror(code))


def ensure_directory(path, io):
    try:
        os.mkdir(path, 0o700)
        io.sync_dir(path.parent)
    except FileExistsError:
        pass
    io.private_dir(path)


def save(db, record):
    db.execute("UPDATE project_workspace_imports SET phase=?,document=? WHERE id=?", (record["phase"], encode(record), record["id"]))


def inspect_source(directory, io, files=None):
    if files is None:
        if os.path.lexists(directory / "INCOMPLETE"):
            raise ValueError("The transferred directory is incomplete")
        if {entry.name for entry in directory.iterdir()} != set(io.FILES):
            raise ValueError("Transferred package must contain only the four matched files, without SQLite sidecars")
        files = {name: directory / name for name in io.FILES}
    manifest = io.inspect_files(files)
    c = manifest["checkpoint"]
    project = c["projectId"]
    source = io.connect(files["control.db"])
    project_db = io.connect(files["project.db"])
    try:
        row = source.execute("SELECT vm_id,spec FROM ai_sandbox_projects WHERE id=?", (project,)).fetchone()
        if not row:
            raise ValueError("The source project registration is missing")
        config = json.loads(row[1])
        if config.get("id") != project or not isinstance(config.get("spec"), dict) or config["spec"].get("vmId") != row[0]:
            raise ValueError("The source project registration differs from its backup")
        vm = source.execute("SELECT vm_id FROM project_workspaces WHERE project_id=?", (project,)).fetchone()
        if vm != (row[0],):
            raise ValueError("Source project and workspace have different VM owners")
        route_row = source.execute("SELECT phase,document FROM sandbox_task_routes WHERE project_id=?", (project,)).fetchone()
        route = None
        if route_row:
            if route_row[0] != "project":
                raise ValueError("Finish the source Sandbox migration before export")
            route = json.loads(route_row[1])
            marker = project_db.execute("SELECT operation,source_sha FROM sandbox_migration WHERE singleton=1").fetchone()
            if marker != (route["operation"], route["source_sha"]):
                raise ValueError("Canonical task database and migration route do not match")
            if route["binding"]["database_id"] != c["databaseId"] or route["binding"]["volume_id"] != c["volumeId"]:
                raise ValueError("The migration route refers to other resources")
            canonical = project_db
        else:
            tables = {row[0] for row in project_db.execute("SELECT name FROM sqlite_master WHERE type='table'")}
            if "sandbox_migration" in tables and project_db.execute("SELECT 1 FROM sandbox_migration").fetchone():
                raise ValueError("Unregistered canonical task marker in the source")
            if "ai_sandbox_tasks" in tables and project_db.execute("SELECT 1 FROM ai_sandbox_tasks").fetchone():
                raise ValueError("Private tasks have no canonical migration route")
            canonical = source
        tables = {row[0] for row in project_db.execute("SELECT name FROM sqlite_master WHERE type='table'")}
        if "file_operations" in tables and project_db.execute("SELECT 1 FROM file_operations WHERE state='pending'").fetchone():
            raise ValueError("Pending project file mutations must be reconciled before transfer")
        catalog = source.execute("SELECT ordinal,id,project_id,attempt_id,ready FROM sandbox_task_catalog WHERE project_id=? ORDER BY ordinal", (project,)).fetchall()
        tasks = canonical.execute("SELECT id,spec FROM ai_sandbox_tasks WHERE project_id=?", (project,)).fetchall()
        actual = set()
        by_id = {row[1]: row for row in catalog}
        for key, raw in tasks:
            saved = json.loads(raw)
            task = saved["task"]
            io.identifier(key)
            if task["id"] != key or task["projectId"] != project or task["state"] not in TERMINAL_STATES or saved.get("guest_pending") or saved.get("guest_staged"):
                raise ValueError("Only settled task history can be imported")
            catalog_row = by_id.get(key)
            attempt = canonical.execute("SELECT project_id,attempt_id FROM ai_sandbox_attempts WHERE task_id=?", (key,)).fetchone()
            admission = canonical.execute("SELECT 1 FROM ai_sandbox_admissions WHERE task_id=?", (key,)).fetchone()
            index = canonical.execute("SELECT ordinal,busy FROM ai_sandbox_task_index WHERE id=?", (key,)).fetchone()
            if not catalog_row or catalog_row[4] != 1 or attempt != (project, task["attemptId"]) or catalog_row[3] != task["attemptId"] or not admission or index != (catalog_row[0], 0):
                raise ValueError("Task, attempt, admission, ordinal or idle index differs")
            actual.add(key)
        for ordinal, key, owner, attempt, ready in catalog:
            io.identifier(key)
            io.identifier(attempt)
            if ordinal <= 0 or owner != project:
                raise ValueError("Invalid source task reservation")
            if key not in actual and ready:
                raise ValueError("Published task reservation has no canonical history")
        if canonical is project_db:
            for table in ("ai_sandbox_tasks", "ai_sandbox_attempts", "ai_sandbox_task_index"):
                if canonical.execute("SELECT 1 FROM " + table + " WHERE project_id!=? LIMIT 1", (project,)).fetchone():
                    raise ValueError("Private task DB includes another project's records")
        return manifest, config, route, catalog, actual
    finally:
        source.close()
        project_db.close()


def validate_destination(db, args, project, database, volume, catalog):
    vm = db.execute("SELECT state,purpose,last_runtime_id FROM vms WHERE id=?", (args.vm,)).fetchone()
    if vm != ("created", "instance", None):
        raise ValueError("Choose a newly created user VM that has never been started")
    if db.execute("SELECT 1 FROM ai_sandbox_projects WHERE id=? OR vm_id=?", (project, args.vm)).fetchone():
        raise ValueError("Destination project ID or VM is already assigned")
    if db.execute("SELECT 1 FROM project_workspaces WHERE project_id=? OR database_id=? OR volume_id=? OR vm_id=?", (project, database, volume, args.vm)).fetchone():
        raise ValueError("A destination workspace already owns one of these resources")
    if db.execute("SELECT 1 FROM micro_block_storages WHERE id=? OR name=? OR (attached_vm_id=? AND mount_path='/workspace/shared')", (volume, args.volume_name, args.vm)).fetchone():
        raise ValueError("Destination volume identity, name or mount is already registered")
    if db.execute("SELECT 1 FROM sandbox_task_routes WHERE project_id=?", (project,)).fetchone():
        raise ValueError("Destination already has a task storage route")
    if db.execute("SELECT 1 FROM sandbox_task_catalog WHERE project_id=?", (project,)).fetchone():
        raise ValueError("Destination already reserves task identities for this project")
    for ordinal, key, _, attempt, _ in catalog:
        if db.execute("SELECT 1 FROM sandbox_task_catalog WHERE ordinal=? OR id=? OR attempt_id=?", (ordinal, key, attempt)).fetchone():
            raise ValueError("Task ID, attempt ID or ordinal collides; choose an empty task catalog destination")
        if db.execute("SELECT 1 FROM ai_sandbox_tasks WHERE id=? OR rowid=?", (key, ordinal)).fetchone() or db.execute("SELECT 1 FROM ai_sandbox_attempts WHERE task_id=? OR attempt_id=?", (key, attempt)).fetchone():
            raise ValueError("Destination already contains one of these attempts")
        if db.execute("SELECT 1 FROM ai_sandbox_admissions WHERE task_id=?", (key,)).fetchone() or db.execute("SELECT 1 FROM ai_sandbox_task_index WHERE id=?", (key,)).fetchone():
            raise ValueError("Destination retains a conflicting task admission or index")
    mounts = db.execute("SELECT mount_path FROM micro_block_storages WHERE attached_vm_id=?", (args.vm,)).fetchall()
    target = "/workspace/shared"
    if len(mounts) >= 8 or any(path == target or path.startswith(target + "/") or target.startswith(path.rstrip("/") + "/") for (path,) in mounts):
        raise ValueError("Destination VM has overlapping mounts or no free disk slot")
    # A user VM cannot simultaneously be owned by a service or cluster placement.
    def mentions(value):
        if isinstance(value, dict):
            return any((key in ("vmId", "vm_id") and item == args.vm) or mentions(item) for key, item in value.items())
        if isinstance(value, list):
            return any(mentions(item) for item in value)
        return value == args.vm
    if db.execute("SELECT 1 FROM cluster_worker_receipts WHERE id=?", (args.vm,)).fetchone():
        raise ValueError("Destination VM is a cluster worker")
    for table, column in (("micro_service_deployments", "deployment"), ("cluster_worker_receipts", "data"), ("cluster_placements", "data"), ("cluster_services", "data"), ("cluster_service_workers", "data")):
        for (raw,) in db.execute("SELECT " + column + " FROM " + table):
            if mentions(json.loads(raw)):
                raise ValueError("Destination VM has a service or cluster owner")
    pool = db.execute("SELECT path FROM micro_storages WHERE id=?", (args.storage,)).fetchone()
    if not pool:
        raise ValueError("Use a destination MicroStorage UUID registered in this controller")
    path = Path(pool[0])
    if not path.is_absolute():
        raise ValueError("The registered destination pool must use an absolute path")
    info = os.lstat(path)
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.geteuid() or info.st_mode & 0o022:
        raise ValueError("Destination pool must be owner-controlled and not writable by others")
    return path.resolve(strict=True)


def remap_catalog(db, catalog):
    """Reserve ordered history after all existing destination ordinals."""
    if not catalog:
        return catalog
    maximum = db.execute("SELECT COALESCE(MAX(ordinal),0) FROM sandbox_task_catalog").fetchone()[0]
    maximum = max(maximum, db.execute("SELECT COALESCE(MAX(rowid),0) FROM ai_sandbox_tasks").fetchone()[0])
    sequence = db.execute("SELECT seq FROM sqlite_sequence WHERE name='sandbox_task_catalog'").fetchone()
    maximum = max(maximum, sequence[0] if sequence else 0)
    if maximum + len(catalog) > (1 << 63) - 1:
        raise ValueError("Destination task catalog ordinal space is exhausted")
    return [(maximum + index, key, project, attempt, ready)
            for index, (_, key, project, attempt, ready) in enumerate(catalog, 1)]


def planned_catalog(catalog, mapping):
    """The journal owns the mapping, including reservations without a task row."""
    if set(mapping) != {row[1] for row in catalog}:
        raise ValueError("Retained task ordinal mapping differs from source history")
    values = [mapping[row[1]] for row in catalog]
    if any(type(value) is not int or value <= 0 or value >= (1 << 63) for value in values) or values != sorted(set(values)):
        raise ValueError("Retained task ordinal mapping is invalid")
    return [(mapping[key], key, project, attempt, ready)
            for _, key, project, attempt, ready in catalog]

def new_stage(source, folder, record, role, io):
    path = folder / (".import-" + record["id"] + "-" + role + "-" + str(uuid.uuid4()))
    io.copy_file(source, path)
    return path


def sanitize_project_database(path, project, io, ordinals):
    """Keep historical bytes and private drafts; revoke all source-host execution leases."""
    db = rw_database(path)
    try:
        db.execute("PRAGMA journal_mode=DELETE")
        tables = {r[0] for r in db.execute("SELECT name FROM sqlite_master WHERE type='table'")}
        def change():
            if "terminal_sessions" in tables:
                for key, raw in db.execute("SELECT id,record FROM terminal_sessions").fetchall():
                    value = json.loads(raw)
                    value.update(status="interrupted", proof=None, cleanup_required=False)
                    db.execute("UPDATE terminal_sessions SET record=? WHERE id=?", (encode(value), key))
                db.execute("UPDATE terminal_controls SET generation=generation+1,owner=NULL,caller=NULL,client=NULL,token_sha=NULL,expires=0")
            if "private_editors" in tables:
                for key, raw in db.execute("SELECT id,record FROM private_editors").fetchall():
                    value = json.loads(raw)
                    value.update(status="interrupted", proof=None, token=uuid.uuid4().hex + uuid.uuid4().hex)
                    db.execute("UPDATE private_editors SET record=? WHERE id=?", (encode(value), key))
            if "ai_sandbox_tasks" in tables:
                clear_task_paths(db, project)
                for key, ordinal in db.execute("SELECT id,ordinal FROM ai_sandbox_task_index WHERE project_id=?", (project,)).fetchall():
                    if key not in ordinals:
                        raise ValueError("Private task index has no retained destination ordinal")
                    db.execute("UPDATE ai_sandbox_task_index SET ordinal=? WHERE id=?", (ordinals[key], key))
        transaction(db, change)
        if db.execute("PRAGMA integrity_check").fetchall() != [("ok",)] or db.execute("PRAGMA foreign_key_check").fetchone():
            raise ValueError("Prepared destination DB failed integrity checks")
    finally:
        db.close()
    with io.private_file(path) as file:
        os.fsync(file.fileno())


def clear_task_paths(db, project):
    # Workspace directories from the old host must never resolve to data on this host.
    for key, raw in db.execute("SELECT id,spec FROM ai_sandbox_tasks WHERE project_id=?", (project,)).fetchall():
        index = db.execute("SELECT * FROM ai_sandbox_task_index WHERE id=?", (key,)).fetchone()
        value = json.loads(raw)
        value["workspace"] = None
        db.execute("UPDATE ai_sandbox_tasks SET spec=? WHERE id=?", (encode(value), key))
        if not index:
            raise ValueError("Imported task index is missing")
        # The existing UPDATE trigger removes its derived index row.
        db.execute("INSERT INTO ai_sandbox_task_index VALUES(?,?,?,?,?,?)", index)


def prepare_files(db, source, record, io):
    folders = record["folders"]
    for key in ("project", "volume", "backup", "archive"):
        path = Path(folders[key])
        ensure_directory(path, io)
        allowed = {"project": {"project.db"}, "volume": {"data.ext4"}, "backup": {record["checkpoint"]}, "archive": set(io.FILES)}[key]
        for child in path.iterdir():
            if child.name not in allowed:
                raise ValueError("Unknown destination artifact requires operator inspection")
    for key in ("projectStage", "volumeStage", "archiveStage"):
        ensure_directory(Path(folders[key]), io)
        for child in Path(folders[key]).iterdir():
            if not child.name.startswith(".import-" + record["id"] + "-"):
                raise ValueError("Unknown file in retained staging directory")
            with io.private_file(child):
                pass
    if sum(1 for key in ("projectStage", "volumeStage", "archiveStage") for entry in Path(folders[key]).iterdir()) > 128:
        raise ValueError("Too many retained partial copies; inspect them before retrying")
    planned = []
    c = record["manifest"]["checkpoint"]
    expected = {"project.db": c["databaseSha256"], "data.ext4": c["volumeSha256"], "control.db": record["manifest"]["controlDatabaseSha256"], "manifest.json": record["request"]["manifestSha256"]}
    for name in io.FILES:
        stage = new_stage(source / name, Path(folders["archiveStage"]), record, "archive", io)
        sha = io.digest(stage)
        if sha != expected[name]:
            raise ValueError("Source archive changed while preparing destination files")
        planned.append({"stage": str(stage), "path": str(Path(folders["archive"]) / name), "sha": sha})
    live = new_stage(source / "project.db", Path(folders["projectStage"]), record, "database", io)
    if io.digest(live) != expected["project.db"]:
        raise ValueError("Project source changed during copy")
    sanitize_project_database(live, record["project"], io, record.get("taskOrdinals", {}))
    planned.append({"stage": str(live), "path": str(Path(folders["project"]) / "project.db"), "sha": io.digest(live)})
    disk = new_stage(source / "data.ext4", Path(folders["volumeStage"]), record, "volume", io)
    if io.digest(disk) != expected["data.ext4"]:
        raise ValueError("Volume source changed during copy")
    # Replay the stopped guest's journal and safe fixes only on the disposable live copy.
    # The protected original archive remains byte-for-byte identical to the checkpoint.
    result = subprocess.run(["e2fsck", "-f", "-p", str(disk)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
    if result.returncode not in (0, 1):
        raise ValueError("Destination ext4 copy needs operator filesystem repair")
    with io.private_file(disk) as file:
        os.fsync(file.fileno())
    planned.append({"stage": str(disk), "path": str(Path(folders["volume"]) / "data.ext4"), "sha": io.digest(disk)})
    for folder in folders.values():
        io.sync_dir(Path(folder))
    record["files"] = planned
    record["phase"] = "prepared"
    transaction(db, lambda: save(db, record))


def validate_file_plan(record, io):
    folders = {key: Path(value) for key, value in record["folders"].items()}
    allowed = {folders["archive"] / name: folders["archiveStage"] for name in io.FILES}
    allowed.update({folders["project"] / "project.db": folders["projectStage"], folders["volume"] / "data.ext4": folders["volumeStage"]})
    if len(record["files"]) != len(allowed) or {Path(item["path"]) for item in record["files"]} != set(allowed):
        raise ValueError("Retained import file set differs from its destination identities")
    for item in record["files"]:
        stage, path = Path(item["stage"]), Path(item["path"])
        if stage.parent != allowed[path] or not stage.name.startswith(".import-" + record["id"] + "-"):
            raise ValueError("Retained import stage is outside its private staging directory")
    # Validate the entire remaining set before publishing another member of the pair.
    for item in record["files"]:
        stage, path = Path(item["stage"]), Path(item["path"])
        if os.path.lexists(path):
            if os.path.lexists(stage) or io.digest(path) != item["sha"]:
                raise ValueError("A destination file conflicts with the retained installation plan")
        elif io.digest(stage) != item["sha"]:
            raise ValueError("Prepared file differs from its retained proof")
    return {Path(item["path"]): Path(item["path"]) if os.path.lexists(item["path"]) else Path(item["stage"]) for item in record["files"]}


def install_files(record, io):
    validate_file_plan(record, io)
    for item in record["files"]:
        stage, path = Path(item["stage"]), Path(item["path"])
        if not os.path.lexists(path):
            rename_new(stage, path)
        io.sync_dir(path.parent)
        io.sync_dir(stage.parent)


def prepared_archive(db, control, args, record, io):
    """Resolve only journaled files beneath the original owner-controlled namespaces."""
    c = record["manifest"]["checkpoint"]
    for name in ("id", "projectId", "databaseId", "volumeId"):
        io.identifier(c[name])
    if c["id"] != record["checkpoint"] or c["projectId"] != record["project"]:
        raise ValueError("Import journal and checkpoint identities differ")
    row = db.execute("SELECT path FROM micro_storages WHERE id=?", (args.storage,)).fetchone()
    if not row or not Path(row[0]).is_absolute():
        raise ValueError("Registered storage is unavailable")
    pool = Path(row[0])
    info = os.lstat(pool)
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.geteuid() or info.st_mode & 0o022:
        raise ValueError("Registered storage is not owner-controlled")
    pool = pool.resolve(strict=True)
    info = os.stat(pool)
    if record["poolIdentity"] != [info.st_dev, info.st_ino]:
        raise ValueError("Destination filesystem identity changed")
    parent = control.with_suffix(".sandbox")
    projects, backups, volumes = parent / "projects", parent / "project-backups", pool / "volumes"
    expected = dict(project=str(projects / c["projectId"]), volume=str(volumes / c["volumeId"]), backup=str(backups / c["projectId"]), archive=str(backups / c["projectId"] / c["id"]), projectStage=str(projects / (".project-import-" + args.operation)), volumeStage=str(volumes / (".project-import-" + args.operation)), archiveStage=str(backups / (".project-import-" + args.operation)))
    if record["folders"] != expected:
        raise ValueError("Import journal paths differ from registered resources")
    for path in (parent, projects, backups):
        io.private_dir(path)
    volume_info = os.lstat(volumes)
    if not stat.S_ISDIR(volume_info.st_mode) or volume_info.st_uid != os.geteuid() or volume_info.st_mode & 0o022:
        raise ValueError("Volume namespace is not owner-controlled")
    for path in expected.values():
        io.private_dir(Path(path))
    resolved = validate_file_plan(record, io)
    archive = Path(expected["archive"])
    return {name: resolved[archive / name] for name in io.FILES}


def register(db, args, record, config, route, catalog, actual, io):
    c = record["manifest"]["checkpoint"]
    project = record["project"]
    b = dict(project_id=project, database_id=c["databaseId"], volume_id=c["volumeId"], vm_id=args.vm, generation=record["generation"], provisioning=False, verified_runtime_id=None)
    config = copy.deepcopy(config)
    config["spec"].update(vmId=args.vm, sourcePath=None, selection=None, environmentId=None)
    local = {"principal": "local", "operator": True}
    ordinals = {row[1]: row[0] for row in catalog}
    def write():
        pool = validate_destination(db, args, project, c["databaseId"], c["volumeId"], catalog)
        if pool != Path(record["folders"]["volume"]).parent.parent:
            raise ValueError("Registered storage pool path changed")
        db.execute("INSERT INTO ai_sandbox_projects(id,vm_id,spec) VALUES(?,?,?)", (project, args.vm, encode(config)))
        db.execute("INSERT INTO micro_block_storages(id,name,size_gib,storage_root,phase,attached_vm_id,mount_path) VALUES(?,?,?,?,'ready',?,'/workspace/shared')", (c["volumeId"], args.volume_name, record["manifest"]["volumeBytes"] >> 30, args.storage, args.vm))
        db.execute("INSERT INTO project_workspaces VALUES(?,?,?,?,?,0,NULL)", (project, c["databaseId"], c["volumeId"], args.vm, record["generation"]))
        if route:
            new_route = copy.deepcopy(route)
            new_route["binding"] = b
            db.execute("INSERT INTO sandbox_task_routes VALUES(?,'project',?)", (project, encode(new_route)))
        else:
            controller = io.connect(Path(record["folders"]["archive"]) / "control.db")
            try:
                for table in TASK_TABLES:
                    if table == "ai_sandbox_admissions":
                        rows = controller.execute("SELECT a.* FROM ai_sandbox_admissions a JOIN ai_sandbox_tasks t ON t.id=a.task_id WHERE t.project_id=?", (project,))
                    else:
                        rows = controller.execute("SELECT * FROM " + table + " WHERE project_id=?", (project,))
                    for row in rows:
                        columns = ",".join('"' + d[0] + '"' for d in rows.description)
                        if table == "ai_sandbox_tasks":
                            key = row[0]
                            ordinal = ordinals[key]
                            db.execute("INSERT INTO ai_sandbox_tasks(rowid," + columns + ") VALUES(" + ",".join("?" for _ in range(len(row) + 1)) + ")", (ordinal,) + row)
                        else:
                            if table == "ai_sandbox_task_index":
                                row = row[:2] + (ordinals[row[0]],) + row[3:]
                            db.execute("INSERT INTO " + table + "(" + columns + ") VALUES(" + ",".join("?" for _ in row) + ")", row)
                clear_task_paths(db, project)
            finally:
                controller.close()
        for ordinal, key, owner, attempt, _ in catalog:
            db.execute("INSERT INTO sandbox_task_catalog VALUES(?,?,?,?,?)", (ordinal, key, owner, attempt, int(key in actual)))
        for key in actual:
            db.execute("INSERT INTO project_workspace_import_tasks VALUES(?,?)", (key, record["id"]))
        now = time.time_ns() // 1_000_000
        backup_public = dict(id=c["id"], projectId=project, request=dict(expectedGeneration=record["generation"], restoreCheckpoint=None), phase="complete", pendingReconciliation=False, protected=True, cleanupId=None, cleanedAt=None, createdAt=c["createdAt"], updatedAt=now, manifest=record["manifest"], restoredGeneration=None, error=None)
        backup = dict(public=backup_public, binding=b, caller=local, active=False, scheduled=False, original=None)
        db.execute("INSERT INTO project_backup_operations VALUES(?,?,0,?)", (c["id"], project, encode(backup)))
        previous = dict(b, generation=c["generation"])
        public = dict(backup_public, id=record["id"], protected=False, request=dict(expectedGeneration=c["generation"], restoreCheckpoint=c["id"]), createdAt=now, restoredGeneration=record["generation"])
        receipt = dict(public=public, binding=previous, caller=local, active=False, scheduled=False, original=dict(database=None, volume=None, wal=None, shm=None))
        db.execute("INSERT INTO project_backup_operations VALUES(?,?,0,?)", (record["id"], project, encode(receipt)))
        record["phase"] = "complete"
        record["completedAt"] = now
        save(db, record)
    transaction(db, write)


def check_abandon_contents(role, path, checkpoint, operation, io, prepared):
    """Reject unexpected entries without deleting retained staging or installed files."""
    entries = list(path.iterdir())
    if role in ("project", "volume"):
        allowed = {"project.db"} if role == "project" and prepared else {"data.ext4"} if role == "volume" and prepared else set()
        for entry in entries:
            if entry.name not in allowed:
                raise ValueError("Unknown live resource artifact prevents abandonment")
            with io.private_file(entry):
                pass
    if role == "backup":
        for entry in entries:
            if entry.name != checkpoint:
                raise ValueError("Unknown backup artifact prevents abandonment")
            io.private_dir(entry)
            for artifact in entry.iterdir():
                if not prepared or artifact.name not in io.FILES:
                    raise ValueError("Unknown checkpoint artifact prevents abandonment")
                with io.private_file(artifact):
                    pass
    if role.endswith("Stage"):
        for entry in entries:
            if not entry.name.startswith(".import-" + operation + "-"):
                raise ValueError("Unknown staging artifact prevents abandonment")
            with io.private_file(entry):
                pass


def verify_prepared_abandon(record, paths, targets, plan, io):
    """Verify the complete prepared pair, including directories already quarantined."""
    current = {}
    for role, path in paths.items():
        target, expected = targets[role], plan[role]
        source_exists, target_exists = os.path.lexists(path), os.path.lexists(target)
        if expected is None or source_exists == target_exists:
            raise ValueError("Prepared abandonment directory is missing or ambiguous")
        directory = path if source_exists else target
        io.private_dir(directory)
        info = os.lstat(directory)
        if [info.st_dev, info.st_ino] != expected:
            raise ValueError("Prepared abandonment directory identity changed")
        check_abandon_contents(role, directory, record["checkpoint"], record["id"], io, True)
        current[role] = directory
    io.private_dir(current["backup"] / record["checkpoint"])
    archive = Path(record["folders"]["archive"])
    allowed = {archive / name: ("backup", Path(record["checkpoint"]) / name, "archiveStage") for name in io.FILES}
    allowed[paths["project"] / "project.db"] = ("project", Path("project.db"), "projectStage")
    allowed[paths["volume"] / "data.ext4"] = ("volume", Path("data.ext4"), "volumeStage")
    items = record.get("files", [])
    if len(items) != len(allowed) or {Path(item["path"]) for item in items} != set(allowed) or len({Path(item["stage"]) for item in items}) != len(items):
        raise ValueError("Prepared abandonment file set differs from the retained plan")
    c = record["manifest"]["checkpoint"]
    source_hashes = {"project.db": c["databaseSha256"], "data.ext4": c["volumeSha256"], "control.db": record["manifest"]["controlDatabaseSha256"], "manifest.json": record["request"]["manifestSha256"]}
    for item in items:
        original_path, original_stage = Path(item["path"]), Path(item["stage"])
        role, relative, stage_role = allowed[original_path]
        if original_stage.parent != paths[stage_role] or not original_stage.name.startswith(".import-" + record["id"] + "-"):
            raise ValueError("Prepared stage differs from its retained plan")
        final_file = current[role] / relative
        stage_file = current[stage_role] / original_stage.name
        final_exists, stage_exists = os.path.lexists(final_file), os.path.lexists(stage_file)
        if final_exists == stage_exists:
            raise ValueError("Prepared file is missing or duplicated")
        if role == "backup" and item["sha"] != source_hashes[relative.name]:
            raise ValueError("Prepared checkpoint hash differs from its manifest")
        if io.digest(final_file if final_exists else stage_file) != item["sha"]:
            raise ValueError("Prepared file differs from its retained proof")


def abandon_import(args, io):
    """Quarantine an unregistered import without deleting files or existing resources."""
    if not sys.platform.startswith("linux") or not getattr(ctypes.CDLL(None), "renameat2", None):
        raise ValueError("Import abandonment requires Linux renameat2 support")
    if not args.confirm_abandon:
        raise ValueError("Explicit confirmation is required to abandon this import")
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
            if (record.get("project"), record.get("phase"), record.get("id")) != (row[0], row[1], args.operation):
                raise ValueError("Import journal identity or phase is inconsistent")
            if record["request"]["control"] != str(control):
                raise ValueError("Import belongs to another controller")
            if row[1] == "complete" and record.get("outcome") == "abandoned":
                print(encode(dict(status="abandoned", operationId=args.operation, projectId=row[0])))
                return
            if row[1] not in ("staging", "prepared"):
                raise ValueError("Only an unregistered staging or prepared import can be abandoned")
            prepared = row[1] == "prepared"
            if not prepared and record.get("files"):
                raise ValueError("Staging import unexpectedly contains a prepared file plan")
            c = record["manifest"]["checkpoint"]
            for name in ("id", "projectId", "databaseId", "volumeId"):
                io.identifier(c[name])
            if c["projectId"] != row[0] or c["id"] != record["checkpoint"]:
                raise ValueError("Import journal and checkpoint identities differ")
            if db.execute("SELECT 1 FROM ai_sandbox_projects WHERE id=?", (row[0],)).fetchone() or db.execute("SELECT 1 FROM project_workspaces WHERE project_id=? OR database_id=? OR volume_id=?", (row[0], c["databaseId"], c["volumeId"])).fetchone() or db.execute("SELECT 1 FROM micro_block_storages WHERE id=?", (c["volumeId"],)).fetchone():
                raise ValueError("A project or volume resource is already registered")
            pool_row = db.execute("SELECT path FROM micro_storages WHERE id=?", (record["request"]["storage"],)).fetchone()
            if not pool_row or not Path(pool_row[0]).is_absolute():
                raise ValueError("Registered destination storage is unavailable")
            pool_path = Path(pool_row[0])
            info = os.lstat(pool_path)
            if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.geteuid() or info.st_mode & 0o022:
                raise ValueError("Registered storage is not owner-controlled")
            pool = pool_path.resolve(strict=True)
            info = os.stat(pool)
            if record["poolIdentity"] != [info.st_dev, info.st_ino]:
                raise ValueError("Destination filesystem identity changed")
            parent = control.with_suffix(".sandbox")
            projects, backups, volumes = parent / "projects", parent / "project-backups", pool / "volumes"
            folders = dict(project=str(projects / c["projectId"]), volume=str(volumes / c["volumeId"]), backup=str(backups / c["projectId"]), archive=str(backups / c["projectId"] / c["id"]), projectStage=str(projects / (".project-import-" + args.operation)), volumeStage=str(volumes / (".project-import-" + args.operation)), archiveStage=str(backups / (".project-import-" + args.operation)))
            if folders != record["folders"]:
                raise ValueError("Import journal paths differ from registered resources")
            for directory in (parent, projects, backups):
                if os.path.lexists(directory):
                    io.private_dir(directory)
            if os.path.lexists(volumes):
                v = os.lstat(volumes)
                if not stat.S_ISDIR(v.st_mode) or v.st_uid != os.geteuid() or v.st_mode & 0o022:
                    raise ValueError("Volume namespace is not owner-controlled")
            roles = ("project", "volume", "backup", "projectStage", "volumeStage", "archiveStage")
            paths = {role: Path(folders[role]) for role in roles}
            targets = {role: path.parent / (".abandoned-import-" + args.operation + "-" + role) for role, path in paths.items()}
            new_plan = not record.get("abandonPlan")
            if new_plan:
                plan = {}
                for role in roles:
                    path, target = paths[role], targets[role]
                    if os.path.lexists(target):
                        raise ValueError("Abandonment destination already exists")
                    if not os.path.lexists(path):
                        plan[role] = None
                        continue
                    io.private_dir(path)
                    check_abandon_contents(role, path, c["id"], args.operation, io, prepared)
                    st = os.lstat(path)
                    plan[role] = [st.st_dev, st.st_ino]
                if prepared:
                    verify_prepared_abandon(record, paths, targets, plan, io)
                record["abandonPlan"] = plan
                transaction(db, lambda: save(db, record))
            plan = record["abandonPlan"]
            if set(plan) != set(roles):
                raise ValueError("Abandonment journal is invalid")
            if prepared and not new_plan:
                verify_prepared_abandon(record, paths, targets, plan, io)
            for role in roles:
                path, target, expected = paths[role], targets[role], plan[role]
                source_exists, target_exists = os.path.lexists(path), os.path.lexists(target)
                if expected is None:
                    if source_exists or target_exists:
                        raise ValueError("Unexpected artifact appeared during abandonment")
                    continue
                if source_exists == target_exists:
                    raise ValueError("Abandonment directory is missing or conflicts with another entry")
                current = path if source_exists else target
                io.private_dir(current)
                st = os.lstat(current)
                if [st.st_dev, st.st_ino] != expected:
                    raise ValueError("Abandonment directory identity changed")
                check_abandon_contents(role, current, c["id"], args.operation, io, prepared)
                if source_exists:
                    rename_new(path, target)
                io.sync_dir(target.parent)
            info = os.stat(pool)
            if record["poolIdentity"] != [info.st_dev, info.st_ino]:
                raise ValueError("Destination filesystem identity changed during abandonment")
            record["phase"] = "complete"
            record["outcome"] = "abandoned"
            record["completedAt"] = time.time_ns() // 1_000_000
            transaction(db, lambda: save(db, record))
            print(encode(dict(status="abandoned", operationId=args.operation, projectId=row[0])))
        finally:
            db.close()
    finally:
        os.close(lock)


def run_import(args, io):
    """Import is explicit and Linux-only; this function is never called by export or inspect."""
    if not sys.platform.startswith("linux"):
        raise ValueError("Destination import requires Linux")
    if not shutil.which("e2fsck") or not getattr(ctypes.CDLL(None), "renameat2", None):
        raise ValueError("Destination import requires e2fsck and Linux renameat2 support")
    if not args.confirm_source_quiesced:
        raise ValueError("Quiesce the source project and acknowledge it explicitly")
    if not args.volume_name.strip() or len(args.volume_name.encode()) > 128:
        raise ValueError("Use a nonempty volume name of at most 128 bytes")
    control = Path(args.destination_control).absolute()
    with io.private_file(control):
        pass
    control = control.resolve(strict=True)
    lock = os.open(control, os.O_RDWR | os.O_NOFOLLOW)
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        db = rw_database(control)
        try:
            required = {"project_workspace_imports", "project_workspace_import_tasks"}
            tables = {r[0] for r in db.execute("SELECT name FROM sqlite_master WHERE type='table'")}
            if not required.issubset(tables):
                raise ValueError("Initialize the destination once with the updated controller, then stop it before import")
            source = Path(args.directory).absolute()
            previous = db.execute("SELECT project_id,phase,document FROM project_workspace_imports WHERE id=?", (args.operation,)).fetchone()
            fixed = dict(vm=args.vm, storage=args.storage, volumeName=args.volume_name, control=str(control))
            if previous:
                record = json.loads(previous[2])
                if (record["project"], record["phase"]) != previous[:2] or record["id"] != args.operation:
                    raise ValueError("Import journal identity or phase is inconsistent")
                if record.get("abandonPlan") or record.get("outcome") == "abandoned":
                    raise ValueError("This import is being abandoned or was abandoned; use a new operation")
                if any(record["request"].get(key) != value for key, value in fixed.items()):
                    raise ValueError("Import operation has another immutable request")
                if record["phase"] == "complete" and not os.path.lexists(source):
                    print(encode(dict(status="complete", operationId=args.operation, projectId=record["project"], generation=record["generation"])))
                    return
            files = None
            if previous and record["phase"] == "prepared" and not os.path.lexists(source):
                files = prepared_archive(db, control, args, record, io)
                m = io.manifest_at(files["manifest.json"].parent, files["manifest.json"])
                manifest_sha = io.digest(files["manifest.json"])
            else:
                m = io.manifest_at(source)
                manifest_sha = io.digest(source / "manifest.json")
            c = m["checkpoint"]
            request = dict(manifestSha256=manifest_sha, **fixed)
            if previous:
                if record["request"] != request or record["manifest"] != m:
                    raise ValueError("Import operation has another immutable request or manifest")
                if record["phase"] == "complete":
                    print(encode(dict(status="complete", operationId=args.operation, projectId=record["project"], generation=record["generation"])))
                    return
            manifest, config, route, catalog, actual = inspect_source(source, io, files)
            if manifest != m:
                raise ValueError("Source manifest changed during import admission")
            if previous:
                # Older pending journals used source ordinals; retain their original plan.
                mapping = record.get("taskOrdinals", {row[1]: row[0] for row in catalog})
                catalog = planned_catalog(catalog, mapping)
                record.setdefault("taskOrdinals", mapping)
            else:
                catalog = remap_catalog(db, catalog)
            pool = validate_destination(db, args, c["projectId"], c["databaseId"], c["volumeId"], catalog)
            pool_stat = os.stat(pool)
            if m["volumeBytes"] % (1 << 30) or not 1 <= m["volumeBytes"] >> 30 <= 65535 or c["generation"] >= (1 << 63) - 1:
                raise ValueError("Unsupported volume size or exhausted generation")
            if db.execute("SELECT 1 FROM project_workspace_imports WHERE phase!='complete' AND id!=?", (args.operation,)).fetchone():
                raise ValueError("Resume the existing incomplete import first")
            parent = control.with_suffix(".sandbox")
            projects = parent / "projects"
            backups = parent / "project-backups"
            volumes = pool / "volumes"
            folders = dict(project=str(projects / c["projectId"]), volume=str(volumes / c["volumeId"]), backup=str(backups / c["projectId"]), archive=str(backups / c["projectId"] / c["id"]), projectStage=str(projects / (".project-import-" + args.operation)), volumeStage=str(volumes / (".project-import-" + args.operation)), archiveStage=str(backups / (".project-import-" + args.operation)))
            if previous:
                if record["phase"] not in ("staging", "prepared"):
                    raise ValueError("Unknown retained import phase")
                if record["folders"] != folders or record["poolIdentity"] != [pool_stat.st_dev, pool_stat.st_ino]:
                    raise ValueError("Destination filesystem identity changed")
            else:
                if args.operation == c["id"]:
                    raise ValueError("Import operation must differ from the source checkpoint")
                for key in (args.operation, c["id"]):
                    if db.execute("SELECT 1 FROM project_backup_operations WHERE id=?", (key,)).fetchone() or db.execute("SELECT 1 FROM project_backup_cleanups WHERE id=?", (key,)).fetchone():
                        raise ValueError("An operation ID is already in use")
                for key in ("project", "volume", "backup", "projectStage", "volumeStage", "archiveStage"):
                    if os.path.lexists(folders[key]):
                        raise ValueError("Destination directory already exists; import never overwrites it")
                record = dict(id=args.operation, project=c["projectId"], checkpoint=c["id"], phase="staging", request=request, manifest=manifest, folders=folders, poolIdentity=[pool_stat.st_dev, pool_stat.st_ino], generation=c["generation"] + 1, taskOrdinals={row[1]: row[0] for row in catalog})
                transaction(db, lambda: db.execute("INSERT INTO project_workspace_imports VALUES(?,?,'staging',?)", (args.operation, c["projectId"], encode(record))))
            for path in (parent, projects, backups):
                ensure_directory(path, io)
            try:
                os.mkdir(volumes, 0o700)
                io.sync_dir(pool)
            except FileExistsError:
                info = os.lstat(volumes)
                if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.geteuid() or info.st_mode & 0o022:
                    raise ValueError("Destination volume namespace is not owner-controlled")
            if record["phase"] == "staging":
                required = {}
                sizes = [(projects, (source / "project.db").stat().st_size), (volumes, manifest["volumeBytes"]), (backups, sum((source / name).stat().st_size for name in io.FILES))]
                for folder, size in sizes:
                    device = os.stat(folder).st_dev
                    total, free = required.get(device, (0, shutil.disk_usage(folder).free))
                    required[device] = (total + size, min(free, shutil.disk_usage(folder).free))
                if any(free < size + (64 << 20) for size, free in required.values()):
                    raise ValueError("Insufficient free space for the live pair and retained source archive")
                prepare_files(db, source, record, io)
            for folder in folders.values():
                io.private_dir(Path(folder))
            install_files(record, io)
            if os.stat(pool).st_dev != pool_stat.st_dev or os.stat(pool).st_ino != pool_stat.st_ino:
                raise ValueError("Destination pool changed during installation")
            register(db, args, record, config, route, catalog, actual, io)
            print(encode(dict(status="complete", operationId=args.operation, projectId=record["project"], databaseId=c["databaseId"], volumeId=c["volumeId"], vmId=args.vm, generation=record["generation"])))
        finally:
            db.close()
    finally:
        os.close(lock)
