# Private editor browser lifecycle

Related: #550 and Draft PR #562. This behavior remains unmerged.

## Close and reopen

Use **Close editor** to dispose the current editor surface. Use the Canvas tab close button to close its workspace tab. **Back to canvas** only hides the editor and keeps its document alive. Closing only the editor preserves unsaved Canvas layout changes, including pending/failed metadata saves. Closing a Canvas tab requires its layout to be confirmed first.

An explicit close freezes that iframe, waits for pending shared saves, and asks the browser extension to persist its current text through the existing private draft API. The controller verifies accepted revisions and content hashes. It atomically stores an immutable copy of those exact bytes with the open-file list and active file in the private project SQLite DB. Only its acknowledgement permits detaching that window. The workload editor process, project DB, volume, and other clients remain retained.

A failed or ambiguous request keeps the iframe. **Retry close** reconciles the same pending operation or confirms an already accepted result. **Keep editing** waits for that window's resume acknowledgement before enabling input. Unsupported dirty editors and untitled documents block closing until their contents are preserved.

New editor windows default to manual shared-file saving. Private draft persistence does not save the shared project file. Explicit user auto-save preferences can override that default; shared writes still require the expected saved version.

Reopening the original saved Canvas restores its accepted private drafts and open files. A new empty tab has a different private identity. If another window has since changed a private draft, the earlier close copy remains available through **Firecrab: Open Closed Editor Recovery Copy**. Recovery never replaces a newer accepted draft silently. Opening our virtual files is deferred until VS Code activation returns, avoiding the activation/filesystem deadlock.

## Window identity and authorization

Each native execution has an opaque identity. Each Canvas iframe registers a browser client. Each served workbench document also receives a distinct window proof through trusted workbench defaults. The extension reads configuration defaults, not user/workspace overrides, for that identity. An authenticated external bridge answers the parent's fresh challenge; the parent checks source window, exact origin, client, and challenge. Copying a workbench URL cannot authorize a different window to acknowledge its original's close.

All metadata and snapshot endpoints enforce current project authorization, private caller/view ownership, and project owner fencing. Retained reads, accepted draft operations, and close reports do not require a running workload VM. Native execution termination remains a separate conditional API; browser detach does not stop it.

## API

Paths are relative to `/api/ai-sandbox/projects/{project}/workspace`.

- `PUT /editors/{view}/clients/{client}` registers `{instance}` for a protocol-1 editor. The client is a non-empty canonical UUID.
- `GET /editors/{view}/clients/{client}` reads its control revision and status.
- `PUT /editors/{view}/clients/{client}/control` accepts `{operationId, expectedRevision, action, windowProof}`. Actions are `close`, `cancel`, and `detach`.
- `POST /editors/{view}/clients/{client}/report` accepts `active`, `error`, or `flushed` reports with the exact revision/window proof. A flushed report supplies private file revisions/hashes plus `openFiles` and `activeFile`. Bytes come from the authorized retained drafts, not from an unverified browser snapshot body.
- `GET /editors/{view}/closed-snapshots` lists the latest 32 retained copies for that private editor.
- `GET /editors/{view}/closed-snapshots/{client}/{revision}` reads an immutable, checksummed copy.

The authenticated editor gateway exposes corresponding `/_firecrab/` routes. It validates canonical IDs and exact route suffixes. Workbench defaults are merged as JSON and escaped back into HTML. The proof bridge reuses the native script nonce and does not weaken script policy.

Schema 7 adds private client and snapshot tables transactionally. Limits are 4096 retained clients/project, 16 document activations/client, 1024 snapshots/project, and 128 MiB serialized snapshot storage/project. Each close supports 32 open files and 64 retained paths, at most 48 MiB serialized. Existing private drafts remain limited to 32 active 1 MiB buffers per view. Quota failures preserve all earlier data and keep the browser open; no automatic eviction is added.

## Browser shutdown limit

Closing a browser window, force-quitting the browser, losing power, or killing a process cannot reliably await an asynchronous flush. The page warns before unloading while an editor is retained. Debounced private saves and acknowledged close copies survive; text that has never received a controller acknowledgement has no durability guarantee. Use explicit close before shutting down when confirmation is required. Earlier unversioned editors cannot prove safe disposal and remain retained until their contents are preserved.

See the [validation record](../development/project-editor-browser-close-2026-11-01.md) for the separation between DB tests, synthetic browser-controller regressions, real editor UI checks, and pending workload VM acceptance.
