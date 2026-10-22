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
| `scripts/test-ingress-install.sh` | Isolated initial install/reinstall; executable mode, all attributions, and saved routes, with fake service controls |
| `scripts/ci-qa-pipe-databases.sh --browser` | PDB1–PDB12; real API, PostgreSQL/MySQL, TLS and desktop/mobile browser checks |
| `scripts/test-pipe-databases-qa.py` | Database QA remote-host guard and cleanup after failures |
| `firecrab-e2e` `test:secrets` | SEC1–SEC8; isolated Secrets UI checks with a mocked API |
| `firecrab-e2e` `test:secrets:live` | SEC9–SEC14; Chromium, actual API processes, encrypted vaults, and local providers |
| `scripts/ci-qa-secrets.py` | Secrets unit tests, lint/types/builds, SEC1–SEC14; JSON/Markdown report, SEC15/SEC16 manual warnings |
| `scripts/test-ci-qa-secrets.py` | Failure aggregation, missing-case reporting, manual warnings, and stale-build prevention |

See [PIPE database E2E and QA](pipe-databases-qa.md) for the case matrix,
fixture-only browser commands, live database prerequisites and cloud acceptance limits.

Guest references are `alpine:3.21`, `ubuntu:24.04`, and `fedora:42`.
Expanded shared checks run on the first reference.
Preinstalled aliases are preserved; unperformed import rows become `WARNING`.
Later references still run after a failure; any failure produces a nonzero aggregate exit.
nginx is a separate pass using `nginx:1.27-alpine`.

R4f helper migration requires `FIRECRAB_QA_HELPER_UPGRADE=1` on a disposable Linux CI host.
Manual cases outside Ubuntu CI include I2/I3/I4/I7/I10/U1.

## PipeSecrets QA

Install the frontend/E2E dependencies and Chromium using the [browser setup](../../firecrab-e2e/README.md#setup).
Run `python3 scripts/ci-qa-secrets.py` on Linux or managed Debian.
This pass does not require a VM, KVM, a network helper, or a running installed API.
It uses temporary stores and local providers. It never targets the installed host database.
See the [Secrets suite](../../firecrab-e2e/README.md#pipesecrets) for individual commands and artifact paths.

Record SEC1–SEC14 from the generated report. The report retains each command and exit code.
Missing or skipped automated cases are `WARNING` and produce a nonzero aggregate exit.
SEC15 and SEC16 remain `WARNING` until the following external-account checks are performed separately.

1. Use a dedicated Infisical QA project/environment/path and an AWS QA prefix such as `qa-<run>/`.
2. Use short-lived credentials scoped to those resources. Enter them in the web console.
3. Create synthetic source values. Preview and import them. Reveal a copy and check that its source is correct.
4. Change a local copy. Preview the upload, select the replacement, and verify the external value.
5. Change the remote value after preview. Confirm that upload refuses that stale preview.
6. Check a denied read/write or expired credential. Confirm a visible error without raw provider data.
7. Remove the connection. Confirm that imported local copies remain. Delete only this run's QA values in both stores.
8. Record provider/region, scope, timestamp, case result, and cleanup status. Keep secret values and credentials out of reports and screenshots.

Use `WARNING` if an account, permission, or cleanup step is unavailable.
Infisical protocol fixtures and AWS SigV4 tests do not substitute for SEC15/SEC16.
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
