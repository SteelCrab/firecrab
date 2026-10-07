# microManager on macOS

microManager runs Firecrab inside a managed Debian VM built on Apple `Virtualization.framework`.
Linux hosts continue to run Firecracker directly through KVM and do not use this launcher.
The complete install, nested KVM, Firecracker workload, launchd recovery, and localhost API path has been validated on an Apple M5 running macOS 26.6.2.
Other M3-or-later models remain runtime-gated by Apple's nested-virtualization API until their end-to-end evidence is recorded.

## Current requirements

The launcher requires Apple silicon, macOS 15 or later, and a host on which `VZGenericPlatformConfiguration.isNestedVirtualizationSupported` returns true.
Apple documents nested virtualization for M3 and later, but the API result remains the runtime authority.
The helper must carry the `com.apple.security.virtualization` entitlement.
The macOS release includes an ad-hoc-signed helper with that entitlement.
The management kernel, initial RAM disk, and disk images must match the host's arm64 architecture.

Run the capability check before preparing or booting a VM.

```sh
firecrab service doctor
firecrab service doctor --json
```

Unsupported machines fail before a VM starts and mark the affected check as `FAILED`.
Human output uses one `PASS`, `WARNING`, or `FAILED` line per check, while `--json` retains fixes and end-to-end validation gates.

## Build from a checkout

Build and sign the native helper, then point the Rust CLI at it.

```sh
scripts/build-micromanager-macos.sh target/debug/firecrab-micromanager-macos
export FIRECRAB_MICROMANAGER_HELPER="$PWD/target/debug/firecrab-micromanager-macos"
cargo run -p firecrab-cli -- service doctor
```

The build script uses SwiftPM and applies only the Virtualization entitlement.
The default NAT network does not request the restricted bridged-network entitlement.

## Develop from a checkout

From the repository root, build the CLI and signed helper. If microManager is already installed, start its launchd service with the checkout CLI; this starts the management VM, guest API services, and the SSH tunnel to the Mac's localhost API together:

```sh
cargo build -p firecrab-cli --locked
scripts/build-micromanager-macos.sh target/debug/firecrab-micromanager-macos
./target/debug/firecrab service start
./target/debug/firecrab service debug --logs --tail 100
curl -fsS http://127.0.0.1:5523/api/host
```

`cargo run -p firecrab-cli -- service start` is the build-and-run equivalent of the start command. On a new host, use `./target/debug/firecrab service install` instead; installation provisions the guest and starts the API. To replace installed CLI/helper binaries with checkout builds, use `./target/debug/firecrab service reinstall`; it stops and reprovisions the management VM while preserving managed data. `service debug` is read-only, so a `launchd agent not loaded` result means to run `service start`. Stop the resident service with `./target/debug/firecrab service stop` when finished.

Building `firecrab-cli` does not rebuild the Firecrab API inside Debian. `service start` runs the guest API binary installed by microManager, not edited `firecrab-api/` source from this checkout.

### Run the API and network helper from local source

