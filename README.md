<p align="center">
  <a href="https://www.rust-lang.org"><img alt="Rust" src="https://img.shields.io/badge/rust-1.94%2B-orange?logo=rust&logoColor=white"></a>
  <a href="https://codecov.io/gh/SteelCrab/firecrab"><img alt="Codecov" src="https://codecov.io/gh/SteelCrab/firecrab/branch/main/graph/badge.svg"></a>
  <a href="https://www.linux.org"><img alt="Linux" src="https://img.shields.io/badge/platform-linux-blue?logo=linux&logoColor=white"></a>
  <a href="./LICENSE"><img alt="License" src="https://img.shields.io/badge/license-Apache--2.0-blue"></a>
</p>

<h1 align="center">firecrab</h1>

<p align="center">A lightweight microVM platform for your own server.</p>

<p align="center">
  <a href="./README.ko.md">한국어</a> ·
  <a href="./README.zh.md">中文</a> ·
  <a href="./README.ja.md">日本語</a>
</p>

firecrab runs and manages isolated [Firecracker](https://firecracker-microvm.github.io/)
microVMs on a Linux host you control. It combines a Rust API, a browser dashboard,
and two small system services so that creating a VM also means choosing its image,
network, disk location, and outbound-access policy.

It is intended for a private, single-host microVM environment: a practical way to
operate workloads that need stronger isolation than containers without introducing a
full cloud control plane. It is not a hosted service or a multi-host scheduler.

## Core capabilities

- **Run microVMs:** create, inspect, edit inactive VMs, start, stop, delete, and use
  each VM's browser-based serial console.
- **Choose an isolated network:** create explicit **MicroNetworks**; each VM belongs
  to one network and receives a persistent IPv4, MAC, and hostname. Networks are
  isolated from one another, with per-VM internet or isolated egress.
- **Manage images and disks:** install or remove templates, bootstrap supported
  distributions in a temporary builder VM, and place VM disks on configured storage
  roots or registered **MicroStorage** pools.
- **See what is happening:** inspect startup progress, console logs, and host status
  in the dashboard, available in English and Korean.
- **Keep host privileges small:** the API runs unprivileged; the separate
  `firecrab-net-helper` owns only the capabilities needed for host networking.

## Architecture

```text
Browser dashboard / REST clients
              │ HTTP + WebSocket
              ▼
  firecrab-api (Rust, SQLite, Firecracker process manager)
       │                         │
       │ Unix socket             └── Firecracker → one microVM per process
       ▼
firecrab-net-helper (privileged, capability-bounded)
       └── bridges · TAPs · nftables · dnsmasq
```

The API verifies template artifacts before using them and serves the built dashboard
itself in an installed deployment. See the detailed [architecture](docs/10-overview/architecture.md).

## Install on a Linux host

Requirements are a Linux host with `/dev/kvm`, network access, and a user allowed to
run `sudo`. Run the installer as that regular user — do **not** prefix the script with
`sudo`. It keeps source builds and user-level tools owned by the invoking user, and
uses `sudo` only for the individual package, systemd, and host-setup operations that
need it.

```sh
git clone https://github.com/SteelCrab/firecrab.git
cd firecrab
./install.sh
```

Useful installer modes:

```sh
./install.sh --check                 # report prerequisites and planned changes
./install.sh --doctor                # diagnose KVM, firewall, socket, and host setup
./install.sh --with-ubuntu-image
./install.sh --with-rocky-image
./install.sh --uninstall         # retain data by default
./install.sh --uninstall --purge # also remove /var/lib/firecrab
```

The default install builds the dashboard and an Alpine guest image. KVM cannot be
enabled by the script: if `/dev/kvm` is absent, enable hardware virtualization (or
nested virtualization) first. For every option, install path, upgrade detail, and
troubleshooting step, read the [installation guide](docs/20-guides/install.md).

## Quick start

Open `http://127.0.0.1:3000/` after installation, then:

1. Create a **MicroNetwork**.
2. Choose an installed image and create a VM in that network.
3. Start the VM, wait for it to become `running`, then open **Terminal**.

Creating the network first is intentional: firecrab has no hidden default subnet, so
every VM is placed in a network chosen by the operator.

For API request formats, lifecycle semantics, and error envelopes, see the
[API guide](docs/20-guides/api.md). For image packages and browser-driven bootstrap,
see the [image guide](docs/20-guides/m2image-builder.md).

## Develop from source

Use three terminals: the network helper, API, and Vite dashboard. Run the API from
the repository root because its local data paths are relative to the working directory.

```sh
# Terminal 1 — privileged network operations
cargo build -p firecrab-net-helper
sudo -u root -g "$(id -gn)" FIRECRAB_NET_HELPER_ALLOWED_UID="$(id -u)" \
  ./target/debug/firecrab-net-helper

# Terminal 2 — API and Firecracker manager
cargo run -p firecrab-api

# Terminal 3 — dashboard at http://localhost:8080/
npm install --prefix firecrab-frontend
npm run dev --prefix firecrab-frontend
```

For a production-like local run, build the dashboard and let the API serve it:

```sh
npm run build --prefix firecrab-frontend
FIRECRAB_STATIC_ROOT="$PWD/firecrab-frontend/dist" cargo run -p firecrab-api
# http://127.0.0.1:3000/
```

Run the Rust test suite with:

```sh
cargo test --workspace
```

More development notes and browser workflow details are in the [web dashboard guide](docs/20-guides/web.md).

## Documentation

The Korean documentation vault in [`docs/`](docs/HOME.md) contains the architecture,
guides, API contract, verification procedures, and bug records. It can also be opened
directly as an Obsidian vault.

## License

Licensed under the [Apache License, Version 2.0](./LICENSE).
