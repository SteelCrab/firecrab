# Project workspace recovery acceptance

Run `scripts/ci-qa-project-workspace.py` on a Linux/KVM host with systemd.
The harness starts a private API and helper in a fresh network namespace.
It stops only its child API and the MicroVM units created by that run.
The installed API/helper and existing workload units are preserved.

## Prepare an owned fixture

1. Create a root-owned `0700` directory on a local disk with enough space for
   the installed image, one workload disk, a 1 GiB volume, and retained evidence.
2. As the operator, write a fresh run UUID to `ROOT/owner`, with mode `0600`.
   The harness does not create this marker for itself.
3. Put matching, verified Linux `firecrab-api` and `firecrab-helper` binaries in
   `ROOT/bin`. Use root-owned regular files without group/other write access.
   The helper also requires root-owned executable ancestor directories. When
   staging a bundle as root, use `tar --no-same-owner` to discard CI user IDs.
4. Prepare `ROOT/images` with the installed `ubuntu-26.04` template and its
   complete `.templates.json` registration. Prepare images before entering
   the isolated network namespace. Do not share writable image state with
   an installed API.
5. Prepare empty `ROOT/api`, `ROOT/store`, and `ROOT/evidence` directories.
   Stop any owned preparation API before running the gate.

The existing macOS managed Linux guest can host this isolated fixture when
nested KVM is available. Use `firecrab service shell` for commands and file
transfer. Do not restart its installed `firecrab-api.service` or change the
ownership of its existing data directory. A task-owned bind mount under `/opt`
can expose a private data-volume directory through root-owned ancestors.

Run inside Linux as root:

```sh
python3 scripts/ci-qa-project-workspace.py \
  --root /opt/OWNED_QA_ROOT \
  --run-id RUN_UUID --template ubuntu-26.04 --live
```

The harness reserves its own namespace and a short, owned `/q550...` storage
mount for Firecracker's Unix sockets. Its API listens on namespace-local
`127.0.0.1:19550`. A dummy uplink and a local DNS answer satisfy guest network
readiness without giving the workload Internet access.

## Checks and evidence

- Bind a real ext4 volume, reconcile the guest mount, and write a synced file.
- Close a real HTTP client and reopen the same project from a new client.
- Restart and kill the owned API independently. Require a new API PID, the
  same running workload unit/boot, and unchanged project metadata and file hash.
- Query project metadata while its workload VM is stopped.
- Stop/start the VM. Require a new guest boot and a new mount proof without
  reimporting or overwriting the retained file.
- Delete the old stopped VM, create a replacement, and bind the same database
  and volume with the next generation. Verify the original file hash.
- Reject a stale generation on an actual binding change. Require an identical
  binding retry to keep the same generation. Reject a missing retained disk, ordinary project
  deletion, volume deletion, and direct detach.
- Confirm the installed API PID and pre-existing workload unit inventory are
  unchanged after cleanup. Require each VM unit to run in the owned network namespace.

`ROOT/evidence/summary.json` records timestamps, executable and harness SHA-256
hashes, template identities, project/database/volume/VM IDs, guest boot IDs,
file hashes, and completed checks. It is updated after each check with mode
`0600`. Acceptance also requires exit status zero and the final cleanup `PASS`
line. A workload report alone does not establish successful cleanup.
Failed runs keep their owned databases, disks, logs, and report.
Do not blindly repeat a failed run against its existing project or volume.
Inspect the failure first and explicitly prepare another owned fixture.
Qualification applies only to the selected template and architecture.

Export the report before removing an owned fixture. Do not publish VM private
keys or a copy of the entire QA root. This gate does not exercise editor state,
model tasks, or process-memory restoration. Its explicit `sync` verifies
persisted writes, not durability of unflushed data after power loss.

Run `python3 scripts/test-ci-qa-project-workspace.py` for the ownership,
explicit-live-mode, and process-signal guards. Linux adds a real unrelated-child
process check. These guard tests do not establish MicroVM recovery acceptance.

## Recorded runs

- [2026-10-30: Ubuntu 26.04/aarch64 backend recovery](project-workspace-recovery-2026-10-30.md).
  Browser/editor and installed-daemon acceptance remain outside this record.
