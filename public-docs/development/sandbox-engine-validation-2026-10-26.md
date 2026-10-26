# Sandbox engine validation — 2026-10-26

This record contains actual MicroVM and model execution. It is separate from
unit fixtures and the operator image-preparation preflight.

## Runtime and boundary

- Initial Docker/failed-image API source `ec67bf6f`, binary SHA-256
  `ae54f26552c2d79dc9b38aaad852f6a386f8fd9398e545d0b50f45970e62c63a`.
- CLI source `5bdbb4f2`, binary SHA-256
  `13d96e1a79631b1328eaa972a13cb0db888799ca019a9f0443a8a3df6e0ba9ec`.
- Official MCP SDK `1.32.2`, interactive stdio session. The operator/model read
  each tool result before choosing the next call. No automatic lifecycle script
  chose the resources or substituted fixture responses.
- Existing saved connection `b6804610-c932-40b3-9d37-3cebd426b8e8`,
  model `gpt-6.1-sol`. No browser, login, OAuth request, credential read or
  account-database copy occurred.
- Each new VM used Ubuntu 26.04 version `ubuntu-26.04-v5`, one CPU,
  512 MiB RAM, 4 GiB disk, explicit isolated VM egress, an isolated owned
  network, no port forwards and no public listener.
- Engines were installed by the operator from SHA-verified public offline
  packages. The model received no socket, root flag or engine control.

The private trace is `/private/tmp/firecrab-414-modes-docker-live.jsonl`.
It includes both the Docker trial and the first nspawn trial. Task exports,
artifact bytes, operator logs and REST receipts are under
`/private/tmp/firecrab-414-engine-prep`, with evidence files mode 0600.

## Docker: actual task execution

Project `4fe811f6-d077-4ed0-8baf-d28f2bb53e75`, Environment
`4ed5d6d4-3df5-42a1-a57d-87148ee83798`, generation
`60969edd-2df5-44a0-a25d-43f430a30fd9`, VM
`7b5f6d3c-d48e-4ed7-bfe7-9c51ea005c8b`, runtime
`3c06ff30-bdb3-4c67-93d1-4965539b095d`.

Docker 29.1.3 pulled the exact repository digest
`127.0.0.1:24144/sandbox414/busybox@sha256:6f9343062f60921e9e19ccfeef46159b3ba4e5184b4f7df8632449b55ef3d3ce`.
The temporary registry used loopback TLS with a one-day guest-only CA. It was
stopped before model admission. An HTTP-only pull failure remains in the logs.
No daemon flags or VM/network policy were relaxed.

| Case | Actual task / attempt | Evidence |
| --- | --- | --- |
| Isolation and intentional failure | `bc6c13f3-7772-49df-bfbf-4ac6e5d24934` / `f741b7f3-8876-4c76-9469-a961b8a072b9` | Exactly one supplied `run_argv`; UID 1000, NoNewPrivs 1, CapEff 0, no Docker socket, command wrapper not writable, actual stdout/stderr and exit 7. The recorded test is false, as required by the actual exit. |
| Wall timeout | `0d684830-a983-4cae-b55b-e44e4fd3b1c8` / `d75b08ad-7c96-4ce6-a38c-46c97a89ab56` | Sleep 30 with wall limit 2 produced exit 124. Only the started evidence exists; the finish file does not. No exact elapsed duration is claimed. |
| Cancellation | `1ea3cb51-0224-43c9-8ccf-9b86f7af7a0e` / `1d3b7d7c-0f6a-4e61-b942-e5e920b4c0ca` | The guest process and Docker container were observed running. Cancel first acknowledged in progress, then reached cancelled with exit 143. Only the started file was exported. |

For all three tasks, the recorded `run_argv` input equals the JSON supplied in
the task request. The Docker container was inspected with the exact attempt
label, UID 1000:1000, dropped capabilities, no-new-privileges, 64 MiB command
memory and a 128-process limit. After cancellation, individual queries for all
three attempt labels returned no containers. A loopback connection check also
confirmed that the temporary registry listener was gone.

Actual MCP artifact reads verified these bytes:

| Artifact | Bytes | SHA-256 |
| --- | ---: | --- |
| `581459bf-cde8-4afe-8033-ab795aeeaa90`, `docker-evidence.txt` | 222 | `774504ac951c0f36df981993a87d786c58e53f6ee5465c98fb27bfde19576fb1` |
| `724be644-960a-46ca-b767-0b86e43d9fee`, `docker-timeout.txt` | 83 | `06b4ea0cb8d8de4d9a0d0f08b0642e886512f69be0b759f47aa513774e5fc439` |

