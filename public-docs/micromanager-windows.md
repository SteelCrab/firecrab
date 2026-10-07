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
The managed distribution and every downloaded artifact match the architecture of `firecrab.exe`.
ARM64 artifacts are verified, but no ARM64 Windows host has run the install end to end yet.

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

## Develop from a checkout

Open PowerShell in the repository root and build the Windows CLI:

```powershell
cargo build -p firecrab-cli --locked
.\target\debug\firecrab.exe service dev
.\target\debug\firecrab.exe service debug --logs --tail 100
Invoke-RestMethod http://127.0.0.1:5523/api/host
```

`service dev` installs microManager when missing and keeps its scheduled task running.
It snapshots the checkout's Rust sources and compile-time resources with Windows
`tar.exe`, then streams the archive and embedded build script into the managed
distribution through `wsl.exe`. Local configuration, `.env` files, Git metadata,
and build output stay on Windows. The API and net-helper build with the checkout's
pinned Rust toolchain inside Debian for Linux x86_64 or ARM64, matching the guest.
The first build installs compiler dependencies and Rust; later builds reuse
`/var/lib/firecrab/dev` for sources and compiler caches. Windows CRLF script
endings are normalized before the Linux build. An already installed pinned
toolchain is reused without refreshing its remote channel metadata.

Deployment switches only the two systemd `ExecStart` overrides after the build
succeeds, then checks the guest services and Windows localhost API. A failed
build preserves the running services; a failed deployment restores their previous
executables. Run the command again after source edits; it does not watch files.

```powershell
.\target\debug\firecrab.exe service dev --source 'C:\work\firecrab'
.\target\debug\firecrab.exe service dev --release
.\target\debug\firecrab.exe service dev --yes
.\target\debug\firecrab.exe service dev --restore # Installed release binaries
npm run dev --prefix firecrab-frontend            # http://localhost:8080
```

Vite proxies `/api` and `/ws` to `127.0.0.1:5523` through WSL localhost forwarding.
For running MicroVMs, the source-built net-helper binds each TCP forwarded port
on Debian's loopback and relays it to the guest. WSL discovers those sockets
and makes them reachable at the same Windows localhost port. The listener is
removed when the VM stops; a port occupied by another process fails VM startup.
UDP forwarding has no Windows localhost relay.
`--restore` needs no checkout and can recover a failed development API.
`service start` resumes the currently selected binaries without rebuilding;
`service run` opens a root console. Stop the resident distribution with
`service stop`. Continue invoking the checkout executable to test CLI changes;
`service reinstall` does not copy it into the user's installed binary path.

Run the shared QA against the source-built API and net-helper:

```powershell
.\scripts\ci-qa-windows-e2e.ps1 -Phase all -Cli .\target\debug\firecrab.exe -Source . -WaitFactor 3
```

