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

It checks tools, KVM, systemd, images, and firewall state.

## Install

```sh
curl -fsSL https://github.com/SteelCrab/firecrab/releases/latest/download/install.sh | bash
```

Pin a version by replacing `latest` with the tag, for example `v0.1.0`.

To patch an installed host with binaries you built yourself:

```sh
git clone https://github.com/SteelCrab/firecrab.git
cd firecrab
cargo build --release -p firecrab-api -p firecrab-net-helper
./install.sh --bin-dir target/release --dashboard-dir firecrab-frontend/dist
```

Open the dashboard after the services start.

```text
http://127.0.0.1:3000/
```

The default install builds the Alpine image.

## Common options

| Option | Result |
| --- | --- |
| `--check` | Report readiness without changes |
| `--doctor` | Run runtime diagnostics |
| `--no-deps` | Do not install missing tools |
| `--no-images` | Skip guest image creation |
| `--with-ubuntu-image` | Also build Ubuntu 26.04 |
| `--with-rocky-image` | Also build Rocky Linux 9.8 without Docker |
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
firecrab-doctor
curl -s http://127.0.0.1:3000/api/vms
curl -s http://127.0.0.1:3000/api/micro-networks
```

A new host has no MicroNetwork.
Create one before creating a VM.

## Upgrade

Run the installer again.
It replaces binaries from the latest release, or from `--bin-dir` when you pass one.
The installer keeps the database, VM disks, and `api.env`.

## Related

- [Operations](operations.md)
- [Networking](networking.md)
- [Images](images.md)
- [Troubleshooting](troubleshooting.md)