## Required export and owned cleanup

Only the first task's 222-byte public artifact was fault-injected. Its
project/attempt/artifact UUID path, regular-file status, size and SHA were checked
before temporarily renaming it to a private sibling. No database was read or
changed. Both Environment export and DELETE failed with HTTP 400. Environment
state stayed ready with exportedAt null; VM, network and security-group refs
remained. REST confirmed the VM still running and both other resources present.

The original artifact was restored without modifying its bytes. MCP read
verified its SHA again. Environment export then returned all three task IDs.
Environment DELETE completed, followed by project DELETE with confirmedAbsent.
REST returned 404 for VM `7b5f6d3c-d48e-4ed7-bfe7-9c51ea005c8b`, network
`7c2c25aa-47a6-4fcc-82aa-25213e44bc00` and security group
`440cf430-66a8-49c1-8eb6-fdddcbdc02f2`.

## nspawn: failed image, real TTL cleanup

Project `9d62d3ec-a7bc-404e-85c1-6a7626b0bd31`, Environment
`42426d99-2fd6-40cd-b241-aa747c01360a`, generation
`1627d548-3933-41f5-a8b7-7d9977e6094a`, VM
`21c739cc-4d2d-4fa8-aa8a-8e95af31abfb`, runtime
`fc5ca4d9-db5c-4917-8784-513d8c833308`.

Actual task `c4c3c9b3-64da-456c-aa2a-06f2d09922c1`, attempt
`cac6a49e-f909-4022-ad07-53772576aa89`, failed before the supplied command ran.
The BusyBox-only image pin `sha256:01a1ffd69933520827f23b86ca833c9c5b64737d9f90b089242ab91cc166d990`
lacks getent. nspawn emitted `Failed to resolve user …`, exit 1. The test is
false and no requested artifact exists. This is not successful nspawn model
execution, even though the engine/archive readiness check reported ready.

That failed task's durable result was exported before expiry. The Environment
had TTL 900 seconds and expired at `2026-10-26T12:03:33.605Z`. Maintenance set it
to deleted at `12:03:42.779Z`; no direct Environment DELETE was sent. Actual REST
then returned 404 for its VM, network `772fd587-be15-4c4a-b9c8-f4c09d642521` and
security group `726c4e5b-324f-4bdd-bf68-d6302bd94e13`. Project DELETE subsequently
confirmed absence.

A new immutable rootfs with Ubuntu's verified getent/libc/loader reproduces
`sha256:0044c4c1159dd6882276707f9f40a87e388addecc80cfd004ea312940e817fee`.
An operator preflight also found that the container root must be mode 0755;
0700 prevents non-root exec after lookup. Correcting only that separate
preflight root yielded UID 65532, NoNewPrivs 1, CapEff 0 and exit 0.

## nspawn: corrected image and stream contract

The corrected-image trial used Environment
`c79c7ebd-755c-468b-977e-ecddc981e1ec`, generation
`348659da-ef2d-4a93-b96d-9f8970dc7c6d`, project
`43f3c1d1-f8f5-42dd-ad42-6fcc4cb4d8ac` and VM
`4e2d4dd7-477e-4c63-a0dd-a23723e5f0a3` on isolated `172.30.223.0/24`.
The image remained pinned to `0044c4c1…` throughout this trial.

API `bfa6e847`, SHA-256
`8e10745328d995ee798fbb40689831ae2601d474a02ab4f66ffe2dbe516a4b1d`,
ran these actual model tasks on runtime `be82d41b-0de1-4359-a0f2-f5224740cc0f`:

| Case | Task / attempt | Actual result |
| --- | --- | --- |
| Non-root command | `65d49ed3-8e70-4a69-8199-41a3fbfbd57e` / `feaf1dfd-8385-4065-8a4d-337ab750fd9c` | UID 1000, NoNewPrivs 1, CapEff 0, no socket, wrapper not writable, intentional exit 7. Default nspawn PTY merged stderr into CRLF stdout, so the stream contract failed. The original output remains preserved. |
| Timeout | `ba732adb-b200-4407-ae30-d5b64d68a635` / `f6eb4538-39eb-41eb-a075-ea1ac1bac443` | Sleep 30 with wall limit 2 ended at exit 124. Only the 64-byte started artifact exists, SHA-256 `8842c3be8695251943d1056658eb73aea0247c2550e985b8ecdd618d34436261`. |
| Cancel | `f129df27-cd36-4200-bc91-69a13f9d2963` / `410294b5-875f-4d3f-9c49-ceba743d20fa` | PIDs 9664/9677 with start ticks 41864/41912 were observed running. Cancellation reached cancelled, exit 1, with nspawn's KILL diagnostic. The started file was exported; no finished file exists. Both exact process identities were absent afterward on the same guest boot. |

