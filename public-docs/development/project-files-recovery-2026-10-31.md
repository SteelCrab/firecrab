# Project file recovery on Ubuntu and Alpine: 2026-10-31

Related: #550, Draft PR #562. This report covers the file API and retained
workspace foundation. It does not establish completion of the editor, agent,
skill-store, cross-host restore or integrated multi-user acceptance phases.

## Tested source and environment

- API/helper source: `c0696ccf194f29c2e1b3a7cade3a6da59e478e38`.
- Harness source: `642944e3` (`scripts/ci-qa-project-workspace.py --files`).
- Architecture: Linux aarch64; Firecracker 1.17.0 with real nested KVM.
- API SHA-256: `67281d0f1603b337e6e397a097010474a17f9ed99dd8db8a86cee44495d01672`.
- Helper SHA-256: `d882c06cc0e4613431690b1be9ce9d068b35dbc2de5ff215725d9ad7f7408555`.
- Static guest-helper artifact SHA-256: `e2bc87fa2125ed4f6ee07dffd172727223ded66234b4e19aa22aeb45886d4ac6`.
- Harness SHA-256: `23ac89aa55e67cea3511234585d767efe427413f0621aff31a36ccb0188dd172`.

Each run used a fresh project, control database, private project database,
retained ext4 volume, network namespace and temporary API/helper processes.
API/helper processes ran as root in these owned fixtures. Production service-user
and browser/editor acceptance remain separate checks. Installed services and
pre-existing workload units were not restarted or replaced.

| Runtime | Run UUID | UTC start | UTC finish | Result |
| --- | --- | --- | --- | --- |
| Ubuntu 26.04 v5 | `1027018f-f940-4ee5-999d-9ce3a9f66d7b` | 10:43:09 | 10:46:57 | 17/17 PASS |
| Alpine 3.24.1 v5 | `fd3be679-d225-4ea4-aa22-44fe457c899c` | 11:01:10 | 11:03:06 | 17/17 PASS |

The Alpine run verifies the native timed-lock fallback on a BusyBox guest.
Base images were shared as immutable files. Each run had its own writable image
catalog, VM disks, project database and retained volume.

## Observed results

Both runtimes passed these checks:

1. Verify a real ext4 mount and a synced random proof file.
2. Save, reject a stale save, compare versions and restore by adding a new version.
3. Reject reserved metadata paths and a symlink to a system file.
4. Close a save's HTTP connection and retry the same operation ID; retain one operation and one version.
5. Reconnect a new HTTP client without losing project metadata.
6. Restart the API while retaining the same live shim and guest kernel boot.
7. Kill and restart the API while retaining the same live shim and guest kernel boot.
8. Query file history and retry a completed save while the VM is stopped.
9. Query the private project database while the VM is stopped.
10. Stop/start the VM, require a fresh mount proof and recover the files/history.
11. Delete a stopped VM while retaining its project database and volume.
12. Reject a stale binding generation.
13. Read the same file contents and history after replacing the VM.
14. Rebind the original database and volume to the replacement runtime.
15. Retry the completed binding change without advancing its generation again.
16. Report a missing retained volume as unavailable without creating replacement data.
17. Reject ordinary project/volume deletion or detach that would bypass retention.

The outer ownership gate also confirmed the installed API PID and pre-existing
workload-unit inventory were unchanged after each run. The fixtures retained
their private summaries and data volumes. No unflushed power-loss guarantee is
inferred from these synced-write tests.

## Reproduction

Prepare an operator-owned Linux/KVM QA root using the ownership requirements in
the harness. Keep it separate from installed API/helper storage and runtime files.
For each prepared fresh root, run:

```sh
python3 scripts/ci-qa-project-workspace.py --root /opt/q5501027018f --run-id 1027018f-f940-4ee5-999d-9ce3a9f66d7b --template ubuntu-26.04 --files --live
python3 scripts/ci-qa-project-workspace.py --root /opt/q550fd3be679 --run-id fd3be679-d225-4ea4-aa22-44fe457c899c --template alpine-3.24.1 --files --live
```

Those UUIDs identify the completed runs. Do not execute either command against
its retained completed fixture. Use a new owned root and run UUID for a new run.
