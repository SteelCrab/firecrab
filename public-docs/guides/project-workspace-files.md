# Project workspace files

The file API reads and changes the mounted `/workspace/shared` volume. The
navigator, editor and other clients use the same file contents. Private project
SQLite files stay on the controller and are outside this filesystem interface.

## Access and readiness

Read operations require the existing Sandbox project's Read permission. Saving,
restoring and reconciling also require Write permission. Guest operations recheck
permission after waiting and after guest I/O. Reconciliation belongs to the
original writer, project owner or controller operator.

Live file access requires a running, reconciled workspace with the expected
volume, project, database and import identity. A trusted guest helper verifies
the actual ext4 device and opens each path through directory descriptors. Links,
hard-linked files, special files and `.firecrab-*` metadata are rejected. `.git`
and project-root `AGENTS.md` are ordinary retained project files.

The guest helper must support `--workspace-files`. An older guest helper reports
an explicit unavailable/conflict result. Update the host bundle and guest helper
before accessing files through this protocol.

## API

All paths below follow `/api/ai-sandbox/projects/{project}/workspace`.
File and directory paths are UTF-8 workspace-relative strings in `?path=...`.

| Operation | Route | Result |
| --- | --- | --- |
| Directory children | `GET /files/tree?path=src` | Names and directory flags |
| Current file | `GET /files?path=src/main.rs` | Version metadata and base64 bytes |
| Save | `PUT /files?path=src/main.rs` | Immutable resulting file version |
| History | `GET /files/history?path=src/main.rs` | Up to 100 versions, newest first |
| Older history page | `GET /files/history?path=src/main.rs&before=42` | Versions below that ID |
| Historical contents | `GET /files/versions/{version}?path=src/main.rs` | One immutable file version |
| Compare | `GET /files/diff?path=src/main.rs&from=1&to=2` | Before/after metadata and bytes |
| Restore | `POST /files/restore?path=src/main.rs` | A new version referencing the restored one |
| Write status | `GET /file-operations?path=src/main.rs` | Pending/applied/failed operation metadata |
| Reconcile | `POST /file-operations/{operation}/reconcile` | Confirm or resume that exact admitted save |

Save body:

```json
{
  "operationId": "f407628f-a9c0-46e2-8a72-4f9d38172fb0",
  "expectedVersion": 12,
  "contentBase64": "aGVsbG8K"
}
```

Use zero as the expected version only when the file is currently absent. Reuse
an operation ID only for the identical path, base version, bytes and writer.
A changed file returns HTTP 409; reload it before admitting a new save.

Omit `path` on history or operation listing to inspect the project-wide timeline.

Restore uses `operationId`, `expectedVersion` and the historical `version` ID.
The selected historical version must belong to the same path and project.
Restoration appends history. Earlier versions remain available.

## Recovery and bounds

SQLite records the immutable save before dispatch. The guest synchronizes a
private write-ahead record, replaces the file atomically, synchronizes its parent,
and publishes an operation receipt. The controller then commits the version.
A lost response or controller restart can recover the same receipt without
overwriting later edits. A pending operation with no confirmed outcome remains
pending until its original request is reconciled. Client disconnect does not
cancel its owned worker or release lifecycle locks early.

History, comparisons, operation status and confirmed save retries remain readable
with the VM stopped. A file changed outside the API receives an observed version
when read; this does not attribute an API writer or capture every intermediate
external write.

- Files are limited to 1 MiB. A directory response is limited to 4096 children.
- The project database admits at most 128 MiB of file bytes and 4096 combined
  history/operation records. Completed operations reference their history bytes.
- Guest receipts are limited to 256 MiB and 4096 records on the retained volume.
- Limits report conflicts and retain existing data. History is not silently evicted.
- At most 16 guest-file workers run per controller. Saturation reports `workspace_busy`.
- Schema 2 adds file tables transactionally to schema 1. Existing metadata and
  identity remain unchanged. Completed schema-1 checkpoints remain inspectable.

Use [project checkpoints](project-workspaces.md#operator-recovery-procedure) before
history maintenance or destructive data migrations. Browser/editor integration
and interactive terminals use separate lifecycle contracts.
