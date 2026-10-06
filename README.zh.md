<p align="center">
  <a href="https://www.rust-lang.org"><img alt="Rust" src="https://img.shields.io/badge/rust-1.96%2B-orange?logo=rust&logoColor=white"></a>
  <a href="https://codecov.io/gh/SteelCrab/firecrab"><img alt="Codecov" src="https://codecov.io/gh/SteelCrab/firecrab/branch/main/graph/badge.svg"></a>
  <a href="https://www.linux.org"><img alt="Linux" src="https://img.shields.io/badge/platform-linux-blue?logo=linux&logoColor=white"></a>
  <a href="./LICENSE"><img alt="License" src="https://img.shields.io/badge/license-Apache--2.0-blue"></a>
  <a href="./CHANGELOG.md"><img alt="Changelog" src="https://img.shields.io/badge/changelog-0.3.1-informational"></a>
</p>

```text
███████ ██ ██████  ███████  ██████ ██████   █████  ██████
██      ██ ██   ██ ██      ██      ██   ██ ██   ██ ██   ██
█████   ██ ██████  █████   ██      ██████  ███████ ██████
██      ██ ██   ██ ██      ██      ██   ██ ██   ██ ██   ██
██      ██ ██   ██ ███████  ██████ ██   ██ ██   ██ ██████
```

<p align="center">可直接运行在自有服务器上的轻量 microVM 平台</p>

<p align="center">
  <a href="./README.md">English</a> ·
  <a href="./README.ko.md">한국어</a> ·
  <a href="./README.ja.md">日本語</a> ·
  <a href="./README.zh.md">中文</a> ·
  <a href="./README.id.md">Bahasa Indonesia</a>
</p>

![Firecrab 演示](assets/dashboard/firecrab-demo.gif)

## 概述

<details>
<summary>目标</summary>

在自己管理的单台 Linux 主机上创建和运行 Firecracker microVM，通过浏览器仪表盘、CLI 或 REST API 管理。适用于个人服务器、家庭实验室和开发环境。

</details>

<details>
<summary>主要功能</summary>

- **VM 管理** — 创建、启动、停止、删除 microVM，以及浏览器串行控制台。
- **镜像与磁盘** — M2Image 模板、OCI 镜像导入和 MicroStorage 磁盘位置配置。
- **网络** — MicroNetwork 子网与每个 VM 的互联网访问或隔离策略。
- **主机平台** — Linux 直接运行；macOS 和 Windows 使用 microManager（Windows 为 Preview）。

</details>

<details>
<summary>平台比较</summary>

