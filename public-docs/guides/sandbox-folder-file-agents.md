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

Local validation passed the 16 folder-profile tests, 16 instruction tests,
17 Sandbox-persistence tests and 75 workspace tests; these groups overlap.
API/types Clippy with warnings denied and public-document link checks also passed.
The tests cover metadata provenance, stopped-VM confirmed receipts, unresolved-write
retention, boot/generation fencing, grants, caller-scoped context, path restrictions,
credential redaction and unsupported command dispatch. These tests do not establish a
live ChatGPT/Claude execution or Linux/KVM folder-agent acceptance. Those checks require
a configured provider and runtime environment and remain open.
