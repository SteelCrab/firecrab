# API

REST and console WebSocket share `127.0.0.1:5523` by default.
Keep the unauthenticated listener on a trusted loopback path.

## Contents

- [Request rules](#request-rules)
- [VM endpoints and fields](#vm-endpoints)
- [Startup reconciliation](#api-startup-reconciliation)
- [Other endpoints](#other-endpoints)
- [Host responses](#host)
- [Update](#update)
- [Images and kernels](#images-and-kernels)
- [MicroNetwork](#micronetwork)
- [MicroRegistry](#microregistry)
- [Docker Hub login](#docker-hub-login)
- [Errors](#errors)

## Request rules

- JSON bodies require `Content-Type: application/json` and are limited to 64 KiB.
- Every response includes `X-Request-Id`.
- REST requests have a ten-second deadline, except `POST /api/network/reconcile`.
- Invalid paths return a JSON error.
- Browser mutations must use an allowed origin.

## VM endpoints

| Method | Path | Effect |
| --- | --- | --- |
| GET, POST | `/api/vms` | List / create |
| GET, PUT, DELETE | `/api/vms/{id}` | Read / edit / delete |
| POST | `/api/vms/{id}/start`, `/stop` | Start / stop |
| GET | `/api/vms/{id}/log` | Full console transcript |
| GET | `/api/vms/{id}/ssh-key` | Operator private key (`firecrab-<name>.pem`) |
| GET | `/api/vms/{id}/ssh-host-key` | Guest fingerprint and public key |
| GET | `/api/vms/{id}/ssh-host-key/check` | Scan and compare the injected guest host key |
| PUT | `/api/vms/{id}/storage` | Assign storage before disk creation |
| PUT | `/api/vms/{id}/shells` | Pin shell revisions |
| PUT | `/api/vms/{id}/port-forwards` | Replace forwarding rules |
| GET | `/ws/vms/{id}/console` | Serial-console WebSocket |

### Create a VM

Install an image and [create a network](../guides/networking.md#create) first.
Replace `NETWORK_ID` with its UUID.

```sh
curl -fsS -X POST http://127.0.0.1:5523/api/vms \
  -H 'Content-Type: application/json' \
  -d '{"name":"demo","template":"alpine-3.24.1","cpu":1,"ram":512,"diskGb":2,"microNetworkId":"NETWORK_ID"}'
```

Success returns `201` and the VM UUID.

### VM fields

| Field | Rule |
| --- | --- |
| `name` | 1–64 safe name characters |
| `template` | Installed image alias |
| `cpu` | 1–32 vCPUs |
| `ram` | Power of two, 128–32768 MiB |
| `diskGb` | Image minimum through 500 GiB; grow only |
| `microNetworkId` | Existing network UUID, required |
| `egressPolicy` | `internet` or `isolated` |
| `storageRoot` | Optional registered storage ID |
| `shellIds` | Optional shell IDs; latest revisions are pinned |
| `env` | Optional string map; create omission means `{}`, PUT omission preserves values, `{}` clears |

Environment keys use POSIX names; limits are 64 entries, 256 bytes per key, and 4096 bytes per value.
NUL is rejected.
Values are stored in plaintext in the guest.
Updating `env` while running restarts the app service; images without one ignore runtime environment changes.
See [Guest runtime](guest-runtime.md#environment).
CPU, RAM, disk, and egress edits require `created`, `stopped`, or `error`.

### VM states and console

```text
created -> starting -> running -> stopping -> stopped
              |           |          |
              +-> error <-+----------+
```

Start accepts `created`, `stopped`, and `error`; delete requires an inactive VM.
The console closes when a VM stops.
Guest logout closes it with `4000 session_ended` while the VM keeps running.
A new attachment replays only the new login session; reconnecting before logout retains current history.
Complete terminal queries are omitted from replay; live queries still pass through.

## API startup reconciliation

VM list/detail responses include `reconciliation: null` or a snapshot:

```json
{
  "outcome": "networkFailed",
  "checkedAtMs": 1791000000000,
  "detail": "failed to re-attach TAP: helper unavailable"
}
```

| Outcome | Meaning |
| --- | --- |
| `reconnected` | Reattached to a running/stopping shim; pending stop resumes |
| `gone` | No live shim or exit record; recorded `stopped` |
| `mismatched` | Shim cannot be controlled; unit stop attempted, `error` if stop fails |
| `networkFailed` | Running VM reattached but host networking recovery failed; lifecycle unchanged |
| `interrupted` | API restart interrupted startup; stop attempted and `error` recorded |
| `exited` | Exit record found; clean/requested exit means `stopped`, otherwise `error` |

`checkedAtMs` is Unix milliseconds; `detail` is optional and nullable.
Only records left `starting`, `running`, or `stopping` are checked at startup.
Results stay in memory, clear on an accepted new start/deletion, and are recomputed after restart.
Older APIs omit this field.

`POST /api/network/reconcile` retries bridges, TAPs, owned nftables state, forwarding, and serving DHCP state up to three times.
It keeps VM processes running and updates running VMs' snapshots.
Stopped/exited VM results stay unchanged.
It returns `204` or `503 network_recovery_failed` and has no ten-second REST deadline.
Repeat after repairing the host/helper; this is an explicit check, not periodic monitoring.

## Other endpoints

| Resource | Paths |
| --- | --- |
| MicroNetwork | `/api/micro-networks`, `/{id}` |
| Storage | `/api/storage`, `/api/storage/devices`, `/api/micro-storages` |
| Shells | `/api/shells`, `/{id}`, `POST /{id}/revisions`, `GET /{id}/revisions/{revisionId}` |
| Images | `/api/images`, `/{alias}`, `/{alias}/package`, `/{alias}/install`, `/{alias}/kernel`, `/{alias}/bootstrap` |
| Kernels | `/api/kernels`, `/{version}/install`, `/{version}` |
| OCI | `GET /api/oci/inspect`, `POST /api/oci/import`, `GET /api/oci/import/{alias}` |
| Registry | `/api/microregistry`, `/register`, `/register/{alias}`, `/local/{alias}`, `/docker-hub` |
| Host | `/api/host`, `/api/network`, `/api/info` |
| Update | GET / POST `/api/update`, GET `/api/update/progress` |

## Host

`GET /api/host` reports load, memory, disk, uptime, and `platform`:

```json
{
  "os": "windows",
  "name": "Windows 11",
  "version": "10.0.26200.9457",
  "architecture": "x86_64",
  "virtualization": "WSL2 2.7.14.0",
  "system": "Debian GNU/Linux 13 (trixie)",
  "kernel": "6.18.33.2-microsoft-standard-WSL2"
}
```

`os` is `linux`, `macos`, or `windows`; ARM64 is spelled `arm64`.
Native Linux uses os-release `PRETTY_NAME` for `name`/`system` and null `version`/`virtualization`.
Under microManager, `system`/`kernel` describe Debian; outer-host fields come from `/etc/firecrab/host-platform.json`.
Disk figures describe the filesystem holding the VM directory.
Older APIs omit `platform`.

`GET /api/info` reports:

| Fields | Source |
| --- | --- |
| `version`, `prefix`, `datadir`, `confdir`, `unitdir`, `apiBase` | Running API build, environment, and installer defaults |
| `firecrackerVersion` | Configured runtime's version, or `null` when unavailable |
| `apiService`, `netHelperService` | systemd `ActiveState`, or `unknown` |
| `helperUnit` | Canonical or legacy helper unit queried |

Runtime probes have two-second deadlines and cannot hide installation fields.
CLI `firecrab info --json` retains only the original six installation fields.

## Update

`GET /api/update` compares this build with the newest GitHub release, cached for 30 minutes.

```json
{
  "current": "0.3.0",
  "latest": "0.3.1",
  "updateAvailable": true,
  "notes": "Summary line.\n\n### Added\n\n- …",
  "releaseUrl": "https://github.com/SteelCrab/firecrab/releases/tag/v0.3.1"
}
```

- A failed check has `error` and no `latest`.
- `notes` is the Changelog section of the release notes, as Markdown cut to 16,000 characters. It is text from GitHub: show it as text, never as HTML.
- `releaseUrl` is always an `https://` URL. Both fields are omitted when there is no release or it has no notes, and by APIs before them.

`POST /api/update` starts `firecrab update --apply` detached and answers `202` with `{ "current", "pid" }`.
`GET /api/update/progress` says how far that run got; `idle` means none has run.

```json
{
  "phase": "downloading",
  "percent": 47,
  "target": "0.3.1",
  "downloadedBytes": 5242880,
  "totalBytes": 11178942,
  "pid": 4242,
  "updatedAtMs": 1791000000000
}
```

| Phase | Meaning | `percent` while in it |
| --- | --- | --- |
| `idle` | No run recorded | 0 |
| `checking` | Looking up the release | 0 |
| `downloading` | Fetching the host bundle; `downloadedBytes` and `totalBytes` count it | 5 to 80 |
| `verifying` | Comparing its SHA-256 with `SHA256SUMS` | 80 |
| `applying` | `firecrab-helper` replacing the binaries | 85 |
| `restarting` | The services are restarting | 97 to 99 |
| `done` | The API that answers runs `target` | 100 |
| `failed` | `error` says why; `percent` is where it stopped | as reached |

- `percent` is overall progress, not the stage's. A run is not `done` before the new API says so, so it never reads 100 early.
- The updater keeps the record in `$DATADIR/updates/progress.json` (its hidden `--progress-file`), so it survives the API restart that ends an update.
- `pid` is the updater's. It matches the `pid` of the `POST` that started the run, so a client can tell this run from an earlier one.
- A run still going when a process running `target` answers is `done`. One whose updater died before the helper took over, or that wrote nothing for 10 minutes, is `failed`.


## Images and kernels

Image list/detail records include installed state, kernel filename, artifact digests, and managed `kernelVersion` when present.
The kernel catalog is architecture-specific; inspect `GET /api/kernels` for available versions.

| Method | Path | Effect |
| --- | --- | --- |
| GET, POST | `/api/kernels/{version}/install` | Read / start verified download |
| DELETE | `/api/kernels/{version}` | Delete an unused cache entry |
| PUT | `/api/images/{alias}/kernel` | Pair using `{"kernelVersion":"7.2.2"}` |

Pairing preserves rootfs/initrd and requires an installed, verified kernel.
Missing/invalid cache returns `409 kernel_required`; referenced images return `409 in_use`.
Kernel deletion also returns `409 in_use` while an image references it.
Image deletion retains the independent kernel cache.
See [Images](../guides/images.md) and [Kernels](../guides/kernels.md) for task examples.

## MicroNetwork

Create requires `name` and `subnetCidr`.
Optional fields are `internetEnabled` (default `true`), `uplink`, `ipv6Cidr`, and `ipv6AddressMode`.

| Field | Rules |
| --- | --- |
| Create `uplink` | Omitted/null uses default-route interface; empty string returns `400` |
| PATCH `uplink` | Omitted preserves it; a name pins it; `""` resets to auto |
| PATCH `internetEnabled` | Required |
| `ipv6Cidr` | Optional unique-local or global `/64` |
| `ipv6AddressMode` | `slaac` or `dhcpv6`; mode alone generates ULA `/64`, prefix alone selects SLAAC |

Omit both IPv6 fields for IPv4-only.
Responses expose stored `uplink`; detail `nat.uplink` is the effective interface.
IPv6 response fields are `ipv6Cidr`, `ipv6Gateway`, `ipv6AddressMode`, and `ipv6Egress` (`nat66`/`direct`).
All four and VM `ipv6` are null on IPv4-only networks.

`GET /api/network` reports the default uplink and eligible `/sys/class/net` interfaces, excluding `lo`, `fct*`, and `mnb*`.
An invalid or missing interface name returns `400 validation_failed` on `uplink`.

## MicroRegistry

GET lists host-architecture release packages and persistent local registrations.
Local rows remain available during catalog outages; no local rows plus unavailable catalog returns `503`.

| Method | Path | Contract |
| --- | --- | --- |
| POST | `/api/microregistry/register` | Body `{"alias":"nginx-1.27","version":"1"}`; returns `202` job |
| GET | `/api/microregistry/register/{alias}` | `running`, `succeeded`, `failed`, or `idle` for unknown alias |
| DELETE | `/api/microregistry/local/{alias}` | `204`; unknown alias `404`; active job `409` |

Jobs contain `alias`, `status`, `log`, and timestamps.
Blank alias/version returns `400`; unknown, uninstalled, or `__microboot` alias returns `404`.
Name collisions return `409 alias_collision`; an active job returns `409 register_in_progress`.
Foreign/unsupported kernels fail without an archive or row; unclassifiable kernels are accepted.

Success stages `{alias}.tar.zst` and its SHA-256 locally and marks the row `downloadable`.
It publishes nothing remotely.
After template deletion, POST to `/api/images/{alias}/install` reinstalls the local package after verifying hash, alias, and version.
Poll the usual install endpoint; local aliases also support package-status GET.
Missing packages return `409 package_required`; modified packages fail without template registration.
Package-download POST remains limited to release aliases.
Delete the installed image and staging package before removing its local registration.

## Docker Hub login

| Method | Path | Contract |
| --- | --- | --- |
| PUT | `/api/microregistry/docker-hub` | Body `username` and `secret`; use a personal access token |
| GET | Same | `configured` and `username`; secret is never returned |
| DELETE | Same | Forget login; always `204` |

PUT trims surrounding whitespace, rejects blank values with `400 validation_failed`, and replaces any saved login.
The host stores one login in owner-only (`0600`) SQLite.
It is used only for Docker Hub token exchanges, including the toolbox pull.
Other registries are anonymous.
Anonymous quota exhaustion returns `429`; see [OCI images](../guides/oci.md).

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

Common statuses: `400`, `404`, `409`, `413`, `415`, `429`, `500`, `503`, `504`.
Use `requestId` to locate the server log.

## Related

- [Guest runtime](guest-runtime.md)
- [CLI](cli.md)
- [Troubleshooting](../guides/troubleshooting.md)
