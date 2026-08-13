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
firecrab-doctor
```

## API configuration

Installed settings live in `/etc/firecrab/api.env`.
Restart the API after changing them.

```sh
systemctl restart firecrab-api
```

| Variable | Default | Job |
| --- | --- | --- |
| `FIRECRAB_BIND_ADDR` | `127.0.0.1:3000` | HTTP listen address |
| `FIRECRAB_ALLOWED_ORIGINS` | Development origin | Browser origin list |
| `FIRECRAB_IMAGE_ROOT` | Installed image path | Kernels and rootfs files |
| `FIRECRAB_IMAGE_BASE_URL` | Public MicroRegistry | Image package base URL; `none` disables remote installs |
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

Pull the new source and run `./install.sh` again.
The installer preserves VM data and `api.env`.

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
- [Networking](networking.md)
- [Troubleshooting](troubleshooting.md)
