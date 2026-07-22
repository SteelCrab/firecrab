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

## Quick start

### Terminal session 1 — API server

The Rust REST API server that stores VM state and manages Firecracker processes (`http://localhost:3000`).

```sh
cargo run -p firecrab-api
```

### Terminal session 2 — Frontend

The React dashboard dev server for creating/viewing VMs and attaching to their console (`http://localhost:8080`).

```sh
cd firecrab-frontend
npm run dev
```

Open `http://localhost:8080/`. See [docs/web.md](docs/web.md) for the full walkthrough.

## License

Licensed under the [Apache License, Version 2.0](./LICENSE).
