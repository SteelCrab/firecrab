# Web dashboard

The dashboard is a React and TypeScript application built with Vite.
It manages VMs, terminals, networks, storage, images, and host status.

## Development run

Use three terminals from the repository root.

Terminal 1 starts the privileged network helper.

```sh
./scripts/dev-net-helper.sh
```

Terminal 2 starts the API.

```sh
cargo run -p firecrab-api
```

Terminal 3 starts Vite.

```sh
npm install --prefix firecrab-frontend
npm run dev --prefix firecrab-frontend
```

Open `http://localhost:8080/`.
Use `localhost` because the development origin allowlist is exact.

Vite proxies `/api` and `/ws` to `127.0.0.1:3000`.

## Main screens

| Screen | Purpose |
| --- | --- |
| MicroVM | Create, list, start, stop, edit, and delete VMs |
| Terminal | Open a VM serial console and export logs |
| Networks | Create and inspect MicroNetworks |
| Storage | Register and inspect MicroStorage pools |
| Images | Download, install, bootstrap, and delete M2Images |
| Host | Show host capacity and network status |

The VM list polls every three seconds.
Repeated connection failures slow polling to 15 seconds.

## VM form

The form requires a name, image, CPU, RAM, disk size, and MicroNetwork.
Storage and egress policy can also be selected.

Only installed images appear as usable templates.
Only registered networks and storage roots can be selected.

Server validation messages are shown beside the matching field.

## VM details

Select a VM name to open its details.
The view shows resources, network, storage, start progress, and logs.

Resources can be changed only while the VM is inactive.
Changes take effect on the next start.

Disk size can grow but cannot shrink.

## Terminal

The terminal uses `/ws/vms/{id}/console`.
It is available only while the VM has an active console.

The serial stream includes boot output and the login prompt.
The toolbar can copy or save the captured log.

## Test M2Image downloads locally

Build packages and serve them from another terminal.

```sh
./scripts/build-m2images.sh
python3 -m http.server --bind 127.0.0.1 --directory dist/m2images 8765
```

Start the API with an empty image root and package base URL.

```sh
FIRECRAB_IMAGE_ROOT=/tmp/firecrab-m2image-test \
FIRECRAB_IMAGE_BASE_URL=http://127.0.0.1:8765 \
cargo run -p firecrab-api
```

## Production serving

The API can serve the built dashboard.

```sh
npm run build --prefix firecrab-frontend
FIRECRAB_STATIC_ROOT="$PWD/firecrab-frontend/dist" cargo run -p firecrab-api
```

Open `http://127.0.0.1:3000/`.
The installer configures this mode automatically.

## Common development issues

- Run the API from the repository root.
- Stop old API or Vite processes before restarting.
- Check ports `3000` and `8080` when a process cannot bind.
- Start the network helper before starting a VM.
- Update matching TypeScript bindings when Rust API types change.

See [troubleshooting](troubleshooting.md) for symptom-based checks.
