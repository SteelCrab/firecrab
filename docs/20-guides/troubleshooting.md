# Troubleshooting

Start with the host doctor.
It is read-only and prints a suggested action for each failure.

```sh
./install.sh --doctor
```

Use `firecrab-doctor` after a system installation.

## Collect basic status

```sh
systemctl status firecrab-net-helper firecrab-api
journalctl -u firecrab-api -n 100 --no-pager
journalctl -u firecrab-net-helper -n 100 --no-pager
ls -l /dev/kvm /run/firecrab/net-helper.sock
df -h
```

For development, also check listening ports.

```sh
ss -ltnp | grep -E ':3000|:8080'
```

## Dashboard cannot reach the API

Confirm that `firecrab-api` is running.
The dashboard slows polling after three failed requests.

```sh
curl -i http://127.0.0.1:3000/api/vms
```

In development, open `http://localhost:8080`.
`http://127.0.0.1:8080` is a different origin and can return `403`.

Stop old API and Vite processes when rebuilt code does not appear.
An old process can still own port `3000` or `8080`.

## The API shows an empty database

Development data paths use the current working directory.
Run `cargo run -p firecrab-api` from the repository root.

Running from `firecrab-api/` creates a different `data` directory.
The doctor reports multiple database paths when it finds them.

## Every VM start fails immediately

Check KVM access.

```sh
ls -l /dev/kvm
id -nG
```

The API service user must be able to open `/dev/kvm`.
Installed systems add the `firecrab` user to the `kvm` group.

Development users may need group membership and a new login session.

```sh
sudo usermod -aG kvm "$USER"
```

## Network helper is unavailable

The API and helper must use the same Unix socket.
The default is `/run/firecrab/net-helper.sock`.

For development, start the helper first.

```sh
./scripts/dev-net-helper.sh
```

For an installed host, inspect the service and socket.

```sh
systemctl status firecrab-net-helper
ls -l /run/firecrab/net-helper.sock
journalctl -u firecrab-net-helper -n 100 --no-pager
```

Check `FIRECRAB_NET_HELPER_SOCK` in both service environments when a custom path is used.

## A VM stays in `starting`

Open the VM detail view.
The active start step identifies disk, process, or network work.

Read the API log and the VM console log.
Use the request ID when the start request returned an API error.

Check host disk capacity and latency when `preparingDisk` is slow.

```sh
df -h
iostat -xz 1
```

Several large disk copies on one device can saturate it.
Use [MicroStorage](micro-storage.md) to place VMs on separate mounted devices.

See the [concurrent start investigation](../50-bugs/vm-startup-stuck-under-concurrent-load.md) for historical details.

## A guest boots but becomes `error`

firecrab waits for `FIRECRAB_NETWORK_READY` on the serial console.
An old image may boot without the readiness service.

Rebuild or reinstall the image.
Restart the API after replacing image artifacts because the template registry validates them at startup.

## Guest has no IPv4 address

Inspect the selected MicroNetwork and its host bridge.

```sh
curl -s http://127.0.0.1:3000/api/micro-networks/<network-id>
ip -br link show type bridge
sudo ss -lunp | grep ':67'
```

Check the helper log for dnsmasq and bridge errors.
Check the guest console for `FIRECRAB_NETWORK_FAILED`.

UFW can block DHCP, DNS, or forwarded traffic on `mnb*` bridges.
Run the doctor and review `ufw status verbose`.

firecrab does not rewrite firewall rules owned by UFW.
Operators must allow required traffic or use the firecrab nftables policy without conflicting UFW rules.

See these investigations for known failure patterns:

- [DHCP never reaches the guest](../50-bugs/dhcp-never-reaches-guest.md)
- [Alpine network readiness race](../50-bugs/alpine-network-ready-races-dhcpcd.md)
- [UFW blocks outbound forwarding](../50-bugs/vm-outbound-forward-blocked-by-ufw.md)

## Internet access fails

Check both network-level and VM-level policies.

- `MicroNetwork.internetEnabled` must be `true`.
- `VM.egressPolicy` must be `internet`.
- The host must have an IPv4 default route.
- Host firewall rules must allow forwarding to the uplink.

Inspect the owned nftables table.

```sh
sudo nft list table inet firecrab
```

DHCP and gateway DNS can work even when forwarded internet traffic is blocked.

## Image install fails with a permission error

The API service user must be able to write the image root.
Check the configured `FIRECRAB_IMAGE_ROOT` and its parent directories.

```sh
namei -l /var/lib/firecrab/images
```

Do not leave root-owned image output in a user development tree.

## Image package download returns `503`

Set `FIRECRAB_IMAGE_BASE_URL` and restart the API.
The expected URL is `<base>/<alias>.tar.zst`.

```sh
curl -I "$FIRECRAB_IMAGE_BASE_URL/alpine-3.24.tar.zst"
```

Builder VM output can be installed from its local staged package without a remote base URL.

## Bootstrap fails

Read the bootstrap job log first.
The displayed log can contain only the latest part of a long console session.

The builder uses a small recovery environment.
Its root is memory-backed and does not behave like a normal installed distribution.

Common causes include:

- The builder cannot reach package mirrors.
- The builder runs out of memory.
- A chroot has no usable DNS configuration.
- A staged mount was not unmounted before `mkfs.ext4 -d`.
- The guest e2fsck is older than the tool that created the filesystem.

Rocky bootstrap requires a Rocky builder image.
Only one bootstrap job can run at a time.

See [M2Image builder](m2image-builder.md) for the supported flow.

## Terminal disconnects

The WebSocket route is `/ws/vms/{id}/console`.
The VM must have an active Firecracker process.

Check the browser network panel and API log.
Confirm that a reverse proxy forwards WebSocket upgrades for `/ws`.

Repeated cursor position text such as `;1R;80R` was an echo loop.
See the [terminal investigation](../50-bugs/terminal-cursor-position-echo-loop.md).

## Delete returns `409`

The resource is still active or in use.

- Stop a running VM before deleting it.
- Delete dependent VMs before deleting a MicroNetwork.
- Move or delete dependent VMs before deleting a MicroStorage.
- Delete dependent VMs before deleting an image.

Read `error.code`, `error.fields`, and `requestId` in the response.
See [API errors](api-error.md) for the common error shape.

## More detailed records

The [test index](../40-tests/MOC-tests.md) contains longer manual checks.
The [bug index](../50-bugs/MOC-bugs.md) contains investigation history.
