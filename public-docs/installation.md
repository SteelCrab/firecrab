# Installation

`install.sh` installs firecrab on one Linux host.
It downloads the host bundle for this architecture (`x86_64` or `aarch64`) and libc (`gnu` / glibc, or `musl`).
glibc hosts (Debian, Fedora, Arch, openSUSE, Ubuntu) get the gnu bundle.
musl hosts (Alpine) get the musl bundle.
Pass `--libc gnu` or `--libc musl` to override.

## Contents

| Section | Content |
| --- | --- |
| [Requirements](#requirements) | Linux host prerequisites |
| [Check the host](#check-the-host) | Read-only readiness check |
| [Install](#install) | Full Linux host installation |
| [CLI-only installation](#cli-only-installation) | Linux, macOS, and Windows remote client |
| [Common options](#common-options) | Full host installer flags |
| [Default paths](#default-paths) | Full host filesystem layout |
| [Check the result](#check-the-result) | Service and API verification |
| [Upgrade](#upgrade) | Full host upgrade |
| [Related](#related) | Other documents |

## Requirements

- Linux with systemd
- Hardware virtualization and `/dev/kvm`
- A normal user with `sudo` access
- Network access
- `apt-get`, `dnf`, `zypper`, `pacman`, or `apk`

Do not run the whole script with `sudo`.
The script asks for privilege only when needed.

## Check the host

Run the read-only check first.

```sh
./install.sh --check
```

It checks tools, KVM, systemd, and firewall state.

## Install

```sh
curl -fsSL https://github.com/SteelCrab/firecrab/releases/latest/download/install.sh | bash
```

The installer prompts for your sudo password on the terminal.

Pin a version by replacing `latest` with the tag, for example `v0.1.0`.

To patch an installed host with files built from a checkout, choose one of the following paths.

### Prepared local payload

Use the repository script to build the required host binaries and the dashboard:

```sh
git clone https://github.com/SteelCrab/firecrab.git
cd firecrab
./scripts/ci-prepare-install-payload.sh
./install.sh --bin-dir target/release
```

The preparation script builds `firecrab-api`, `firecrab-net-helper`, and the `firecrab` CLI,
then creates `firecrab-frontend/dist`.
The installer detects that dashboard directory automatically.
This local build path requires the repository Rust toolchain, Node.js, and npm.

### Manual local payload

Build the same payload one component at a time when inspecting or changing individual build steps:

```sh
git clone https://github.com/SteelCrab/firecrab.git
cd firecrab
cargo build --release --locked \
  -p firecrab-api \
  -p firecrab-net-helper \
  -p firecrab-cli
npm ci --prefix firecrab-frontend
npm run build --prefix firecrab-frontend
./install.sh \
  --bin-dir target/release \
  --dashboard-dir firecrab-frontend/dist
```

The manual path must produce `firecrab-api`, `firecrab-net-helper`, `firecrab`, and
`firecrab-frontend/dist/index.html` before installation.

Open the dashboard after the services start.

```text
http://127.0.0.1:5523/
```

The install never installs a guest image.
Import one afterwards with [OCI import](oci.md) or the dashboard Images page.

## CLI-only installation

`firecrab` is a standalone remote client for Linux, macOS, and Windows.
The CLI-only installers require no root access, Rust toolchain, Firecracker, or local host services.
After installation, save and verify a remote endpoint with the [CLI host profiles](firecrab-cli.md#host-profiles).

### Linux and macOS

The POSIX installer detects Linux or macOS and x86_64 or ARM64, verifies `SHA256SUMS`, and installs to `~/.local/bin`.

```sh
curl -fsSL https://github.com/SteelCrab/firecrab/releases/latest/download/install-cli.sh | sh
```

Pin a version or choose another user-writable directory when needed.

```sh
curl -fsSL https://github.com/SteelCrab/firecrab/releases/latest/download/install-cli.sh \
  | sh -s -- --version v0.1.3 --install-dir "$HOME/bin"
```

The installer prints an `export PATH=...` line when the destination is not already on `PATH`.
Pass `--version TAG` to pin the client archive; changing only the installer script URL does not change its default `latest` archive selection.
Use an installation directory controlled by the current user, not a shared writable directory.

### Windows

Run the user-level PowerShell installer.

```powershell
irm https://github.com/SteelCrab/firecrab/releases/latest/download/install-cli.ps1 | iex
```

It installs `firecrab.exe` under `%LOCALAPPDATA%\Programs\firecrab\bin` and adds that directory to the user `PATH`.
Open a new terminal after the first installation.

Pin a client archive by passing `-Version` to the downloaded installer.

```powershell
$installer = irm https://github.com/SteelCrab/firecrab/releases/latest/download/install-cli.ps1
& ([scriptblock]::Create($installer)) -Version v0.1.3
```

Use an installation directory controlled by the current user, not a shared writable directory.

### Release assets

The installers select one of these standalone archives.

| Client | Asset |
| --- | --- |
| Linux x86_64 | `firecrab-cli-x86_64-linux.tar.gz` |
| Linux ARM64 | `firecrab-cli-aarch64-linux.tar.gz` |
| macOS Intel | `firecrab-cli-x86_64-macos.tar.gz` |
| macOS Apple silicon | `firecrab-cli-aarch64-macos.tar.gz` |
| Windows x86_64 | `firecrab-cli-x86_64-windows.zip` |

Archive URLs may be pinned by replacing `latest` with a tag, for example `v0.1.0`.
When using either installer script, pass `--version` or `-Version` as shown above.

### From source

```sh
git clone https://github.com/SteelCrab/firecrab.git
cd firecrab
cargo build --release --locked -p firecrab-cli
```

Requires the repository Rust toolchain (`rust-toolchain.toml`).
Copy `target/release/firecrab` or `target\release\firecrab.exe` into a directory on the user `PATH`.

### On a host with a full install

`install.sh` with `--no-frontend --no-deps` updates only the service binaries and skips the dashboard and package installs.
Pass `--bin-dir` when installing from a local build:

```sh
# update all service binaries from the latest release, no dashboard refresh
curl -fsSL https://github.com/SteelCrab/firecrab/releases/latest/download/install.sh \
  | bash -s -- --no-frontend --no-deps

# or from a local build
./install.sh --bin-dir target/release --no-frontend --no-deps
```

## Common options

| Option | Result |
| --- | --- |
| `--check` | Report readiness without changes |
| `--doctor` | Run runtime diagnostics |
| `--no-deps` | Do not install missing tools |
| `--no-frontend` | Skip the dashboard |
| `--version VER` | Use that GitHub Release tag |
| `--libc gnu` or `--libc musl` | Force glibc or musl instead of auto-detect |
| `--bin-dir DIR` | Install local binaries instead of the release |
| `--dashboard-dir DIR` | Install this built dashboard |
| `--uninstall` | Remove services but keep data |
| `--uninstall --purge` | Also delete VM data |

`--purge` is destructive.

## Default paths

| Path | Content |
| --- | --- |
| `/usr/local/lib/firecrab/` | Service binaries |
| `/usr/local/share/firecrab/dashboard/` | Built dashboard |
| `/var/lib/firecrab/data/` | Database and VM artifacts |
| `/var/lib/firecrab/images/` | Kernels and root filesystems |
| `/etc/firecrab/api.env` | API settings |
| `/run/firecrab/net-helper.sock` | Helper socket |

Use `PREFIX`, `DATADIR`, `CONFDIR`, and `UNITDIR` to change paths.

```sh
DATADIR=/srv/firecrab PREFIX=/opt ./install.sh
```

## Check the result

```sh
systemctl status firecrab-net-helper firecrab-api
firecrab doctor
curl -s http://127.0.0.1:5523/api/vms
curl -s http://127.0.0.1:5523/api/micro-networks
```

A new host has no MicroNetwork.
Create one before creating a VM.

## Upgrade

Run the installer again.
It replaces binaries from the latest release, or from `--bin-dir` when you pass one.
The installer keeps the database, VM disks, and `api.env`.

## Related

- [Operations](operations.md)
- [firecrab CLI](firecrab-cli.md)
- [Networking](networking.md)
- [Images](images.md)
- [Troubleshooting](troubleshooting.md)
