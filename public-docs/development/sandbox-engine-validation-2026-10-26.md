# Sandbox engine validation — 2026-10-26

This record contains actual MicroVM and model execution. It is separate from
unit fixtures and the operator image-preparation preflight.

## Runtime and boundary

- API source `ec67bf6f`, binary SHA-256
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
preflight root yielded UID 65532, NoNewPrivs 1, CapEff 0 and exit 0. Model
acceptance with the product fix remains pending the next API deployment.

## Preservation and remaining work

After both cleanup paths, borrowed recovery VM
`65124348-da4f-424a-975c-1e32efe23467` still returned running/200. Actual shared
Ubuntu rootfs and kernel SHA-256 remained, respectively:

- `c5700031e35f10bcb68e73c28b1f3740602bffc4f55417a27034681982f5247a`
- `d171d906729b5548b7400f12de09cb3dd08a06a6e2b74075c8b9e9482e720aa1`

The corrected nspawn model cases and Workspace stop/resume, excluded
package/config preservation and final explicit deletion remain pending. A raw
VM stop does not count as a confirmed durable Workspace stop. Workspace final
deletion requires a rejected unauthorized request and explicit authorization
for that exact owned QA Environment. Public HTTPS/Tunnel publication and
Canvas runtime integration are outside this validation.
