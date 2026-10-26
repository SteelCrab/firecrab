# OpenRC guest V1 QA: offline preparation only

This is a deferred recipe for Root. No API, provider, model, SSH, setup, fault,
cancel or deployment command was executed while preparing it. Static plan tests
are not live OpenRC acceptance. AI Canvas implementation remains blocked until
all AI Sandbox implementation and direct actual QA are complete.

The target is fixed:

| Reference | Value |
| --- | --- |
| Project | `70b3dc57-c6e6-4033-8409-d00d7b406d30` |
| Environment | `4b9b2c97-bff6-4623-aa30-df7e9812c141` |
| Role | `debug` |
| Existing approved connection | `b6804610-c932-40b3-9d37-3cebd426b8e8` |
| Model | `gpt-6.1-sol` |

Do not change the other strict-pair role/Environment, saved connection, BaseSeed,
DB fixture, network policy, engine image or service configuration. The current
project instructions say **NoModel**. They remain in force. Preparing JSON files
does not authorize overriding them or submitting an inference request.

## Root gates before any setup or model admission

1. Complete and export the strict-pair baseline, deployment, fault/recovery and
   network checks. Wait for Root's explicit release of the hold.
2. Export the original project and Environment DTOs privately, including the
   complete NoModel instructions, selection, source/context, generation, role
   mode, VM/runtime, ownership and strict DNS posture. Record their byte hashes.
   Require the exact project/Environment/role, private_only DNS, ready agent and
   network, and the current debug VM/runtime. Do not assume a runtime survives a
   reboot. Do not change Docker/nspawn/Workspace mode or a pin to make QA pass.
3. Recheck the existing runtime's actual V1 capability and OpenRC init using the
   existing pinned operator probe or an already valid scoped guest health
   endpoint. An installed OpenRC file alone is not the complete init/readiness
   proof. Missing Python, trusted UID helper or hard limits is unsupported;
   retain that result instead of provisioning silently or using a shell fallback.
4. Root explicitly reviews source binding and any project selection/instruction
   change after the baseline. Save both the original NoModel spec and the exact
   approved replacement. Preserve all unrelated fields and environmentId;
   keep vmId null for this managed Environment. Re-read the saved DTO and compare
   its full spec before admission. The preparer creates no project PUT payload.
5. Check the saved connection/model proof through the existing Sandbox contract.
   Only Root may request an allowed real verification/inference later. Use the
   given connection; never sign in, issue new OAuth, read/copy a credential or
   disconnect this borrowed approved connection. A failed proof stops the run.

Do not run `ci-qa-ai-sandbox-recovery.py` or the coordination runner unchanged on
this shared manager: their dedicated-host/sole-account assumptions and controller
fault actions differ. Reuse their explicit identity ledger, durable admission,
bounded waiting and export-before-cleanup rules. Root alone operates faults.

## Offline preparer and public payload

`scripts/qa-sandbox-openrc-v1.py` has no networking, subprocess, SSH or exec mode.
It writes private local plan/source files only. Its guest payload is never
imported or run by the preparer/tests. A new output directory is mandatory; it
refuses overwriting an existing run. For later Root use:

```sh
python3 -B scripts/qa-sandbox-openrc-v1.py \
  --output /PRIVATE/NEW_PLAN_DIRECTORY --run-id NEW_RUN_UUID
```

After the gates, Root may also pass `--project-json` and `--environment-json`
with the preserved original NoModel DTOs. The offline guard freezes their spec
hash, ownership, strict DNS, generation and debug runtime without changing them.
All `rootGates` remain false in the prepared plan; no file is a claim that the
live gates passed. The `admission()` helper validates exported task identities
against that frozen record and rejects changed attempts, requests or runtimes.

The remote source path is planned as
`/srv/firecrab-qa/openrc-v1-414-RUN_HEX/source`; the preparer never creates it.
Only after explicit setup approval does Root create that new controlled path.
Copy only `source/probe.txt` and `source/qa cwd/probe.py`, with root-owned 0755
directories and 0644 public files readable by the API. Keep plan/results private
0700/0600. Do not copy a real project, provider state, SSH files, DB or seed data.
The source sentinel is exactly `not executed\n`.

Root may establish a local Git baseline there with hooks, fsmonitor and signing
disabled, following the existing QA runner's sanitized Git setup. Record the
commit and file hashes. Existing retained Workspace source/Git-origin checks may
reject a different source: stop and review that rejection, never bypass it by
deleting/rebinding a retained VM or changing generation.

## Literal native inputs and scenario evidence

Each `task-CASE.json` contains a review wrapper. Submit only its `request` object
after all gates. Every request explicitly selects debug and the existing model,
sets retryOf null, dependencies empty and retryLimit 0. Task UUIDs derive from the
run UUID and case, so the exact identity is known before any mutation.

The native `run_argv` tool uses `env` as an array of name/value entries. The
internal `SandboxGuestCommand` DTO's map is not that tool schema. The plan uses
version 1, Python argv, cwd `qa cwd`, explicit public env, 3 CPU seconds, 64 or
96 MiB address-space limits and bounded per-stream output. It never selects a
shell. Literal arguments contain spaces, quotes, dollar/command-substitution
syntax, wildcards, a newline and an empty argument. None may be interpolated.
Only stdin contains an embedded NUL; argv and env NUL remain invalid by contract.
The public probe checks the exact stdin bytes and records their hex/digest.

