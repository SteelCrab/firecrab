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
| `scripts/ci-qa-api.sh` | G4–G5, H1–H3, N1–N5, S1–S3, L1–L3, I1/I8/I9, V14, C3/C5, X1–X4/X6 |
| `scripts/ci-qa-nginx.sh` | NGX1–NGX9, including real SSH, env, shell, and forwarded HTTP |
| `scripts/ci-qa-ssh.sh` | V8a–V8d; called by nginx and guest boot |
| `scripts/ci-qa-guest.sh` | I5/I6, V1/V2/V6–V9/V11–V13/V15, N6, C1/C2/C2b/C4, X5 |
| `scripts/ci-qa-lifetime.sh` | R1–R7, including R4b–R4e, plus X7 |
| `scripts/ci-qa-macos-e2e.sh` | Native Mac gate, shell, repair, browser, API, nginx, guest, and lifetime |
| `scripts/ci-qa-windows-e2e.ps1` | Native Windows gate/shell, WSL API/nginx/guest/lifetime/browser, plus Windows HTTP |
| `scripts/ci-qa-macos-e2e.sh repair` | G7/G7a runtime recovery and preservation; [detailed checks](macos-repair.md) |
| `firecrab-e2e` | B1–B17; full or explicit import/form mode |
| `firecrab-e2e` `test:dashboard` | Mocked dashboard/API/console checks |

Guest references are `alpine:3.21`, `ubuntu:24.04`, and `fedora:42`.
Expanded shared checks run on the first reference.
Preinstalled aliases are preserved; unperformed import rows become `WARNING`.
Later references still run after a failure; any failure produces a nonzero aggregate exit.
nginx is a separate pass using `nginx:1.27-alpine`.

R4f helper migration requires `FIRECRAB_QA_HELPER_UPGRADE=1` on a disposable Linux CI host.
Manual cases outside Ubuntu CI include I2/I3/I4/I7/I10/U1.

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
