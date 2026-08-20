# Operations

An installed host runs two systemd services.
The API manages VMs and the helper manages host networking.

## Services

| Service | Job |
| --- | --- |
| `firecrab-api` | HTTP, WebSocket, SQLite, and Firecracker |
| `firecrab-net-helper` | Bridge, TAP, DHCP, NAT, and firewall |

The helper should start before the API.

## Status and logs

```sh
systemctl status firecrab-net-helper firecrab-api
journalctl -u firecrab-api -f
journalctl -u firecrab-net-helper -f
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
| `FIRECRAB_BIND_ADDR` | `127.0.0.1:5523` | Management and API listen address |
| `FIRECRAB_BENCH_BIND_ADDR` | `127.0.0.1:15523` | Benchmark dashboard listen address |
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
The dashboard's bottom-left indicator runs the same two steps.

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
