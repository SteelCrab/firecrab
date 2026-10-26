# Prepared distroless validation record

This record covers the 2026-10-26 disposable local QA run. It separates image
preparation, Docker smoke, actual MicroVM operator execution and V1 API/model
acceptance. No account credentials or private keys are included.

## Immutable inputs

| Input | Identity |
| --- | --- |
| Public source, Linux arm64 | `gcr.io/distroless/static-debian12@sha256:b6d8cd9eeccef7a63b91771ba943056f484c0dc92396ec3140a5fe2c6279d581` |
| Preparation tools | `alpine@sha256:28bd5fe8b56d1bd048e5babf5b10710ebe0bae67db86916198a6eec434943f8b` |
| Static toolbox | `busybox@sha256:8d7b1636e974e0adfd8d945955fca609304f0a56c18799dfd032d6e661382d84` |
| Static QA app SHA256 | `bdf3e73c0142919e6777340d267424b9e4bf1f22ab0db8af59a5a8b2cd9c740f` |
| Static guest helper SHA256 | `657c12253f8b871380a01e199c5edff9f1328f97ef2b1c357cb49677d06d9189` |
| Final v3 OCI manifest | `sha256:0f27461b476a219f4f4f7b1a5a92e04f5877d068ef784372fb9b33e9bc9cad3e` |
| Final Docker-save archive SHA256 | `fd988f2529ef61f6c5b795d20a6084bcfdadea38189dd69d1a8b738d1e572ad8` |
| Archive bytes | `6274048` |

The helper is the statically linked aarch64 glibc QA build. This is not a musl
release-build claim. The final image has the QA app, the isolated SSH runtime,
BusyBox tools and that helper. Preparation explicitly supplies root's `/bin/sh`
login shell. Other passwd rows, root's password/UID/GID/home and application
metadata are preserved. Normal specialization supplies unique per-VM keys and
the BusyBox VM init. No keys or provider credentials are baked into the image.

## Preserved failures

1. The original preparation installed `pax-utils`, but current Alpine did not
   supply `lddtree`. Its pipeline hid the failure, so build returned zero while
   executing the resulting SSH wrapper returned 127 for a missing loader.
   Commit `e91deb61` installs/checks `lddtree`, propagates list failures and runs
   the isolated loader before completing build.
2. The v2 image passed a container SSH fixture that manually set root's shell.
   The actual VM refused authentication: `User root not allowed because shell
   /sbin/nologin does not exist`. The original distroless passwd entry was still
   present because OCI specialization selected a new shell only for bash.
   Commit `cf1fb2ad` prepares root's shell and removes the fixture's manual
   repair. The unrepaired v2 now fails that fixture with exit 79; v3 logs in
   without repair. The original v2 image, environment and actual console log
   remain negative evidence. Its zero-task environment was later exported;
   deletion without explicit Workspace authorization refused. Explicit owned
   cleanup removed that failed VM and preserved its image and raw evidence.
3. The running API's tar feature probe places `--help` after another option.
   BusyBox rejects this ordering despite supporting excludes. Its actual
   `tar --exclude=null -cf /dev/null -C /dev null` operation succeeds. The
   environment initially reported `unsupported: missing_tar_exclude`. Commit
   `5c84b830` corrected the probe. Deployment and same-ID reconciliation then
   produced actual V1 health with execution enabled.
4. The first real model task reached staging, where BusyBox created a user but
   did not create its named group without an external `addgroup` path. No command
   invocation was created. Commit `ace2a176` explicitly creates a trusted group,
   avoids GIDs already used by any account, and checkpoints the positive UID
   before group validation. Linux process tests cover partial creation, group-only
   failure retention, preexisting-user refusal and unchanged unrelated accounts.
   The old attempt had no UID checkpoint. An explicitly approved operator repair
   verified its exact owner, unique UID 1000 and zero processes, then used a new
   root-0600/O_EXCL temporary file, atomic rename and sync to record that UID for
   cleanup only. One explicit reconciliation cancelled the old attempt. This
   manual cleanup is not automatic staging acceptance.

