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
   remain negative evidence.
3. The running API's tar feature probe places `--help` after another option.
   BusyBox rejects this ordering despite supporting excludes. Its actual
   `tar --exclude=null -cf /dev/null -C /dev null` operation succeeds. The
   current environment reports `unsupported: missing_tar_exclude`. Commit
   `5c84b830` corrects the probe; deployment/reconciliation is still required
   before model acceptance can proceed.

The failed v2 environment is `275bee46-d777-482e-b0a7-b3f22cf6ba11`, VM
`c6d654f2-817b-4ea6-9c1e-74893ff61f08`. Its original OCI manifest is
`sha256:416fd50d39470da9786d784fc3ec80e6ae240742537358ef367eb6b952d8da24`.
It was not repaired to substitute for new-image acceptance.

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
  operator fingerprints. This is not a cross-VM wrong-key authentication test.

The manager retained at least 1.45 GiB available after these checks, above the
768 MiB gate. Evidence lives in the private QA artifact tree
`/var/lib/firecrab/qa-artifacts/414-distroless`, with final records under `v3/`.
Raw output, typed requests, actual console logs, image pins and hashes are
retained. Source files are in a separate API-readable 0755/0644 tree outside
the API service's private `/tmp` namespace.

## Pending V1 API/model acceptance

REST confirms zero tasks in this project. The current API reports the known
tar-probe unsupported state, so no model command was admitted. Operator checks
and Docker fixtures do not substitute for these remaining actual checks:

- Corrected authenticated V1 health with execution enabled.
- Real model `run_argv` events proving exact input and output ownership.
- Enforced memory allocation refusal, CPU termination, output overflow 125 and
  wall timeout 124 through V1.
- Actual process identity inspection, forged-identity signal refusal, task
  cancellation and absence of remaining owned processes.
- Credential/key guards executed by the admitted task UID.

Follow the [prepared image recipe](../../scripts/fixtures/sandbox-distroless/README.md)
and the [guest acceptance recipe](sandbox-environments-qa.md). Reconcile the same
environment IDs after deployment. Do not replay uncertain commands or replace
failed evidence with fixture success.
