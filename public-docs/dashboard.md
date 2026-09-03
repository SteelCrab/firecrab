# Dashboard

React and TypeScript UI on the API and console WebSocket.

## Contents

- [Development](#development)
- [Screens](#screens)
- [VM workflow](#vm-workflow)
- [Networks](#networks)
- [Images](#images)
- [Kernels](#kernels)
- [Production](#production)
- [Check](#check)
- [Related](#related)

## Development

Terminal 1:

```sh
./scripts/dev-net-helper.sh
```

Terminal 2:

```sh
cargo run -p firecrab-api
```

Terminal 3:

```sh
npm install --prefix firecrab-frontend
npm run dev --prefix firecrab-frontend
```

- Open `http://localhost:8080/`
- Use `localhost` (exact CORS origin)

## Screens

| Screen | Job |
| --- | --- |
| MicroVM | List at `#/vms`. Create at `#/vms/new` |
| Terminal | Serial console |
| Networks | MicroNetworks at `#/networks`. IPv6 is a create-time select |
| Storage | MicroStorage pools |
| Images | M2Image install or OCI import |
| Kernels | Digest-pinned kernel installation and lifecycle |
| Host | Host health and capacity |

## VM workflow

1. Create a MicroNetwork
2. Choose an installed image
3. Open Create (`#/vms/new`)
4. Set CPU, RAM, disk, storage, and egress
5. Create — returns to `#/vms`
6. Start
7. Open Terminal after `running`

- Three ways to the same SSH panel: the console SSH tab, `SSH` in VM detail, and
  `⋯` → `SSH connect` in the VM list, which opens it as a dialog
- Row actions (`Terminal`, `SSH connect`, `start` / `stop` / `delete`) sit behind the row's
  `⋯` menu; Esc closes the menu, then the dialog
- SSH panel: download `firecrab-<name>.pem`, or reveal it behind the eye toggle and copy the
  text; the key is fetched only when asked for. A copyable `wget` block does the same
  download from a shell (`-O <name>.pem && chmod 600`). Then the fingerprint block,
  `ssh-keyscan | ssh-keygen -lf` verify block, a copyable `check` one-liner, then `ssh -i …`
- `check ipv4` / `check ipv6` compare on the host and print `MATCH` or `MISMATCH`, so two base64
  fingerprints never have to be read side by side
- `proxy jump` block: `ssh -J <hostUser>@<hostIP> …` reaches the guest through the Firecrab
  host and needs no rule, so inbound stays denied; a non-standard host SSH port goes on the
  jump target as `…:2222`
- `port forward` block: pick a host port and the panel writes `host:PORT → guest 22/tcp`
  through `PUT /api/vms/{id}/port-forwards`, then prints `ssh -p PORT …`; Remove takes back
  only that rule. The port opens on this host — an outside client also needs the router to
  forward it
- Both commands carry `<hostIP>` as a placeholder: the address the dashboard is served from
  is often not the one that reaches the host from elsewhere
- SHA256 is the guest `ssh_host_ed25519` key, not the PEM. First `ssh` prompt must match.
- Terminal chrome is light; the serial surface stays dark
- Inspect rail: four equal cards (general, specs, network, usage) then ports + storage; a bottom white bar toggles it
- Terminal Network group: ipv4, ipv6 (`—` when the network is IPv4-only), mac, egress, network id
- List poll: 3 seconds
- Detail: start progress and logs
- While `running`, list / detail / terminal show guest OS CPU percent and memory used (MemTotal − MemAvailable) when the Firecrab Metrics Agent is in the guest (systemd on Ubuntu/Rocky, OpenRC on Alpine)
- Detail and terminal: sparklines from recent samples
- Agent missing: values stay `null`, start still succeeds
- After an API upgrade: stop/start reinstalls the agent on the guest disk
- Resource changes: inactive VMs only
- Disk: grow only
- Per-VM `env`: editable while `running`; save restarts the guest service; stored in plaintext
- Image without `/etc/firecrab/services.d/app`: ignores runtime env (`hasGuestService` on `GET /api/images`)

## Networks

- Route: `#/networks`
- IPv4 CIDR: required
- IPv6 select:
  - **Off (IPv4 only)** — omit `ipv6Cidr` and `ipv6AddressMode`; prefix and addressing fields are hidden
  - **Enabled (auto ULA /64)** — prefix and addressing expand; send `ipv6AddressMode` (SLAAC or DHCPv6) and an optional prefix
- Blank prefix with IPv6 on: unique-local `/64`
- List IPv6 column: prefix + NAT66 or direct routing, or Off
- Detail: IPv6 off is one line; IPv6 on shows prefix, gateway, mode, and NAT66/direct

## Images

- Inspect an OCI reference, then import
- Poll until the alias is in the local catalog and can create a VM
- Installed custom alias: register into this host's MicroRegistry catalog
- Local catalog row: SQLite, survives restart
- Image detail shows the kernel version, filename, and digests
- Image detail can switch an installed image to an installed managed kernel when no instance VM uses it

## Kernels

- Route: `#/kernels`
- Install and verify the host architecture's digest-pinned kernel releases
- View version, architecture, image digest, usage, and job logs
- Delete only kernels no image references
- See [Kernel management](kernels.md) for the REST contract

## Production

```sh
npm run build --prefix firecrab-frontend
FIRECRAB_STATIC_ROOT="$PWD/firecrab-frontend/dist" \
  cargo run -p firecrab-api
```

- Open `http://127.0.0.1:5523/`
- Host installer uses this mode

## Check

- Ports `5523` and `8080` free of old processes
- API started from the repository root
- Helper running before a VM start
- Reverse proxy forwards WebSocket upgrades for `/ws`

## Related

- [API](api.md)
- [Kernel management](kernels.md)
- [OCI images](oci.md)
- [Networking](networking.md)
- [Troubleshooting](troubleshooting.md)
