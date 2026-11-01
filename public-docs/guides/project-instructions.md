# Saved project instructions

A project can retain an immutable snapshot of saved `AGENTS.md` files in its own SQLite database. Schema 6 adds the snapshot table without removing existing files, versions, private views, drafts, or editor records.

## Capture and recovery

Use a stable nonzero UUID for one logical capture. Send `PUT /api/ai-sandbox/projects/{project}/workspace/instructions/snapshots/{snapshot}` with `{"directory":"api"}`. The directory is relative to the retained project folder. Use an empty string for the root.

The server verifies the running workspace and reads saved files through the existing protected file transport. For `api/auth`, it reads `AGENTS.md`, `api/AGENTS.md`, and `api/auth/AGENTS.md`, in that order. Missing files are omitted. Private editor drafts are excluded. Deeper descendants are not scanned.

The result contains the exact text, file-history version and SHA-256 for each saved rule file. Its bundle checksum covers the selected directory and the ordered file records. Later shared-file edits cannot change an accepted snapshot.

Retry the same UUID and directory to retrieve the accepted result. Reusing it for another directory returns a conflict. A completed retry and `GET` on the same endpoint remain available while the workload VM is stopped. A new capture requires a running verified workspace.

## Access and bounds

Project Read permits capturing and reading one's own private context. Verified transport identity selects the caller. Another caller cannot read or replace that snapshot, including an operator with a different identity. Access is rechecked around retained database operations. Snapshot capture does not grant VM execution rights.

Rules must be UTF-8 without NUL bytes. Each file is limited to 32 KiB. The total rule text is limited to 64 KiB. Serialized snapshots are limited to 256 KiB. A project retains at most 1024 snapshots and 64 MiB of snapshot documents. A full quota rejects a new capture and preserves accepted retries. No existing snapshot is evicted automatically.

## Integration status

This endpoint is a foundation for #550. Sandbox task admission does not yet consume these snapshots. It does not start a model, dispatch a task, or install a skill. Project-local task migration and provider-backed agent acceptance remain pending. No provider connection is available for the current live model gate.
