<p align="center">
  <a href="https://www.rust-lang.org"><img alt="Rust" src="https://img.shields.io/badge/rust-1.94%2B-orange?logo=rust&logoColor=white"></a>
  <a href="https://codecov.io/gh/SteelCrab/firecrab"><img alt="Codecov" src="https://codecov.io/gh/SteelCrab/firecrab/branch/main/graph/badge.svg"></a>
  <a href="https://www.linux.org"><img alt="Linux" src="https://img.shields.io/badge/platform-linux-blue?logo=linux&logoColor=white"></a>
  <a href="./LICENSE"><img alt="License" src="https://img.shields.io/badge/license-Apache--2.0-blue"></a>
</p>

<h1 align="center">firecrab</h1>

<p align="center">A private, self-hosted microVM cloud built on <a href="https://firecracker-microvm.github.io/">AWS Firecracker</a>.</p>

<p align="center">
  <a href="./README.ko.md">한국어</a> ·
  <a href="./README.zh.md">中文</a> ·
  <a href="./README.ja.md">日本語</a>
</p>

## What is it?

firecrab is a lightweight control plane for building a private cloud on your own Linux
host, powered by [AWS Firecracker](https://firecracker-microvm.github.io/) microVMs.
Firecracker is the same KVM-based VMM that runs AWS Lambda and Fargate: it boots in a few
hundred milliseconds — far faster than a general-purpose VM — while still giving you
full hardware-virtualization isolation. firecrab runs Firecracker directly, with no
dependency on AWS, so you get the same speed, isolation, and low overhead on your own
servers.

It's aimed at migrating heavier legacy VMs (KVM, VMware) to lighter, faster microVMs.
Create, start, stop, and delete VMs from a web dashboard or a REST API, and attach to
any VM's serial console straight from the browser to watch it boot and get a live shell.

The API server (`firecrab-api`, written in Rust) stores VM state in SQLite and manages
Firecracker processes directly, serving kernel/rootfs templates only after verifying
their integrity by hash. Host network operations that need root — bridge, TAP, firewall
— are handled exclusively by a separate, privilege-separated helper process
(`firecrab-net-helper`) over a Unix socket, so the API server itself runs unprivileged.

## Features

- REST API + React dashboard for the full VM lifecycle
- Multiple boot templates (Ubuntu, Alpine)
- Live serial console over WebSocket
- SQLite-backed state, host network isolation via a privileged helper process

## Install (recommended)

On any Linux host with KVM and network access:

```sh
git clone https://github.com/SteelCrab/firecrab && cd firecrab
sudo ./install.sh
```

That is the whole thing. The installer finds what is missing and installs it —
packages via whichever manager the host has (apt/dnf/zypper/pacman/apk), Firecracker,
the Rust toolchain, and a guest image — then creates the service account, installs two
systemd daemons, and serves the dashboard at `http://127.0.0.1:3000/`. Re-running it is
safe; it repairs instead of duplicating.

```sh
./install.sh --check       # see what is missing first (no root, changes nothing)
sudo ./install.sh --uninstall   # remove the daemons; VM data is kept unless --purge
```

KVM is the one thing it cannot install for you — if `/dev/kvm` is missing, enable
virtualization in the BIOS (or nested virtualization, if this host is itself a VM).
Full usage — options, layout, upgrades, uninstall, troubleshooting — is in
[docs/install.md](docs/install.md).

## Run from source (development)

Three terminals, no installation: the privileged network helper, the API, and the Vite
dev server whose proxy lets the browser reach the API.

```sh
# 1 — privileged network helper (bridge, TAP, firewall, DHCP)
cargo build -p firecrab-net-helper
sudo -u root -g "$(id -gn)" FIRECRAB_NET_HELPER_ALLOWED_UID="$(id -u)" \
     ./target/debug/firecrab-net-helper

# 2 — API server (run from the repo root: paths are relative to the working directory)
cargo run -p firecrab-api

# 3 — dashboard dev server
cd firecrab-frontend && npm run dev
```

Open `http://localhost:8080/`. See [docs/web.md](docs/web.md) for the full walkthrough.

To serve the built dashboard from the API instead of running the third terminal, point
it at the build output:

```sh
cd firecrab-frontend && npm run build && cd ..
FIRECRAB_STATIC_ROOT="$PWD/firecrab-frontend/dist" cargo run -p firecrab-api
# http://localhost:3000/
```

## License

Licensed under the [Apache License, Version 2.0](./LICENSE).
