# Troubleshooting

Start with the read-only host doctor.

```sh
./install.sh --doctor
```
Use `firecrab-doctor` on an installed host.

## Basic status

```sh
systemctl status firecrab-net-helper firecrab-api
journalctl -u firecrab-api -n 100 --no-pager
journalctl -u firecrab-net-helper -n 100 --no-pager
ls -l /dev/kvm /run/firecrab/net-helper.sock
df -h
```

## Dashboard cannot reach the API

Check the API directly.

```sh
curl -i http://127.0.0.1:3000/api/vms
```

Use `http://localhost:8080` for Vite development.
`127.0.0.1:8080` is a different browser origin.

Check for old processes on ports `3000` and `8080`.

## Data appears empty

Run the development API from the repository root.
Default data paths use the current working directory.

The host doctor reports multiple database locations.

## VM start fails immediately

Check KVM access.

```sh
ls -l /dev/kvm
id -nG
```

The API user must be in the `kvm` group.
A new group membership needs a new login session.

## Network helper is unavailable

The API and helper must use the same socket.
The default is `/run/firecrab/net-helper.sock`.

Start the development helper first.

```sh
./scripts/dev-net-helper.sh
```

Check the installed socket and service.

```sh
systemctl status firecrab-net-helper
ls -l /run/firecrab/net-helper.sock
```

## VM stays in `starting`

Open the VM details and read the active start step.
Check the API log and console log.

Check disk space and latency during disk preparation.

```sh
df -h
iostat -xz 1
```

Use separate MicroStorage pools when one disk is saturated.

## Guest has no IPv4 address

Inspect the network and host bridge.

```sh
curl -s http://127.0.0.1:3000/api/micro-networks/<id>
ip -br link show type bridge
sudo ss -lunp | grep ':67'
```

Read the helper log for dnsmasq errors.
Read the guest log for `FIRECRAB_NETWORK_FAILED`.

The host firewall can block DHCP on `mnb*` bridges. Restart
`firecrab-net-helper` after an upgrade. An imported image has no `ping`;
use `/etc/firecrab/busybox ping 1.1.1.1` or restart for PATH tools.

## Guest PID 1

`readlink -f /proc/1/exe` is the running init (`/etc/firecrab/busybox`
on an import; `ps` may say `init`). `ls -l /sbin/init` can still show
systemd. Catalog Ubuntu/Rocky use systemd. See [API](api.md).

## Internet access fails

Check both policy levels.

- The MicroNetwork needs `internetEnabled: true`.
- The VM needs `egressPolicy: internet`.
- The host needs a default route.
- The host firewall must allow forwarding.

```sh
sudo nft list table inet firecrab
```

## OCI pull fails

`error sending request` is DNS/TLS/firewall, not a missing Docker Hub login.
`401` is a bad login; `429` is the spent anonymous quota — save a token, see [OCI](oci.md).
Check as the API user: `sudo -u firecrab curl -sI https://registry-1.docker.io/v2/` (`401` is expected).

## Image download returns `503`

Unset or set `FIRECRAB_IMAGE_BASE_URL` to a package base (not `none`/`-`).
The package URL is `<base>/<alias>.tar.zst`. Restart the API after changing it.

## Image install has a permission error

The API user must be able to write `FIRECRAB_IMAGE_ROOT`.

```sh
namei -l /var/lib/firecrab/images
```

## Bootstrap fails

Read the bootstrap job log first.

Common causes are network failure, low memory, chroot DNS, and tool mismatch.
Rocky 9.8 downloads a pinned Container-Base tarball; that URL must be reachable.
Only one bootstrap job can run at a time.

## Terminal disconnects

The VM must have an active Firecracker process.
The proxy must forward WebSocket upgrades for `/ws`.

Check the browser network panel and API log.

## Delete returns `409`

The resource is active or still in use.

- Stop a VM before deleting it.
- Remove VMs before deleting their network.
- Move or remove VMs before deleting their storage pool.
- Remove VMs before deleting their image.

Use the response `requestId` to find the server log.

## Related

- [API](api.md)
- [Networking](networking.md)
- [Storage](storage.md)
- [Images](images.md)
