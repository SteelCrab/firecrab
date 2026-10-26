# Real Debug → Test acceptance

Use `scripts/ci-qa-ai-sandbox-coordination.py` on the same dedicated Linux QA
host prepared for [recovery acceptance](ai-sandbox-recovery-qa.md). Authentication
is completed by the operator. This harness neither logs in nor reads provider
tokens. It keeps the existing connection, VM and network for the validation batch.

## Prerequisites

Prepare the checked-in `scripts/fixtures/sandbox-spring-boot` source, Java 17 or
newer, Maven, and its complete offline dependency cache. Install the tools under
`/opt/firecrab/qa/{java,maven,m2}` in the selected isolated Ubuntu MicroVM. The
task UID needs read/execute access to these tools. Keep them outside task exports.
Do not give the guest provider credentials or internet access.

The fixture uses Spring Boot 3.5.0 and deliberately omits the String bean required
by its controller. Prepare its dependency cache by reproducing that failure, then
verifying the documented known fix once before restoring the original source.
Record the tool archive hash and versions. This preparation is distinct from the
subsequent model-driven execution.

Run on the owned Linux API host, replacing the identifiers and paths:

```sh
python3 scripts/ci-qa-ai-sandbox-coordination.py \
  --api http://127.0.0.1:5523 \
  --run-id RUN_UUID --connection CONNECTION_UUID --vm VM_UUID \
  --model VERIFIED_MODEL_ID \
  --owner-marker /run/firecrab-sandbox-qa/RUN_UUID \
  --fixture /srv/firecrab-qa/fixtures/sandbox-spring-boot \
  --results-dir /srv/firecrab-qa/coordination-UNIQUE_UUID \
  --live
```

## Required evidence

The harness creates a source-contained skill/reference and local Git commit.
It submits Debug once, then submits Test with the Debug task as a dependency.
Test must first remain Waiting without tool events. Debug must reproduce the
missing-bean failure, write the known fix, and run the actual regression test.
Test then receives the successful Debug attempt and independently runs the same
regression in its own isolated copy.

The native argv channel uses a 120-second wall/CPU ceiling and a 2048 MiB
address-space ceiling for the JVM. The VM's own memory limit still applies.
The Java heaps are separately capped at 128 MiB. Commands create guest-generated
nonces and reject a repeated invocation marker. Provider environment variables
must be absent and the task must have a non-root UID and no-new-privileges.

The report verifies actual stdout/stderr and exit status, test-event provenance,
task/attempt/runtime and dependency references, JUnit XML artifact downloads,
hashes, durable reload, and unchanged original source. A model statement alone
cannot pass. The report and downloaded artifacts are private files with mode 0600.

The project, source and results remain available for UI/API-restart verification.
Delete only these owned resources after export. Disconnect the retained QA
connection at the end of the whole validation batch. On any uncertain admission
or command, inspect the recorded UUID and attempt before reconciliation or retry.
The harness does not resubmit a failed HTTP request.

Run `python3 scripts/test-ci-qa-ai-sandbox-coordination.py` for offline evidence
guards. Those tests do not establish real provider, guest, or two-agent acceptance.
