# microManager on Windows

microManager runs Firecrab inside a managed Debian distribution on WSL2.
Linux hosts continue to run Firecracker directly through KVM and do not use this launcher.
The commands, their output, and the managed-data layout match [microManager on macOS](micromanager-macos.md).
The install, nested KVM, Firecracker workload, scheduled-task recovery, and localhost API path has been validated in a Windows 11 lab VM that itself runs on Linux KVM.
That lab nests one level deeper than a real Windows host, so its timings are not Windows performance figures.

## Current requirements

The launcher requires an x86_64 or ARM64 Windows host with WSL 2 from the Microsoft Store (`wsl --version` must answer).
WSL2 must expose `/dev/kvm`, which needs hardware virtualization and nested virtualization for the WSL2 utility VM.
No administrator rights are needed; the lab ran every command from a non-elevated session.
The managed distribution and every pinned artifact match the architecture of `firecrab.exe`.
ARM64 artifacts are pinned and verified, but no ARM64 Windows host has run the install end to end yet.

Run the capability check before installing.

```powershell
firecrab service doctor
firecrab service doctor --json
```

Unsupported hosts fail before anything is imported and mark the affected check as `FAILED`.
Human output uses one `PASS`, `WARNING`, or `FAILED` line per check, while `--json` retains fixes and end-to-end validation gates.
Check ids and JSON field names are the ones the macOS helper reports.

`doctor` probes the managed distribution once it exists and the default distribution before that.
Every WSL2 distribution shares one kernel, so either one answers for `/dev/kvm`.
A host with WSL but no distribution reports `WARNING`; `install` imports its own distribution and checks it there.
`kvm_intel` loads about 25 seconds after a cold WSL2 start, so a missing `/dev/kvm` during that window is a `WARNING`, not a failure.

## Install lifecycle

```powershell
firecrab service install
```

`install` performs the complete managed-host gate before returning success:

1. Download the pinned Debian 13 WSL rootfs 1.26.0.0, Firecracker v1.17.0, and Firecrab v0.2.2 guest assets.
2. Verify every SHA-256 digest before atomically publishing an artifact, resuming interrupted downloads.
3. Import the `firecrab-debian` distribution with `wsl --import`; the user's own distributions are never touched.
4. Install the guest packages, Firecracker, and Firecrab with `install.sh --no-deps` under the distribution's systemd.
5. Require usable guest `/dev/kvm` and boot a real nested Firecracker busybox workload to its serial success marker.
6. Register and start the scheduled task that keeps the distribution running, then wait for the localhost API.

The Debian rootfs is the build Microsoft's WSL manifest uses for `wsl --install -d Debian`.
The nested workload boots Debian's own kernel from `linux-image-amd64` or `linux-image-arm64`, because WSL runs Microsoft's kernel.

```powershell
firecrab service status
firecrab service stop
firecrab service start
firecrab service reinstall          # provision the distribution again, preserve data
firecrab service uninstall          # remove the scheduled task, preserve the distribution and data
firecrab service uninstall --purge  # also unregister the distribution and delete managed data
```

The dashboard and API are available at `http://127.0.0.1:5523/` while status is healthy.
The API stays loopback-only inside Debian and reaches Windows through WSL localhost forwarding, so no SSH tunnel is involved.
`--purge` is deliberately destructive and refuses unsafe paths, symlinks, and roots whose final component is not `micromanager`.

## Resident service

WSL stops a distribution about 15 seconds after its last client exits, even with systemd running inside.
The per-user scheduled task `\Firecrab\microManager` holds it open with `wslg.exe -- sleep infinity`, which shows no console window.
The task starts at logon and fires again every minute; `IgnoreNew` makes that a no-op while the holder runs.
Together they bring the distribution back after `wsl --shutdown` or a crash, like launchd's `KeepAlive` on macOS.
In the lab the API answered again 100 seconds after `wsl --shutdown`: up to a minute for the trigger, then the boot.

