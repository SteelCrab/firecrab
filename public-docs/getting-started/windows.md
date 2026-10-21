# microManager on Windows

microManager runs Firecrab in the managed `firecrab-debian` WSL2 distribution.
The dashboard and API are available at `http://127.0.0.1:5523/`.

Change CPU, RAM, disk, or networking through the [resource CLI or Host screen](../guides/micromanager-resources.md).
CPU, RAM, and networking settings affect all WSL2 distributions.

## Requirements

- x86_64 or ARM64 Windows with Microsoft Store WSL2; `wsl --version` must work.
- Hardware and nested virtualization exposing `/dev/kvm` inside WSL2.
- The [Windows CLI](installation.md#windows).

Commands run from a normal PowerShell session.
ARM64 artifacts are pinned, but ARM64 installation has no end-to-end validation.
Recorded runtime results come from a Windows 11 x64 lab VM on Linux KVM.
See [validation history](../development/validation-history.md) for scope and limits.

## Install and start

```powershell
firecrab service doctor
firecrab service install
firecrab service status
Invoke-RestMethod http://127.0.0.1:5523/api/host
```

Require `doctor` to report `ready: true` for runtime tests.
`doctor --json` includes suggested fixes.
With no WSL distribution yet, KVM is unverified until installation imports Debian.
A cold WSL2 start can also take about 25 seconds to load `kvm_intel`.

Installation verifies pinned Debian 13 WSL rootfs, Firecracker v1.17.0, and Firecrab v0.2.2 downloads.
It imports only `firecrab-debian`, provisions systemd services, and boots a nested BusyBox workload.
The exact Debian kernel selected by the [release manifest](../reference/micromanager-manifest.md) boots the nested test.
A per-user scheduled task then keeps Debian running and checks the localhost API before saving the installed TOML.
`install` and `reinstall` migrate explicitly; updating the CLI alone preserves existing installations.
Interrupted downloads resume; failed provisioning is retried by the next install.

## Known limitation

The pinned Firecrab v0.2.2 guest bundle cannot start workload MicroVMs on stock WSL2.
Its helper requires nftables `bridge` support and conflicts with WSL's loopback DNS listener.
The API, images, and networks still work.

The current source helper uses `netdev` rules and keeps dnsmasq off loopback.
Deploy it with [service dev](../development/micromanager.md#windows) to run workloads.
`service dev --restore` selects the installed binaries and restores their release limitations.
Source lab passes do not establish stock-release or physical-Windows compatibility.

## Service commands

| Command | Effect |
| --- | --- |
| `firecrab service start` / `stop` | Enable and start / disable and stop the managed distribution |
| `firecrab service status` | Check the task, guest units, and Windows API |
| `firecrab service validate` | Recheck downloads, distribution, provisioning, and task state |
| `firecrab service shell` | Open a root guest shell; `run` is an alias |
| `firecrab service shell -- COMMAND` | Run one guest command |
| `firecrab service repair` | Back up and repair the WSL filesystem, rebuild the task, and restart guest services |
| `firecrab service reinstall` | Clear installation state and reprovision from TOML; retain data |
| `firecrab service uninstall` | Remove the task; retain distribution and data |
| `firecrab service uninstall --purge` | Also unregister the distribution and delete managed data |

The scheduled task `\Firecrab\microManager` runs the native resource controller in a hidden window.
The controller holds WSL open with a windowless `wsl.exe --exec sleep infinity` child and serves resource controls on loopback port 5524.
It starts at logon and, by default, retries every minute without creating duplicate holders.
The installed `micromanager.toml` accepts `runtime.auto_restart` to disable periodic relaunch.
See [runtime settings](../reference/micromanager-manifest.md#runtime-settings).
The SSH settings apply only to macOS; Windows continues to use WSL.
`stop` leaves the task disabled until the next `start`.
Stopping the distribution stops its workload MicroVMs.

## Networking and data

WSL localhost forwarding exposes the guest's loopback API on Windows without SSH.
The source helper also relays TCP forwards to the same Windows localhost ports.
UDP has no Windows localhost relay.
A TCP port occupied by another process fails VM startup.

Every WSL2 distribution shares the kernel, `/dev`, and network namespace.
Other distributions can see Firecrab's bridges and nftables tables.
Never create `/dev/kvm` by hand; a regular file there blocks the real device for all distributions until `wsl --shutdown`.

Managed files default to `%LOCALAPPDATA%\Firecrab\micromanager`.
Use `FIRECRAB_MICROMANAGER_HOME` for an isolated checkout or test.

| Path under the managed root | Content |
| --- | --- |
| `micromanager.toml` | Installed release inputs and editable runtime settings |
| `downloads\` | Verified release artifacts |
| `distro\ext4.vhdx` | Debian and Firecrab data, including `/var/lib/firecrab` |
| `provision\` | Guest setup scripts |
| `runtime\` | Markers, logs, and task XML |

## Diagnose and repair

```powershell
firecrab service debug --logs --tail 100
firecrab service debug --json
# Run repair from PowerShell as Administrator.
firecrab service repair
```

`debug` is read-only and does not start a stopped distribution.
It reports host checks, task registration, provisioning, guest services, and the localhost API.
Guest checks are unverified while WSL is stopped.
`--tail` requires `--logs` and accepts 1–1000 lines.
Inspect redacted log excerpts before sharing them.

Repair checks the disk, provisioning marker, distribution registration, task prerequisites,
administrator privileges, and WSL rescue tools before stopping the managed distribution.
Offline checking uses a separate registered WSL2 Linux distribution, such as Debian
or Ubuntu, with `e2fsprogs` and `util-linux`. It may start that rescue distribution.
It never uses `firecrab-debian` or Docker Desktop's distributions as the rescue system.
If no rescue distribution exists, install one with `wsl --install -d Debian` and retry.
It backs up the complete `ext4.vhdx`, including OS and user data, under
`runtime\repairs\vhd-*\ext4.before.vhdx`. It checks a detached copy with `e2fsck -f -n`.
Attempt directories include the host's local date/time and numeric UTC offset,
for example `vhd-2026-10-15_23-45-12+0900-Ab12Cd`. The random suffix prevents
collisions within one second; names contain no Windows-invalid characters.
Backup and working files use CoW block clones when the host filesystem supports
them, such as ReFS. NTFS and other unsupported filesystems use full copies.
`[SNAPSHOT]` reports the selected method; `snapshots.json` records it with the
source and logical byte size. Logical size does not measure unique disk usage.
Each snapshot is independent and has no parent-file dependency.
Check exits 1–7 and 12 (an aborted check with uncorrected errors) trigger `e2fsck -f -y`.
Only repair exits 0–3 followed by a read-only check returning zero can replace the disk.
An unchanged clean disk keeps its original file. Failures preserve the installed disk
and backup. Diagnostics (`result.json`, `console.log`, and `error.log` when available)
are stored separately in `runtime\logs\repairs\<attempt-name>\`.
The date-based attempt name matches the backup directory. `[LOGS]` prints this path;
errors report backup and log paths separately. Backup directories retain only
disks and the `snapshots.json`/`retention.json` metadata needed for retention.
Retention keeps one rollback backup and one failed attempt's backup/working VHDX
set. The previous rollback is retired only after the final task, guest-service,
and API checks succeed. Failed or partial snapshot copies cannot displace the
last complete failure backup. A clean unchanged attempt removes its disk copies
and keeps its diagnostic logs. Log history keeps the latest ten attempts
independently of backup retention. Expiring logs never removes a retained backup;
unneeded backup directories are deleted instead of holding log history.
`retention.json` records completion state; `[CLEANUP]` reports deletion.
The next repair also prunes recognized `vhd-*` directories with dated or legacy
random names. Inline diagnostics from recognized older backups move to the matching
log directory before cleanup; existing logs are never overwritten. Unknown files,
symlinks, and unrecognized layouts are preserved. Cleanup warnings do not hide the
repair result.
The repair copy must be the only newly attached disk and must be unmounted before checking.
It must detach successfully before the verified copy can replace the installed disk.
Repair then recreates the task/XML, reloads systemd, clears failed units, and restarts
the installed helper and API. Binaries and development overrides stay in place.
It downloads nothing, restarts only `firecrab-debian`, and never runs `wsl --shutdown`.
Use current Store WSL with `--mount --vhd --bare` support.
Microsoft documents the administrator requirement for [WSL disk attachment](https://learn.microsoft.com/windows/wsl/wsl2-mount-disk).
Windows filesystem recovery does not restore missing packaged OS files from a cached image.
Offline checking has a 120-second deadline.
Readiness has a five-minute deadline; any failed stage returns nonzero.
An incomplete installation requires `service reinstall`.

Run the command and disk-preservation tests without an installed distribution:

```powershell
cargo test -p firecrab-cli --locked micromanager::windows
cargo test -p firecrab-cli --locked --test windows_service_repair
```

On a Windows host with a provisioned installation, run `service repair` from an
elevated PowerShell, then require `service status` and the localhost API to pass.
Tests on other operating systems use command stand-ins; they do not validate a live WSL mount.

Inspect `runtime\guest-provision.log`, `runtime\provision.phase`, and `runtime\provision.failed` for installation failures.

## Related

- [Source development](../development/micromanager.md#windows)
- [Windows runtime E2E](../development/ci.md#windows-runtime-e2e)
- [Recorded validation results](../development/validation-history.md)
- [microManager on macOS](macos.md)
