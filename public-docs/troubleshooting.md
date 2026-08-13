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

UFW can block DHCP, DNS, or forwarding on `mnb*` bridges.
The host doctor reports common UFW conflicts.

## Internet access fails

Check both policy levels.

- The MicroNetwork needs `internetEnabled: true`.
- The VM needs `egressPolicy: internet`.
- The host needs a default route.
- The host firewall must allow forwarding.

```sh
sudo nft list table inet firecrab
```

## Image download returns `503`

The API uses the public MicroRegistry when `FIRECRAB_IMAGE_BASE_URL` is
unset. If the variable is empty or set to `none`/`-`, set it to a package
base URL (or unset it) and restart the API.
The package URL must be `<base>/<alias>.tar.zst`.

## Image install has a permission error

The API user must be able to write `FIRECRAB_IMAGE_ROOT`.

```sh
namei -l /var/lib/firecrab/images
```

## Bootstrap fails

Read the bootstrap job log first.

Common causes are network failure, low memory, chroot DNS, and filesystem tool mismatch.
Rocky 9.8 host builds and MicroBoot bootstraps download the pinned official
Container-Base tarball, so verify that the configured Rocky repository and
its checksum file are reachable.

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