The failed v2 environment is `275bee46-d777-482e-b0a7-b3f22cf6ba11`, VM
`c6d654f2-817b-4ea6-9c1e-74893ff61f08`. Its original OCI manifest is
`sha256:416fd50d39470da9786d784fc3ec80e6ae240742537358ef367eb6b952d8da24`.
It was not repaired to substitute for new-image acceptance. Its retained
environment record is now a deleted tombstone.

## Actual v3 MicroVM operator proof

| Reference | Value |
| --- | --- |
| Project | `1c0e36c2-a708-4341-a11e-1c99b99b7a76` |
| Environment / generation | `519df326-d9b6-48aa-90f9-de9d8c27f926` / `5551f54f-85ba-4d68-b2ef-6b71d726f05f` |
| Role / mode | `distroless` / `persistent_workspace` |
| VM / runtime | `8ff2bb66-0b85-420f-9f36-fe63902a4918` / `5b0ef680-dbb8-45af-9a32-9d47f4556ed2` |
| Resources | 1 vCPU, 256 MiB, 1 GiB disk |
| Network | Dedicated IPv4 `172.30.222.0/24`, no Internet/IPv6 allows |
| Installed kernel SHA256 | `d171d906729b5548b7400f12de09cb3dd08a06a6e2b74075c8b9e9482e720aa1` |
| Installed rootfs SHA256 | `c5c76ffbbb921c30fca82c5599dd05ecac601c84897d1b54c0794d4561558b4e` |
| Boot arguments SHA256 | `1c61410753d83b9f71b514dd173f9096a34033391356099fce8f51d071a273b2` |

Each digest was inspected and imported once. The v3 VM authenticated through
its own operator key and pinned host key without post-boot passwd repair.
Private key files were used only as `ssh -i` inputs. Actual observations:

- PID 1 is `init`; `/proc/1/exe` resolves to `/etc/firecrab/busybox`.
- `/etc/os-release` reports Distroless / Debian 12. Package managers, OpenRC
  and systemd executables are absent. The static application remains running.
- The helper is root-owned mode 0755. No util-linux UID/timed-lock utility is
  available. The static fallback executes with UID/GID 64555, zero supplementary
  groups and no-new-privileges equal to one.
- The app receives exact literal argv, explicit environment, owned cwd and stdin
  including a NUL. Stdout/stderr remain separate and the confirmed exit is 7.
- Reported soft/hard limits agree: nofile 1024, address space 64 MiB and CPU 3 s.
- A competing inherited-descriptor lock returns 81 in the same second. Releasing
  the supervisor descriptor permits reacquisition.
- The operator private-key path from the controller is absent in the guest.
  The QA UID cannot read the guest host private key. Explicit environment checks
  disclose no credential values. Two owned QA VMs have distinct public host and
  operator fingerprints. Actual cross-VM authentication is recorded below.

The manager retained at least 1.45 GiB available after these checks, above the
768 MiB gate. Evidence lives in the private QA artifact tree
`/var/lib/firecrab/qa-artifacts/414-distroless`, with final records under `v3/`.
Raw output, typed requests, actual console logs, image pins and hashes are
retained. Source files are in a separate API-readable 0755/0644 tree outside
the API service's private `/tmp` namespace.

## Actual V1/model acceptance

The final API-only deployment was source `2ff12235`, binary SHA256
`fd449a22cd8337409d7cddc77a9de208803a1327d1e2f80780c76eecb6e26a44`.
The helper and all VM/environment/runtime identities stayed unchanged.
Authenticated task-scoped health returned version 1, `init: distroless`,
`execution: true`, Docker/nspawn false and no unsupported reason.

| Actual task | Outcome |
| --- | --- |
| `e4708e4a-02a3-4797-8c61-f24cbfe349c4` | Original staging failure; no command invocation; retained and cancelled after the separate operator cleanup described above. |
| `0396e49c-253f-479f-b1ee-b510ec20b5aa` | Independent fixed-engine run, real `gpt-6.1-sol`, Result; six exact `run_argv` inputs executed once each; one command-backed successful test and one artifact. |
| `e22f98a8-25a6-4d46-ad0a-200f748c6928` | Cancellation observer missed its window because the short stdout marker was buffered. Zero cancel requests; the 120 s command ended in Result. This remains observer-failure evidence and is not cancellation proof. |
| `f0b8d6c0-3859-4f9f-89a8-63011ea5780d` | A separately authorized cancellation probe with immediate API process/cmdline observation; Cancelled after one explicit request. |