All results were exported before a normal Environment stop. The next API source
`2ff12235`, SHA-256
`fd449a22cd8337409d7cddc77a9de208803a1327d1e2f80780c76eecb6e26a44`,
adds `--console=pipe`. Resuming the same VM produced runtime
`d264de12-1edb-4dff-80f3-cc0a5332be51`. The previous three complete task JSON
records, including immutable runtime references, remained equal as parsed JSON
after both API and guest restart.

New model task `6becb49a-88a0-46b3-9a36-5ae86fae9ff6`, attempt
`503d7fa1-3346-4e54-b0fe-bf72c3a50882`, executed the exact supplied invocation
once. It produced UID 1000, NoNewPrivs 1, CapEff 0 and intentional exit 7.
Stdout was exactly 222 bytes with no CRLF; stderr was exactly 56 bytes:
`NSPAWN414:stderr:2917317b1aad2a4bfd701e4687f4126e:exit7` followed by LF.
The stderr marker was absent from stdout. The model preserved the two streams
separately and recorded the command's failed test accurately.

MCP downloaded artifact `0a6bbde8-4ee8-4f18-9d36-fdfe13d21a8f`, 222 bytes,
SHA-256 `be1a241a93f82bcf0744ed062497c47d8d812cd67f3ebc235705dc30b7681aa3`.
All four tasks were exported before Environment and project deletion. Actual
REST returned 404 for this VM, network `41dc1fbf-83fa-4099-a2e4-61a42dd0aefc`
and security group `08af5b7e-a7b5-4d4b-852e-eec770029eaa`. The borrowed recovery
VM still returned running/200.

## Workspace: normal stop/resume persistence

The Workspace trial used the same `2ff12235` API and existing saved model
connection. Project `ffb94657-6936-4278-9152-a35972c58dbe` owned Environment
`d3e8f5a0-f6c8-45fa-b302-31879f5f500d`, generation
`a09f33d9-8953-4cc3-9db9-ef70da45ee4a`, VM
`aef30a99-28cb-4ab5-85d2-ea6241431b45`, network
`2dd4dcea-cd7e-4eda-a526-aaeb942750be` on `172.30.224.0/24`, and security group
`8084752a-bf1b-4000-bf5e-ea9e4437c38b`. The role was explicitly
`persistent_workspace`, with no TTL.

The operator installed only verified offline Python/BusyBox packages in this
guest. Model task `2ecaa19e-3e49-46ad-b454-18070d918723`, attempt
`48e7e8b9-1689-4b83-86c6-370cb3e36ca1`, ran one exact supplied command and
returned exit 0 on runtime `41e1b0ef-a309-49c4-b9e7-f0ce273cc70c`.
It created a tiny Python user distribution and metadata under `.local`, a
public nonce/config under `.config`, and nonce markers under `.cache` and
`.venv`. Python imported the distribution and read its installed version.
The command observed UID 1000, NoNewPrivs 1 and CapEff 0.

After that command, the model used `write_file` for `workspace-file-only.txt`.
The edit contains nonce `a35243a0c5c2684c6e0c0f2d2fadcae6`. No command followed
this file-only edit. The task's exported changes contained only the normal
evidence JSON and file-only edit, not the excluded package/config/cache trees.
Actual MCP artifact `5064d78a-a65e-48f8-ae18-2c85d5d871fc` is 719 bytes,
SHA-256 `96928b18e6e95805bd52e857f44e3aff85107cf1b317292d4c71fde455615088`.

Environment export succeeded, then the normal Environment Stop endpoint
returned stopped with no error. REST also confirmed the VM stopped. No raw VM
stop or power cut substituted for this check. Resume kept the same VM/disks
and Environment generation, but admitted a new runtime
`7533be2b-773e-4930-8175-71c3ee501954`. Actual guest boot ID changed from
`4ad72eb2-721b-458e-9864-92bd0acc0434` to
`b1f7b5da-bdae-4523-808d-f790cf41d6a8`.

The first read-only probe, task `4ab80ea2-70f6-4ee8-bc9c-36731824b43e`,
attempt `1a53d274-bd5b-41df-afb5-423d4e618862`, failed at its OS-package text
comparison. The operator had double-escaped the expected separator. Package,
config, cache, venv, nonce, boot and file-only assertions before that line had
passed. Preserve this exit 1, its failed test and the diagnostic; it is a QA
expectation error, not a successful command or a product persistence failure.

