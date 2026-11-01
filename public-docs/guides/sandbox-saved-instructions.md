# Saved AGENTS.md context for Sandbox tasks

A code task can select an immutable instruction snapshot from its project's retained
workspace. Admission copies the accepted saved-file versions into the private task
record. Restart, reconnect, a moving file or another user's editor buffer cannot
silently change those rules.

This capability supplies saved guidance to the existing Sandbox engine. Its file
and command tools still use the existing isolated source-copy workflow. Direct
execution in `/workspace/shared` and folder-agent sessions remain later #550 work.
Selecting a snapshot does not grant additional filesystem or runtime access.

## Prerequisites

- The project has a retained workspace and completed Sandbox project-DB migration.
- The caller can read and execute the project and its selected VM/model connection.
  Existing Sandbox source-directory read rights are still required.
- The project has the existing Sandbox source directory and a verified code model.
- Code execution uses the project's selected user VM. Managed Environments do not
  accept this option yet.

See [workspace setup](project-workspaces.md) and the
[task storage migration procedure](../development/project-sandbox-migration-2026-11-01.md).

## Capture and select

Save the shared AGENTS.md files first. Capture a stable snapshot UUID for the
selected directory; this includes saved ancestor rules in root-to-leaf order.
Private editor drafts are never captured.

```sh
curl -fsS -X PUT "$API/api/ai-sandbox/projects/$PROJECT/workspace/instructions/snapshots/$SNAPSHOT" \
  -H 'Content-Type: application/json' -d '{"directory":"api/auth"}'
```

Submit through the existing idempotent Sandbox task endpoint:

```sh
curl -fsS -X PUT "$API/api/ai-sandbox/projects/$PROJECT/tasks/$TASK" \
  -H 'Content-Type: application/json' \
  -d "{\"kind\":\"code\",\"input\":\"Update authentication tests\",\"instructionSnapshotId\":\"$SNAPSHOT\"}"
```

After an uncertain response, retry the identical task UUID and body. The original
request and saved context are returned; the server does not recapture live rules.
A different snapshot is a different request. Use a new task UUID for that change.

## Scope, permissions and replay

Each AGENTS.md applies to its containing directory and descendants. Deeper rules
in the selected ancestry override ancestor rules only within that scope. This
version does not automatically discover every descendant rule in the project.
The current user task remains after the saved guidance in the provider history.
Both existing model adapters consume the same ordered user-level context.

The caller must own the saved snapshot. Another caller receives the same missing
response as an unknown snapshot, even with project read access. Task admission
retains the database ID, volume ID and binding generation. Subsequent effects
recheck that binding, existing Sandbox rights and the exact admitted VM runtime.
Changed bindings require a new attempt. Invalid or missing context never falls
back to current files or editor drafts.

Known provider credentials are rejected before the snapshot enters a task.
Every model turn refreshes credential redaction on raw fields before JSON
escaping. Provenance hashes describe the original accepted saved bytes, even when
the provider-facing copy needs redaction. Public `instruction_context` events
contain snapshot/file IDs, versions and hashes; they omit the saved file contents.

Legacy requests omit `instructionSnapshotId` and keep their existing behavior.
No real provider execution or live folder-agent acceptance is established by this
guide. Those #550 checks remain open until their runtime evidence is available.
