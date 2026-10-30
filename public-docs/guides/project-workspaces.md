# Persistent project workspaces

A project workspace combines one private project SQLite database with one retained
MicroBlockStorage data volume. The existing AI Sandbox project UUID owns both.
The controller database keeps authorization and resource bindings. Sandbox task and
attempt records remain in their current store; migrating them belongs to the later
agent integration phase of #550.

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
retained workspace data. Permanent project data deletion belongs to the explicit
retention/deletion phase of #550.

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
an incomplete checkpoint rather than reporting success. Remote transfer, scheduling,
retention automation and cross-host restore remain Phase 7 work.

### Operator recovery procedure

1. Preserve the current database/disk and record the project's registered identities.
2. Stop workload VMs and the API so neither the project DB nor disk has an active writer.
3. Select one completed checkpoint. Verify both file hashes, project/database/volume
   identities and schema version against its manifest. Also verify the disk's ext4 UUID.
4. Restore its `project.db` and `data.ext4` together using staged copies and atomic
   replacement on each destination filesystem. Keep owner-only modes and sync files
   and parent directories. Quarantine the old closed database's WAL/SHM sidecars so
   they cannot be paired with the restored database.
5. Keep or restore the matching control registry from its separate backup. This phase
   restores an already registered project; automatic re-registration on a new host
   is not implemented. Never substitute a different volume/database identity.
6. Start the API, inspect the project while its VM is stopped, then start the VM and
   reconcile its mount. Confirm the metadata revision and representative file hashes.

SQLite/disk restore does not restore process memory. Durable session/terminal history
and task reconciliation are separate later integrations; old commands are never
replayed by this foundation.

## Validation

The Rust workspace tests cover private SQLite identities/permissions, stale writes,
owner fencing, project authorization, controller reopen, interrupted provisioning,
VM rebinding/deletion, disk loss and matched checkpoint integrity. Volume-header and
runtime records in those tests are fixtures. The Linux generated-shell test exercises
real tar/base64/hash tools with fixture mount metadata; it is not a live MicroVM proof.
Real MicroVM stop/start and controller-restart acceptance must be recorded separately.
See [Persistent data volumes](volumes.md) for the existing volume lifecycle and
[Storage](storage.md#backup-and-retention) for controller-level backup requirements.
