# Retained-folder file agents

An explicit Sandbox code-task profile can read and edit files in a selected directory
of the project's retained volume. It uses the existing task admission, cancellation,
retry and reconciliation lifecycle. It does not stage a host source copy.

This is a **file-only profile**. Its model tools are `list_files`, `read_file` and
`write_file`. Shell commands, terminal input, deletion, dependency inheritance,
artifact collection and test execution are unavailable. A model response cannot
add those capabilities. Full folder shell agents and their session UI remain #550 work.

## Admission

The project must have a verified running retained workspace, completed project-local
Sandbox DB migration and a verified code model. The caller needs Project Read, Write
and Execute, the selected VM's Execute grant and the existing model-connection rights.
The task uses the project's selected user VM. Managed Environments are not supported.

Save shared `AGENTS.md` files, then [capture a saved instruction snapshot](project-instructions.md)
for the intended directory. Use `""` for the project root. Set `projectDirectory` to
that exact directory when admitting the task through the existing idempotent endpoint:

```sh
curl -fsS -X PUT "$API/api/ai-sandbox/projects/$PROJECT/tasks/$TASK" \
  -H 'Content-Type: application/json' \
  -d "{\"kind\":\"code\",\"input\":\"Update the authentication helper\",\"projectDirectory\":\"api\",\"instructionSnapshotId\":\"$SNAPSHOT\"}"
```

A host `sourcePath` is not required or read in this mode. Clear host context Git refs,
reference files and selected skills before admission. Dependencies and an environment
role are rejected. Selecting a snapshot by itself, without `projectDirectory`, retains
the existing isolated-copy workflow.

The accepted database, volume, generation, selected directory and VM boot are fixed
before dispatch. Identity changes stop execution. They do not silently select another
runtime or directory. VM and project execution rights are checked at file-I/O boundaries.

## File changes and history

Paths are relative to the selected directory. Parent traversal, symlinks, reserved
workspace metadata and the Sandbox credential-file exclusions remain blocked.
`read_file` returns UTF-8 content, a folder-relative `path`, a project-relative
`projectPath`, an exact `version` and SHA-256. A missing file returns version zero.
Reads are limited to 32 KiB. Encoded tool results above 15 KiB fail rather than returning
partial JSON or a truncated version. Use the editor for larger files or directories.

`write_file` requires `expectedVersion` from the latest read. Use zero only to create a
missing file. Concurrent changes cause a conflict and require another read. The native
file transport bounds writes to 1 MiB. Known connected-provider credentials are redacted
from raw read fields and rejected in writes.

Writes take effect immediately in the retained folder. The task's `folderMutations`
records each confirmed operation ID, project path, before/after versions and SHA-256.
It does not combine concurrent users' edits into a single attributed diff. Existing file
history, diff and explicit restore APIs remain available. The isolated-copy task `apply`
endpoint rejects this profile because its confirmed writes are already saved.

Previous provider context is selected by project, authenticated caller, directory and
assigned agent. Unrelated recent tasks cannot evict that scope's history. This does not
change the existing project permissions for viewing public task records.

## Folder session records

The existing task list accepts `projectDirectory` and `assignedAgent` query filters:

```text
GET /api/ai-sandbox/projects/{project}/tasks?projectDirectory=api&assignedAgent=Backend
```

An empty `projectDirectory=` selects the retained root. Omit `assignedAgent` to
select only unassigned tasks in that directory. An agent filter without a directory
is rejected. The server fixes the caller from its verified transport identity;
clients cannot supply another caller. This scope needs Project Read but no host
source grant or running VM. It includes active, failed and interrupted attempts.

Each page contains at most 50 summaries in chronological order. Continue with
`before` set to the oldest returned task ID. `since` is an inclusive update-time
filter. Drain older pages in the same poll window before advancing that watermark,
and merge updates by task ID. These filters run before the page bound, so another
caller's recent tasks cannot hide the selected session's older records. Omitting
the folder filter preserves the existing board and conversation query behavior.

## Interruption and recovery

The server chooses each write's opaque operation ID and stores it in the canonical task
event before dispatch. The existing pending-effect fence blocks more actions after an
uncertain result. A task has at most 128 retained activity events in this profile.

Task finalization and explicit task reconciliation observe native write receipts only.
They never repeat a write, run a model or send a shell command. Confirmed file versions
remain queryable while the VM is stopped. A pending operation without a verifiable
receipt leaves the task Interrupted. Preserve its operation ID. Inspect and explicitly
recover that original file operation using the existing file-operation API, then reconcile
the task. Do not submit a fresh task to guess whether an uncertain write happened.
Cancellation stops further work; it does not roll back already confirmed file versions.

## Validation boundary

Final local validation passed 109 distinct tests selected across folder agents,
instructions, Sandbox persistence and project workspaces.
API/types Clippy with warnings denied and public-document link checks also passed.
The tests cover metadata provenance, stopped-VM confirmed receipts, unresolved-write
retention, boot/generation fencing, grants, caller-scoped context, path restrictions,
credential redaction and unsupported command dispatch. These tests do not establish a
live ChatGPT/Claude execution or Linux/KVM folder-agent acceptance. Those checks require
a configured provider and runtime environment and remain open.

## Canvas and Personal Page sessions

Open a project in AI Canvas and select **Folder agent** in the navigator. The new
session binds to the folder currently selected in that canvas. Changing the
navigator folder does not retarget an existing session. The Personal Page shows
the same sessions beside Web VS Code, with a session selector and an explicit
new-session action. Switching views keeps the private request draft.

Choose a connected ChatGPT/OpenAI or Claude model explicitly. The selector reads
caller-authorized account metadata and existing model proofs. Loading the page
does not probe providers. Refreshing the catalog and verifying tool support are
explicit actions. An unavailable or unverified saved selection never falls back
to another connection or model. Configure accounts in **Pipe → AI**.

Before dispatch, the private canvas saves the exact task UUID, snapshot UUID,
model and request. A failed save cannot capture rules or start a task. After a
lost response, reopening probes the original task UUID. An explicit retry uses
the same IDs and body. It does not create a replacement task. Task records remain
authoritative in the existing Sandbox store, including after project-DB migration.

Requests use saved `AGENTS.md` snapshots, not private editor buffers. Inspect a
task to see the captured rule-file versions and confirmed file mutations. Version
links open the existing append-only file history. Earlier tasks retain their
original model, rules and result after the session's model changes.

Closing a panel removes its private visual attachment. It does not cancel server
work. Use **Cancel task** to cancel active work. **Check saved receipts** reconciles
an interrupted file task without replaying writes. Reopen the retained private
canvas to retrieve its session. A new blank canvas has no inherited sessions.

This profile currently supports bounded file operations. Shell commands, official
skill installation and real provider/native-runtime acceptance remain pending.