Task `0396…`, attempt `78171863-b20b-4ad0-9be6-4573ec7176df`, received the actual
raw exits `7, 42, 137, 125, 137, 0`. Its first event proves UID/GID 1000, zero
supplementary groups, no-new-privileges, exact literal argv/environment/cwd/NUL
stdin, separated streams and hard/soft nofile 1024, address-space 64 MiB and
CPU 3 s. The 32 MiB memory probe refused its 64 MiB allocation with ENOMEM and
exit 42. The CPU-1-second probe ended with 137. Output overflow returned 125 with
at most 16 retained bytes per stream. The final admitted UID command could not
read guest SSH private keys or the controller key path and received no provider
or SSH-agent environment. Its actual exit-zero event 7 backs the recorded test.

The wall-2-second static payload waits indefinitely. BusyBox's `timeout -s KILL`
returned raw 137, which the model reported unchanged. The measured interval from
that tool event's start to the next event's start was 6.85 s, an upper bound
including staging, transport and any model turn. The next command succeeded and
an operator check confirmed zero processes for the owned UID on the same boot.
The original recipe's expected 124 was not observed; it is not substituted for
137. Numeric exits alone do not distinguish CPU, deadline or cancellation.

The true cancellation attempt is `f07e97a1-a122-4f54-8a89-d1eb90177ffe`. Its live
payload had PID 460, start tick 427636 and boot
`91bc74a1-154d-4527-b6f7-9192b8b6cf33`. Changing either start tick or boot produced
HTTP 409. The exact process remained alive after both refusals. One cancel
request completed in 1.03 s; the actual recovered exit was 143. A pinned operator
check found zero owned-UID processes without a reboot. The cancellation artifact
was retained before the blocking command. It snapshots bytes produced by the
earlier observer task; it is not presented as newly produced cancellation output.

## Actual key isolation and durable cleanup

The other checkpointed QA VM was `aef30a99-28cb-4ab5-85d2-ea6241431b45`, runtime
`7533be2b-773e-4930-8175-71c3ee501954`, on its separate environment. Both correct
key/host-pin positive SSH checks returned zero. Using A's controller key against
B and B's key against A each returned 255 with explicit public-key permission
denial, not a timeout. Each negative was attempted once. Those clients disabled
agent forwarding and interactive/password authentication. Private keys were
referenced only through `ssh -i`; the agent did not inspect or copy key contents,
or send them to a guest or model.
The other VM's generation/runtime stayed unchanged and its owner held lifecycle
changes until all four checks finished.

All four task results were exported before v3 cleanup. DELETE without explicit
Workspace authorization refused. Explicit owned cleanup then removed only the
v3 VM, security group and network; all three exact REST resources returned 404.
The original installed image pin and project remained. Task events, tests and
artifact metadata were compared before/after deletion and remained identical.
All three artifact downloads still succeeded after VM deletion:

| Task / artifact | Bytes / SHA256 |
| --- | --- |
| `0396…` / `d381c33c-26c6-4999-933f-6b233009c7b6` | 45 / `105272534419a91f69494a9ec2ed1db3f373282d99ca09503cd284f3b7322b18` |
| `e22f…` / `fbd0b3e4-0a87-4b89-9914-06dd1825fa62` | 49 / `bfd25c948a17c478a0957835f57f9e02f921f0a999aaa8be74333ee232872405` |
| `f0b8…` / `256ccda1-ec4a-42e6-8691-3d35e13cfc1a` | 49 / `bfd25c948a17c478a0957835f57f9e02f921f0a999aaa8be74333ee232872405` |

Both owned loopback registry PIDs were stopped only after matching their recorded
start identities. No shared image, other VM, account, provider connection or
registry was deleted. The manager had 3.31 GiB available after cleanup. Original
failed image/task/observer evidence remains beside the successful records.

The [prepared image recipe](../../scripts/fixtures/sandbox-distroless/README.md)
and [guest acceptance recipe](sandbox-environments-qa.md) describe reproduction.
These results validate this prepared distroless V1 path. They do not claim
unprepared-image support, Docker/nspawn execution, public tunnels or interactive
PTY sessions.
