# Persistent project workspaces

A project workspace combines one private project SQLite database with one retained
MicroBlockStorage data volume. The existing AI Sandbox project UUID owns both.
The controller database keeps authorization and resource bindings. Existing Sandbox
task and attempt records remain in the controller database until an operator completes
the opt-in [project database migration](../development/project-sandbox-migration-2026-11-01.md).
After migration, the project database owns task bodies, attempts and admissions.
The controller keeps only routing identifiers and the migration receipt.

This foundation exposes a backend lifecycle. It does not start an editor, shared PTY,
or model task. Consumers must reconcile the workspace and require `ready` before
starting tools that use its mounted folder.

## Create and reopen

1. Create an ordinary inactive user MicroVM with the existing VM API.
2. Save an AI Sandbox project selecting that VM. `sourcePath` may select an authorized
   source directory on the API host, or be absent for an empty workspace.
3. Create a named data volume with `firecrab volume create NAME --size 1`.
4. Bind that existing volume to the project:

```sh
curl -fsS -X PUT "$API/api/ai-sandbox/projects/$PROJECT/workspace" \
  -H 'Content-Type: application/json' -d '{"volumeId":"VOLUME_UUID"}'
```

Binding requires the controller operator, an inactive user VM, and a completed volume.
A volume already attached at another mount path must be detached before its first
project binding. A project keeps the same volume and private database identity for
its lifetime. Repeating the same binding is idempotent.

Start the selected VM using the existing lifecycle API, then reconcile:

```sh
curl -fsS -X POST "$API/api/ai-sandbox/projects/$PROJECT/workspace/reconcile"
curl -fsS "$API/api/ai-sandbox/projects/$PROJECT/workspace"
```

Reconciliation uses the existing pinned-key operator SSH channel. Once admitted, its
bounded operation retains lifecycle/database ownership after HTTP disconnect. It checks the
actual ext4 mount and filesystem UUID at `/workspace/shared`. The first successful
initialization imports a bounded, sanitized snapshot of `sourcePath`, or an empty
archive. It reuses Sandbox's source-file limits and credential exclusions. Git
metadata, source symlinks, and reserved `.firecrab-workspace`/`lost+found` paths are
not imported. This operation does not clone remote repositories or mirror later
changes to the original source directory.

An existing unrecognized non-empty disk is rejected. The import archive and digest
are retained before guest mutation. A pending guest marker permits an interrupted
first import to finish. Once a ready marker is published, retries and future VM
boots verify its identity without overwriting user edits. A missing ready marker on
an initialized volume requires recovery; it does not authorize reimporting source.

## Project database and access

The private file is `<control-database-with-.sandbox-extension>/projects/<project-id>/project.db`.
For `data/firecrab.db`, this is `data/firecrab.sandbox/projects/<project-id>/project.db`.
Private directories use mode `0700`; database, lock, archive and checkpoint files use
`0600`. They remain on the API host, outside agent/editor source access.

Each operation acquires the project's advisory ownership lock. Opening the schema
advances a persisted owner epoch; mutations verify that epoch. SQLite uses WAL,
`synchronous=FULL`, foreign keys and a two-second busy timeout. Different projects
have separate locks/databases. A competing owner gets `workspace_busy` instead of
an unbounded wait. Unknown schemas, unexpected database identities, symlinks and
hard-linked database files are rejected.

The operator provisions, rebinds, initializes and checkpoints resources. Delegated
hosts need the existing exact project `read` or `write` grant to read or update
workspace metadata. Permission checks precede private file access. No endpoint
accepts an arbitrary database path or host command.

`GET /api/ai-sandbox/projects/{project}/workspace` returns the binding, schema,
revision, bounded metadata document, initialization flag and observed readiness.
`PUT .../workspace/state` accepts:

```json
{"expectedRevision":0,"document":{"name":"Retained project state"}}
```

Documents must be JSON objects of at most 64 KiB. Saving advances the revision;
a stale revision returns `409`. These are project metadata, not an alternate task
state machine or the future user-private draft API. Metadata stays queryable while
the VM is stopped or has been deleted.

## Runtime replacement and failures

