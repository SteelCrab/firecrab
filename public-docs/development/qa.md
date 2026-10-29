# QA

Use the [English](testing.md) or [Korean](testing.ko.md) checklist as the case list.
This page maps those cases to scripts and records the reporting rules.

## Prepare and report

1. Use a dedicated test subnet and `qa-` resource names.
2. Require `/api/host` and the dashboard to answer before resource checks.
3. Create each resource, exercise it, delete it, and verify cleanup.
4. Record `PASS`, `FAILED`, or `WARNING` for each applicable case.

Use `WARNING` for skipped checks or leftover resources.
For H1, read `GET /api/update` and `GET /api/update/progress`; neither starts an update.
Apply updates only during U1; delete only images installed by the run.
Restore any previous Docker Hub login and manual DNS/management SSH changes.

Record source revision and dirty state, OS/architecture, virtualization layers, command/options, exit code, and artifact paths.
Retain guest console output before deleting failed guests.
A build/unit pass does not establish guest boot, SSH, or forwarding success.

## Script coverage

Run commands from the repository root.
The Firecrab API host is native Linux or managed Debian on macOS/Windows.

| Script or suite | Cases / purpose |
| --- | --- |
| `scripts/ci-qa-api.sh` | G4–G5, H1–H3, N1–N5, S1–S3, L1–L3, I1/I8/I9, P1–P6, V14, C3/C5, X1–X4/X6 |
| `scripts/ci-qa-nginx.sh` | NGX1–NGX9, including real SSH, env, shell, and forwarded HTTP |
| `scripts/ci-qa-ssh.sh` | V8a–V8d; called by nginx and guest boot |
| `scripts/ci-qa-guest.sh` | I5/I6, V1/V2/V6–V9/V11–V13/V15, N6, C1/C2/C2b/C4, X5 |
| `scripts/ci-qa-lifetime.sh` | R1–R7, including R4b–R4e, plus X7 |
| `scripts/ci-qa-macos-e2e.sh` | Native Mac gate, shell, repair, browser, API, nginx, guest, and lifetime |
| `scripts/ci-qa-windows-e2e.ps1` | Native Windows gate/shell, WSL API/nginx/guest/lifetime/browser, plus Windows HTTP |
| `scripts/ci-qa-macos-e2e.sh repair` | G7/G7a runtime recovery and preservation; [detailed checks](macos-repair.md) |
| `firecrab-e2e` | B1–B19; full or explicit import/form mode |
| `firecrab-e2e` `test:dashboard` | Mocked dashboard/API/console checks |
| `firecrab-e2e` `test:pipe` | Pipe and registry browser checks; English/Korean host-bundle recovery guidance |
| `scripts/test-pipe-cli.py` | Real systemd CLI routing, default private-engine discovery, explicit overrides, missing-engine status without traffic loss, rollback, and cleanup |
| `scripts/test-pipe-api.py` | Unprivileged API through the systemd helper; route preservation and ingress lifecycle |
| `scripts/test-pipe-runtime.py` | Real ingress routing, health, reload, independent Quick Tunnel targets, and rejected-config rollback |
| `scripts/check-public-tunnel.py` | Read-only public DNS, TLS, Console and registration preflight; no app acceptance |
| `scripts/check-public-tunnel-app.py` | HTTPS fixture response and WebSocket echo through an owned public test address; does not manage connections |
| `scripts/ci-qa-deploy.py --public-tunnel` | Operator-approved public Tunnel verification with two disposable addresses, real MicroVMs and Nginx; dashboard cleanup remains manual |
| `scripts/test-ingress-install.sh` | Isolated initial install/reinstall; executable mode, all attributions, and saved routes, with fake service controls |
| `scripts/ci-qa-pipe-databases.sh --browser` | PDB1–PDB12; real API, PostgreSQL/MySQL, TLS and desktop/mobile browser checks |
| `scripts/test-pipe-databases-qa.py` | Database QA remote-host guard and cleanup after failures |
| `firecrab-e2e` `test:secrets` | SEC1–SEC8; isolated Secrets UI checks with a mocked API |
| `firecrab-e2e` `test:secrets:live` | SEC9–SEC14; Chromium, actual API processes, encrypted vaults, and local providers |
| `scripts/ci-qa-secrets.py` | Secrets unit tests, lint/types/builds, SEC1–SEC14; JSON/Markdown report, SEC16 manual warning |
| `scripts/test-ci-qa-secrets.py` | Failure aggregation, missing-case reporting, manual warnings, and stale-build prevention |
| `scripts/ci-qa-deploy.py --queue-controls` | Real Nginx/MicroVM queued cancellation, rollback priority, retained traffic, API restart, and cleanup |

