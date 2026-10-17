# Installation

Choose the installation that matches your task.

| Task | Install |
| --- | --- |
| Run VMs on Linux | [Full Linux host](#linux-host) |
| Run VMs on Apple silicon macOS | [CLI](#linux-and-macos), then [microManager](macos.md) |
| Run VMs on Windows | [CLI](#windows), then [microManager](windows.md) |
| Manage a remote host | [CLI only](#cli-only-installation) |

## Linux host

Use Linux with systemd, hardware virtualization, and `/dev/kvm`.
The installer supports `apt-get`, `dnf`, `zypper`, `pacman`, and `apk`.
Network access and a user with `sudo` access are required.

Run as your normal user; the installer requests privilege when needed.

```sh
curl -fsSL https://github.com/SteelCrab/firecrab/releases/latest/download/install.sh | bash
```

From a checkout, `./install.sh --check` checks readiness without changing the host.
The installer selects `x86_64` or `aarch64` and glibc (`gnu`) or `musl` automatically.

Check the result, then open `http://127.0.0.1:5523/`.

```sh
systemctl status firecrab-helper firecrab-api
firecrab doctor
curl -fsS http://127.0.0.1:5523/api/host
```

A new host has no network or guest image.
Create a [network](../guides/networking.md), then install an [image](../guides/images.md) or [import OCI](../guides/oci.md).

## CLI-only installation

The standalone CLI manages a remote API from Linux, macOS, or Windows.
Its installers need no root access or Rust toolchain.
Download and verify the installer before running it.

### Linux and macOS

Supports Linux x86_64/ARM64 and Apple silicon macOS.

```sh
curl -fLO https://github.com/SteelCrab/firecrab/releases/latest/download/install-cli.sh
curl -fLO https://github.com/SteelCrab/firecrab/releases/latest/download/SHA256SUMS
grep ' install-cli.sh$' SHA256SUMS > install-cli.sh.sha256
if command -v sha256sum >/dev/null; then
  sha256sum -c install-cli.sh.sha256
else
  shasum -a 256 -c install-cli.sh.sha256
fi
sh install-cli.sh
```

The default destination is `~/.local/bin`.
Follow the printed `PATH` instruction if needed.
Use a directory controlled by your user.

```sh
sh install-cli.sh --version v0.1.3 --install-dir "$HOME/bin"
sh install-cli.sh --uninstall
```

Uninstall keeps host profiles; add `--purge` to delete `~/.firecrab/config.toml`.
On macOS, remove the managed service before removing the CLI.

### Windows

```powershell
Invoke-WebRequest https://github.com/SteelCrab/firecrab/releases/latest/download/install-cli.ps1 -OutFile install-cli.ps1
Invoke-WebRequest https://github.com/SteelCrab/firecrab/releases/latest/download/SHA256SUMS -OutFile SHA256SUMS
$expected = ((Get-Content SHA256SUMS | Where-Object { $_ -match ' install-cli\.ps1$' }) -split '\s+')[0]
if ((Get-FileHash install-cli.ps1 -Algorithm SHA256).Hash -ne $expected) { throw 'installer checksum mismatch' }
& ./install-cli.ps1
```

The installer selects x86_64 or ARM64 and adds `%LOCALAPPDATA%\Programs\firecrab\bin` to the user `PATH`.
Open a new terminal after installation.
Pass `-Version TAG` to pin the client archive.

After installing the CLI, set a [host profile](../reference/cli.md#host-profiles) or install the local [macOS](macos.md) / [Windows](windows.md) service.

## Linux installer options

| Option | Effect |
| --- | --- |
| `--check` / `--doctor` | Readiness check / runtime diagnostics |
| `--version TAG` | Pin the release |
| `--libc gnu` / `--libc musl` | Override libc detection |
| `--no-deps` / `--no-frontend` | Skip packages / dashboard |
| `--with-pipe` | Install Nginx and the verified cloudflared connector; configure [Pipe routes](../guides/pipe.md) separately |
| `--bin-dir DIR` | Install locally built binaries |
| `--dashboard-dir DIR` | Install locally built dashboard assets |
| `--uninstall` | Remove services and binaries; retain data |
| `--uninstall --purge` | Also delete the configured data directory |

For the CLI installer, pass `--version TAG` or `-Version TAG` to select the archive version.
Changing only the installer download URL does not pin that archive.

## Default Linux paths

| Path | Content |
| --- | --- |
| `/usr/local/lib/firecrab/` | Service binaries |
| `/usr/local/share/firecrab/dashboard/` | Dashboard |
| `/var/lib/firecrab/data/` | Database and default VM artifacts |
| `/var/lib/firecrab/images/` | Kernels and root filesystems |
| `/etc/firecrab/api.env` | API settings |
| `/run/firecrab/net-helper.sock` | Helper socket |

Override paths with `PREFIX`, `DATADIR`, `CONFDIR`, and `UNITDIR`.

```sh
DATADIR=/srv/firecrab PREFIX=/opt ./install.sh
```

## Related

- [Dashboard: first VM](../guides/dashboard.md#vm-workflow)
- [Operations: upgrades and removal](../guides/operations.md)
- [Build from a checkout](../../CONTRIBUTING.md#run-from-source)
- [Troubleshooting](../guides/troubleshooting.md)
