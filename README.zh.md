<p align="center">
  <a href="https://www.rust-lang.org"><img alt="Rust" src="https://img.shields.io/badge/rust-1.94%2B-orange?logo=rust&logoColor=white"></a>
  <a href="https://codecov.io/gh/SteelCrab/firecrab"><img alt="Codecov" src="https://codecov.io/gh/SteelCrab/firecrab/branch/main/graph/badge.svg"></a>
  <a href="https://www.linux.org"><img alt="Linux" src="https://img.shields.io/badge/platform-linux-blue?logo=linux&logoColor=white"></a>
  <a href="./LICENSE"><img alt="License" src="https://img.shields.io/badge/license-Apache--2.0-blue"></a>
</p>

<h1 align="center">firecrab</h1>

<p align="center">基于 <a href="https://firecracker-microvm.github.io/">AWS Firecracker</a> 的私有 microVM 云平台</p>

<p align="center">
  <a href="./README.md">English</a> ·
  <a href="./README.ko.md">한국어</a> ·
  <a href="./README.ja.md">日本語</a>
</p>

## 简介

firecrab 是一个轻量级控制平面，用于在自建 Linux 主机上基于
[AWS Firecracker](https://firecracker-microvm.github.io/) microVM 构建私有云。Firecracker
正是驱动 AWS Lambda、Fargate 的同一套基于 KVM 的 VMM（虚拟机监控器）——启动时间仅需几百毫秒，
远快于一般虚拟机，同时仍具备硬件级虚拟化隔离。firecrab 直接运行 Firecracker，不依赖 AWS，因此
在自有服务器上也能获得同样的启动速度、强隔离性与低开销。

它面向将较重的传统虚拟机（KVM、VMware）迁移到更轻量、更快的 microVM 的场景而设计。通过网页控制台
或 REST API 创建、启动、停止和删除虚拟机，并可直接在浏览器中连接任意虚拟机的串行控制台，实时查看
启动日志并获得可交互的 shell。

由 Rust 编写的 API 服务器（`firecrab-api`）将虚拟机状态存储在 SQLite 中并直接管理 Firecracker
进程，内核/根文件系统模板会先经过哈希完整性校验才对外提供。需要 root 权限的主机网络操作
（网桥、TAP、防火墙）全部由独立的、权限分离的 helper 进程（`firecrab-net-helper`）通过 Unix
套接字处理，API 服务器本身以非特权方式运行。

## 主要功能

- 覆盖完整虚拟机生命周期的 REST API + React 控制台
- 多种启动模板（Ubuntu、Alpine）
- 基于 WebSocket 的实时串行控制台
- SQLite 状态存储，通过特权 helper 进程实现主机网络隔离

## 快速开始

### 终端会话 1 — API 服务器

负责存储 VM 状态并管理 Firecracker 进程的 Rust REST API 服务器（`http://localhost:3000`）。

```sh
cargo run -p firecrab-api
```

### 终端会话 2 — 前端

用于创建/查看 VM 并连接控制台的 React 仪表盘开发服务器（`http://localhost:8080`）。

```sh
cd firecrab-frontend
npm run dev
```

打开 `http://localhost:8080/`。完整使用说明见 [docs/web.md](docs/web.md)。

## 许可证

遵循 [Apache License, Version 2.0](./LICENSE) 许可证发布。