See [PIPE database E2E and QA](pipe-databases-qa.md) for the case matrix,
fixture-only browser commands, live database prerequisites and cloud acceptance limits.

Guest references are `alpine:3.21`, `ubuntu:24.04`, and `fedora:42`.
Expanded shared checks run on the first reference.
Preinstalled aliases are preserved; unperformed import rows become `WARNING`.
Later references still run after a failure; any failure produces a nonzero aggregate exit.
nginx is a separate pass using `nginx:1.27-alpine`.

R4f helper migration requires `FIRECRAB_QA_HELPER_UPGRADE=1` on a disposable Linux CI host.
Manual cases outside Ubuntu CI include I2/I3/I4/I7/I10/U1.

## Deploy queue QA

```sh
sudo python3 scripts/ci-qa-deploy.py --disposable-host --queue-controls --report target/qa/deploy-queue.json
```

Use an empty disposable Linux/KVM host with at least four CPUs.
This pass holds an active Deploy in drain, cancels queued initial/update attempts,
and restarts the API with a rollback behind an ordinary Deploy.
The active drain must finish first. The rollback must then drain before the
ordinary Deploy starts. HTTP requests keep the five-second deadline and must
finish without errors. The JSON report records request counts, operation IDs,
the four-VM/vCPU limit, and cleanup results. Local OCI fixtures do not validate
an external registry or public DNS/TLS.

## Public Tunnel QA

Public Tunnel acceptance also requires an explicitly approved host and two new,
owned `qa-` dashboard addresses. Do not reuse an existing address or token. The
host must use the default Firecrab paths, and its API, Pipe configuration, VMs,
services, pools, and Auth must be empty. If two addresses cannot be issued, stop
without changing other resources. Keep each token in a separate root-owned 0600
file with no symbolic links; never put token values in commands or reports.

Build the fixture for the host's Linux architecture. For an x86_64 host with
the Rust musl target installed:

```sh
rustc --edition=2021 --target x86_64-unknown-linux-musl -O -C target-feature=+crt-static scripts/public_tunnel_fixture.rs -o /tmp/public-tunnel-fixture
sudo python3 scripts/ci-qa-deploy.py --disposable-host --public-tunnel --public-target-approved \
  --public-fixture-binary /tmp/public-tunnel-fixture \
  --public-token qa-example-a /root/qa-token-a --public-token qa-example-b /root/qa-token-b \
  --report target/qa/public-tunnel.json
```

Replace both example labels and file paths with the approved test resources.
The runner first performs the existing Deploy acceptance. It then creates two
fixture services and checks registration, HTTPS, WebSocket echo, VM and Nginx
targets, retargeting, API restart, and address isolation. At each prompt, revoke
the named test token through an authorized dashboard operator method and press
Enter. The current dashboard UI may not expose a revoke button. Stop if the
operator cannot revoke the token; rotating or deleting it is not proof of token
revocation. A 401/410 registration response alone cannot distinguish revocation
from expiry. Confirm the dashboard's revoked state separately.

The report records measured stages and local cleanup errors, but its final
public status remains `pending_operator_verification`. Confirm both token
revocations and remove only the test-owned dashboard reservations. Verify that
the public addresses no longer serve traffic. Remove the token files and the
disposable host, then record those results separately. Never mark acceptance
complete from a local test, a failed probe, or an operator prompt alone.

## PipeSecrets QA

