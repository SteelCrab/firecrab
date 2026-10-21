# Platform validation history

These are dated measurements of specific source snapshots.
They do not establish current CI, stock-release, or untested-platform success.

## Windows source validation, 2026-10-03

Measured on `feat/windows-source-dev`, including uncommitted changes after baseline `085c23f`.
Environment: QEMU/KVM → Windows 11 Enterprise 26200.9457 x86_64 → WSL 2.7.14.0 / Debian 13 → Firecracker 1.17.0. The native Windows CLI and Linux services used debug builds; these are lab results with an extra virtualization layer.

| Check | Result | Evidence filename |
| --- | --- | --- |
| Unicode checkout + CRLF source deployment | PASS; services and Windows API healthy | `source-crlf-final.log` |
| Shared API QA | PASS, exit 0 | `api-crlf-final.log` |
| nginx, Windows localhost HTTP, SSH/env/shell/lifecycle | PASS, HTTP 200 | `nginx-crlf-final.log` |
| Complete Chromium suite, real boot and IPv4/IPv6 SSH | PASS: 9 passed; WARNING: 1 existing failed-register skip; exit 0 | `browser-crlf-final.log` |
| Alpine public-image guest QA | PASS, console/SSH/lifecycle | `guest-initial-summary.log` |
| Fedora console fallback | PASS, commands execute without agetty respawn | `fedora-console-real.log` |
| Ubuntu 24.04 public-image SSH QA | FAILED; APT exit 124, sshd absent; archive.ubuntu.com connection failed | `ubuntu-guest-final.log`, `ubuntu-apt-final-console.log` |
| Fedora 42 public-image SSH QA | FAILED; repository DNS/metadata delays, SSH not ready within budget; package completion unverified | `fedora-guest-final.log` |
| Final cleanup and cold restart | PASS; no VMs/networks, TCP 18080/18888/18022 closed, source services/API healthy, DNS restored | `final-clean-runtime.log`, `lab-dns-restored.log` |
| Updated Windows QA runner contracts | PASS: 8 checks, including failed/unrun/running phase reporting | `windows-qa-runner-contract.log` |
| Updated runner's API QA | PASS, finalized summary and exit 0 | `api-runner-update.log`, `api-runner-update-summary.json` |
| Updated runner's complete Chromium suite | PASS: 9 passed, 1 existing skip, 11.7 minutes; JSON/JUnit archive retained | `browser-runner-update.log`, `browser-runner-results.tar.gz` |
| `main` `15934df` Windows CLI `service shell` | PASS: G6, default stdin shell, `run` alias, literal arguments, stdio, exit codes; 203 native units and 11 runner checks | `../service-shell-qa-20261003/native-windows-tests-and-shell-qa.log`, `../service-shell-qa-20261003/default-shell-stdio-and-arguments.log` |

Evidence is retained in the local Windows lab's `microvm-qa-20261002` directory (shell logs in sibling `service-shell-qa-20261003`); these are not CI uploads. Shell checks reused the existing API/helper; latest-main service deployment and terminal keyboard/resize/Ctrl-C were not exercised.
The full public-image guest phase and `-Phase all` are **not PASS**.
Windows ARM64, native macOS runtime, native Windows release builds, and stock pinned v0.2.2 MicroVM operation were not validated by this run.
The lab's installed binary paths contained earlier experimental fixes; restore-path validation does not establish stock-release runtime success.
Temporary lab DNS settings were restored and QA management SSH services stopped after testing.

For SSH failures, retain the guest console and distinguish boot/network readiness from sshd readiness. `ci-qa-ssh.sh` fails early when both `FIRECRAB_PACKAGES_FAILED` and `FIRECRAB_SSHD skipped: no /usr/sbin/sshd` appear. A still-running package worker does not trigger that terminal-failure shortcut.
`ci-m2-guest-boot.sh` prints the last 8,000 console characters before failure cleanup.

## Windows source QA continuation, 2026-10-04
On `feat/windows-source-dev` after `main` baseline `15934df`, the Windows 11 x64 QEMU lab (4 vCPU / 6 GiB) passed native CLI HTTP tests (17) and `service shell` E2E (8). Earlier Windows runner contracts, shared API/shell QA, nginx including Windows localhost HTTP, and VM lifetime results remain recorded in the initial source snapshot.
The final full WSL Chromium run passed 12 cases with one existing explicit skip, zero failures, and exit 0. All three console cases passed, including two viewers ending together and command execution after reattachment. The DHCP guest authenticated SSH with the downloaded key through IPv4 forwarding and over IPv6 after its host key became ready. Tests used one worker and zero retries.
Public-image guest QA passed Alpine 3.21, Ubuntu 24.04, and Fedora 42 with exit 0, including actual SSH authentication and resource cleanup (`-WaitFactor 15`). Alpine was already installed, so its import rows remain `WARNING`; Ubuntu and Fedora were freshly imported. Fedora's SPDX SBOM contained 127 RPM packages plus the image entry. The existing Ubuntu template was backed up and replaced with a fresh import because provisioning changes are embedded in each image.
The Linux lab host repeatedly attempted suspend, removing its network routes and causing QEMU connections to return `ENETUNREACH`. A temporary sleep inhibitor kept the network available during QA. Source fixes also bound registry connection retries, allow 60/65 seconds for API/CLI inspection, give APT network stages 600 seconds without interrupting dpkg configuration, recover DNF metadata once, and parse exported RPM headers correctly. Console and SSH tests wait for actual readiness before sending input.
Final logs, JSON summaries, and JUnit/browser archives are retained in the local lab's `failure-fix-20261004` directory. Earlier failures remain in `qa-e2e-20261004` and separate failed-run artifacts. This validates the source lab's guest and full browser runs; Windows `-Phase all` was not rerun, and physical Windows, stock release, and native Windows Chromium remain unverified.

## Related

- [Windows setup](../getting-started/windows.md)
- [CI and runtime E2E](ci.md)
- [Test checklist](testing.md)
