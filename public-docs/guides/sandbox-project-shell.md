# Bounded project shell commands

A retained-folder task can explicitly enable commands in its selected project VM.
The existing Sandbox engine remains authoritative for task identity, admission,
cancellation, events, restart interruption and reconciliation.

## Enable commands

The default remains file-only. In Canvas or the Personal Page session, select
**Enable shell for new requests**, choose a verified model and submit a new task.
The private view saves that capability with the exact pending request before any
snapshot capture or task admission. Retries cannot silently change it.

API clients add `"projectShell":true` to an existing folder-task request:

```json
{
  "kind": "code",
  "input": "Run the project tests and report the result",
  "projectDirectory": "api",
  "instructionSnapshotId": "11111111-1111-4111-8111-111111111111",
  "projectShell": true
}
```

The project requires initialized retained storage, project-local Sandbox task
migration, saved instruction capture, a running verified VM and a helper with
project-command protocol 1. Admission probes that helper before starting the task.
Existing VMs with an older helper fail explicitly; this change does not restart or
silently replace an existing runtime. Project Read/Write/Execute, VM Execute and
model-connection rights still apply.

## Command contract

The additional model tool is `run_argv`. It accepts protocol version 1, literal
`argv`, a folder-relative `cwd`, explicit `env` name/value entries, UTF-8 `stdin`
and enforced limits. Empty `cwd` selects the admitted folder. A shell is explicit:
`["/bin/sh", "-c", "cargo test"]`. The controller never executes these arguments.

- Wall deadline and CPU ceiling: 1–120 seconds each.
- Address-space ceiling: 16–65536 MiB.
- Retained output: 1–65536 bytes for each of stdout and stderr.
- Fixed limits: 256 open descriptors, 128 processes, zero core dumps and 64 MiB
  per regular output file. The kernel enforces these before executing the program.
- Up to 128 argv entries and 8192 argv bytes; up to 64 explicit environment names
  and 16 KiB of environment data; stdin up to 64 KiB. The model schema has a
  stricter 32-entry environment bound.

A native root supervisor verifies the volume UUID and project/database/import
marker. It reserves a non-reused unprivileged UID, drops supplementary groups,
sets `NoNewPrivs`, closes inherited descriptors and creates a fresh home. Controller
and provider environments are not inherited. The project group provides shared
file access without recursively changing file owners to each command's UID.

The folder is the starting directory, not a filesystem jail. Commands have their
VM user's normal file/network access. Only callers already authorized to execute
in that VM can enable this capability. Private controller databases remain outside
the guest mount. This profile does not expose a controller/host shell.

## Effects and retained evidence

The controller generates an operation UUID and records the exact request in the
existing Sandbox event/pending-effect checkpoint before native dispatch. The
native journal retains the immutable payload, boot and process identities,
separate bounded output and the observed exit status under the protected project
marker. At most 4096 command records are retained per project. Exhaustion rejects
new commands without evicting existing records.

The command's supervisor survives SSH/client disconnect. Repeating a launch UUID
and identical payload only returns its retained state; it cannot execute again.
A different payload conflicts. Observing a missing record never launches it.
Cancellation of a missing record creates a durable cancellation fence, so a late
arrival of that original launch cannot start after reconciliation.

Commands modify shared files immediately. They do not produce per-write file API
receipts or an isolated apply diff. Reading changed files through the file API
observes new saved versions. Other terminals and users may edit concurrently;
command execution does not make a filesystem transaction. The supervisor syncs
the retained filesystem before publishing a normal final result.

Task activity retains stdout, stderr and an actual exit code. Output truncation
is labeled. Timeouts report 124; cooperative cancellation reports 143. A lost
supervisor retains partial output and reports interruption without inventing an
exit code. A zero exit status is required to claim a successful command/test.

## Stop and recover

The guest enforces its deadline even while the controller is absent. On completion
or cancellation it terminates the command UID's descendants, including detached
processes. pidfds and kernel start times prevent signaling recycled PIDs. A
retained UID ledger prevents another terminal/command from receiving that UID.
Old-boot records cannot signal a current-boot process.

The existing task cancel/reconcile workflow sends only observation/cancellation
operations for recorded command IDs. It never replays argv or a model request.
Failure to confirm cleanup retains the canonical pending fence and interrupts the
task. Keep the original identity. After a VM stop/start, first reconcile the same
workspace mount. Receipt observation/cancellation may then use its new verified
boot while retaining the original database, volume, VM and binding generation.
Launch remains pinned to the original boot. Native boot checks prevent old process
identities from signaling current-boot processes. The saved outcome can remain
interrupted; it does not become a successful command result.

Settle pending commands before rebinding to another VM/generation. A stopped VM
cannot supply new output; confirmed task events remain queryable from the project
DB while it is off. Full runtime-replacement recovery acceptance remains open.

Commands are non-interactive and bounded. Process memory and environment variables
do not carry into the next command. Use the separate project PTY terminals for
interactive or long-running development servers.

## Validation boundary

Native Linux process tests cover literal argv, stdin, clean environments, limits,
output caps, cancellation, detached descendants, sibling isolation, immutable
request retries, late-launch cancellation fences, supervisor loss and boot/UID
reuse. They use an isolated root container with temporary project data.

Browser fixtures verify the capability selection and retained request identity.
They do not execute a VM or provider. Real ChatGPT/Claude plus workload-VM execution,
production service-user operation and complete restart/replacement acceptance
remain #550 gates. The PR remains Draft and must not be merged yet.
