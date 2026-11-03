# Project database and volume backups

Issue [#550](https://github.com/SteelCrab/firecrab/issues/550) is delivered in
[Draft PR #562](https://github.com/SteelCrab/firecrab/pull/562), against
`release/v0.5.0`. Keep it unmerged until the entire issue is implemented and verified.

## Operator workflow

Open **AI Canvas → Backups & recovery** in the project navigator. The same control
is available on a failed project connection screen. Its APIs read the control
registry without opening the damaged project DB.

Scheduling is disabled by default. Enable it explicitly and choose an interval
between five minutes and seven days. The UI offers five minutes, one hour and one
day. The project volume must have completed initialization before scheduling can
be enabled. The scheduler checks every 30 seconds and defers running/busy VMs; it
never stops a workload or launches one. Schedule edits made during a copy win over
the older attempt's result.

For a manual backup, finish/reconcile project tasks and pending file writes, then
stop the workload VM. Select **Back up now**. A failed or uncertain response retains
its immutable operation ID in the browser. **Retry same request** reuses that ID
and request after a reload. The durable server operation catalog is authoritative.
Closing a tab or dialog does not cancel an admitted operation. A fresh request that
receives a definitive rejection is released only after an exact receipt lookup
confirms absence. An earlier uncertain request retains its ID after a rejected retry.
Unreconciled file writes or an unhealthy source defer scheduling before its durable
fence is installed, so the VM can still start for repair/reconciliation.

Select a completed backup, review **Restore the entire project?**, then explicitly
confirm. The operator must stop the workload VM first. The operation replaces the
project DB and volume together. Current files and SQLite WAL/SHM sidecars are
preserved with the operation's UUID on their existing filesystems. No old file is
silently deleted. The control DB, authorization, global task/attempt ID catalog,
provider permissions and other projects are not rolled back.

## API

All paths below are relative to `/api/ai-sandbox/projects/{project}/workspace`.
Every backup endpoint requires the verified controller operator and the project
read or write permission appropriate to the action. No supplied HTTP header grants
identity. The API never returns host paths or database/volume bytes.

| Method | Path | Purpose |
| --- | --- | --- |
| GET | `/backups/binding` | Current generation, even when the project DB is missing |
| GET | `/backups/policy` | Durable schedule and deferred/failed attempt status |
| PUT | `/backups/policy` | `{expectedGeneration, enabled, intervalSeconds}` |
| GET | `/backups` | Retained operation receipts, newest first |
| PUT | `/backups/operations/{operation}` | `{expectedGeneration, restoreCheckpoint: null}` creates a backup; a completed backup UUID selects restoration |
| GET | `/backups/operations/{operation}` | Exact retained operation receipt |
| POST | `/backups/operations/{operation}/retry` | Resume the same retained operation |
| DELETE | `/backups/operations/{operation}` | Abort before installation; retain all artifacts |

An operation UUID cannot be reused for another project, generation or restore source.
Completed retries return their retained receipt without replacing newer live data.
Progress phases are `pending`, `database_frozen`, `prepared`, `installing`,
`complete` and `aborted`. The manifest is provisional until `complete`.
`pendingReconciliation` remains true while the durable maintenance gate still needs
cleanup, including a crash after data commit. Resume that exact receipt before using
the project. A completed receipt with cleanup confirmed returns false.

After installation begins, abort is rejected: resume the exact operation instead.
A retained error keeps the project fenced and requires an explicit retry or a
permitted pre-installation abort. The scheduler does not repeatedly copy a failing
disk. An intent that never acquired its durable fence is aborted during restart
recovery; it is not executed later after intervening work.

## Storage and recovery guarantees

New archives live under `project-backups/<project UUID>/<backup UUID>/`, next to
`projects/` under the private workspace parent. They are outside the live project
folder and guest-visible source tree. Directories are owner-only, and files are
regular, owner-only, single-link files. Unsafe symlinks/aliases are rejected.

Each completed archive contains:

- `project.db`, created through the SQLite Backup API while normal project writes
  are fenced and the VM is stopped.
- `data.ext4`, a full logical copy of the inactive volume. Zero ranges remain sparse.
- `control.db`, a separate private controller snapshot. It can contain credentials;
  it is never exposed through workspace tools or downloadable API responses.
- A synced manifest containing all three SHA-256 hashes, exact volume length,
  project/database/volume identities, generation, schema and project revision.

Normal readers/writers and VM start/delete operations reject a pending maintenance
fence. An OS service lock outside the live project directory prevents a second API
owner from taking over even if that directory is deleted. Before filesystem effects,
backup work owns the task admission claim, VM lock, service lease and DB owner lock.
Its blocking copy owns those locks through cancellation. At most two copy workers
are admitted; the shared task admission claim serializes lifecycle admissions while
a copy runs.

Restoration verifies archive hashes, SQLite integrity/foreign keys, application and
project identities, schema/revision, canonical Sandbox migration provenance, idle
archived task state, ext4 UUID and exact registered volume size. It stages on each
destination filesystem, journals original hashes, preserves old WAL/SHM, and installs
the pair using separately synced renames. The durable fence prevents access to a
mixed pair. A restart resumes an admitted interrupted operation, including a crash
between the two file replacements or after the control commit. Missing/corrupt DB
files and a missing volume can be recovered explicitly with their existing identities.

Controller startup interrupts available task records independently. A fenced, missing
or damaged registered project DB does not prevent the recovery API from starting.
That project remains blocked for normal task reads, scheduling and writes. Startup
never substitutes controller task records for an unavailable private project DB.

For a missing canonical Sandbox DB, explicit disaster recovery additionally requires
no active controller task workers. Admission stays locked. Restored task indexes must
be idle, the migration marker must match the current route, and global task/attempt
UUID reservations remain intact. Lost newer tasks are not redispatched.

The final control transaction advances the binding generation exactly once and
invalidates the runtime mount proof. Restored project writes require
`X-Firecrab-Workspace-Generation` matching that generation. The dashboard remembers
its original acknowledgement; polling does not silently adopt a restored generation.
Web VS Code receives its generation from the workbench configuration default. A
project setting cannot supply it. Preserve unsaved text, reload other open pages,
reopen editors, and reconcile the mount after explicitly starting the VM. An older
editor bundle without this protocol cannot save after restoration.

## Limits and open acceptance

These are offline, host-local full copies. They are not online filesystem snapshots,
a remote backup service, or process-memory restoration. Backups on the same host do
not protect against losing that host. Preserve the controller registry, archives and
storage pools with a separate host backup.

The catalog admits at most 64 completed backups and 4,096 operation receipts per
project. Completed archive tombstones no longer occupy checkpoint slots. Explicit
[retention and cleanup controls](project-backup-retention.md) can remove selected
checkpoint files, aborted temporary copies and completed-restore originals. Automatic
cleanup is disabled by default. Protected and active checkpoints remain retained.
A full disk produces a retained error, not a successful backup. Existing legacy
`/checkpoints` files remain readable for their original API and migration workflow;
new managed restoration selects backups from the new operation catalog.

Live KVM interruption/reboot, production service-user permissions, multi-user
qualification, live retention interruption and genuine different-host restore remain issue
acceptance gates. The Linux component QA uses real ext4 bytes and SQLite in its own
container. It does not boot a workload VM or call an AI provider.
