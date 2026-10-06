# QA work list

For a checkable test plan with command blocks, see the [English TEST checklist](TEST.md) or [Korean TEST checklist](TEST.ko.md).

Use this list on **Linux**, **macOS**, and **Windows**; the API is at `http://127.0.0.1:5523`.
Mark every row `PASS`, `FAILED`, or `WARNING`; `WARNING` is skip or leftover, not a silent pass.

Flow for every resource: **add → use or mutate → delete → leftover empty**.
Prefix names with `qa-` and use a dedicated subnet per run.
Do not apply `POST /api/update` unless that row is in scope, and do not delete catalog images you did not install in this run.

CLI and curl are equivalent for shared resource commands.
Env, shells, kernels, storage assignment, port forwards, and Docker Hub are API or dashboard only.

## Contents

- [Platform gate](#platform-gate)
- [Shared host](#shared-host)
- [MicroNetwork](#micronetwork)
- [MicroStorage](#microstorage)
- [Shells](#shells)
- [Images, kernels, OCI](#images-kernels-oci)
- [MicroVM](#microvm)
- [VM lifetime](#vm-lifetime)
- [nginx scenario (NGX)](#nginx-scenario-ngx)
- [CLI](#cli)
- [Cleanup](#cleanup)
- [CI map](#ci-map)
- [Windows source validation, 2026-10-03](#windows-source-validation-2026-10-03)
- [Related](#related)

## Platform gate

Reach `GET /api/host` → 200 before the shared list; the install command differs by OS, the API contract does not.

| ID | OS | Work |
| --- | --- | --- |
| G1 | Linux | `firecrab doctor` then `firecrab service install` / `start` / `status` |
| G2 | macOS | `firecrab service doctor` then `install` / `status` (nested virt required) |
| G3 | Windows | `firecrab service doctor` then `install` / `status` (WSL2 with nested virt required) |
| G4 | all | `GET /api/host` 200; dashboard `GET /` HTML 200 |
| G5 | all | `GET /api/no-such-route` JSON 404 with `requestId` |
| G6 | macOS, Windows | `firecrab service shell -- id -un` is `root`; arguments arrive unchanged; a command's exit code comes back; no command opens a root login |

GitHub-hosted ARM64 macOS runners do not expose nested virtualization. They
run build/unit/signing checks only. This repository does not register a
persistent self-hosted Mac, so microManager runtime E2E is manual on a native
M3-or-later Mac; `doctor` must report `ready: true` before it runs.
GitHub-hosted Windows runners do not expose nested virtualization to WSL2
either, so Windows runtime E2E is manual on a Windows host whose `doctor`
reports `ready: true`.
`FIRECRAB_QA_WAIT_FACTOR` multiplies the guest waits in the nginx and SSH
checks for hosts whose guests install first-boot packages slowly.
It does not extend package-install deadlines or Playwright timeouts.

Linux `firecrab service` drives host systemd.
macOS `firecrab service` drives the management VM, not a workload MicroVM.
Windows `firecrab service` drives the managed WSL2 distribution the same way.

## Shared host

| ID | Work | Add | Delete / cleanup |
| --- | --- | --- | --- |
| H1 | `GET /api/update` | check only | do not `POST` unless U1 |
| H2 | `GET /api/network` | uplink present; no `lo` / `fct*` / `mnb*` in picker | none |
| H3 | `GET /api/info` | version and install paths | none |
| U1 | `POST /api/update` | optional dedicated run | host comes back; API 200 |

## MicroNetwork

| ID | Work | Add | Delete / cleanup |
| --- | --- | --- | --- |
| N1 | IPv4 network | `POST /api/micro-networks` name + `subnetCidr` → 201 | `DELETE` → 204, GET → 404 |
| N2 | list and detail | `GET /api/micro-networks` and `GET /{id}` | with N1 |
| N3 | internet toggle | `PATCH` `internetEnabled` false then true | with N1 |
| N4 | IPv6 SLAAC | `POST` with `ipv6AddressMode=slaac` (ULA `/64`) | `DELETE` that id |
| N5 | empty uplink | `POST` `uplink=""` → 400 | no row |
| N6 | busy network | `DELETE` while a VM is attached → 409 | delete VM first, then network |

## MicroStorage

| ID | Work | Add | Delete / cleanup |
| --- | --- | --- | --- |
| S1 | list roots | `GET /api/storage` has `default` | none |
| S2 | host devices | `GET /api/storage/devices` | none |
| S3 | register pool | `POST /api/micro-storages` absolute host path → 201 | `DELETE` → 204, GET → 404 |

The path is on the Firecrab host (Linux, or the Debian guest on macOS/Windows).

## Shells

| ID | Work | Add | Delete / cleanup |
| --- | --- | --- | --- |
| L1 | create | `POST /api/shells` name + `/bin/sh` body → 201 | `DELETE` → 204 |
| L2 | revision | `POST /api/shells/{id}/revisions` | with L1 |
| L3 | get body | `GET` shell and revision | with L1 |
| L4 | pin on VM | `shellIds` on create or `PUT /api/vms/{id}/shells` | guest `/var/lib/firecrab/shells/00.sh` (see NGX) |

## Images, kernels, OCI

| ID | Work | Add | Delete / cleanup |
| --- | --- | --- | --- |
| I1 | catalogs | `GET /api/images`, `/api/kernels`, `/api/microregistry` | none |
| I2 | kernel | `POST /api/kernels/{version}/install` to succeeded | `DELETE` if unused; 409 `in_use` if paired |
| I3 | M2Image | `POST /api/images/{alias}/package` then `/install` | `DELETE` package staging; delete image only if this run installed it |
| I4 | kernel pair | `PUT /api/images/{alias}/kernel` | 409 `kernel_required` if cache missing |
| I5 | OCI inspect | `GET /api/oci/inspect?reference=…` | none (no blobs) |
| I6 | OCI import | `POST /api/oci/import` then poll `/import/{alias}` | `DELETE /api/images/{alias}` |
| I7 | register | `POST /api/microregistry/register` for a custom installed alias | delete disk, package, then `/api/microregistry/local/{alias}` |
| I8 | Docker Hub | `GET` (no secret); optional `PUT` fake then `DELETE` | `configured=false` or prior login restored |
| I9 | missing alias | `GET /api/images/does-not-exist` → 404 | none |
| I10 | bootstrap | `POST /api/images/{alias}/bootstrap` | `DELETE /api/images/bootstrap/{id}` always drops the builder VM |

I10 is slow.
Run it as its own pass.

`fakeroot` must be on the Firecrab host or OCI import fails at ext4 pack.

CI boots these OCI references (inspect, import, start, stop, delete alias):
`alpine:3.21`, `ubuntu:24.04`, `fedora:42`.
The first reference additionally covers N6, V2, V6, V9, V12, C1, C2, and C4;
the nginx row below is a separate pass. If the first alias is already
installed, CI preserves it and marks the import rows `WARNING` rather than
claiming an import that did not run.

## MicroVM

Need an installed template (I3 or I6) and a network (N1).

| ID | Work | Add | Delete / cleanup |
| --- | --- | --- | --- |
| V1 | create | `POST /api/vms` name, template, cpu, ram, disk, `microNetworkId`, `env` | after V10 |
| V2 | get | `GET /api/vms` and `GET /{id}`; state `created` | with V1 |
| V3 | env (stopped) | `env` on create or `PUT`; guest `/etc/firecrab/vm.env` | with V1 |
| V4 | ports | `portForwards` on create or `PUT`; curl host port after start | with V1 |
| V5 | shells pin | `shellIds` on create or `PUT`; guest `/var/lib/firecrab/shells/00.sh` | with V1 |
| V6 | storage | `PUT /api/vms/{id}/storage` | with V1 |
| V7 | start | `POST /{id}/start` → `running`; log has `FIRECRAB_NETWORK_READY` | `POST /stop` |
| V8 | ssh | key, host-key, check, live login (V8a–V8d) | with V7 |

SSH is required after `running`, not only on nginx.

| ID | Work | Expect |
| --- | --- | --- |
| V8a | `GET /api/vms/{id}/ssh-key` | OpenSSH PEM (`firecrab-<name>.pem`) |
| V8b | `GET /api/vms/{id}/ssh-host-key` | `fingerprint` and `publicKey` |
| V8c | `GET /api/vms/{id}/ssh-host-key/check` | `status=match` (poll until sshd answers) |
| V8d | `ssh -i key -o IdentitiesOnly=yes root@<ipv4> true` | exit 0; `uname -s` non-empty |

On macOS, V8d reaches the workload guest through the management VM SSH proxy.
Missing proxy configuration is a failure in the required E2E suite.

| ID | Work | Add | Delete / cleanup |
| --- | --- | --- | --- |
| V9 | console | `GET /ws/vms/{id}/console` → 101 | with V1 |
| V10 | env (running) | `PUT env` while `running` (restarts `services.d/app`) | with V7 |
| V11 | stop | `POST /{id}/stop` → `stopped` | with V1 |
| V12 | delete running | `DELETE` while `running` → not allowed | stop first |
| V13 | delete stopped | `DELETE /api/vms/{id}` → 204, GET → 404 | confirm list |
| V14 | no network | `POST /api/vms` without `microNetworkId` → 400 | no row |
| V15 | console session end | `exit` in the guest shell closes `/ws/vms/{id}/console` with `4000` `session_ended`; VM stays `running` | with V7 |

CPU, RAM, disk, and egress edits only in `created` / `stopped` / `error`.
Env may change in `running`.

## VM lifetime

Needs a running VM (V7) and root on the API host; on macOS, use the management VM over SSH. Every VM runs in its own `firecrab-vm-<simple id>.service` unit.

| ID | Work | Expect |
| --- | --- | --- |
| R1 | shim in its unit | `firecrab-api vm-shim --vm-id <id>` is Firecracker's parent and the main process of an active `firecrab-vm-<simple id>.service`; its parent is PID 1 and it runs as the API user; the runtime directory has `shim.sock` and `console.log`; the unit's `MemoryMax` is the guest RAM plus the larger of 256 MiB and an eighth of it and its `CPUQuota` is one core per vCPU plus one (768 MiB and 200% for the 512 MiB, one-vCPU QA VM) |
| R2 | stop from outside the API | `systemctl stop firecrab-vm-<simple id>.service` → `stopped`; `exit.json` has `"stop_requested":true`; the TAP is gone |
| R3 | quick restart | stop, then start at once → `running` (the previous unit's name does not block the new one) |
| R4 | API restart | shim and Firecracker PIDs unchanged; VM stays `running`; journal has `adopted=1`; TAP still on its bridge; guest ping/SSH/forwarded HTTP and V9/V11 work |
| R4b | owned nft drift with helper alive | corrupt QA VM DNAT/egress/L2 policy while API is down; restart repairs it, guest ping/SSH/forwarded HTTP work, unchanged VM PIDs, unrelated table preserved |
| R4c | TAP/bridge drift | detach/down TAP, bridge down/wrong MTU/gateway prefix/forwarding; restart repairs all, guest traffic works, unchanged VM PIDs |
| R4d | DHCP drift/failure | corrupt hosts/base config and kill dnsmasq at unchanged revision; restart restores serving reservations; guest renews DHCP and traffic works |
| R4e | helper outage and operator retry | startup and retry failures report `networkFailed`/503; helper restoration + `POST /api/network/reconcile` gives 204 and `reconnected`, unchanged VM PIDs, guest traffic works |
| R4f | helper rename (disposable Linux CI only) | real legacy unit migrates to canonical + alias; executable path and legacy drop-in retained; helper PID matches under both names; VM PIDs and guest traffic survive |
| R5 | crash while the API is down | stop the API, `kill -9` Firecracker, start the API → `error`; its `fct*` TAP and nft rules are gone; the VM keeps its IPv4 |
| R6 | interrupted start | restart the API after the shim appears but before `running` → `error`; no unit remains |
| R7 | normal stop | V11 → `stopped`; `exit.json` has `"stop_requested":true`; `systemctl --failed` lists no `firecrab-vm-*` unit |

## nginx scenario (NGX)

One OCI guest that must hit env, DNAT, and the Shell repository together.
Reference: `nginx:1.27-alpine`.
CI script: `scripts/ci-qa-nginx.sh`.

| ID | Work | Expect |
| --- | --- | --- |
| NGX1 | I5 inspect + I6 import `nginx:1.27-alpine` | alias installed |
| NGX2 | L1 create a POSIX shell | 201 `shellId` |
| NGX3 | V1 create with `env.QA_NGINX=ci`, `shellIds`, `portForwards` 18080→80/tcp | 201 |
| NGX4 | V7 start → `running` with ipv4 | ping or `FIRECRAB_NETWORK_READY` |
| NGX5 | V4 live | `curl http://127.0.0.1:18080/` → 200 |
| NGX6 | V8a–V8d | PEM, host-key `match`, `ssh root@ipv4 true` |
| NGX6b | V3 live | SSH `cat /etc/firecrab/vm.env` has `QA_NGINX=ci` |
| NGX7 | V5 live | SSH `test -x /var/lib/firecrab/shells/00.sh` |
| NGX8 | V10 live | `PUT env QA_NGINX=two` while running; guest file updates |
| NGX9 | V11 stop, V13 delete VM, X5 delete alias, delete shell and network | leftover empty |

On macOS the API and port-forward live behind the management VM. The suite
runs NGX5 inside that VM and again on the Mac's `127.0.0.1` through the
microManager relay, and proxies NGX6–NGX8 SSH through it; these rows are
required rather than warnings.

On Windows, nginx QA runs inside WSL and also requires HTTP 200 from native
Windows `127.0.0.1:18080` through the source net-helper's TCP relay.
Browser IPv4/IPv6 SSH runs inside WSL; it does not prove native Windows IPv6 access.

## CLI

Shared on every OS once the API is up.

| ID | Work | Add | Delete / cleanup |
| --- | --- | --- | --- |
| C1 | `firecrab vm list\|create\|start\|stop\|delete` | same rules as V* | leftover `[]` |
| C2 | `firecrab vm console` | attach then detach | with C1 |
| C2b | `firecrab vm console` then `exit` | prints `guest session ended`, exits 0; VM stays `running` | with C1 |
| C3 | `firecrab network list\|create\|delete` | same as N* | leftover `[]` |
| C4 | `firecrab image list\|inspect\|import\|import-status` | same as I5–I6 | delete imported alias |
| C5 | `firecrab host add\|list\|use\|show\|remove` | local `~/.firecrab` | `host remove` |

Linux-only (skip on macOS/Windows CLI, or run inside the management guest):
`doctor`, `info`, `status`, `update --check|--apply`, systemd `service start|stop|restart|enable|disable`.

## Cleanup

| ID | Work |
| --- | --- |
| X1 | `GET /api/vms` is `[]` for `qa-*` names |
| X2 | `GET /api/micro-networks` has no `qa-*` |
| X3 | `GET /api/micro-storages` has no `qa-*` |
| X4 | `GET /api/shells` has no `qa-*` |
| X5 | custom OCI alias gone; catalog fixtures only if you chose to keep them |
| X6 | Docker Hub not left with a QA secret |
| X7 | no `firecrab-vm-*` unit remains for `qa-*` VMs |
| X8 | QA forwarded TCP ports are closed after VM stop/delete |
| X9 | Restore any manual QA DNS or management SSH changes; retain logs and result summaries |

## CI map

| Script | Runs |
| --- | --- |
| `scripts/ci-qa-api.sh` | G4 G5 H1 H2 H3 N1–N5 S1–S3 L1–L3 I1 I8 I9 V14 C3 C5 X1–X4 X6 |
| `scripts/ci-qa-nginx.sh` | NGX1–NGX9 including V8a–V8d SSH |
| `scripts/ci-qa-ssh.sh` | V8a–V8d (called from guest boot and nginx) |
| `scripts/ci-qa-lifetime.sh` | R1–R7 (including R4b–R4e) X7; root commands use `sudo` on Linux, direct execution in root WSL, or management VM SSH on macOS |
| `scripts/ci-qa-guest.sh` | I5 I6 V1 V2 V6 V7 V8 V9 V11 V12 V13 V15 N6 C1 C2 C2b C4 X5; expanded rows on the first reference, later references still checked after failure; aggregate exit 1 for any failure |
| `firecrab-e2e` `test:dashboard` | dashboard rows that fake the API and console (`@dashboard`), including V15 in the web terminal |
| Linux installer Chromium | B1–B17 with real boot, IPv4 forwarded SSH, and IPv6 SSH; B5 is the existing explicit skip |
| GitHub-hosted macOS | Swift/Rust checks, signed helper, and diagnostic JSON; no runtime E2E |
| GitHub-hosted Windows | CLI, PowerShell QA, and Node runner contracts plus diagnostic JSON; no runtime E2E |
| Windows host manual run | `ci-qa-windows-e2e.ps1`; optional source deployment, G6 `service shell`, then API/nginx/guest/Chromium inside WSL; nginx also checks Windows HTTP |
| Native M3+ manual run | `ci-qa-macos-e2e.sh`; install, G6 `service shell`, and browser/API/nginx/guest/lifetime E2E; capability failure is fatal |
| microManager PR report | `micromanager-pr-report.py`; comments both hosted jobs' results, log tails, and the manual E2E commands on PRs that touch them |

Not in GitHub Ubuntu CI: I2 I3 I4 I7 I10 U1. Linux installer CI runs Playwright; native macOS/Windows runtime is manual and ARM64 KVM CI is unregistered.
Windows `all` collects every test phase after successful prerequisites, retains logs/summary/browser archive, and exits nonzero if any phase fails.
See [CI and runtime E2E](ci.md) for job coverage, Windows phases, and source/release options.

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

## Related

- [CI and runtime E2E](ci.md)
- [API](api.md), [Dashboard](dashboard.md), and [firecrab CLI](firecrab-cli.md)
- [Installation](installation.md) and [Operations](operations.md)
- [Networking](networking.md) and [Storage](storage.md)
- [Images](images.md) and [OCI images](oci.md)
