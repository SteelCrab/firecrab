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
| MicroVM | Create and manage VMs |
| Terminal | Use the serial console |
| Networks | Manage MicroNetworks |
| Storage | Manage MicroStorage pools |
| Images | Install and build M2Images |
| Host | Show host health and capacity |

## VM workflow

1. Create a MicroNetwork.
2. Choose an installed image.
3. Set CPU, RAM, disk, storage, and egress.
4. Create the VM.
5. Start it.
6. Open Terminal after it reaches `running`.

The list refreshes every three seconds.
The detail view shows start progress and logs.

While a VM is `running`, the list, detail view, and terminal page show host
Firecracker process CPU percent and RSS next to the allocated CPU and RAM.
Detail and terminal also draw short sparklines from recent samples.
Those values are host process usage, not guest free memory.

Resource changes are allowed only while the VM is inactive.
Disk size can grow but cannot shrink.

## Production

Build the dashboard and let the API serve it.

```sh
npm run build --prefix firecrab-frontend
FIRECRAB_STATIC_ROOT="$PWD/firecrab-frontend/dist" \
  cargo run -p firecrab-api
```

Open `http://127.0.0.1:3000/`.
The host installer configures this mode.

## Check

- Confirm that ports `3000` and `8080` are not used by old processes.
- Run the API from the repository root.
- Start the helper before starting a VM.
- Forward WebSocket upgrades for `/ws` through a reverse proxy.

## Related

- [API](api.md)
- [Networking](networking.md)
- [Troubleshooting](troubleshooting.md)
