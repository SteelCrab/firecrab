# Sandbox recovery acceptance

Use `scripts/ci-qa-ai-sandbox-recovery.py` after the
[combined model/MicroVM gate](ai-sandbox-foundation-qa.md).
This gate uses a real verified model and a real disposable user MicroVM.
It terminates the dedicated API process and stops/starts the selected VM.
Never run it on a shared API host.

## Prepare the owned environment

Use a separate [worktree development instance](worktree-dev.md) or a disposable
Linux API host. A macOS source-build instance needs at least 24 GiB of free host
storage. For a prebuilt API/helper fixture, budget management disks, image staging
and the fixed workload separately; monitor physical host free space during setup.
The current API and helper must run on this host, with working KVM and pinned
operator SSH to the selected workload VM.

1. Generate a run UUID. Replace `RUN_UUID` below with that value.
2. Create one Pipe AI connection named `sandbox-recovery-RUN_UUID`. Complete its
   normal authentication. Keep credentials in the API's private store.
3. Create one user Ubuntu MicroVM and its own MicroNetwork, both named
   `sandbox-recovery-RUN_UUID`. Use one CPU, 512 MiB RAM and the minimum image disk.
   Disable internet access and IPv6; use the VM's isolated egress policy.
4. Start it and verify operator SSH with its pinned host key. Do not bind it to
   another project. The API must have no other VMs, connections or projects.
5. Prepare `/srv/firecrab-qa` with directory traversal for the API service user.
   Use a new results directory below it. Avoid `/tmp`: systemd `PrivateTmp` can
   hide the operator's fixture from the API.
6. As the owner of this disposable host, create a root-owned `0700` directory
   `/run/firecrab-sandbox-qa`. Inside it, create a `0600` regular file named
   `RUN_UUID` whose contents are exactly that UUID. The harness does not create
   this ownership approval for itself.

Run as root **inside that disposable Linux API host**:

```sh
python3 scripts/ci-qa-ai-sandbox-recovery.py \
  --api http://127.0.0.1:5523 \
  --run-id RUN_UUID --connection CONNECTION_UUID --vm VM_UUID \
  --model VERIFIED_MODEL_ID \
  --owner-marker /run/firecrab-sandbox-qa/RUN_UUID \
  --vms-dir /var/lib/firecrab/vms \
  --results-dir /srv/firecrab-qa/recovery-RUN_UUID \
  --live --allow-owned-faults
```

Use the actual storage root's `vms` directory if it differs. No key contents are
copied into arguments or reports. The operator SSH client uses only that VM's
private key and pinned public host key, with agent/forwarding disabled.
The API port must belong to the main process of `firecrab-api.service`.
The harness verifies its executable and listening socket before sending SIGKILL.

## Required checks

- Verify real text and native-tool permission for the configured model.
- End a real guest command at its two-second command deadline and return its
  actual output and exit status to the model.
- Cancel a running command and retain its separate stdout, stderr, exit status
  and unapplied file changes. Shell-generated signal diagnostics remain intact.
- Close an admission HTTP connection without reading its response. Inspect the
  same task identity, without repeating an uncertain admission or command.
- Kill the owned API process. Confirm a new controller PID with the same binary,
  an interrupted durable attempt, no automatic replay, and explicit reconciliation.
- Retry that reconciled task explicitly. Require a new attempt, the original
  VM/runtime identity and the actual guest result returned to the model.
- Stop/start the same workload VM. Confirm a different guest boot, preserve the
  original task/runtime references and reconcile without rerunning the command.
- Inject a possible PID/start-time reuse after the real reboot. Confirm the old
  boot's supervisor record cannot signal a different root process in the new boot.
- Preserve source files, the borrowed VM/network and a root-owned retained-data
  sentinel across reconciliation and project deletion.
- Export the report before deleting the owned project. Disconnect only the named
  QA connection and confirm remote grant retirement.

The separate ten-minute task deadline is tested with Tokio's paused clock.
The live two-second command timeout does not establish a real ten-minute task run.
The root-container boot-fence test uses a simulated old boot marker; it is distinct
from this gate's actual MicroVM stop/start.
The live reboot fixture calls `sync` before stopping the VM. This verifies recovery
of persisted data and the new-boot process fence, not durability of unflushed bytes
after a power loss.

