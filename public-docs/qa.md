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
- [CLI](#cli)
- [Cleanup](#cleanup)
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

Linux `firecrab service` drives host systemd.
macOS and Windows `firecrab service` drive the management VM, not a workload MicroVM.

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

## MicroVM

Need an installed template (I3 or I6) and a network (N1).

| ID | Work | Add | Delete / cleanup |
| --- | --- | --- | --- |
| V1 | create | `POST /api/vms` name, template, cpu, ram, disk, `microNetworkId`, `env` | after V10 |
| V2 | get | `GET /api/vms` and `GET /{id}`; state `created` | with V1 |
| V3 | env (stopped) | `PUT /api/vms/{id}` with `env` | with V1 |
| V4 | ports | `PUT /api/vms/{id}/port-forwards` tcp or udp | with V1 |
| V5 | shells pin | `PUT /api/vms/{id}/shells` | with V1 |
| V6 | storage | `PUT /api/vms/{id}/storage` | with V1 |
| V7 | start | `POST /{id}/start` → `running`; log has `FIRECRAB_NETWORK_READY` | `POST /stop` |
| V8 | ssh | `GET /ssh-key`, `/ssh-host-key`, `/ssh-host-key/check` | with V1 |
| V9 | console | `GET /ws/vms/{id}/console` → 101 | with V1 |
| V10 | env (running) | `PUT env` while `running` (restarts `services.d/app`) | with V7 |
| V11 | stop | `POST /{id}/stop` → `stopped` | with V1 |
| V12 | delete running | `DELETE` while `running` → not allowed | stop first |
| V13 | delete stopped | `DELETE /api/vms/{id}` → 204, GET → 404 | confirm list |
| V14 | no network | `POST /api/vms` without `microNetworkId` → 400 | no row |

CPU, RAM, disk, and egress edits only in `created` / `stopped` / `error`.
Env may change in `running`.

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
