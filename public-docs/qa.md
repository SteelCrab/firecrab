# QA work list

Use this list on **Linux**, **macOS**, and **Windows**.
The product surface after the API is up is the same: `http://127.0.0.1:5523`.
Mark every row `PASS`, `FAILED`, or `WARNING`.
`WARNING` is skip or leftover, not a silent pass.

Flow for every resource: **add → use or mutate → delete → leftover empty**.
Prefix names with `qa-` and use a dedicated subnet per run.
Do not apply `POST /api/update` unless that row is in scope.
Do not delete catalog images you did not install in this run.

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
- [nginx scenario (NGX)](#nginx-scenario-ngx)
- [CLI](#cli)
- [Cleanup](#cleanup)
- [CI map](#ci-map)
- [Related](#related)

## Platform gate

Reach `GET /api/host` → 200 before the shared list.
The install command differs by OS.
The API contract does not.

| ID | OS | Work |
| --- | --- | --- |
| G1 | Linux | `firecrab doctor` then `firecrab service install` / `start` / `status` |
| G2 | macOS | `firecrab service doctor` then `install` / `status` (nested virt required) |
| G3 | Windows | Same shape as macOS when microManager ships; until then mark WARNING |
| G4 | all | `GET /api/host` 200; dashboard `GET /` HTML 200 |
| G5 | all | `GET /api/no-such-route` JSON 404 with `requestId` |

GitHub-hosted ARM64 macOS runners do not support nested virtualization, so
microManager macOS CI runs on CircleCI. The CircleCI capability gate must
report `ready: true`; unsupported executors fail instead of skipping E2E.

Linux `firecrab service` drives host systemd.
macOS `firecrab service` drives the management VM, not a workload MicroVM.
Windows management-VM support is future until microManager ships; keep G3 `WARNING`.

## Shared host

| ID | Work | Add | Delete / cleanup |
| --- | --- | --- | --- |
| H1 | `GET /api/update` | check only | do not `POST` unless U1 |
| H2 | `GET /api/network` | uplink present; no `lo` / `fct*` / `mnb*` in picker | none |
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
| I7 | register | `POST /api/microregistry/register` for a custom installed alias | image delete in I6 |
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

CPU, RAM, disk, and egress edits only in `created` / `stopped` / `error`.
Env may change in `running`.

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
runs NGX5 inside that VM and proxies NGX6–NGX8 SSH through it; these rows are
required rather than warnings.

## CLI

Shared on every OS once the API is up.

| ID | Work | Add | Delete / cleanup |
| --- | --- | --- | --- |
| C1 | `firecrab vm list\|create\|start\|stop\|delete` | same rules as V* | leftover `[]` |
| C2 | `firecrab vm console` | attach then detach | with C1 |
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

## CI map

| Script | Runs |
| --- | --- |
| `scripts/ci-qa-api.sh` | G4 G5 H1 H2 N1–N5 S1–S3 L1–L3 I1 I8 I9 V14 C3 C5 X1–X4 X6 |
| `scripts/ci-qa-nginx.sh` | NGX1–NGX9 including V8a–V8d SSH |
| `scripts/ci-qa-ssh.sh` | V8a–V8d (called from guest boot and nginx) |
| `scripts/ci-qa-guest.sh` | I5 I6 V1 V2 V6 V7 V8 V9 V11 V12 V13 N6 C1 C2 C4 X5; expanded rows run for the first OCI reference, API guest flow for the remaining `alpine:3.21` `ubuntu:24.04` `fedora:42` references |
| CircleCI macOS M4Pro | Swift/Rust checks, signed helper, fresh install, API/nginx/guest E2E; capability failure is fatal |
| GitHub Actions | Linux, Windows, docs, frontend, installer, and release checks; no microManager macOS job |

Not in GitHub Ubuntu CI: I2 I3 I4 I7 I10 U1.

## Related

- [API](api.md)
- [Installation](installation.md)
- [firecrab CLI](firecrab-cli.md)
- [Networking](networking.md)
- [Storage](storage.md)
- [Images](images.md)
- [OCI images](oci.md)
- [Operations](operations.md)
- [Dashboard](dashboard.md)
