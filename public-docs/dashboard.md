# Dashboard

The dashboard is a React and TypeScript application.
It uses the API and console WebSocket.

## Development

Start the network helper in terminal 1.

```sh
./scripts/dev-net-helper.sh
```

Start the API in terminal 2.

```sh
cargo run -p firecrab-api
```

Start Vite in terminal 3.

```sh
npm install --prefix firecrab-frontend
npm run dev --prefix firecrab-frontend
```

Open `http://localhost:8080/`.
Use `localhost` because the development origin is exact.

## Screens

| Screen | Job |
| --- | --- |
| MicroVM | List VMs at `#/vms`. Create opens `#/vms/new`. |
| Terminal | Use the serial console |
| Networks | Manage MicroNetworks |
| Storage | Manage MicroStorage pools |
| Images | Install M2Images or import an OCI image |
| Host | Show host health and capacity |
| Benchmarks | Show performance trends, result tables, and current VM state at `#/benchmarks` |

## VM workflow

1. Create a MicroNetwork.
2. Choose an installed image.
3. Open Create (`#/vms/new`).
4. Set CPU, RAM, disk, storage, and egress.
5. Create the VM. The dashboard returns to `#/vms`.
6. Start it.
7. Open Terminal after it reaches `running`.

The list refreshes every three seconds.
The detail view shows start progress and logs.

While a VM is `running`, the list, detail view, and terminal page show
**guest OS** CPU percent and memory used (MemTotal − MemAvailable) when the
Firecrab Metrics Agent is running inside the guest (systemd on Ubuntu/Rocky,
OpenRC on Alpine). Detail and terminal also draw short sparklines from recent
samples. Values are `null` until the agent reports (missing agent does not
block VM start). A stop/start after an API upgrade reinstalls the agent into
the guest disk.

Resource changes are allowed only while the VM is inactive.
Disk size can grow but cannot shrink.
Per-VM `env` can be edited while `running`; save restarts the guest service. Stored in plaintext.
An image without `/etc/firecrab/services.d/app` ignores runtime env (`hasGuestService` on `GET /api/images`).

## Images

The Images screen can inspect an OCI reference and start an import.
Poll the job until the alias appears in the local catalog and can create a VM.
A locally installed custom alias can also be registered into this host's MicroRegistry catalog; the row is stored in SQLite and survives restart.

## Production

Build the dashboard and let the API serve it.

```sh
npm run build --prefix firecrab-frontend
FIRECRAB_STATIC_ROOT="$PWD/firecrab-frontend/dist" \
  cargo run -p firecrab-api
```

Open `http://127.0.0.1:5523/`.
The host installer configures this mode.

## Check

- Confirm that ports `5523` and `8080` are not used by old processes.
- Run the API from the repository root.
- Start the helper before starting a VM.
- Forward WebSocket upgrades for `/ws` through a reverse proxy.

## Related

- [API](api.md)
- [OCI images](oci.md)
- [Networking](networking.md)
- [Troubleshooting](troubleshooting.md)
