# Installation

`install.sh` installs firecrab on one Linux host.
It downloads the host bundle for this architecture (`x86_64` or `aarch64`) and libc (`gnu` / glibc, or `musl`).
glibc hosts (Debian, Fedora, Arch, openSUSE, Ubuntu) get the gnu bundle.
musl hosts (Alpine) get the musl bundle.
Pass `--libc gnu` or `--libc musl` to override.

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

`firecrab` (the CLI) is a single static binary.
Install it on a machine that already runs `firecrab-api` elsewhere, or on the host itself alongside an existing full install.

### From a GitHub Release

Download the host bundle for the target architecture and libc, then extract only the `firecrab` binary.

```sh
# x86_64 glibc host (Debian, Ubuntu, Fedora, Arch, openSUSE)
curl -fsSL -o firecrab-host.tar.gz \
  https://github.com/SteelCrab/firecrab/releases/latest/download/firecrab-host-x86_64-gnu.tar.gz
tar -xzf firecrab-host.tar.gz firecrab
sudo install -m 755 firecrab /usr/local/bin/firecrab
```

Replace `x86_64-gnu` with the correct variant for your machine:

| Architecture | libc | Filename suffix |
| --- | --- | --- |
| x86\_64 | glibc (Debian, Ubuntu, Fedora, …) | `x86_64-gnu` |
| x86\_64 | musl (Alpine) | `x86_64-musl` |
| aarch64 | glibc | `aarch64-gnu` |
| aarch64 | musl | `aarch64-musl` |

Pin a version by replacing `latest` with a tag, for example `v0.1.0`.

### From source

```sh
git clone https://github.com/SteelCrab/firecrab.git
cd firecrab
cargo build --release --locked -p firecrab-cli
sudo install -m 755 target/release/firecrab /usr/local/bin/firecrab
```

Requires the repository Rust toolchain (`rust-toolchain.toml`).

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