See [Architecture](architecture.md#source-development-on-macos) for the source build and frontend proxy diagram.

```sh
cargo build -p firecrab-cli --locked
scripts/build-micromanager-macos.sh target/debug/firecrab-micromanager-macos
./target/debug/firecrab service dev
```

`service dev` installs the management VM when missing, starts the resident VM and
API tunnel, uploads the current checkout, and builds `firecrab-api` and
`firecrab-helper` inside Linux ARM64 Debian. It uses `rust-toolchain.toml`
and `Cargo.lock`, installs compiler prerequisites on its first run, and caches
Rust and Cargo artifacts on the persistent data disk. The Mac's `target/`, local
root configuration, nested checkouts, and `.env` files are not uploaded.

After the build succeeds, systemd runs both source-built binaries and the command
checks guest and localhost API readiness. Open `http://127.0.0.1:5523/#/vms` as
usual. Run `service dev` again after each source edit; it does not watch files.
The resident API tunnel stays available during source development. Console login
is not required; inspect guest logs with `service debug --logs`.

```sh
./target/debug/firecrab service dev --source /path/to/firecrab # Select checkout
./target/debug/firecrab service dev --release                 # Optimized local build
./target/debug/firecrab service dev --yes                     # Skip download prompt
./target/debug/firecrab service debug --logs --tail 100       # Inspect logs
./target/debug/firecrab service shell                         # Root shell in the guest
./target/debug/firecrab service dev --restore                 # Return to installed binaries
./target/debug/firecrab service dev --help                    # List options
```

`dev` builds local sources in debug mode; `--release` optimizes the same sources.
`--restore` returns to the packaged API/helper from `service install` or the latest
`service reinstall`, without rebuilding or undoing local edits. It cannot be
combined with `--source`, `--release`, or `--yes`.

The installed release binaries and other systemd settings remain in place. A
build failure keeps the existing services running; failed deployment restores
their previous executables. `--restore` removes only the development overrides
and restarts the release binaries. Development overrides persist across
`service stop`/`start`, until restored. The dashboard remains its installed build;
frontend source development uses `npm run dev --prefix firecrab-frontend`.

Guest service restart briefly interrupts API access and network-helper requests.
Stop workloads before changing their runtime. A partially installed VM must first
be repaired with `service reinstall`.

## Install lifecycle

Build both sibling executables in a checkout, then install them for the current user.

```sh
cargo build -p firecrab-cli
scripts/build-micromanager-macos.sh target/debug/firecrab-micromanager-macos
./target/debug/firecrab service install
```

The default binary directory is `~/.local/bin`, and `FIRECRAB_INSTALL_DIR` overrides it.
`install` performs the complete managed-host gate before returning success:

1. Download the pinned Debian 13 ARM64 archive and Firecracker v1.17.0, plus the latest Firecrab release's guest assets, which are verified against that release's `SHA256SUMS`; when GitHub cannot be reached, the release recorded by the last install is used.
2. Verify every SHA-512 or SHA-256 digest before atomically publishing an artifact.
3. Extract the 3 GiB GPT OS disk and create a separate 16 GiB sparse persistent data disk.
4. EFI-boot Debian with a cloud-init seed and install Firecracker, Firecrab, OpenSSH, and systemd units.
5. Require usable guest `/dev/kvm` and boot a real nested Firecracker busybox workload to its serial success marker.
6. Register and start the launchd resident VM plus the key-only SSH localhost API tunnel.

A clean install downloads about 306 MiB and normally takes minutes rather than seconds.
On a terminal, `install` and `reinstall` first list each pending artifact's mirror, digest, and size and ask before downloading; `--yes` skips the question, and a script or CI run never sees it.
An interrupted download resumes from `downloads/<artifact>.partial` on the next run.

```sh
firecrab service status
firecrab service stop
firecrab service start
firecrab service reinstall          # replace binaries/OS when needed, preserve data
firecrab service uninstall          # stop daemon and remove binaries, preserve data
firecrab service uninstall --purge  # also delete managed OS and persistent data
```

The dashboard and API are available at `http://127.0.0.1:5523/` while status is healthy.
Each start pushes the macOS version into the guest's `/etc/firecrab/host-platform.json`, so the dashboard's Host view names macOS, not the Debian VM.
Ordinary uninstall retains the 0600 management SSH private key with the other managed data; use `--purge` to remove it.
The API remains loopback-only inside Debian and reaches macOS through a key-only SSH tunnel.
The launchd `io.firecrab.micromanager` agent restarts the management VM and tunnel after a process crash.
Running VMs' TCP port forwards are also served on `127.0.0.1:<hostPort>` through one management `ssh -L` per forward, since macOS local network privacy blocks the launchd agent from the VM's NAT address.
Interactive processes such as Terminal can also reach every forward at the management VM address that `service status` prints, which is the only path for UDP forwards.
A host port another Mac process already holds is skipped and logged once in `runtime/daemon.log`, and port 5523 always belongs to the API.
The relay restarts on its own after a crash without touching the VM; installs from before the relay pick it up with `firecrab service reinstall`.
A dropped API tunnel (sleep, a network stall) reconnects without restarting the VM, so running microVMs survive it; after five failed reconnects the agent restarts both.

`service stop` waits for the management VM process to exit before returning.
If the guest cannot acknowledge shutdown within 20 seconds, the native helper
stops its Virtualization.framework instance so a kernel panic cannot leave the
disk images locked indefinitely. Failure to stop within 40 seconds is an error.
`--purge` is deliberately destructive and refuses unsafe paths, symlinks, and roots whose final component is not `micromanager`.

## Validate and boot

The replaceable Debian system and persistent Firecrab data use fixed managed paths.
The default root is `~/Library/Application Support/Firecrab/micromanager`.
Set `FIRECRAB_MICROMANAGER_HOME` only when a checkout or test needs a different root.

```text
<managed-root>/
  downloads/                       verified immutable release artifacts
  system/Image                     direct-boot ARM64 kernel
  system/initrd.img
  system/debian-system.raw         replaceable Debian OS
  data/firecrab-data.raw           persistent DB, images, and workload disks
  provision/                       cloud-init and nested E2E scripts
  runtime/                         EFI state, SSH key, markers, and daemon logs
```

`validate` prints every managed setting and its `PASS`, `WARNING`, or `FAILED` status in one run.
`validate` does not accept kernel, disk, CPU, or memory options.

```sh
firecrab service validate
```

The launcher enables nested virtualization explicitly and uses VZ NAT for the outer Debian VM.
Use `service start` for the resident API and `service dev` to rebuild the guest API and network helper from local source.
The guest publishes its DHCP address through a private virtiofs marker, and the daemon establishes the localhost SSH tunnel only after Firecrab API, net-helper, and SSH are active.
Stop requests guest `systemctl poweroff`, then escalates from helper TERM to KILL after 30 seconds.
A provisioning schema change replaces only the OS disk; the ext4 data disk is detected by label and never reformatted on reinstall.

## Validation and troubleshooting

`doctor` checks host capability, while `validate` checks the prepared kernel, initrd, OS/data separation, resources, and VZ configuration.
For an install or start failure, collect one host-side snapshot before changing the service:

```sh
firecrab service debug
firecrab service debug --logs --tail 100
firecrab service debug --json
```

For a hands-on look, `firecrab service shell` opens a root shell in the management VM over its key-only SSH, and `firecrab service shell -- <command>` runs one command there, for example `firecrab service shell -- journalctl -u firecrab-api -n 50`.

`debug` reports host capability checks and fixes, the launchd agent, management VM services, localhost API, provisioning phase/failure marker, and available logs. `--logs` adds a bounded excerpt of each log and the running guest's Firecrab systemd journal; `--tail` accepts 1–1000 lines and requires `--logs` (default 200). The command does not start or stop the management VM. When the VM is stopped, guest journal output is unavailable, but the host-side markers and logs remain visible. The report omits credential files and redacts recognized secrets in log excerpts; inspect a log locally before sharing it.

If `service start` reports that guest provisioning stopped at a phase such as `firecrab`, the API inside Debian is not ready; starting the launchd agent again cannot finish the installation. Run `firecrab service reinstall` from a checkout with the signed helper beside the CLI. Reinstall rebuilds an incomplete Debian OS disk and keeps `data/firecrab-data.raw`. After it succeeds, check `firecrab service status` and `curl -fsS http://127.0.0.1:5523/api/host`. A VM process left running after `service stop` can keep both disk images busy; inspect running `firecrab-micromanager-macos` processes before retrying and preserve the data disk when recovering them.

If `ssh` to the management VM fails with `kex_exchange_identification: read: Connection reset by peer`, the guest's sshd is blocking the Mac under `PerSourcePenalties` (up to 600 seconds after crashed or unauthenticated sessions); wait, or run `firecrab service stop` and `service start`.
If `runtime/vm-console.log` shows `Internal error: Oops`, `Kernel panic`, or `EXT4-fs error`, the guest kernel failed under nested load: restart the service, and after ext4 errors stop it, keep a clone of the data disk (`cp -c data/firecrab-data.raw data/firecrab-data.raw.bak`), repair it with Homebrew `e2fsck -fy`, and run `firecrab service reinstall`.

If the guest finishes provisioning but its VM does not power off within two minutes, `install` stops the provisioning VM and continues from the recorded markers.
A successful install additionally records these guest gates in `runtime/provisioned`:

- Debian 13 and systemd completed EFI first boot.
- `/dev/kvm` is readable and writable in the management guest.
- Firecrab API and net-helper are active.
- Firecracker v1.17.0 booted a real nested busybox workload and observed `FIRECRAB_NESTED_WORKLOAD_OK` plus KVM poweroff.

Use these files when a stage fails:

```text
runtime/guest-provision.log   apt, disk, kernel, Firecracker, and Firecrab install
runtime/provision.phase       last first-boot phase
runtime/vm-console.log        normal direct-kernel boot console
runtime/daemon.log            launchd wrapper, SSH tunnel, and port-forward relay
runtime/daemon-ready          VM PID, tunnel PID, and guest IP
```

The validated M5 run measured about 41 seconds for the first artifact download, 1 minute 48 seconds for a schema-changing cached reinstall including nested E2E, about 5 seconds for daemon start, and about 4 seconds for orderly stop.
Performance and sleep/wake evidence for other supported Mac models still needs to be published before broad compatibility claims.

## Related

- [microManager on Windows](micromanager-windows.md)
- [Architecture](architecture.md)
- [Installation](installation.md)
- [firecrab CLI](firecrab-cli.md)
- [Operations](operations.md)
