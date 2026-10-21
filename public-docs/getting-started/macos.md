# microManager on macOS

microManager runs Firecrab in a managed Debian VM using Apple's `Virtualization.framework`.
The dashboard and API are available at `http://127.0.0.1:5523/`.

Change the management VM's CPU, RAM, disk, or network through the [resource CLI or Host screen](../guides/micromanager-resources.md).

## Requirements

- Apple silicon, macOS 15 or later, and nested virtualization support.
- A signed helper with the `com.apple.security.virtualization` entitlement, included in the CLI release.
- The [macOS CLI](installation.md#linux-and-macos).

Apple documents nested virtualization for M3 and later.
The runtime capability check decides whether this host can run it.
The complete path has been validated on M5 with macOS 26.6.2; other models still require runtime evidence.

## Install and start

```sh
firecrab service doctor
firecrab service install
firecrab service status
curl -fsS http://127.0.0.1:5523/api/host
```

Require `doctor` to report `ready: true` before installation.
`doctor --json` includes checks and suggested fixes.

Installation downloads about 306 MiB of pinned Debian 13 ARM64, Firecracker v1.17.0, and Firecrab v0.2.2 assets.
It verifies checksums, provisions the manifest-selected kernel, and checks nested Firecracker and resident boot before saving the installed manifest.
`install` and `reinstall` select the bundled [release manifest](../reference/micromanager-manifest.md).
Existing installations migrate only on an explicit install or reinstall.
Interactive installs show pending downloads and ask first; `--yes` skips that prompt.
Interrupted downloads resume on the next run.
Installation streams actual guest output, including package downloads, unpacking,
and setup. `[LOGS]` identifies the complete `guest-provision.log`, which also keeps
the shell trace for debugging. Output from earlier installations is not replayed.
Keep installation running until it reports success; the provisioning VM shuts down
before launchd registration and the resident management VM are started.
`Ctrl-C` before completion leaves the service unregistered. For an interrupted
first install whose binaries already exist, rerun `~/.local/bin/firecrab service install`
(or the CLI in the configured install directory). Verified downloads are reused.

## Service commands

| Command | Effect |
| --- | --- |
| `firecrab service start` / `stop` | Start / stop the management VM and API tunnel |
| `firecrab service status` | Check launchd, guest services, and localhost API |
| `firecrab service validate` | Check prepared assets and VM configuration |
| `firecrab service shell` | Open a root shell inside Debian |
| `firecrab service shell -- COMMAND` | Run one guest command |
| `firecrab service repair` | Recover the existing OS and service registration |
| `firecrab service reinstall` | Clear installation state and reprovision from TOML; retain data |
| `firecrab service uninstall` | Remove the service and binaries; retain managed data |
| `firecrab service uninstall --purge` | Also delete the managed OS, data, and SSH key |

Stopping or repairing the management VM stops its workload MicroVMs.
`stop` waits for the VM to exit and fails if shutdown cannot finish within 40 seconds.

## Networking and data

The launchd agent `io.firecrab.micromanager` keeps the management VM and key-only SSH API tunnel running.
A dropped tunnel reconnects; by default, five failed reconnects restart the management VM.
Edit `[runtime]` and `[runtime.ssh]` in the installed `micromanager.toml` to configure automatic relaunch, SSH reuse, keepalives, and retries.
Set `runtime.ssh.restart_vm_on_failure = false` to keep the VM running while SSH reconnects.
See [runtime settings](../reference/micromanager-manifest.md#runtime-settings).

| Traffic | Address on the Mac |
| --- | --- |
| API and dashboard | `127.0.0.1:5523` through SSH |
| VM TCP port forwards | `127.0.0.1:<hostPort>` through SSH relays |
| VM UDP port forwards | Management VM address from `service status` |

A TCP port already held by another Mac process is skipped and logged in `runtime/daemon.log`.
Port 5523 is reserved for the API.

Managed files default to `~/Library/Application Support/Firecrab/micromanager`.
Override with `FIRECRAB_MICROMANAGER_HOME` for an isolated checkout or test.

| Path under the managed root | Content |
| --- | --- |
| `micromanager.toml` | Installed release inputs and editable runtime settings |
| `downloads/` | Verified release artifacts |
| `system/` | Kernel, initrd, and replaceable 3 GiB Debian OS disk |
| `data/firecrab-data.raw` | Persistent 16 GiB sparse disk for databases, images, and VMs |
| `provision/` | Provisioning scripts |
| `runtime/` | SSH key, boot markers, registration, and logs |

Reinstall preserves the data disk and detects it by label.
Manifest migration replaces only the OS/boot files and resets the server fingerprint; management SSH client keys remain.
Ordinary uninstall also retains the management SSH key.
The default binary directory is `~/.local/bin`; override it with `FIRECRAB_INSTALL_DIR`.

## Diagnose and repair

Collect diagnostics before changing a failed service.

```sh
firecrab service debug --logs --tail 100
firecrab service debug --json
```

`debug` is read-only and includes host checks, registration, guest services, API, and provisioning state.
Guest journals are available only while the VM runs.
Log excerpts redact recognized secrets; inspect them before sharing.
`--tail` requires `--logs` and accepts 1–1000 lines.

| Symptom | Action |
| --- | --- |
| Completed installation has missing registration or an unhealthy tunnel/API | Run `service repair` |
| Missing binaries or incomplete provisioning | Run `service reinstall` with the signed helper available |
| SSH connection reset | Save logs, then try repair; the reset alone does not identify the cause |
| Console shows kernel panic or ext4 errors | Stop the service and preserve disk copies before recovery |

```sh
firecrab service repair
firecrab service status
curl -fsS http://127.0.0.1:5523/api/host
```

Repair checks prerequisites before stopping anything.
It backs up the OS and checks a detached copy without the data disk, directory share, or network.
A verified cached Debian image can restore missing packaged files; no cache produces a filesystem-only warning.
Existing files, `/etc`, application data, binaries, SSH keys, and development overrides are preserved.
Only a repaired image that passes the final read-only filesystem check is installed.
A clean check keeps the original OS image.

Repair recreates the wrapper and plist, starts the VM, and verifies the live API and current-boot guest report.
It prints six stages and reports pending waits every ten seconds.
Local VM/API readiness has a 120-second deadline; offline filesystem checks have 120 seconds.
Failures return nonzero. Disk backups remain under `runtime/repairs/os-*/`;
diagnostic logs are stored separately under `runtime/logs/repairs/os-*/`.
The CLI suggests repair for runtime failures; it does not run repair automatically.

| Log | Use |
| --- | --- |
| `runtime/guest-provision.log` | Package and installation failures |
| `runtime/provision.phase` | Last provisioning phase |
| `runtime/vm-console.log` | Guest boot and kernel failures |
| `runtime/daemon.log` | launchd, tunnel, and port relays |

## Related

- [Build services from a checkout](../development/micromanager.md#macos)
- [Repair QA and preservation checks](../development/macos-repair.md)
- [Architecture](../reference/architecture.md)
- [microManager on Windows](windows.md)