## Evidence and failure handling

`summary.json` is atomically written with mode `0600`. It records the harness and
API binary hashes, timestamps, task/attempt/runtime references, model responses,
actual output, changes, supervisor/boot identities and check results.
Guest-generated random nonces and one-line execution counters prove that results
come from one actual execution per attempt. A successful assistant message alone
cannot pass.

On failure, preserve the source, project, guest files and stable task IDs.
Inspect the recorded attempt before cancellation, reconciliation or explicit retry.
Do not blindly repeat the gate. An uncertain result is not permission to replay.

On success, the harness removes its project/source after exporting evidence.
It preserves the VM/network and the retained-data sentinel. Export the full report
outside this disposable environment before its operator removes owned resources.
Review private evidence before publication; it contains model output and paths.
For a larger validation batch, `--keep-owned-connection` preserves the same QA
session. The report records retention instead of claiming revocation. Disconnect
that owned connection after the entire batch; do not start more login flows.

Run `python3 scripts/test-ci-qa-ai-sandbox-recovery.py` for offline ownership,
listener, admission and evidence guard tests. Linux CI also runs real SSH and
dedicated-user interruption contracts in its network-isolated root container.
Neither fixture tests nor the root container alone establish this live gate.

## Verified recovery run — 2026-10-26

The complete live gate passed from 09:43:29 to 09:46:05 UTC using the same
human-authorized ChatGPT connection, model `gpt-6.1-sol`, and an isolated Ubuntu
26.04 MicroVM. No additional login flow was started. The deployed API source was
`68fec554`; later environment, coordination, and Tunnel changes are not covered
by this run.

| Scenario | Durable task | Observed result |
| --- | --- | --- |
| Two-second command timeout | `aac8007e-71cf-41eb-8cb4-6c9218c80c3b` | Result, exit 124 |
| Cancellation during execution | `b8d1fdb4-aec9-4d3c-b2f9-d7db4a598400` | Cancelled, exit 137 |
| Unread admission response | `b82bba21-9972-4c14-abc6-cc3197c103f1` | Same admitted task, Result, exit 7 |
| Controller SIGKILL and reconciliation | `f03c593c-d54c-47ea-af2f-fcc5eef7bcda` | New API PID, no replay, Cancelled, exit 137 |
| Explicit retry after reconciliation | `6d80567c-729d-4118-82fd-34bcf2fd3a74` | New attempt, Result, exit 7 |
| Real guest stop/start and reconciliation | `396b8239-3c51-4326-9b0f-2e03409d1e87` | New boot, no replay, Cancelled, unknown exit preserved |

All six tasks retain their attempt IDs and the same VM/runtime references.
Guest nonces and execution counters establish one command execution per attempt.
Separate stdout/stderr, changes, model responses, and result exports were retained.
The unrelated root supervisor injected after reboot was not signalled. The source,
borrowed VM/network, and retained-data sentinel were preserved. The reboot fixture
explicitly called `sync`; it does not establish unflushed power-loss durability.

The final private report was exported before project cleanup. Its SHA-256 is
`2a5ac03614b686dcd502ed1db3dc85df47f2022e8d8cfdd5ea5dbc4d6973b44d`.
The deployed API SHA-256 is
`6c523fde5ee4b003d65b11be2309793e5fc7446855a9213e831259b962cc3fcf`.
The executed harness SHA-256 is
`2a7521b7eb54273aca66fd39fe6acc965ecf27e36f101c736048c71beac54bac`.
The connection was deliberately retained with `--keep-owned-connection` for the
remaining issue #414 validation batch. Remote revocation is not claimed here;
the operator must disconnect it after that batch.

Local validation also passed 41 Linux Sandbox tests and all three privileged
guest contracts, including real SSH interruption and recovery. The Python
recovery guard suite passed 11 tests. These checks complement the real run.
The Canvas handoff still requires CI acceptance for the submitted commit.
