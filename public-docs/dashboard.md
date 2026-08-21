# Dashboard

React and TypeScript UI on the API and console WebSocket.

## Contents

- [Development](#development)
- [Screens](#screens)
- [VM workflow](#vm-workflow)
- [Networks](#networks)
- [Images](#images)
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
| Host | Host health and capacity |

## VM workflow

1. Create a MicroNetwork
2. Choose an installed image
3. Open Create (`#/vms/new`)
4. Set CPU, RAM, disk, storage, and egress
5. Create — returns to `#/vms`
6. Start
7. Open Terminal after `running`

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
  - **Off (IPv4 only)** — omit `ipv6Cidr` and `ipv6AddressMode`
  - **Enabled (auto ULA /64)** — send `ipv6AddressMode` (SLAAC or DHCPv6) and an optional prefix
- Blank prefix with IPv6 on: unique-local `/64`
- List IPv6 column: prefix + NAT66 or direct routing, or Off

## Images

- Inspect an OCI reference, then import
- Poll until the alias is in the local catalog and can create a VM
- Installed custom alias: register into this host's MicroRegistry catalog
- Local catalog row: SQLite, survives restart

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
- [OCI images](oci.md)
- [Networking](networking.md)
- [Troubleshooting](troubleshooting.md)