Stop the old VM, select the replacement VM in the existing project settings, then
repeat the workspace PUT with the same `volumeId` and current `expectedGeneration`.
The operation locks both runtimes, releases the old attachment and attaches the same
disk. Rebinding advances the generation. Start the replacement and reconcile again.
A guest mount proof applies only to its recorded runtime generation.

Statuses are `provisioning`, `stopped`, `awaiting_mount`, `ready` and `unavailable`.
A missing/mismatched volume makes readiness unavailable. An established missing
project DB returns a conflict. Neither is silently replaced with empty data.
Interrupted binding can be retried with the same identities; resource reservation
precedes filesystem/attachment work. A GET never initializes a missing database.

Closing tabs or restarting the controller does not delete project data. VM deletion
releases its volume attachment and preserves the workspace registration/database.
The ordinary project-delete, volume-delete and volume-detach operations reject
retained workspace data. The offline [permanent project deletion workflow](project-workspace-deletion.md)
requires an explicit operator confirmation and retains the borrowed VM and volume
registrations. Its live acceptance remains pending.

## Matched offline checkpoints

Finish/reconcile active project tasks and stop the workload VM. Create an identified
checkpoint with `PUT .../workspace/checkpoints/{checkpoint-uuid}`. The controller holds
the VM lifecycle lock and project database ownership lock throughout the copy, even
if the HTTP request disconnects. It uses the SQLite Backup API, copies the retained
disk and publishes `manifest.json` only after both files are synced. The manifest
records project/database/volume identities, binding generation, schema/revision and
SHA256 hashes. Repeating a completed checkpoint verifies its files and returns the
same manifest. An incomplete directory is retained and requires a new checkpoint ID.
`GET` on that checkpoint returns its completed manifest without exposing host paths.

Checkpoints are under the private project's `checkpoints/<checkpoint-uuid>/` directory.
This initial full-disk copy needs free space for the database and disk; failures keep
an incomplete checkpoint rather than reporting success. The newer [managed backup and restore API](project-workspace-backups.md)
adds opt-in scheduling, independently retained archives and restart reconciliation.
Remote transfer, pruning/deletion and genuine cross-host acceptance remain Phase 7 work.

### Restoration

Use the [managed backup and restore API](project-workspace-backups.md) for new
backups and explicit restoration. It preserves original files, validates the pair,
resumes interrupted installation and advances the write generation before other
clients can save. Stop the workload VM first; the API itself can remain running.

Legacy `/checkpoints` remain available for their original read and Sandbox migration
workflow. They are not automatically imported into the managed backup catalog.
Retain legacy and partial artifacts for operator inspection. Do not substitute a
project DB or volume independently: its matching control registration, task authority,
sidecars and stale-browser fencing must also remain consistent. Genuine legacy
checkpoint disaster recovery and different-host recovery require separate acceptance.

SQLite/disk restoration does not restore process memory or replay old commands.
Preserve unsaved text, reload other pages and reopen editors after restoration.

## File access and history

Use the [project file API](project-workspace-files.md) for confined guest file
access, optimistic saves, retained versions, comparisons and restoration.

## Validation

The Rust workspace tests cover private SQLite identities/permissions, stale writes,
owner fencing, project authorization, controller reopen, interrupted provisioning,
VM rebinding/deletion, disk loss and matched checkpoint integrity. Volume-header and
runtime records in those tests are fixtures. The Linux generated-shell test exercises
real tar/base64/hash tools with fixture mount metadata; it is not a live MicroVM proof.
Real MicroVM stop/start and controller-restart acceptance must be recorded separately.
Use the [isolated Linux/KVM recovery gate](../development/project-workspace-recovery-qa.md)
to record real database, volume, runtime, and file identities.
See [Persistent data volumes](volumes.md) for the existing volume lifecycle and
[Storage](storage.md#backup-and-retention) for controller-level backup requirements.

## Interactive project terminals

See [Project workspace terminals](project-workspace-terminals.md) for scoped PTYs, input control, retained output and restart behavior.

See [Private views and drafts](project-private-views.md) for caller/tab isolation,
optimistic saves and recovery while the workload VM is stopped.
