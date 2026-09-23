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

## Install lifecycle

Build both sibling executables in a checkout, then install them for the current user.

```sh
cargo build -p firecrab-cli
scripts/build-micromanager-macos.sh target/debug/firecrab-micromanager-macos
./target/debug/firecrab service install
```

The default binary directory is `~/.local/bin`, and `FIRECRAB_INSTALL_DIR` overrides it.
`install` performs the complete managed-host gate before returning success:

1. Download the pinned Debian 13 ARM64 archive, Firecracker v1.17.0, and Firecrab v0.2.2 guest assets.
2. Verify every SHA-512 or SHA-256 digest before atomically publishing an artifact.
3. Extract the 3 GiB GPT OS disk and create a separate 16 GiB sparse persistent data disk.
4. EFI-boot Debian with a cloud-init seed and install Firecracker, Firecrab, OpenSSH, and systemd units.
5. Require usable guest `/dev/kvm` and boot a real nested Firecracker busybox workload to its serial success marker.
6. Register and start the launchd resident VM plus the key-only SSH localhost API tunnel.

A clean install downloads about 306 MiB and normally takes minutes rather than seconds.

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
Neither command accepts kernel, disk, CPU, or memory options.

```sh
firecrab service validate
firecrab service run
```

The launcher enables nested virtualization explicitly and uses VZ NAT for the outer Debian VM.
`service start` is the normal resident path; `service run` is a foreground serial-console path for debugging while the launchd service is stopped.
The guest publishes its DHCP address through a private virtiofs marker, and the daemon establishes the localhost SSH tunnel only after Firecrab API, net-helper, and SSH are active.
Stop requests guest `systemctl poweroff`, then escalates from helper TERM to KILL after 30 seconds.
A provisioning schema change replaces only the OS disk; the ext4 data disk is detected by label and never reformatted on reinstall.

## Validation and troubleshooting

`doctor` checks host capability, while `validate` checks the prepared kernel, initrd, OS/data separation, resources, and VZ configuration.
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
runtime/daemon.log            launchd wrapper and SSH tunnel
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