`stop` disables the task before ending it, stops `firecrab-api` and `firecrab-net-helper`, and terminates the distribution.
The task stays disabled, across logons too, until `start` enables it again.
`start` also writes the Windows and WSL versions to the guest's `/etc/firecrab/host-platform.json` for the dashboard's Host view.
A failed write prints a `[WARNING]` and does not stop the start.
`status` reports the task, the guest units, and the API on three lines.

```text
[PASS] task_scheduler: enabled
[PASS] management_vm: firecrab-api=active, firecrab-net-helper=active, ip=172.x.x.x
[PASS] api: http://127.0.0.1:5523/
```

## Validate and run

The managed root defaults to `%LOCALAPPDATA%\Firecrab\micromanager`.
Set `FIRECRAB_MICROMANAGER_HOME` only when a checkout or test needs a different root.

```text
<managed-root>\
  downloads\                       verified immutable release artifacts
  distro\ext4.vhdx                 the firecrab-debian disk, owned by WSL
  provision\                       guest provisioning and nested E2E scripts
  runtime\                         markers, logs, and the scheduled task definition
```

`validate` re-hashes every download and prints the distribution, provisioning, and task state in one run.
`run` opens a root console in the managed distribution; the distribution keeps running while the console is open.

```powershell
firecrab service validate
firecrab service run
```

The distribution's disk holds both Debian and Firecrab's data under `/var/lib/firecrab`.
`reinstall` provisions that same disk again instead of replacing it, so the data survives.

## Shared WSL2 state

Every WSL2 distribution runs in one utility VM.
They share the kernel, `/dev`, and the network namespace, so Firecrab's bridges and nftables tables are visible from the user's other distributions.
Do not create `/dev/kvm` by hand: a regular file there blocks the real device node for every distribution until `wsl --shutdown`.
`doctor` reports that state as `FAILED` with the fix.

## Known limitation

The stock WSL2 kernel has no nftables `bridge` family, and it already listens on `10.255.255.254:53` on `lo`.
The pinned Firecrab v0.2.2 net-helper needs the first for per-VM L2 rules and collides with the second in dnsmasq, so on WSL2 the API, images, and networks work but no MicroVM starts.
A net-helper that keeps L2 rules in per-VM `netdev` tables and keeps dnsmasq off `lo` runs the shared QA list on WSL2; the pinned release moves once one ships.

## Validation and troubleshooting

A successful install records these guest gates in `runtime\provisioned`:

- Debian 13 and systemd are running in `firecrab-debian`.
- `/dev/kvm` is readable and writable in the management guest.
- Firecrab API and net-helper are active.
- Firecracker v1.17.0 booted a real nested busybox workload and observed `FIRECRAB_NESTED_WORKLOAD_OK`.

Use these files when a stage fails:

```text
runtime\guest-provision.log   apt, Firecracker, Firecrab, and nested E2E
runtime\provision.phase       last provisioning phase
runtime\provision.failed      exit code of the failed phase
```

A failed provisioning run is retried by the next `install`.

The lab run, one nesting level deeper than a real host, measured about 8 minutes for a clean install including downloads and the nested E2E, 2 minutes 11 seconds for `reinstall`, 26 seconds for an `install` that found everything preserved, 28 seconds for `start`, and 3 seconds for `stop`.
Publish measurements from real Windows hosts before quoting Windows performance.

## End-to-end check

`scripts/ci-qa-windows-e2e.ps1` is the Windows counterpart of the macOS E2E script.
Its gate runs `doctor`, a fresh `install`, and `status`, then checks the API from Windows.
The `api`, `nginx`, and `guest` phases run the shared QA scripts inside `firecrab-debian`, the Firecrab host.

```powershell
scripts\ci-qa-windows-e2e.ps1 -Phase all -Cli target\debug\firecrab.exe
firecrab service uninstall --purge
```

`-WaitFactor` stretches the guest waits on hosts whose guests install first-boot packages slowly.

## Related

- [microManager on macOS](micromanager-macos.md)
- [Installation](installation.md)
- [firecrab CLI](firecrab-cli.md)
- [QA work list](qa.md)
