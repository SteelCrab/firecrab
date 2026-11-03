# Project backup retention and cleanup

Use **AI Canvas → Backups & recovery → Retention & cleanup**. The controls are
available in English and Korean, including when the project DB cannot be opened.
All endpoints require the local operator role and the existing project permission.
Normal project members do not acquire deletion rights through these controls.

## Policy and protection

Automatic cleanup is **disabled by default**. The initial policy keeps the latest
10 checkpoints, has no age requirement and excludes manual backups.

- `keepLatest`: keep the latest 1–63 checkpoints in durable registry admission order.
  Tied timestamps or a backward clock do not change that order. One of the 64
  archive slots is reserved so a count-only policy can rotate into a new backup.
- `maxAgeDays`: null uses the count alone. A value from 1–3650 also retains younger
  backups. Only older backups outside the recent set can be selected.
- `includeManual`: false excludes manual backups from automatic cleanup.
- **Protect checkpoint** prevents both explicit and automatic deletion.

A policy may retain more than its count because age, manual-backup and protection
rules also apply. For example, keeping 10 with an age of seven days keeps the most
recent 10 plus all younger, protected and excluded manual checkpoints. The 64-slot
backup limit still applies; an unsatisfiable policy defers new backups rather than
silently deleting protected evidence.

Enabling retention explicitly authorizes future policy selections. Disabling it
prevents new admissions; an already admitted cleanup finishes its immutable plan.
Policy result updates cannot overwrite a newer operator edit.

Automatic cleanup selects completed checkpoint archives only. Aborted temporary
copies and preserved pre-restore originals require explicit manual selection.
The scheduler checks before backup scheduling, retries deferred work after a bounded
interval and never starts or stops a workload VM. The VM must be stopped and idle.

## Explicit deletion

Refresh the preview, select specific operation IDs and review **Confirm permanent
cleanup**. Nothing is deleted by opening or selecting the preview. Confirming
**Permanently delete selected files** admits an irreversible cleanup operation.

Candidates can be:

- Completed checkpoint archives: project DB, volume, control snapshot and manifest.
- Aborted backup copies or pre-installation restore staging files.
- Original DB, WAL/SHM and volume files preserved by a completed restoration.

Before deleting a completed checkpoint or preserved restore original, the service
must find an unselected complete checkpoint with valid hashes, SQLite integrity,
project/resource identities and ext4 identity. An unreadable or corrupted alternative
does not satisfy this condition. A corrupted selected archive can be removed if a
separate valid checkpoint survives. The last verified checkpoint cannot be removed.
Aborted pre-installation partial copies can be discarded without a checkpoint;
that path never installed its staging files over the live project.

The preview reports metadata eligibility, not a completed integrity check. Admission
checks the entire batch again. Active operations, protected targets, wrong project
identities, changed bindings and unknown archive entries reject the request before
any deletion. A source used by an interrupted restoration cannot be selected.

Current project files, the live volume, private controller registry and legacy
`/checkpoints` or Sandbox migration archives are outside the cleanup allowlist.

## Recovery and audit

An owned worker holds task admission, VM, permanent service, operation and DB-owner
locks through client cancellation. Cleanup records and all target tombstones commit
in one `synchronous=FULL` SQLite transaction **before the first unlink**. Tombstoned
checkpoints become unavailable for new restorations immediately. A durable maintenance
fence blocks project access, VM start/replacement and competing backup/restore work.

The private plan records exact allowed paths, checksums, byte lengths, device/inode
identities and per-file progress. The API never returns the private plan or file
contents. Symlinks, hard links and unknown files fail closed. Only the planned files
are unlinked; archive directories are removed only when empty. There is no recursive
delete and no wildcard path supplied by the caller.

On restart, admitted work without a retained error resumes. All remaining files and
the surviving checkpoint are checked before another deletion. An interrupted unlink
can be reconciled from its already committed plan. A changed remaining artifact
keeps the gate closed and requires explicit repair/retry. A retained failure is not
repeated automatically on every maintenance tick.

The browser saves its cleanup UUID and exact target list before submitting it.
After an uncertain response, reopen the panel and use **Retry same cleanup request**
or the server receipt's **Resume cleanup**. A definitive rejection releases only a
fresh unadmitted request after an exact missing-receipt proof. An earlier uncertain
request keeps its identity. A completed data phase can still show pending gate
cleanup; resume that receipt before using the project.

After completion, backup/restore receipts, original manifests, cleanup receipts and
private file proofs remain. This preserves generation fencing and audit evidence.
Repeating a completed cleanup returns its receipt and never deletes newly appearing
files, even if they reuse an old artifact path. Cleanup does not advance the project
binding generation or rewind task/attempt identities.

`logicalBytes` measures journaled logical file lengths, not physical sparse-disk
allocation. The registry retains at most 4096 backup/restore and 4096 cleanup receipts
per project. History lists up to 100 public cleanup receipts, always including an active receipt;
older receipts remain queryable by their exact ID. Audit pruning is not implemented.

## API

Paths below are relative to `/api/ai-sandbox/projects/{project}/workspace/backups`.
Generation acknowledgements are explicit request fields. They do not authorize a
stale editor to save against a restored workspace.

| Method | Path | Body or result |
| --- | --- | --- |
| GET | `/retention` | Durable policy and last scheduler result |
| PUT | `/retention` | `{expectedGeneration, enabled, keepLatest, maxAgeDays, includeManual}` |
| GET | `/retention/preview` | Candidate metadata and current policy's proposed target IDs |
| PUT | `/operations/{backup}/protection` | `{expectedGeneration, protected}` |
| GET | `/cleanups` | Bounded public history, including any active operation |
| GET | `/cleanups/{cleanup}` | Exact retained receipt |
| PUT | `/cleanups/{cleanup}` | `{expectedGeneration, targetIds}`; 1–64 distinct nonzero operation UUIDs |
| POST | `/cleanups/{cleanup}/retry` | Resume the retained selection without changing its identity |

A cleanup ID cannot be reused for another project or selection. Backup and cleanup
operation IDs cannot collide. No endpoint deletes the audit receipt itself.

## Qualification limits

These are host-local offline operations. They do not protect against host loss.
Live workload interruption, production service-user/multi-user qualification and
actual different-host restoration remain #550 acceptance gates. The isolated Linux
ext4/SQLite QA uses real filesystem bytes without booting a user workload VM or
calling an AI provider. [Backup and recovery guide](project-workspace-backups.md).
