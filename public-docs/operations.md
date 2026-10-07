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

Stop the API before a consistent offline backup.
Back up the data directory and image directory.

The data directory contains SQLite state and VM disk files.
The image directory contains source templates.

## Upgrade

Update in place from the latest GitHub Release.

```sh
firecrab update --check
sudo firecrab update --apply
```

`--check` is read-only and needs no privilege; it is also what runs with no flag at all.
`--apply` needs root or the `firecrab` service account, because the binary swap goes through the network helper's socket.
`--apply` prints a line for each stage (check, download, verify, apply, restart) with a gauge and the overall percent; off a terminal it prints one plain line per stage.
The dashboard's bottom-left indicator runs the same two steps: its dialog lists what the release changes, then shows a gauge, the percent, and the stage until the API is back on the new version.

The apply replaces binaries and the dashboard, then restarts both services.
The helper writes only into the install layout it derives from its own units, and rejects any other; a host installed with a non-default `PREFIX` needs `install.sh` re-run once so both units carry it.
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

1. Stop every running MicroVM (`firecrab-vm-*` unit). A VM's unit outlives the API on purpose, so nothing else would stop it.
2. Stop and remove the services, then run the helper's teardown: bridges, TAP devices, nftables tables, iptables forward and NAT rules, and UFW rules that firecrab created.
3. Remove the binaries, the kernel extract scripts, the dashboard, and the license files; the library directory goes with them.
4. Put back what the installer recorded in `/etc/firecrab/host-baseline.env`: the `/dev/kvm` ACL it added, and IPv4/IPv6 forwarding on a host that had it off. Forwarding stays on, with a note, while another bridge or container network exists.

Left alone on purpose: Firecracker, the `firecrab` account and group (and its `kvm` membership), and, without `--purge`, `/etc/firecrab` and `/var/lib/firecrab`.
A host installed before the record existed has none to restore, so it keeps the ACL and forwarding as they are until a reboot.
The checks are [UN1–UN7](TEST.md#uninstall).

## CI boot check

The scheduled workflow boots each supported image on KVM hosts.
It creates a network, starts a VM, checks connectivity, and removes the VM.

Run one image check locally after installation.

```sh
scripts/ci-m2-guest-boot.sh alpine-3.24.1
```

## Related

- [Architecture](architecture.md)
- [Installation](installation.md)
- [firecrab CLI](firecrab-cli.md)
- [Networking](networking.md)
- [Troubleshooting](troubleshooting.md)

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
[Helper service migration](installation.md#helper-service-migration).
`firecrab status --json` retains `netHelperService` and adds `helperUnit` to
identify the unit actually queried.