A new task `33897dea-50b1-46d7-b6b7-8758cd11f586`, attempt
`2dabc4fd-5ef0-455b-9c9d-3072125269e8`, reads the original 719-byte evidence
file only after checking its original SHA. It uses those exact bytes as its
expectation, avoiding a manually re-escaped string. It recreated or repaired
none of the persisted data. The actual exact invocation returned exit 0 and
verified all of the following:

- Automatic Python user-package import and distribution version `1.0.0`.
- Python `3.14.4`, `python3=3.14.3-0ubuntu2` and
  `busybox-static=1:1.37.0-7ubuntu1`.
- Package SHA `5cd15fed802de8c876a558eafa4826d20151a402bad8b7ebd62c805f60071699`.
- Config SHA `366c5005f203427706e50d79863c1286fcbfc3d9da1c8ff6bcafe5732aac4d7a`.
- Cache and venv marker SHA
  `a1db4f3bff5ca1d04a9316d1957e6f2232f03a0b2a340f19c59ad9c860619469`.
- The exact final file-only edit and unchanged public source probe.
- Changed guest boot, UID 1000, NoNewPrivs 1 and CapEff 0.

Its passed test is separate from the earlier preserved failed test. MCP
downloaded artifact `d69ec45a-cee1-410d-9160-f20d6ba68dca`, 879 bytes,
SHA-256 `16ea9170dc47505b17c66af71928c0880552920322a5e2cb1511eaea599d3744`.
All three Workspace tasks were exported. Eleven total mode task JSON exports
have private byte-size/SHA receipts in `all-task-json-export.json`; each file
was rehashed before cleanup permission was considered.

### Retained-data refusal and authorized final deletion

The other agent's authorized cross-VM SSH checkpoint completed before any
Workspace lifecycle change. The owner then rechecked the exact project,
Environment generation, VM, network and security-group IDs listed above.
DELETE with `authorizeWorkspaceDeletion:false` returned HTTP 400,
`validation_failed`, with `Explicit authorization is required to delete
retained Workspace data`. It did not stop the VM. Actual Environment/VM/
network/security-group GETs all returned 200; the VM remained running.

An operator read of only the public QA package/config/cache/venv fixtures
confirmed that their four hashes and the file-only edit remained unchanged
after the refusal. No guest data was reconstructed. Root explicitly authorized
final deletion of this exact owned QA Workspace, conditional on the verified
read-only result, complete exports, refusal/data preservation, settled tasks,
completed SSH checkpoint and exact-ID recheck. All conditions were met.

DELETE with `authorizeWorkspaceDeletion:true` then returned deleted. Project
DELETE confirmed absence. Final REST returned 404 for this project, VM,
network and security group. The owned VM artifact directory was absent.

## Final preservation and scope

After the initial cleanup paths, borrowed recovery VM
`65124348-da4f-424a-975c-1e32efe23467` still returned running/200. Actual shared
Ubuntu rootfs and kernel SHA-256 remained, respectively:

- `c5700031e35f10bcb68e73c28b1f3740602bffc4f55417a27034681982f5247a`
- `d171d906729b5548b7400f12de09cb3dd08a06a6e2b74075c8b9e9482e720aa1`

Root separately stopped the borrowed recovery VM after its own sync/settled
checks. After the final Workspace cleanup, that VM still returned 200/stopped
with its original ID and 2 GiB disk declaration. Its actual VM directory and
`d/0360e0b3d68f4bd498b2a7d5de7ca8a1.ext4` disk remained, with size
2,147,483,648 bytes. No borrowed disk content or key bytes were read. Both
actual shared Ubuntu image hashes above were verified again unchanged.

No owned mode VM, network, security group or project remains. Original source
fixtures, private exports/traces and verified offline preparation materials are
preserved for review. Public HTTPS/Tunnel publication and
Canvas runtime integration are outside this validation.

## Separate OCI startup process regression

The production OCI worker's Python fixture was also run in `python:3.12-slim`,
with network none, CPU 2 and RAM 2 GiB. The original fixture failed because its
BusyBox stand-in used external `exec kill`, but slim supplies only the shell's
kill builtin. That made an alive prestarted SSH service appear dead and start
twice. The actual BusyBox applet did not have this fixture defect.

The corrected fixture preserves builtin kill, holds its simulated SSH service
until explicit release, and waits for the complete background package worker
before checking the start count. Both missing-SSH and preinstalled-SSH paths
passed. A temporary copy with the production liveness guard forced false was
rejected for a duplicate start. Disposable containers were removed and no
Cargo cache was created. This is a Linux process fixture and does not claim
an additional OCI MicroVM validation.
