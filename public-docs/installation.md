# Installation

`install.sh` installs firecrab on one Linux host.
It builds the services and dashboard and installs systemd units.

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
git clone https://github.com/SteelCrab/firecrab.git
cd firecrab
./install.sh
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
firecrab-doctor
curl -s http://127.0.0.1:3000/api/vms
curl -s http://127.0.0.1:3000/api/micro-networks
```

A new host has no MicroNetwork.
Create one before creating a VM.

## Upgrade

Pull new source and run `./install.sh` again.
The installer keeps the database, VM disks, and `api.env`.

## Related

- [Operations](operations.md)
- [Networking](networking.md)
- [Images](images.md)
- [Troubleshooting](troubleshooting.md)