Install the frontend/E2E dependencies and Chromium using the [browser setup](../../firecrab-e2e/README.md#setup).
Run `python3 scripts/ci-qa-secrets.py` on Linux or managed Debian.
This pass does not require a VM, KVM, a network helper, or a running installed API.
It uses temporary stores and local providers. It never targets the installed host database.
See the [Secrets suite](../../firecrab-e2e/README.md#pipesecrets) for individual commands and artifact paths.
See [provider account QA](pipe-secrets-provider-qa.md) for recorded AWS and Firecrab results.

Record SEC1–SEC14 from the generated report. The report retains each command and exit code.
Missing or skipped automated cases are `WARNING` and produce a nonzero aggregate exit.
SEC16 remains `WARNING` until the following AWS account checks are performed separately.

1. Use a dedicated AWS QA region and name prefix such as `qa-<run>/`.
2. Use short-lived credentials scoped to those resources. Enter them in the web console.
3. Create synthetic source values. Preview and import them. Reveal a copy and check that its source is correct.
4. Change a local copy. Preview the upload, select the replacement, and verify the external value.
5. Change the remote value after preview. Confirm that upload refuses that stale preview.
6. Check a denied read/write or expired credential. Confirm a visible error without raw provider data.
7. Remove the connection. Confirm that imported local copies remain. Delete only this run's QA values in both stores.
8. Record provider/region, scope, timestamp, case result, and cleanup status. Keep secret values and credentials out of reports and screenshots.

Use `WARNING` if an account, permission, or cleanup step is unavailable.
AWS SigV4 tests do not substitute for SEC16.
These checks validate web-console secret management; VM environment injection is outside this pass.

## Platform limits

Hosted macOS and Windows jobs provide build/unit checks, not nested-VM runtime evidence.
Runtime tests require `service doctor` to report `ready: true` on a capable host.
ARM64 KVM has no registered runtime CI job.
See [CI](ci.md) for phases, deadlines, and retained artifacts.

macOS nginx checks HTTP inside Debian and through the Mac relay, with SSH proxied through Debian.
Windows nginx additionally checks native Windows localhost HTTP.
Windows browser IPv4/IPv6 SSH runs inside WSL and does not establish native Windows IPv6 access.
`FIRECRAB_QA_WAIT_FACTOR` extends nginx/SSH waits, not package-install or Playwright deadlines.

## Failure evidence

Keep package/network errors separate from guest network and SSH readiness.
SSH QA stops early only when both package failure and missing-sshd terminal markers are present.
A running package worker stays subject to its wait budget.
`ci-m2-guest-boot.sh` prints the last 8,000 console characters before failure cleanup.

Past source runs and their remaining limits are in [Validation history](validation-history.md).

## Related

- [Complete checklist](testing.md)
- [Browser E2E](../../firecrab-e2e/README.md)
- [Troubleshooting](../guides/troubleshooting.md)

## MicroVM pause/resume memory continuity

`scripts/ci-qa-vm-pause.py` prepares a temporary API, helper, image copy, and
private network/mount namespaces on a Linux/KVM test host. It requires root,
systemd, iproute2, unshare, dnsmasq, nftables, an SSH client, and a Firecracker
binary on PATH. Choose an installed image that provides a serial root shell and
sshd. The guest readiness DNS probe uses a local fixture inside the private
helper mount namespace, so the test needs no external network access.

```sh
sudo python3 scripts/ci-qa-vm-pause.py \
  --api-binary target/debug/firecrab-api \
  --helper-binary target/debug/firecrab-helper \
  --image-root /var/lib/firecrab/images --template IMAGE_ALIAS \
  --report /tmp/firecrab-pause-qa.json
```

The test checks three pause/resume cycles, an in-memory random token and counter,
guest process and boot identity, VMM PID, IP/MAC, HTTP and new authenticated SSH
connections, Sleepy inhibition,
API restart while paused, Stop from Paused, and cleanup. Existing workload units
and installed services are preserved. A successful JSON report is native KVM
evidence; API fixture and browser results alone do not establish memory continuity.

## Pools

Needs a network (N1) and an installed template (I3 or I6).
`minReady` 0 does not boot a guest.

| ID | Work | Add | Delete / cleanup |
| --- | --- | --- | --- |
| P1 | create idle | `POST /api/pools` name `qa-pool`, template, cpu, ram, diskGb, `microNetworkId`, `minReady` 0, `maxSize` 1, `leaseTtlSeconds` 600 → 201; `templateVersion` is pinned | after P5 |
| P2 | list and detail | `GET /api/pools` and `GET /{id}` | with P1 |
| P3 | resize | `PATCH` `minReady`, `maxSize`, `leaseTtlSeconds` | with P1 |
| P4 | acquire empty | `POST /{id}/acquire` → 409 `pool_exhausted` | no lease row |
| P5 | delete idle | `DELETE` → 202 `deleting: true`, then GET → 404 | leftover empty |
| P6 | warm lease | `minReady` 1 boots one member; acquire → 201; `PUT` or `DELETE /api/vms/{id}` → 409 `pool_owned`; release deletes that VM and its disk | replacement VM id differs |
| P7 | API restart | keep one ready and one leased member; restart the API; member IDs, states and shim/Firecracker PIDs stay unchanged; retry acquire with the same key → 200 and the original active lease | release, then delete pool |

P6 boots a guest.
Run it with V7, not in the API-only pass.
CI runs P1–P7, C6, and X7 on the first OCI reference in `scripts/ci-qa-guest.sh`, after that guest boots and before the image delete.
P7 requires `FIRECRAB_QA_POOL_RESTART=1` on a disposable host. Other runs report it as a warning.
