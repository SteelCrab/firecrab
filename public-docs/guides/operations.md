# Operations

An installed host runs two systemd services.
The API manages VMs and the helper manages host networking.

## Services

| Service | Job |
| --- | --- |
| `firecrab-api` | HTTP, WebSocket, SQLite, and Firecracker |
| `firecrab-helper` | Bridge, TAP, DHCP, NAT, and firewall |

The helper should start before the API.

## Status and logs

```sh
systemctl status firecrab-helper firecrab-api
journalctl -u firecrab-api -f
journalctl -u firecrab-helper -f
```

Run the host doctor after a failure.

```sh
firecrab doctor
```

## API configuration

Installed settings live in `/etc/firecrab/api.env`.
Restart the API after changing them.

```sh
systemctl restart firecrab-api
```

| Variable | Default | Job |
| --- | --- | --- |
| `FIRECRAB_BIND_ADDR` | `127.0.0.1:5523` | HTTP listen address |
| `FIRECRAB_ALLOWED_ORIGINS` | Development origin | Browser origin list |
| `FIRECRAB_IMAGE_ROOT` | Installed image path | Kernels and rootfs files |
| `FIRECRAB_IMAGE_BASE_URL` | Public MicroRegistry | Image package base URL; `none` disables remote installs |
| `FIRECRAB_OCI_MAX_BLOB_BYTES` | 16 GiB | Maximum compressed size of one downloaded OCI config or layer blob |
| `FIRECRAB_OCI_MAX_UNCOMPRESSED_LAYER_BYTES` | 64 GiB | Maximum decoded size of one OCI layer tar stream |
| `FIRECRAB_OCI_MAX_ROOTFS_BYTES` | 32 GiB | Maximum size of one OCI-imported ext4 image |
| `FIRECRAB_OCI_FASTFETCH_PATH` | (unset) | Host path of a fastfetch binary to copy into glibc guests |
| `FIRECRAB_OCI_KERNEL_PATH` | (unset) | Host path of the pinned OCI import kernel, for mirrors and air-gapped hosts |
| `FIRECRAB_STATIC_ROOT` | Installed dashboard | Static UI path |
| `FIRECRAB_STORAGE_ROOTS` | `default=data` | Fixed storage roots |
| `FIRECRAB_NET_HELPER_SOCK` | `/run/firecrab/net-helper.sock` | Helper socket |

Do not expose an unprotected listener to another network.

## Network helper

The helper socket is protected by file permissions and peer UID checks.
The helper derives interface names from UUIDs.

| Variable | Default | Job |
| --- | --- | --- |
| `FIRECRAB_NET_HELPER_SOCK` | `/run/firecrab/net-helper.sock` | Socket path |
| `FIRECRAB_NET_HELPER_ALLOWED_UID` | Helper UID | Extra API peer UID |
| `FIRECRAB_BRIDGE_MTU` | Uplink MTU | Bridge MTU |

The API and helper must use the same socket path.

## Backup

Stop workload VMs, then the API, before an offline backup.
Back up SQLite, every storage root, images, and configuration together.
See [Storage backup](storage.md#backup-and-retention).

## Upgrade

Update in place from the latest GitHub Release.

```sh
firecrab update --check
sudo firecrab update --apply
```

`--check` is read-only and needs no privilege.
It is also the default when no flag is given.
`--apply` needs root or the `firecrab` service account, because the binary swap goes through the network helper's socket.
`--apply` prints a line for each stage (check, download, verify, apply, restart) with a gauge and the overall percent; off a terminal it prints one plain line per stage.
The dashboard's bottom-left indicator runs the same two steps: its dialog lists what the release changes, then shows a gauge, the percent, and the stage until the API is back on the new version.

The apply replaces binaries and the dashboard, then restarts both services.
The helper accepts only the install layout derived from its service units.
For a non-default `PREFIX`, rerun `install.sh` once so both units record it.
It does **not** update the systemd unit files.
A release whose notes say a unit changed needs `install.sh` re-run once.

```sh
curl -fsSL https://github.com/SteelCrab/firecrab/releases/latest/download/install.sh | bash
```

`install.sh` also remains the way to install local builds, with `--bin-dir`.
Both paths preserve VM data and `api.env`.

Check both services and run the doctor after an upgrade.

## Uninstall

`./install.sh --uninstall` and `firecrab service uninstall` do the same on a Linux host, in this order:

1. Stop every running MicroVM unit (`firecrab-vm-*`).
2. Stop/remove services and tear down Firecrab-owned bridges, TAPs, firewall, and NAT state.
3. Remove binaries, kernel extraction scripts, dashboard, licenses, and the library directory.
4. Restore recorded KVM ACL and forwarding changes from `/etc/firecrab/host-baseline.env`.

Forwarding remains enabled while another bridge or container network needs it.

Firecracker and the `firecrab` account/group remain, including `kvm` membership.
Without `--purge`, configuration and data also remain.
A host installed before the record existed has none to restore, so it keeps the ACL and forwarding as they are until a reboot.
The checks are [UN1–UN7](../development/testing.md#uninstall).

## Retry VM network recovery

The API unit permits startup while the helper runtime directory is absent;
existing installations must rerun `install.sh` once to apply this unit change.
A surviving VM can report `networkFailed` after API startup. Inspect its
`reconciliation.detail` and helper logs, repair the reported host/helper problem,
then retry without restarting the VM:

```sh
curl -fsS -X POST http://127.0.0.1:5523/api/network/reconcile
curl -fsS http://127.0.0.1:5523/api/vms
```

The retry returns `204` on success or `503 network_recovery_failed` on failure.
It makes up to three attempts and refreshes running VMs' diagnostics/check times.
Stopped or exited VMs' historical results are retained. Verification covers owned
nft state, TAP attachment, bridge state/MTU/gateway, forwarding, DHCP files and
serving-process liveness. Traffic acceptance is tested separately by lifetime QA.

Helper service names and upgrade compatibility are described in
[Helper service migration](#helper-service-migration).
`firecrab status --json` retains `netHelperService` and adds `helperUnit` to
identify the unit actually queried.

## Helper service migration

`firecrab-helper.service` is the canonical unit.
Rerunning the installer replaces a real legacy `firecrab-net-helper.service` with an alias.
Both names then control one process, and legacy-name drop-ins remain effective.
Database, disks, and `api.env` are preserved.

The legacy executable path remains usable.
Bundles contain both executable names for old installers/update helpers; new installers accept old bundles too.
`firecrab update` replaces binaries without rewriting units, so legacy services work until migrated.

The socket, `FIRECRAB_NET_HELPER_*` variables, protocol, and `netHelperService` JSON stay compatible.
CLI and managed-guest service checks accept either unit name.
`helperUnit` identifies the unit queried.

## Related

- [Installation](../getting-started/installation.md)
- [CLI](../reference/cli.md)
- [Troubleshooting](troubleshooting.md)
