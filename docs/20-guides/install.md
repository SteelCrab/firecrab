# Installation

`install.sh` installs firecrab on one Linux host.
It builds the services and dashboard, installs systemd units, and prepares an image.

## Requirements

- A Linux host with systemd
- Hardware virtualization and `/dev/kvm`
- A normal user with `sudo` access
- Network access for packages and source inputs
- A supported package manager

Supported package managers are `apt-get`, `dnf`, `zypper`, `pacman`, and `apk`.

Do not run the whole installer with `sudo`.
The script uses `sudo` only for steps that need it.

## Check the host

Run the read-only check first.

```sh
./install.sh --check
```

The check reports missing tools, KVM state, systemd state, images, and firewall warnings.
It does not change the host.

## Install

```sh
git clone https://github.com/SteelCrab/firecrab.git
cd firecrab
./install.sh
```

Open the dashboard after installation.

```text
http://127.0.0.1:3000/
```

The default install builds the Alpine image.
Ubuntu and Rocky images are optional.

## Options

| Option | Effect |
| --- | --- |
| `--check` | Report readiness without changes |
| `--doctor` | Run read-only runtime diagnostics |
| `--deps-only` | Install dependencies and stop |
| `--no-deps` | Do not install missing dependencies |
| `--no-images` | Skip guest image creation |
| `--with-ubuntu-image` | Also build Ubuntu 26.04 |
| `--with-rocky-image` | Also build Rocky Linux 9 |
| `--no-frontend` | Skip the dashboard |
| `--uninstall` | Remove services and binaries but keep data |
| `--uninstall --purge` | Also remove VM data and the database |

`--purge` is destructive.
Back up important VM data before using it.

## Path configuration

| Variable | Default |
| --- | --- |
| `FIRECRAB_USER` | `firecrab` |
| `FIRECRAB_GROUP` | `firecrab` |
| `PREFIX` | `/usr/local` |
| `DATADIR` | `/var/lib/firecrab` |
| `CONFDIR` | `/etc/firecrab` |
| `UNITDIR` | `/etc/systemd/system` |

Set variables before the installer when custom paths are required.

```sh
DATADIR=/srv/firecrab PREFIX=/opt ./install.sh
```

## Installed files

| Path | Purpose |
| --- | --- |
| `/usr/local/lib/firecrab/` | Service binaries |
| `/usr/local/bin/firecrab-doctor` | Host diagnostic tool |
| `/usr/local/share/firecrab/dashboard/` | Built dashboard |
| `/var/lib/firecrab/data/` | SQLite state and VM artifacts |
| `/var/lib/firecrab/images/` | Kernels and root filesystems |
| `/etc/firecrab/api.env` | API configuration |
| `/run/firecrab/net-helper.sock` | Helper Unix socket |

Custom path variables change the matching locations.

## Verify the installation

```sh
systemctl status firecrab-net-helper firecrab-api
ls -l /run/firecrab/net-helper.sock
sudo -u firecrab id
firecrab-doctor
curl -s http://127.0.0.1:3000/api/vms
curl -s http://127.0.0.1:3000/api/micro-networks
```

A new installation returns empty VM and MicroNetwork lists.
Create a MicroNetwork before creating a VM.

```sh
curl -s -X POST http://127.0.0.1:3000/api/micro-networks \
  -H 'Content-Type: application/json' \
  -d '{"name":"lab","subnetCidr":"172.30.0.0/24","internetEnabled":true}'
```

## Operations

Use systemd and journald for installed services.

```sh
journalctl -u firecrab-api -f
journalctl -u firecrab-net-helper -f
systemctl restart firecrab-api
```

Edit `/etc/firecrab/api.env` to change API settings.
Restart the API after editing it.

Run `./install.sh` again to repair or upgrade an installation.
The installer keeps the database, VM disks, and existing `api.env` file.

## Diagnose a host

```sh
./install.sh --doctor
```

An installed host can use the shorter command.

```sh
firecrab-doctor
```

The doctor checks KVM, the helper socket, UFW, nftables, and data paths.
See [troubleshooting](troubleshooting.md) when a check fails.
