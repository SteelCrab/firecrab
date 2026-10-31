# Private views and drafts

Each retained project database stores private canvas views and unsaved file
buffers separately from the project's saved files. Schema 4 adds these tables
without moving or deleting previous records. The verified transport identity
selects the user. Requests cannot supply or override that identity. An operator
does not automatically receive another user's private drafts.

## Identity and recovery

A browser tab has a stable, non-empty UUID. Creating a new tab does not attach a
project or copy the current layout. After a user explicitly opens a project,
the pair `(verified caller, tab UUID)` selects its private view in that project's
database. Reopening the same project uses its retained database and volume.
Different users can use the same tab UUID without sharing private state.

Use local browser storage only for tab/project identifiers and the selected tab.
Keep layout, open-file metadata and unsaved buffers in the private project DB.
Do not place provider secrets, terminal control tokens or editor connection
tokens in a view document.

These endpoints do not require a running workload VM. The caller can recover
private state after an API restart and while the project's VM is stopped.
Saving a draft does not save a project file or create a shared file version.
Explicit file saving uses the authorized file API and its expected base version.

## API

Paths below follow `/api/ai-sandbox/projects/{project}/workspace`.

| Method and suffix | Result |
| --- | --- |
| `GET /views` | The caller's view UUIDs, revisions and update times. |
| `GET /views/{view}` | That caller's retained view document. |
| `PUT /views/{view}` | Save `{operationId, expectedRevision, document}`. |
| `GET /views/{view}/drafts` | Paths and revisions of active drafts, without their contents. |
| `GET /views/{view}/draft?path=...` | Draft revision, saved base version and base64 content. |
| `PUT /views/{view}/draft?path=...` | Save `{operationId, expectedRevision, baseVersion, contentBase64}`. |
| `DELETE /views/{view}/draft?path=...` | Discard using `{operationId, expectedRevision}`. |

Project Read permits access to one's own view and drafts. It also permits saving
one's own layout. Draft changes additionally require Project Write. These
permissions do not grant VM execution or access to another user's private data.

The first save expects revision 0. Subsequent changes require the exact returned
revision. A mismatch returns a conflict without overwriting either buffer.
Clients keep local unsaved edits when a request fails and display the conflict.
They must not automatically replace their expected revision and overwrite.

A discarded draft returns `contentBase64: null` with its retained revision.
Use that revision to start a new draft. A draft that never existed returns 404.
This distinction prevents an old create request from reviving discarded text.

Use a new operation UUID for each logical change and retain it through transport
retries. The last 4096 private mutations retain their exact acknowledgement.
Older receipts are compacted; their stale revisions still reject overwriting
or reviving newer data. A retry outside that window may return a conflict and
require reloading its current revision. Compaction never removes view contents
or an active draft.

## Bounds

- View document: JSON object, at most 64 KiB of serialized UTF-8.
- Views: 64 per caller/project and 1024 across a project.
- Draft: at most 1 MiB; at most 32 active drafts per view.
- Retained draft identities, including discard revisions: 4096 per project.
- Combined view/draft contents: 128 MiB per project.
- Draft paths use the same boundaries as the file API. A nonzero base version
  must belong to that file in the same project.

Limits reject the attempted mutation explicitly and preserve prior data.
Tab closure retains its data. Permanent deletion and retention maintenance
must follow the project's explicit data lifecycle.