Every executed case generates a guest nonce, records non-root real/effective UID,
GID, empty supplementary groups, NoNewPrivs=1, CapEff=0, own PID/boot/start ticks,
cwd, OpenRC file/state evidence and actual memory/CPU/nofile hard limits. It
checks sensitive environment-name presence without printing any value. Receipts
are created exclusively, so a replay cannot truncate earlier evidence. The
controller's current nofile wrapper contract is 1024; a mismatch remains a real
failed probe. These public files are not a package/configuration change.

| Case | Input and actual evidence required |
| --- | --- |
| literal | 15-second wall, 96 MiB, 4096 bytes/stream; exact stdout line `OPENRCV1:RUN_HEX:literal:stdout:NONCE\n`, separate stderr `OPENRCV1:RUN_HEX:literal:stderr\n`, actual deliberate exit 7 |
| memory | 64 MiB; one uncaught 256-MiB allocation; retain actual MemoryError/nonzero result and start artifact. A signal alone without allocator/kernel evidence is inconclusive, not a normalized pass |
| wall | 2-second wall request around a 90-second sleep; retain the actual raw exit/status and nonce. Confirm the recorded process stopped under the bound; never assume 124 or 137 |
| output | 256 bytes/stream; 8192-byte public bursts after the nonce; require each retained stream within 256 and preserve the actual controller exit. Do not invent code 125 if it was not observed |
| cancel | First run cancel-ready once, record its actual successful event and retain its immutable required artifact. Then run cancel-running once with a 120-second wall. Root verifies that running process and cancels it; neither call is a retry |

For the ordinary cases the model records the actual completed tool eventSequence
with `record_test`, then calls `retain_artifact` for the planned JSON file. It
must take eventSequence from the tool result, not assume an index: project
context may add an event first. Deliberate exit 7 and actual failing limit events
produce failed test records; reaching Result does not turn them into passed tests.
No command is repeated to obtain a nicer exit. If the command is unsupported,
the task is interrupted or export is uncertain, preserve it and stop.

## Admission, cancellation and export

Before each case, Root confirms the known task UUID is absent, then submits its
exact request once through the existing API/CLI/MCP lifecycle. Persist the
request bytes, task/attempt, environment generation, runtime and controller/CLI
binary hashes. On a lost mutation response, GET that same UUID and inspect the
admitted request/references. Never resend, choose another UUID or infer failure
from an HTTP disconnect. Reconnect only to inspect. If admission remains
uncertain, preserve the plan and resource ledger for Root.

Poll each read with a 10-second request timeout and a 180-second case deadline.
Deadline expiry does not mean a task stopped and does not authorize cleanup or
another case. Root cancels/reconciles only the admitted task before proceeding.

For cancel, wait until cancel-ready is actually retained and cancel-running is
executing. Query the task-scoped guest processes and capture the exact owned
PID, bootId and startTicks. Root may read only this run's public running JSON via
the already pinned operator channel, using the actual captured mode:

- Attempt workspace: `/var/lib/firecrab-ai/ATTEMPT_HEX/workspace/qa cwd/…`.
- Retained Workspace: `/var/lib/firecrab-ai/.workspaces/VM_HEX/qa cwd/…`.

Match its nonce and ready-file digest to the downloaded required artifact. Send
the existing task cancel operation once. HTTP 202 is an acknowledgement; wait
for actual terminal cancellation/export and verify that exact process identity
is gone. A reused PID with different boot/start ticks is not the old process.
After guest cleanup, processes may return an explicit inspection error; do not
convert that error to an empty process list. Use the captured identity and the
owned pinned SSH observation when necessary. Do not read/copy keys: reference
existing `-i` paths and pinned public host keys only. No foreign-key test is
required by this plan.

Preserve the cancel event's actual exitCode, including null if a command exit
was not established. Null is not 0, 143 or a proven normal completion. Separately
record confirmed process termination and the exported cancellation result.
The already retained cancel-ready artifact survives cancellation; the public
cancel-running JSON must also remain in the retained task changes/export.

Export every full task JSON as bytes plus SHA-256, every artifact's exact
downloaded bytes/size/digest, and its taskId/attemptId/artifactId/path metadata.
The offline `artifact()` helper checks those exported bytes against the admitted
request, exact run_argv events, guest nonce, literal argv/env/stdin and actual
test/exit evidence. It rejects source edits and disallowed tool events. It always
returns `liveQaAccepted:false`; wall/cancel still require Root's independent
termination receipt, and a missing actual exit is never normalized.
Export the Environment through its existing export endpoint after settled
tasks; it must validate required artifacts. Root verifies the original source
sentinel/probe hashes, immutable BaseSeed and DB-fixture receipts remain intact.
Do not apply task changes or assume only one changed file in a retained Workspace.
No project/Environment/VM/network/connection/seed cleanup is automatic here.

Offline static/fixture checks are `python3 -B scripts/test-qa-sandbox-openrc-v1.py`.
They validate plan scope, NoModel/ownership guards, literal JSON/NUL transport
inputs, no-retry identities, file privacy and syntax without executing the guest
payload or sending any live request.
