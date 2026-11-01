# Project Sandbox task storage migration

Implementation for #550 in Draft PR #562. Do not merge until the whole issue is
implemented and accepted. This report describes backend behavior; it does not
claim live workload or production migration acceptance.

## Storage boundary

Project database schema 8 adds the existing Sandbox canonical task schema to the
private `project.db`. It reuses `SavedTask`, attempt identities, admission proofs,
summary projections, retry lineage and the existing execution state machine.
There is no parallel agent task implementation.

After cutover, task bodies, attempt references, admission identities and summary
indices have one writable canonical location in the project database. The control
SQLite database retains project ownership, access grants, model proofs, immutable
task/attempt routing identities, global cursor ordinals and the migration receipt.
A private pre-migration archive remains for operator recovery. It is not a second
serving database or a guest-visible file.

The Store and workspace API share the same service ownership lease. Project task
operations use the existing project lock, persisted ownership epoch, local-disk
WAL mode and `synchronous=FULL`. Missing, mismatched, unpaired or busy project
storage fails closed. It never recreates a lost project DB or falls back to stale
controller task records.

## Operator API

Migration is explicit. Existing unbound Sandbox projects and projects that have
not completed this operation continue to use controller task storage. This
change does not automatically provision volumes or migrate production data.

1. Bind and initialize the retained project workspace using the existing API.
2. Finish or reconcile active, waiting and uncertain guest tasks.
3. Stop the workload VM. Keep the API and owning project service available.
4. Create a matched checkpoint with a new checkpoint UUID:
   `PUT /api/ai-sandbox/projects/{project}/workspace/checkpoints/{checkpoint}`.
5. Use a new operation UUID with:

   ```http
   PUT /api/ai-sandbox/projects/{project}/workspace/sandbox-migrations/{operation}
   Content-Type: application/json

   {"checkpointId":"<completed-checkpoint-uuid>"}
   ```

6. Inspect the result with:
   `GET /api/ai-sandbox/projects/{project}/workspace/sandbox-storage`.

GET requires project Read permission. PUT requires the local operator and project
Write permission. Unknown fields, empty identities, a changed binding, an active
VM, an incomplete checkpoint, a changed retained volume, unresolved guest work
or another operation are rejected. Host file paths are not accepted or returned.
The request retains admission and lifecycle guards even if its HTTP client leaves.

A completed response has `storage: "project"`, `operationId`, `checkpointId`,
`sourceSha256` and `taskCount`. The source hash identifies the frozen migration
input. It is not a checksum of later live task changes. Repeating the same
completed operation and checkpoint returns its historical receipt. It does not
re-copy records or require the workload to remain stopped forever.

## Durable handoff and interruption

1. Validate and stream the source records in stable global cursor order. Verify
   immutable attempt/runtime/caller/source/admission references. Reject active or
   uncertain guest work before freezing any source data.
2. Commit a `prepared` registry entry with FULL durability. SQL triggers reject
   legacy task writes. Prepared-source reads remain available; new writes wait
   for recovery of this operation.
3. Use SQLite Backup API to retain a private controller archive. Validate SQLite
   integrity, the selected task hash/count and the archive SHA-256 manifest.
   Sync files and directory entries before accepting the archive.
4. Copy canonical task records and their references in one FULL project database
   transaction. Rebuild the compact summaries from canonical task data. Commit a
   project marker containing the operation identity and source hash.
5. Verify the frozen source again. In one FULL controller transaction, remove its
   migrated task bodies/references and publish the project route. Other projects
   remain in their current storage location. Global cursor ordinals do not change.

Retry an interrupted operation with its original operation and checkpoint IDs.
A crash before target commit leaves the source frozen and available for reads.
Starting the bound workload VM is blocked until the same handoff completes. This
keeps the stopped volume matched to the retained checkpoint during recovery.
A crash after target commit causes the retry to validate the retained target copy
before registry cutover. A crash after cutover returns the existing receipt.
A different operation does not take over a prepared migration. A changed target,
wrong project identity, altered archive or missing project marker stops recovery
before source deletion.

Archives are under the private project directory:
`sandbox-migrations/{operation}/source.db` and `source.sha256`. Directories are
owner-only and files are owner read/write. The full controller archive can contain
controller credentials and records from other projects. Do not expose it through
file APIs, mount it in a workload VM or attach it to an issue. Interrupted partial
files and completed archives are retained. No automatic garbage collection,
rollback, project deletion or retention policy is introduced by this slice.

## Recovery boundaries

Keep the controller registry, project database, matched volume checkpoint and
private migration archive together. Restoring an older controller without its
project route fails closed when the project migration marker remains. Do not
manually delete the route to force a fallback.

This API performs task-storage handoff. It is not a general checkpoint restore
API. Automated matched restore, post-checkpoint task reconciliation and cross-host
resource rebinding remain later #550 work. Live service-user, real workload VM
restart/replacement, concurrent remote controllers and production-data migration
acceptance remain pending.

## Validation

- Focused persistence checks exercise exact canonical records, unchanged cursors,
  history, new writes, interruption recovery, global pending work, immutable
  attempts, controller restart, source write fencing, damaged copied records,
  damaged backup manifests, mismatched identities and successful resumed cutover.
- Workspace HTTP acceptance exercises delegated read access, operator-only
  migration, strict request fields, a matched checkpoint, idempotent receipts and
  conflicting operations. It uses a disposable sparse ext4 fixture, not a live VM.
- The API and API-types Clippy checks use `-D warnings`. Formatting and whitespace
  checks are required before publication.
- The broader Sandbox suite includes existing Linux-only `/proc/self/fd` and
  secure path tests. Its macOS run does not pass; hosted Linux CI is required.
  This limitation is not hidden by ignoring or weakening those tests.

Local results: 15 focused persistence tests and 68 workspace tests pass. API and
API-types Clippy with `-D warnings`, formatting and whitespace checks pass. The
console resize fixture now emits each observed size marker in one `printf` so
periodic guest output cannot split its prefix and value. TypeScript syntax is
checked with `node --check`; actual guest resize/WINCH remains a hosted CI check.
Hosted CI links and results are recorded in PR #562.
