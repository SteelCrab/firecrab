# API

- REST endpoints and one console WebSocket
- Default listen: `127.0.0.1:5523`

## Contents

- [Run](#run)
- [Request rules](#request-rules)
- [VM endpoints](#vm-endpoints)
- [Create a VM](#create-a-vm)
- [VM fields](#vm-fields)
- [Guest `/etc/firecrab`](#guest-etcfirecrab)
- [Other endpoints](#other-endpoints)
- [Images and kernels](#images-and-kernels)
- [MicroNetwork](#micronetwork)
- [MicroRegistry](#microregistry)
- [Docker Hub login](#docker-hub-login)
- [VM states](#vm-states)
- [Errors](#errors)
- [Related](#related)

## Run

```sh
cargo run -p firecrab-api
```

- Run from the repository root
- `RUST_LOG=firecrab_api=debug` for detailed logs

## Request rules

- JSON requests use `Content-Type: application/json`.
- Request bodies are limited to 64 KiB.
- Every response has `X-Request-Id`.
- REST requests have a 10 second deadline.
- Invalid paths return a JSON error.

## VM endpoints

| Method | Path | Job |
| --- | --- | --- |
| `GET`, `POST` | `/api/vms` | List or create VMs |
| `GET`, `PUT`, `DELETE` | `/api/vms/{id}` | Read, edit, or delete a VM |
| `POST` | `/api/vms/{id}/start` | Start a VM |
| `POST` | `/api/vms/{id}/stop` | Stop a VM |
| `GET` | `/api/vms/{id}/log` | Read logs |
| `GET` | `/api/vms/{id}/ssh-key` | Download the operator ed25519 private key (`firecrab-<name>.pem`) |
| `GET` | `/api/vms/{id}/ssh-host-key` | Guest host key fingerprint after first start |
| `GET` | `/api/vms/{id}/ssh-host-key/check` | Scan the guest now and compare it with the injected host key |
| `PUT` | `/api/vms/{id}/storage` | Assign storage |
| `GET` | `/ws/vms/{id}/console` | Open the serial console |

## Create a VM

Create a MicroNetwork first.

```sh
NETWORK_ID=$(curl -s -X POST http://127.0.0.1:5523/api/micro-networks \
  -H 'Content-Type: application/json' \
  -d '{"name":"lab","subnetCidr":"172.30.0.0/24"}' \
  | jq -r '.id')
```

Create the VM with the returned network ID.

```sh
curl -s -X POST http://127.0.0.1:5523/api/vms \
  -H 'Content-Type: application/json' \
  -d "{
    \"name\": \"demo\",
    \"template\": \"alpine-3.24.1\",
    \"cpu\": 1,
    \"ram\": 512,
    \"diskGb\": 2,
    \"microNetworkId\": \"$NETWORK_ID\"
  }"
```

The response has status `201` and includes the VM UUID.

## VM fields

| Field | Rule |
| --- | --- |
| `name` | 1 to 64 safe name characters |
| `template` | Installed image alias |
| `cpu` | 1 to 32 |
| `ram` | 128 to 32768 MiB and a power of two |
| `diskGb` | Image minimum to 500 GiB |
| `microNetworkId` | Existing network UUID |
| `egressPolicy` | `internet` or `isolated` |
| `storageRoot` | Optional storage ID |
| `shellIds` | Optional Shell repository ids (latest revision pinned) |
| `env` | Optional string map. Create omit = `{}`. PUT omit = keep stored; `{}` clears. Allowed while `running` (guest service restarts). POSIX keys, 64 entries, 256-byte keys, 4096-byte values, no NUL. Plaintext in the guest. |

## Guest `/etc/firecrab`

The host file `/etc/firecrab/api.env` is operator API settings; see [Installation](installation.md).
The guest directory `/etc/firecrab` is injected when an OCI image is imported.
Catalog templates (Alpine, Ubuntu, Rocky) do not use this tree.

| Guest path | Role |
| --- | --- |
| `/etc/firecrab/busybox` | Static multi-call toolbox. `/sbin/init` points here so the image boots as PID 1. |
| `/etc/firecrab/rc.boot` | One-shot sysinit: mounts, `/dev/fd`, hostname, metrics, DHCP, readiness sentinel, then `services.d`. |
| `/etc/firecrab/rc.console` | Fallback console (MOTD + ash) when the image has no `agetty`. |
| `/etc/firecrab/dhcp.script` | `udhcpc` hook. Applies address, default route, and `/etc/resolv.conf`. |
| `/etc/firecrab/services.d/` | Directory of guest services. `rc.boot` starts every executable after the sentinel. |
| `/etc/firecrab/services.d/app` | Image Entrypoint, Cmd, Env, and WorkingDir. Never PID 1. |
| `/etc/firecrab/services.d/sshd` | Key-only `sshd -D` after first-boot `openssh-server`. |
| `/etc/firecrab/vm.env` | Per-VM `env` sidecar. `services.d/app` sources it. Plaintext. |
| `/etc/firecrab/base-packages.ok` | Stamp after the first-boot package install. |

`inittab` runs `rc.boot` once, then respawns the serial console.
`rc.boot` mounts `/proc`, `/sys`, `/dev` (with `/dev/fd`), and `/run`, sets the hostname from `/etc/hostname`, starts the metrics agent, brings `eth0` up, runs DHCP, prints `FIRECRAB_NETWORK_READY`, and starts each executable in `services.d`.
When the image ships `agetty` and bash, the console is `ttyS0 → agetty → login → bash`.
Otherwise it is `rc.console`.

Create and start write `env` into `/etc/firecrab/vm.env` and insert one delimited source block in `services.d/app`:

```sh
# >>> firecrab vm env
. /etc/firecrab/vm.env
# <<< firecrab vm env
```

Image `export` lines stay.
VM keys win because the source sits after those lines and before `exec`.
An empty `env` map removes the block.
A missing `services.d/app` is a no-op (`hasGuestService` on `GET /api/images`).
`PUT /api/vms/{id}` with `env` while `running` rewrites `vm.env` and restarts `services.d/app`.
CPU, RAM, disk, and egress still require a stopped VM.

Inspect from the guest console:

```sh
ps -p 1 -o pid,comm,args
readlink -f /proc/1/exe
tr '\0' ' ' < /proc/1/cmdline; echo
ls -la /etc/firecrab /etc/firecrab/services.d
cat /etc/firecrab/vm.env
grep -A2 'firecrab vm env' /etc/firecrab/services.d/app
```

`/proc/1/exe` is the running init. On an imported image that is
`/etc/firecrab/busybox`. `ps` may show `init` because that is the applet
name. Do not trust `ls -l /sbin/init`: usr-merged images (Ubuntu, Debian)
may still show the original `systemd` symlink.

Related guest paths that are not under `/etc/firecrab`:

| Guest path | Role |
| --- | --- |
| `/sbin/init` | Intended symlink to `/etc/firecrab/busybox`. Image links (e.g. systemd) may remain; use `/proc/1/exe`. |
| `/bin/ping`, `/bin/wget`, … | Toolbox applets when the image did not ship them. |
| `/etc/inittab` | busybox init job table. |
| `/etc/hostname`, `/etc/motd` | Written per VM on start. |
| `/usr/local/sbin/firecrab-guest-agent` | CPU and memory samples for the dashboard. |
| `/run/firecrab-app.pid` | PID of the running `services.d/app`. |

Catalog guests keep the agent and Shell repository under `/usr/local/sbin` and `/var/lib/firecrab/shells`.

## Other endpoints

| Resource | Paths |
| --- | --- |
| MicroNetwork | `/api/micro-networks` and `/{id}` |
| MicroStorage | `/api/storage`, `/api/storage/devices`, `/api/micro-storages` |
| Shells | `/api/shells`, `/{id}`, `POST /{id}/revisions`, `GET /{id}/revisions/{revisionId}`; VM pin `PUT /api/vms/{id}/shells` (Alpine OpenRC + Ubuntu/Rocky systemd; prefer POSIX `/bin/sh`) |
| Images | `/api/images`, `/{alias}`, `/{alias}/package`, `/{alias}/install`, `/{alias}/kernel`, `/{alias}/bootstrap` |
| Kernels | `/api/kernels`, `/{version}/install`, `/{version}` |
| OCI | `/api/oci/inspect`, `POST /api/oci/import`, `GET /api/oci/import/{alias}` |
| MicroRegistry | `/api/microregistry`, `POST /register`, `GET /register/{alias}`, `GET`/`PUT`/`DELETE /docker-hub` (Docker Hub login; secret write-only) |
| Host | `/api/host` and `/api/network` |

## Images and kernels

`GET /api/images` lists installed and known-but-uninstalled M2Images.
`GET /api/images/{alias}` returns one complete image detail record.
Image records include `kernelVersion` for a managed kernel, the public
`kernelImage` filename, and the kernel/rootfs/initrd digests.

`GET /api/kernels` lists the host architecture's digest-pinned kernel catalog
and local cache state. The newest catalog entry is Linux `7.2.2`; `7.1.9`
remains available as a compatibility or rollback choice.

| Method | Path | Job |
| --- | --- | --- |
| `GET` | `/api/kernels` | List catalog versions and installed/in-use state |
| `GET`, `POST` | `/api/kernels/{version}/install` | Read or start kernel download and verification |
| `DELETE` | `/api/kernels/{version}` | Remove an unused local kernel cache |
| `PUT` | `/api/images/{alias}/kernel` | Pair an installed image with an installed kernel |

Install a kernel before updating an image.

```sh
curl -s -X POST http://127.0.0.1:5523/api/kernels/7.2.2/install
curl -s -X PUT http://127.0.0.1:5523/api/images/ubuntu-26.04/kernel \
  -H 'Content-Type: application/json' \
  -d '{"kernelVersion":"7.2.2"}'
```

The update keeps the image's rootfs and optional initrd. It returns `409`
`kernel_required` when the selected cache is absent or fails verification,
and `409` `in_use` when an instance VM still references the image.
Deleting an image does not delete a managed kernel cache; deleting a kernel
is refused while any installed image references it.

## MicroNetwork

`POST /api/micro-networks`:

- `name`, `subnetCidr`
- optional `internetEnabled` (default `true`)
- optional `uplink`, `ipv6Cidr`, `ipv6AddressMode`

Uplink:

- Host NIC name
- Omit or `null`: host default-route interface
- Empty string on create: `400` field `uplink`
- `GET` list/detail: stored `uplink`; `null` means auto
- Detail `nat.uplink`: effective interface after that default
- `PATCH /api/micro-networks/{id}`: `internetEnabled` required
- PATCH omit `uplink`: leave stored name
- PATCH a name: pin NAT to that NIC
- PATCH `""`: reset to auto

IPv6:

- Omit both `ipv6Cidr` and `ipv6AddressMode`: IPv4-only
- `ipv6AddressMode` without a prefix: unique-local `/64`
- `ipv6Cidr`: `/64`, unique-local or global
- `ipv6AddressMode`: `slaac` or `dhcpv6`; omitted next to a prefix means SLAAC
- Response fields: `ipv6Cidr`, `ipv6Gateway`, `ipv6AddressMode`, `ipv6Egress` (`nat66` or `direct`)
- No IPv6 (including pre-dual-stack rows): all four `null`
- `VmResponse.ipv6`: stored address, or `null` on IPv4-only

`GET /api/network`:

- `uplink`: default-route iface
- `interfaces`: dashboard picker from `/sys/class/net`, omits `lo`, `fct*`, `mnb*`
- Bad or missing name: `400` `validation_failed` on field `uplink`

## MicroRegistry

`GET /api/microregistry` lists host-arch release packages and local custom aliases.
Local rows stay in SQLite across restart.
They are still returned when the public catalog is down or consume is disabled.
With no local rows and no catalog, GET is 503.

Register an already-installed custom image.

```sh
curl -s -X POST http://127.0.0.1:5523/api/microregistry/register \
  -H 'Content-Type: application/json' \
  -d '{"alias":"nginx-1.27","version":"1"}'
```

The body is `alias` (installed template) and `version`.
The reply is `202` with a job: `alias`, `status`, `log`, and timestamps.
Poll `GET /api/microregistry/register/{alias}`.
`status` is `running`, then `succeeded` or `failed`.
An unknown alias is `idle`.

Empty `alias` or `version` is `400`.
Unknown, uninstalled, or `__microboot` is `404`.
A public-catalog or existing local name is `409 alias_collision`.
A job already running for that alias is `409 register_in_progress`.

Success writes a local `{alias}.tar.zst` and its SHA-256.
Nothing is published remotely.
GET then marks the row `downloadable`.
`/package` and `/install` still accept only release aliases.

A foreign or unsupported kernel fails the job with no row and no archive.
An unclassifiable kernel is accepted.

## Docker Hub login

Anonymous Docker Hub pulls are rate limited per source address.
One shared egress IP spends that quota, and OCI inspect or import then answers `429`.
Save one account so pulls count against it instead.

```sh
curl -s -X PUT http://127.0.0.1:5523/api/microregistry/docker-hub \
  -H 'Content-Type: application/json' \
  -d '{"username":"pista","secret":"dckr_pat_..."}'
```

Use a personal access token, not the account password.
The body is `username` and `secret`; blank either is `400 validation_failed`.
Surrounding whitespace is trimmed, so a pasted token keeps working.
A second `PUT` rotates the stored login.

`GET /api/microregistry/docker-hub` answers `configured` and `username`.
The secret is write-only and never appears in a response.
`DELETE` forgets it and answers `204`, also when nothing was stored.

One login is kept per host, in SQLite, next to the rest of the state.
That database file is owner-only (`0600`), because it now holds a registry secret.
It is sent as HTTP Basic on the registry token exchange only, never on a manifest or blob request.
It is offered to Docker Hub alone — `docker.io` and `registry-1.docker.io` are the same account.
Any other registry, including a private mirror or a `FIRECRAB_OCI_TOOLBOX_IMAGE` override, is pulled anonymously.
Import uses it for the image and for the busybox toolbox pull; see [OCI](oci.md).

## VM states

```text
created -> starting -> running -> stopping -> stopped
              |           |          |
              +-> error <-+----------+
```

Start is allowed from `created`, `stopped`, and `error`.
Delete is allowed only while a VM is inactive.

## Errors

```json
{
  "error": {
    "code": "validation_failed",
    "message": "request validation failed",
    "fields": {},
    "requestId": "<uuid>"
  }
}
```

Common status codes are `400`, `404`, `409`, `413`, `415`, `429`, `500`, `503`, and `504`.
Use `requestId` to find the matching server log.

## Related

- [Networking](networking.md)
- [Storage](storage.md)
- [Images](images.md)
- [Kernel management](kernels.md)
- [OCI images](oci.md)
- [Troubleshooting](troubleshooting.md)
