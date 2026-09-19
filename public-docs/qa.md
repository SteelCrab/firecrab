# microManager macOS QA

This page records a live add → delete → cleanup run of Firecrab on Apple silicon.
The API was `http://127.0.0.1:5523` behind a managed Debian VM.
The date is 2026-09-20.
The host was macOS 26.6.2 on arm64 (Apple M5).

Verdicts are `PASS`, `FAILED`, and `WARNING`.
`WARNING` is a skipped or leftover step, not a product failure.

## Contents

- [Method](#method)
- [Totals](#totals)
- [Gate](#gate)
- [Coverage](#coverage)
- [I10 OCI import](#i10-oci-import)
- [WARNING](#warning)
- [Leftovers](#leftovers)
- [Related](#related)

## Method

Six agents ran in parallel with disjoint name prefixes and subnets.
Each agent created its own resources, then deleted them, then confirmed the list was empty.
Agents did not stop or uninstall the management VM.
They did not apply `POST /api/update`.
They did not delete catalog images `ubuntu-26.04` or `rocky-9.8`.

| Agent | Job | Prefix |
| --- | --- | --- |
| `qa-mac-host` | Host doctor, validate, status, `/api/host` | none (read-only) |
| `qa-network` | MicroNetwork add, patch, delete | `qa-net-a`, `172.31.201.0/24` |
| `qa-storage-shells` | MicroStorage and Shell add, delete | `qa-st-`, `qa-sh-` |
| `qa-images` | Kernel, alpine install, OCI inspect/import | alpine kept as a fixture |
| `qa-vm` | VM create, env, start, stop, delete | `qa-vm-`, `172.31.202.0/24` |
| `qa-registry` | MicroRegistry and Docker Hub negatives | fake Docker Hub login deleted |

## Totals

| Verdict | Count |
| --- | --- |
| PASS | 81 |
| FAILED | 0 |
| WARNING | 8 |

I10 started as `FAILED` and became `PASS` after the guest gained `fakeroot`.

## Gate

| Check | Result |
| --- | --- |
| `firecrab service doctor` | Five host checks PASS, including nested virtualization |
| `firecrab service validate` | Kernel, OS/data disks, and VZ config PASS |
| `firecrab service install` | Cached artifacts; guest provision marker preserved |
| `firecrab service status` | launchd loaded; API reachable; guest `192.168.64.30` |
| `GET /api/host` | HTTP 200 |

Unsupported machines fail before a VM starts.
See [Installation](installation.md) for the CLI.

## Coverage

| Area | Flow | Result |
| --- | --- | --- |
| MicroNetwork | add, patch internet, SLAAC IPv6, delete, empty `uplink` 400 | PASS |
| MicroStorage | register `/var/lib/firecrab/qa-st-pool`, delete | PASS |
| Shell | create, revision, delete | PASS |
| Kernel 7.2.2 | install, pair with alpine | PASS |
| Image alpine-3.24.1 | package, install | PASS |
| OCI inspect `busybox:1.36.1` | aarch64 alias | PASS |
| OCI import `busybox:1.36.1` | succeeded in 1.94s after fakeroot, then delete | PASS |
| MicroVM | create with `env`, mutate env and ports, start, stop, delete | PASS |
| Guest start | `running` in 12.1s; `FIRECRAB_NETWORK_READY`; IPv4 `172.31.202.2` | PASS |
| SSH | operator key, host-key `match` | PASS |
| Console | WebSocket 101 | PASS |
| Docker Hub | PUT fake credential, GET omits secret, DELETE | PASS |
| Dashboard | `GET /` HTML 200 | PASS |
| Unknown API path | JSON 404 with `requestId` | PASS |

`GET /api/update` reported `current=0.2.2` and was not applied.

## I10 OCI import

The first import failed with `OCI ext4 run fakeroot chown failed at …/rootfs: No such file or directory (os error 2)`.
That message is spawn `ENOENT` for the `fakeroot` binary, not a missing tree.
The Debian guest apt list had `e2fsprogs` and not `fakeroot`.
Guest `install.sh --no-deps` does not pull Linux host packages.

After `apt-get install -y fakeroot` (1.37.1.1-1) on the running guest, import succeeded and the template was deleted.
The durable fix installs `fakeroot` on first boot and names a missing binary in the error.

See [OCI images](oci.md).

## WARNING

| ID | Item | Why it is WARNING |
| --- | --- | --- |
| H11 | Delete the management VM | Skipped so the API stayed up |
| H12 | Purge management disks | Same as H11 |
| I15 | Image bootstrap | Skipped; the job is a full builder VM |
| I16 | MicroRegistry register | No leftover custom alias after busybox delete |
| I17 | Delete kernel 7.2.2 | HTTP 409 `in_use` by alpine; fixture kept |
| I19 | Delete alpine-3.24.1 | Shared VM fixture; left installed |
| V14 | Delete a running VM | Needs a second VM; stop-then-delete PASSed |
| R8 | Mac CLI gaps | Env, shells, kernels, and Docker Hub are API/dashboard only |

H11 and H12 are protocol choices.
I17 and I19 leave alpine plus kernel 7.2.2 on the guest.
R8 matches the documented CLI surface in [firecrab CLI](firecrab-cli.md).

## Leftovers

After cleanup, VMs, networks, storages, and shells were empty lists.
Docker Hub was `configured=false`.
`busybox-1.36.1` was not installed.
`alpine-3.24.1` stayed installed with kernel 7.2.2.
The management VM remained running.

## Related

- [API](api.md)
- [OCI images](oci.md)
- [Operations](operations.md)
- [firecrab CLI](firecrab-cli.md)
- [Troubleshooting](troubleshooting.md)