| 核心项 | **Firecrab** | [KVM + libvirt](https://libvirt.org/) | [OpenStack](https://docs.openstack.org/nova/latest/) |
| --- | --- | --- | --- |
| 目标 | 单主机 microVM 管理 | 通用 VM 管理 | 私有云 |
| 虚拟化 | Firecracker + KVM | QEMU/KVM | 通常为 QEMU/KVM |
| 管理 | 仪表盘、CLI、REST | libvirt API、CLI；GUI 单独配置 | Horizon、CLI、REST |
| 部署 | 单台主机 | 按主机管理 | 控制器与计算服务 |

</details>

## 架构

[详细架构](public-docs/architecture.md)：各操作系统的 microManager 层、VM 启动、镜像与内核供应、来宾功能及 CLI 更新流程。

### Firecrab 概览

![Firecrab 概览](assets/architecture/firecrab-at-a-glance.en.svg)

1. 通过浏览器或 CLI 操作，选择 M2Image、MicroNetwork 与 MicroStorage 来创建 MicroVM。
2. Firecrab 验证 M2Image、准备 MicroNetwork，并在 MicroStorage 中创建 VM 专用磁盘。
3. 每台 MicroVM 启动一个 Firecracker 进程，各自使用自己的内核启动。
4. 来宾报告网络就绪后，MicroVM 进入 running 状态。

### 各平台运行方式

![各平台运行方式](assets/architecture/firecrab-runs-anywhere.en.svg)

Linux 只需要 `install.sh`。macOS 与 Windows 使用 `firecrab service install` 创建管理用 Debian VM 来运行同样的 Firecrab，均通过 `localhost:5523` 打开相同的仪表盘。

macOS 需要 Apple silicon M3 或更新型号，已在 Apple M5 上验证。Windows 因 WSL2 尚无法启动 microVM 而标记为 Preview。

Linux、Apple、Debian 标志来自 simple-icons（CC0），齿轮图标来自 Lucide（ISC）。所有标志和商标均属于其各自所有者。

## 安装

<details>
<summary>在 Linux、macOS 或 Windows 上安装</summary>

### Linux

需要 Linux x86_64 或 ARM64、可用的 `/dev/kvm`、网络，以及有 `sudo` 权限的普通用户。以该用户执行安装器，**不要在前面加 `sudo`**。如果 KVM 不可用，请先启用硬件虚拟化或嵌套虚拟化。

```sh
curl -fsSL https://github.com/SteelCrab/firecrab/releases/latest/download/install.sh | bash
```

诊断与卸载选项见[安装指南](public-docs/installation.md)。如果只在 Linux 安装远程 CLI，请使用下面包含校验和验证的 `install-cli.sh` 流程；它自动选择 GNU/musl，默认安装到 `~/.local/bin`。

### macOS

需要 Apple silicon、macOS 15 以上和运行时支持嵌套虚拟化的主机（M3 以上；完整验证为 M5/macOS 26.6.2）。先验证安装器校验和，再安装 CLI 与 helper。

```sh
curl -fLO https://github.com/SteelCrab/firecrab/releases/latest/download/install-cli.sh
curl -fLO https://github.com/SteelCrab/firecrab/releases/latest/download/SHA256SUMS
grep ' install-cli.sh$' SHA256SUMS > install-cli.sh.sha256
if command -v sha256sum >/dev/null; then
  sha256sum -c install-cli.sh.sha256
else
  shasum -a 256 -c install-cli.sh.sha256
fi && sh install-cli.sh
```

然后检查主机能力并安装 Debian 管理环境：

```sh
firecrab service doctor
firecrab service install
```

单独安装 CLI 不会创建管理 VM。microManager 使用 `Virtualization.framework`、独立的持久数据磁盘和常驻 launchd 服务。详见 [macOS 指南](public-docs/micromanager-macos.md)。

### Windows

需要 x86_64 或 ARM64 Windows、Microsoft Store WSL2 和嵌套 KVM。在普通 PowerShell 中下载并验证 CLI 安装器：

```powershell
Invoke-WebRequest https://github.com/SteelCrab/firecrab/releases/latest/download/install-cli.ps1 -OutFile install-cli.ps1
Invoke-WebRequest https://github.com/SteelCrab/firecrab/releases/latest/download/SHA256SUMS -OutFile SHA256SUMS
$expected = ((Get-Content SHA256SUMS | Where-Object { $_ -match ' install-cli\.ps1$' }) -split '\s+')[0]
if ((Get-FileHash install-cli.ps1 -Algorithm SHA256).Hash -ne $expected) { throw 'installer checksum mismatch' }
& ./install-cli.ps1
```

如果 `firecrab` 尚未加入 `PATH`，请打开新的 PowerShell，然后检查能力并安装。ARM64 安装尚未完成端到端验证。详见 [Windows 指南](public-docs/micromanager-windows.md)。

```powershell
firecrab service doctor
firecrab service install
```

**Windows 限制：** 当前固定的 v0.2.2 来宾在标准 WSL2 中支持 API、镜像与网络，但无法启动 MicroVM。需要包含 net-helper 修复的新来宾发行版；本地 nginx VM 也受影响。Windows CLI 仍可管理受支持的远程 Linux 主机。

</details>

## 运行

<details>
<summary>在 Linux、macOS 或 Windows 上启动与停止</summary>

### Linux

安装器会启动两个 systemd 服务。之后使用以下命令启动、查看或停止：

```sh
sudo systemctl start firecrab-helper firecrab-api
systemctl status firecrab-helper firecrab-api
sudo systemctl stop firecrab-api firecrab-helper
```

### macOS

microManager 启动常驻管理 VM 和 localhost API 隧道：

```sh
firecrab service start
firecrab service status
firecrab service debug --logs --tail 100
firecrab service stop
```

### Windows

microManager 通过用户级计划任务保持 WSL2 管理发行版运行：

```powershell
firecrab service start
firecrab service status
firecrab service debug --logs --tail 100
firecrab service stop
```

**Windows 限制：** 当前固定的 v0.2.2 来宾在标准 WSL2 中支持 API、镜像与网络，但无法启动 MicroVM。需要包含 net-helper 修复的新来宾发行版；本地 nginx VM 也受影响。Windows CLI 仍可管理受支持的远程 Linux 主机。

状态正常时打开 `http://127.0.0.1:5523/`。创建 MicroNetwork，选择已安装镜像，创建并启动 VM，待状态为 `running` 后打开终端。远程主机请配置 [CLI 主机配置](public-docs/firecrab-cli.md#host-profiles)。

</details>

## 从源码运行

<details>
<summary>Linux API 与仪表盘 / macOS 与 Windows CLI</summary>

在仓库根目录使用指定的 [Rust 工具链](rust-toolchain.toml)、Node.js 22 以上和 npm。实际运行 VM 还需要 Linux KVM 及[主机前置条件](public-docs/installation.md)。

### Linux

使用三个终端。helper 以特权运行，API 以普通用户运行。本地数据路径以仓库根目录为基准。

```sh
# 1
cargo build -p firecrab-helper --locked
sudo -u root -g "$(id -gn)" FIRECRAB_NET_HELPER_ALLOWED_UID="$(id -u)" \
  ./target/debug/firecrab-helper

# 2
cargo run -p firecrab-api --locked

# 3
npm ci --prefix firecrab-frontend
npm run dev --prefix firecrab-frontend
# http://localhost:8080/
```

让 API 提供已构建的仪表盘时，先停止开发 API，再运行：

```sh
npm run build --prefix firecrab-frontend
FIRECRAB_STATIC_ROOT="$PWD/firecrab-frontend/dist" cargo run -p firecrab-api --locked
# http://127.0.0.1:5523/
```

### macOS

构建当前源码的 CLI 和已签名原生 helper，再安装管理服务；已安装时使用 `service start`。

```sh
cargo build -p firecrab-cli --locked
scripts/build-micromanager-macos.sh target/debug/firecrab-micromanager-macos
./target/debug/firecrab service install
./target/debug/firecrab service status
```

### Windows

在 PowerShell 中构建当前源码的 CLI，再安装管理服务；已安装时使用 `service start`。

```powershell
cargo build -p firecrab-cli --locked
.\target\debug\firecrab.exe service install
.\target\debug\firecrab.exe service status
```

macOS 与 Windows 上的这些命令运行源码构建的 CLI/helper 和已安装的来宾 API，**不会**在 Debian 内重新构建修改后的 `firecrab-api` 源码。完整 API/net-helper 运行时开发请在 Linux 进行；管理服务开发详见上述平台指南。

</details>

## 测试

<details>
<summary>检查、覆盖率与浏览器 E2E</summary>

在仓库根目录执行基本检查：

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --locked
npm ci --prefix firecrab-frontend
npm run lint --prefix firecrab-frontend
npm run build --prefix firecrab-frontend
python3 scripts/check-doc-links.py
python3 scripts/check-changelog.py
```

可选本地覆盖率（需要 `cargo-llvm-cov`）：

```sh
cargo llvm-cov --workspace --locked --lcov --output-path lcov.info
```

跳过来宾启动的浏览器 E2E：

```sh
npm ci --prefix firecrab-e2e
npm run install-browsers --prefix firecrab-e2e
FIRECRAB_E2E_SKIP_GUEST_BOOT=1 npm test --prefix firecrab-e2e
```

PowerShell 中在 `npm test --prefix firecrab-e2e` 前设置 `$env:FIRECRAB_E2E_SKIP_GUEST_BOOT="1"`。这会跳过来宾启动，不验证 KVM 或 nginx HTTP。实际来宾运行及全部安装检查详见 [TEST.md](public-docs/TEST.md)、[韩语检查清单](public-docs/TEST.ko.md)和 [E2E 指南](firecrab-e2e/README.md)。

</details>

## 使用方法：nginx

<details>
<summary>导入 nginx → 创建 microVM → HTTP 访问</summary>

使用 API 与 CLI 正在运行的受支持 Linux 主机或 macOS microManager。以下命令适用于 Linux/macOS 的 POSIX shell。仪表盘中也可按 Images → OCI Import、Networks、MicroVM 完成同样操作。

1. 检查并导入镜像。重复查询状态，直到任务成功且 `nginx-1.27` 已安装：

```sh
firecrab image inspect nginx:1.27
firecrab image import nginx:1.27
firecrab image import-status nginx-1.27
```

2. 选择不重叠的子网创建网络。将下面的 `NETWORK_ID` 替换为第一条命令返回的 UUID：

```sh
firecrab network create --name nginx-net --subnet-cidr 172.31.20.0/24
firecrab vm create --name nginx-demo --template nginx-1.27 --network NETWORK_ID
```

3. 将 `VM_ID` 替换为创建 VM 返回的 UUID，启动后等待列表中的状态变为 `running`：

```sh
firecrab vm start VM_ID
firecrab vm list
```

4. 通过 REST API 配置 TCP 主机端口 `8081` → 来宾端口 `80`，再检查 nginx 响应。此 PUT 会替换 VM 的整个端口转发列表；请在请求中保留其他需要的规则。

```sh
curl -fsS -X PUT http://127.0.0.1:5523/api/vms/VM_ID/port-forwards \
  -H 'Content-Type: application/json' \
  -d '{"portForwards":[{"hostPort":8081,"guestPort":80,"protocol":"tcp"}]}'
```

Linux 上从另一台机器访问 `http://FIRECRAB_HOST_IP:8081/`；DNAT 不提供主机自身的 loopback 访问。macOS 使用 TCP 转发的 `http://127.0.0.1:8081/`。远程访问时还需在主机与路由器防火墙中允许 8081 端口。

```sh
curl -I http://FIRECRAB_HOST_IP:8081/
# macOS
curl -I http://127.0.0.1:8081/
```

OCI import 创建以 `/etc/firecrab/busybox` 为 PID 1 的可启动 rootfs，并将 nginx 入口点作为服务运行。`EXPOSE 80` 不会自动创建主机端口转发。详见 [OCI 镜像](public-docs/oci.md)与[网络指南](public-docs/networking.md)。

停止示例 VM：

```sh
firecrab vm stop VM_ID
```

<details>
<summary>仪表盘界面说明</summary>

![firecrab M2 仪表盘演示](assets/dashboard/firecrab-m2.gif)

仪表盘通过左侧导航将日常操作分为 **MicroVM**、每台 VM 的 **终端**、**网络** 和
**镜像**。

### MicroVM

在表单中选择名称、镜像、CPU、RAM、磁盘、存储位置、MicroNetwork 和出站策略后创建 VM。下方列表
每三秒刷新状态、镜像、资源和 ID；运行中的 VM 会显示 **终端** 和 **停止** 操作。选择 VM 名称可查看
启动进度、日志、网络、存储及其他详情。

![MicroVM 创建与列表](assets/dashboard/microvm.png)

### 终端

运行中的 VM 可通过 **终端** 在独立标签页打开浏览器串口控制台。它实时显示启动输出与登录提示并接受
命令。工具栏可调整显示设置、复制或保存控制台日志，以及切换到仅终端视图；下方各面板显示 VM 的
常规信息、规格、网络和存储。

![VM 浏览器串口终端](assets/dashboard/terminal.png)

### 网络

可使用名称、子网 CIDR 和互联网策略创建 **MicroNetwork**。列表显示每个网络的网关、互联网状态和
ID；可通过 **阻止互联网/启用互联网** 改变整个网络经 NAT 的出站访问，或删除该网络。选择一行可查看
子网地址使用情况、bridge/TAP、NAT、防火墙和成员 VM 的详情。

![MicroNetwork 创建与列表](assets/dashboard/networks.png)

### 镜像

**M2Image** 列表显示每个镜像的大小及 `软件包已就绪`、`已安装` 等状态。选择一行可查看其别名、
版本、最小磁盘、rootfs 大小、状态以及正在使用该镜像的 VM。`…` 菜单会根据状态提供软件包安装、
引导或删除操作。只有已安装的镜像可以用于创建 VM。

同一页面还可以检查 OCI 引用（`nginx:1.27`）是否匹配本机架构，并将其导入为模板。
导入在后台进行，页面会显示进度、错误以及注册后的别名。

![M2Image 列表](assets/dashboard/images.png)

请求格式、生命周期语义和错误 envelope 见 [API 指南](public-docs/api.md)。镜像包与浏览器引导
流程见[镜像指南](public-docs/images.md)。OCI 检查与导入见
[OCI 镜像指南](public-docs/oci.md)。

</details>

</details>

## 文档与贡献

默认文档语言为英语；顶部链接提供韩语、日语、中文与印度尼西亚语 README。仪表盘支持英语与韩语。

- [public-docs/](public-docs/README.md): 安装、API、运维与故障排查
- [CONTRIBUTING.md](CONTRIBUTING.md): 维护者说明、开发环境与 PR 检查

采用 [Apache License, Version 2.0](LICENSE) 许可证。
