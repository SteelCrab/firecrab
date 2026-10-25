# Live Sandbox foundation gate

Run `scripts/ci-qa-ai-sandbox.py` on a disposable Linux API host to connect a real model, an authorized command on a real MicroVM, and the result returned to that model.
It uses the existing Pipe AI and Sandbox REST APIs. Credentials stay in the API's private store.
The checks consume the connected account's model usage and disconnect the selected QA connection.

## Prepare a separate environment

Use a [worktree development instance](worktree-dev.md) or a disposable Linux host running the current API/helper.
Confirm usable KVM and a healthy writable filesystem before creating the workload VM.
Run one gate at a time in this environment.

1. Generate a run UUID, for example with `python3 -c 'import uuid; print(uuid.uuid4())'`.
2. Create a fresh Pipe AI connection named exactly `sandbox-foundation-RUN_UUID`. Complete authentication through Pipe AI; keep tokens and API keys out of arguments, files used as project context, logs, and guests.
3. Install an Ubuntu image with `setpriv`, `flock`, `tar`, `id`, `od`, `tr`, and `awk`. Create a disposable **user VM** named exactly `sandbox-foundation-RUN_UUID`, with one CPU, 512 MiB RAM, and the image's minimum disk size.
4. Place it on its own MicroNetwork with internet access disabled. Start it and check the pinned SSH host key. The API's operator SSH must reach it.
5. Obtain the connection and VM UUIDs through their existing REST/CLI interfaces. Select two different account-visible models; both must support native tools.

Use a connection created for this run. Do not rename a borrowed connection or copy a connected database to pass the ownership guard.
A separate connection keeps its OAuth refresh rotation and disconnection independent of the user's existing connection.
The harness rejects resources with another name and projects already referring to the selected connection or VM.

## Run on the API host

Use the same OS user as the API so it can read the source fixture.
Copy the standalone Python script to the API host if the checkout is elsewhere.
Use that host's loopback API port, which can differ from the desktop's forwarded API and dashboard ports.

```sh
python3 scripts/ci-qa-ai-sandbox.py \
  --api http://127.0.0.1:5523 \
  --run-id RUN_UUID \
  --connection CONNECTION_UUID \
  --vm VM_UUID \
  --default-model DEFAULT_MODEL_ID \
  --override-model OVERRIDE_MODEL_ID \
  --results-dir /tmp/sandbox-foundation-RUN_UUID \
  --live --disconnect-owned-connection
```

Replace the placeholders with actual values. A new results directory is required.
Keep it outside private server paths such as `/var/lib/firecrab`; Sandbox refuses those paths as project sources.
The two explicit flags authorize real provider requests and disconnection of the matching QA connection.

The fixture is a private Git repository containing one baseline file. Its commit is recorded before execution.
The harness creates one Sandbox project and admits two code tasks through REST.
The first uses the project default; the second supplies a distinct task-specific model.

## Required evidence

Every check must pass before accepting this combined-execution step:

- Both account-visible models complete real text/native-tool verification.
- Each task retains its project, effective model, task ID, unique attempt ID, and original VM/runtime reference across inspection.
- The model issues the authorized `run_command` once. The command runs under an unprivileged guest UID with no-new-privileges and without provider-key or SSH-agent environment variables.
- The guest generates an unpredictable nonce, writes it in the task workspace, emits separate stdout/stderr, and exits with status 7. The final model response returns that actual guest nonce.
- Exported file changes contain the same nonce. The original source file and Git worktree stay unchanged before explicit apply.
- A configured nonexistent model fails real verification, receives no inference proof, and cannot admit a task. There is no silent fallback.
- Disconnect removes only the named QA connection. Subsequent catalogue discovery, verification, and task admission are blocked; unrelated connections remain.
- Results are exported before the owned project is deleted. The borrowed VM and MicroNetwork remain running and retain their identities.

`summary.json` is written with mode `0600` and updated atomically as tasks advance.
It retains model responses, command arguments, stdout, stderr, exit status, source changes, resource references, and check results.
Export the report to private evidence storage before removing the disposable environment.
Review it before publishing; a live report can contain provider-generated text and local paths.
The disconnect receipt records provider-side revocation separately. If it is unconfirmed, local call blocking can pass while the operator still needs to retire that QA grant in the provider's account settings.

## Failure and cleanup

Failure returns a nonzero exit code and records `FAILED` plus stable resource IDs.
The source, project, and uncertain attempts remain available for inspection and reconciliation.
Inspect recorded tasks with `firecrab sandbox inspect TASK_UUID`; cancel active work and reconcile interrupted attempts before retrying or deleting the project.
Choose a new results directory for another run. Do not blindly repeat an uncertain admission.

On success the harness disconnects its QA connection, deletes its project, and removes its source fixture after export.
It leaves the VM and network to their operator. Stop/delete them only after checking their recorded ownership and exporting required evidence.
Remove only the images, packages, and development runtime created for the run.

The harness's own regression tests run without live credentials:

```sh
python3 scripts/test-ci-qa-ai-sandbox.py
```

They validate report rejection and ownership protection using fixtures.
The real-provider/real-MicroVM record is separate from those tests.
Controller-crash, guest-reboot, timeout/cancellation, and Canvas handoff acceptance remain separate foundation checks in #414.