The gate deploys the checkout before API, nginx, OCI guest, and Chromium tests.
The browser and local OCI registry run inside the managed distribution, using
its loopback and workload IPv6 routes. The browser phase installs its Linux
Node.js/Chromium dependencies there; Windows `node_modules` are excluded.
The tests clean up their resources and preserve the distribution and QA dependencies.
After the gate and setup succeed, `all` collects every test phase and fails overall if any phase fails.
Use `-Phase browser` to run only Chromium.
See [CI and runtime E2E](ci.md#windows-runtime-e2e) for phase options and artifact retention.

## Install lifecycle

```powershell
firecrab service install
```

`install` performs the complete managed-host gate before returning success:

1. Download the pinned Debian 13 WSL rootfs 1.26.0.0 and Firecracker v1.17.0, plus the latest Firecrab release's guest assets, which are verified against that release's `SHA256SUMS`; when GitHub cannot be reached, the release recorded by the last install is used.
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

`stop` disables the task before ending it, stops `firecrab-api` and `firecrab-helper`, and terminates the distribution.
The task stays disabled, across logons too, until `start` enables it again.
`start` also writes the Windows and WSL versions to the guest's `/etc/firecrab/host-platform.json` for the dashboard's Host view.
A failed write prints a `[WARNING]` and does not stop the start.
`status` reports the task, the guest units, and the API on three lines.

```text
[PASS] task_scheduler: enabled
[PASS] management_vm: firecrab-api=active, firecrab-helper=active, ip=172.x.x.x
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
`shell` opens a root shell in the managed distribution, and `shell -- <command>` runs one command there; the distribution keeps running while the shell is open.
`run` remains an alias of `shell`.

```powershell
firecrab service validate
firecrab service shell
firecrab service shell -- systemctl status firecrab-api
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
The net-helper in Firecrab releases before v0.3.0 needs the first for per-VM L2 rules and collides with the second in dnsmasq, so with such a guest the API, images, and networks work on WSL2 but no MicroVM starts.
From v0.3.0 the net-helper keeps L2 rules in per-VM `netdev` tables and dnsmasq off `lo`. `service install` provisions the latest release for a new guest and leaves an existing one unchanged, but that path has not been validated on Windows; a source-built net-helper, which has the same fixes, enabled workload boot on WSL2.
`service dev` deploys those fixes from the current checkout; `--restore`
returns to the installed binaries and their release limitations.
The [2026-10-03 source QA snapshot](qa.md#windows-source-validation-2026-10-03) records API/nginx/browser passes and remaining Ubuntu/Fedora SSH failures.

### Source QA continuation, 2026-10-04

On `feat/windows-source-dev` after `main` baseline `15934df`, the Windows 11 x64 QEMU lab (4 vCPU / 6 GiB) passed native CLI HTTP tests (17) and `service shell` E2E (8). Earlier Windows runner contracts, shared API/shell QA, nginx including Windows localhost HTTP, and VM lifetime results remain recorded in the initial source snapshot.
The final full WSL Chromium run passed 12 cases with one existing explicit skip, zero failures, and exit 0. All three console cases passed, including two viewers ending together and command execution after reattachment. The DHCP guest authenticated SSH with the downloaded key through IPv4 forwarding and over IPv6 after its host key became ready. Tests used one worker and zero retries.
Public-image guest QA passed Alpine 3.21, Ubuntu 24.04, and Fedora 42 with exit 0, including actual SSH authentication and resource cleanup (`-WaitFactor 15`). Alpine was already installed, so its import rows remain `WARNING`; Ubuntu and Fedora were freshly imported. Fedora's SPDX SBOM contained 127 RPM packages plus the image entry. The existing Ubuntu template was backed up and replaced with a fresh import because provisioning changes are embedded in each image.
The Linux lab host repeatedly attempted suspend, removing its network routes and causing QEMU connections to return `ENETUNREACH`. A temporary sleep inhibitor kept the network available during QA. Source fixes also bound registry connection retries, allow 60/65 seconds for API/CLI inspection, give APT network stages 600 seconds without interrupting dpkg configuration, recover DNF metadata once, and parse exported RPM headers correctly. Console and SSH tests wait for actual readiness before sending input.
Final logs, JSON summaries, and JUnit/browser archives are retained in the local lab's `failure-fix-20261004` directory. Earlier failures remain in `qa-e2e-20261004` and separate failed-run artifacts. This validates the source lab's guest and full browser runs; Windows `-Phase all` was not rerun, and physical Windows, stock release, and native Windows Chromium remain unverified.

## Validation and troubleshooting

For a failed install or start, collect a host-side snapshot before retrying:

```powershell
firecrab service debug
firecrab service debug --logs --tail 100
firecrab service debug --json
```

`debug` reports host capability checks and fixes, the scheduled task, WSL distribution and guest services, localhost API, provisioning phase/failure marker, and available logs. `--logs` adds a bounded provisioning-log excerpt and the running guest's Firecrab systemd journal; `--tail` accepts 1–1000 lines and requires `--logs` (default 200). Diagnostics do not start a stopped distribution or change its scheduled task. When WSL is stopped, KVM and nested virtualization checks are marked as unverified, and the guest journal is unavailable, while host-side provisioning evidence remains visible. The report omits credential files and redacts recognized secrets in log excerpts; inspect a log locally before sharing it. `firecrab service run` opens an interactive root console and can start the managed distribution.

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
Its gate runs `doctor`, installs or reuses microManager, optionally deploys `-Source`, and runs `status`, then checks the API from Windows.
The `shell` phase checks `service shell` (G6) from Windows.
The `api`, `nginx`, and `guest` phases run the shared QA scripts inside `firecrab-debian`, the Firecrab host.
The `lifetime` phase runs the shared VM-unit/API-restart/crash/cleanup QA inside that distribution as root.
The nginx phase also requires HTTP 200 from the Windows host's forwarded port.
The `browser` phase runs Chromium inside the distribution with real guest boot enabled.
It requires the managed API to remain running and clears inherited import-only or management-SSH settings.

```powershell
.\scripts\ci-qa-windows-e2e.ps1 -Phase all -Cli .\target\debug\firecrab.exe -Source .
```

`-WaitFactor` stretches nginx/SSH waits; it does not change package-install or Playwright deadlines.
`-ResultsDir` chooses a Windows output directory; the default is `target/qa/windows/<run-id>`.
Results include `run.log`, `summary.json`, and any browser traces/JSON/JUnit in `browser-results.tar.gz`.

## Related

- [microManager on macOS](micromanager-macos.md)
- [Installation](installation.md)
- [firecrab CLI](firecrab-cli.md)
- [QA work list](qa.md)
- [CI and runtime E2E](ci.md)
