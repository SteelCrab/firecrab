# Firecrab architecture

Firecrab is a single-host control plane for Firecracker microVMs, managing images, networks, disks, VM state, and browser consoles.
This contributor view covers the v0.3.0 system and the current checkout's source development flow, including processes, ports, paths, and event order.
For the simpler overview, see the [README](../README.md#architecture).

## Contents

- [microManager system architecture](#micromanager-system-architecture): Linux, macOS, and Windows
- [Source development on macOS and Windows](#source-development-on-macos-and-windows): guest builds and frontend proxy
- [Firecrab system architecture](#firecrab-system-architecture): components and VM startup
- [Image and kernel supply](#image-and-kernel-supply): catalog, OCI, MicroBoot, and kernels
- [Guest features](#guest-features): usage, shell repository, and SSH
- [CLI and updates](#cli-and-updates): remote control, checks, and Linux updates
- [State, networking, and security](#state-networking-and-security): persistence and trust boundaries

## microManager system architecture

All three hosts use the same Linux `firecrab-api`, `firecrab-helper`, and Firecracker stack.
The virtualization layer and the route to the host's localhost API differ.
Diagram colors distinguish clients, control, privileged/security operations, runtime, storage, networking, and microManager; dashed boxes mark group boundaries.

### Linux

Runs directly on the host, without microManager.

![Linux: native systemd services, direct localhost API, and host KVM](../assets/architecture/micromanager-linux.en.svg)

1. `install.sh` installs the binaries and two systemd units, and creates the data directory (default `/var/lib/firecrab`).
2. systemd starts the privileged `firecrab-helper`, which opens `/run/firecrab/net-helper.sock`.
3. The unprivileged `firecrab-api` starts and serves the dashboard and API at `127.0.0.1:5523`.
4. The browser connects directly to that address.
5. Each running MicroVM gets one `firecrab-vm` shim process, which runs one Firecracker process using host KVM.

### macOS

Runs inside a managed Debian 13 ARM64 VM on Apple `Virtualization.framework`.
Requires Apple silicon, macOS 15 or later, and runtime support for nested virtualization.
The complete path is validated on Apple M5/macOS 26.6.2; other M3-or-later hosts remain runtime and E2E gated.

![macOS: launchd, Swift helper, managed Debian VM, SSH tunnel, and port relay](../assets/architecture/micromanager-macos.en.svg)

1. `firecrab service start` enables the user's launchd agent.
2. `io.firecrab.micromanager` launches the entitlement-signed Swift helper and restarts it after a process crash.
3. The helper boots Debian with nested virtualization and VZ NAT, attaching a replaceable 3 GiB OS disk and a persistent 16 GiB sparse data disk. Each start writes the macOS version to the guest's `/etc/firecrab/host-platform.json`.
4. Guest systemd starts helper, then the API on guest loopback. The guest announces its IP through a virtiofs marker file.
5. Once the API, helper, and SSH are healthy, the daemon opens a key-only SSH tunnel. VM TCP forwards use `ssh -L` at `127.0.0.1:<hostPort>`; UDP forwards use the management VM address.
6. The browser opens `http://127.0.0.1:5523`. A dropped tunnel reconnects while the VM keeps running; after five failed reconnect attempts, the daemon restarts the management VM.

Managed files live under `~/Library/Application Support/Firecrab/micromanager`.
Reinstall and ordinary uninstall preserve managed data; explicit `uninstall --purge` removes it.
See [microManager on macOS](micromanager-macos.md) for provisioning and recovery.

### Windows

Runs inside the managed `firecrab-debian` Debian 13 distribution on WSL2.
The host requires Store WSL2 and usable nested `/dev/kvm`; native ARM64 installation remains unverified.

![Windows: scheduled task, managed WSL2 distribution, localhost forwarding, and current MicroVM limitation](../assets/architecture/micromanager-windows.en.svg)

1. `firecrab service start` enables the per-user scheduled task `\Firecrab\microManager`.
2. The task runs `wslg.exe -- sleep infinity` to keep the distribution alive. It starts at logon and retries every minute, with `IgnoreNew` preventing duplicate holders.
3. WSL2 boots the managed distribution with systemd. OS and data share `distro\ext4.vhdx`; each start writes Windows and WSL versions to the guest's `/etc/firecrab/host-platform.json`.
4. systemd starts helper, then `firecrab-api`.
5. WSL localhost forwarding exposes the guest's loopback API on Windows at `127.0.0.1:5523`, without an SSH tunnel.
6. The browser opens `http://127.0.0.1:5523`. With the source-built helper,
   running MicroVMs' TCP forwards also bind loopback sockets for WSL to expose
   at the same Windows localhost ports; stopping a VM removes its sockets.

**Windows Preview:** guests older than v0.3.0 cannot start MicroVMs on WSL2; `service install` provisions the latest release, which has the fixes but is not validated on Windows yet.
The dashboard, API, images, and networks work. `service dev` builds the current
checkout's helper with the same fixes.
Managed files live under `%LOCALAPPDATA%\Firecrab\micromanager`.
See [microManager on Windows](micromanager-windows.md) for lifecycle and shared WSL2 state.

## Source development on macOS and Windows

`service dev` builds the API and network helper inside the managed Debian VM. Frontend source runs through Vite on the host.
Windows uses the same snapshot, build, deployment, and rollback transaction;
`wsl.exe` carries the archive and script instead of SSH, and Debian selects
Linux x86_64 or ARM64 to match its architecture. WSL localhost forwarding
connects the Windows frontend to the guest API.

![macOS source development: SSH upload, Debian build, systemd deployment, and frontend proxy](../assets/architecture/micromanager-source-dev.en.svg)

1. **Upload:** ensure the management VM and API tunnel are ready, then upload one checkout snapshot over key-only SSH. Run `service dev` again after Rust edits.
2. **Build:** Debian uses the pinned Rust toolchain and `cargo build --locked` for Linux ARM64. Source and compiler caches live under `/var/lib/firecrab/dev` on the persistent data disk.
3. **Deploy:** stage the two executables under `/usr/local/lib/firecrab-dev`, switch systemd `ExecStart` overrides, then start helper followed by the API and check readiness. Build failure keeps the current services; deployment failure restores their previous executables. `service dev --restore` selects the release binaries.
4. **Frontend:** `npm run dev --prefix firecrab-frontend` serves `http://localhost:8080` and proxies `/api` and `/ws` through the Mac's `127.0.0.1:5523` SSH tunnel to the guest API. The installed dashboard remains available at port 5523.

See [microManager on macOS](micromanager-macos.md#run-the-api-and-network-helper-from-local-source) for commands.
Windows commands are in [microManager on Windows](micromanager-windows.md#develop-from-a-checkout).

## Firecrab system architecture

The API owns resource state and VM processes; privileged host networking is delegated over a Unix socket.
The installed API also serves the built dashboard. Development uses Vite, proxying `/api` and `/ws` to the API.

![Firecrab components and the numbered request-to-running flow](../assets/architecture/firecrab-system.en.svg)

### Components and resources

| Component | Responsibility | Boundary |
| --- | --- | --- |
| `firecrab-frontend` | VM, image, network, storage, console, and host UI | Browser |
| `firecrab-cli` | REST/WebSocket client; platform-local service commands | Client/host |
| `firecrab-api` | VM lifecycle, artifact checks, jobs, SQLite, and console broker | Unprivileged service account |
| `firecrab-helper` | Bridge, TAP, DHCP/DNS, firewall, NAT/DNAT, VM units (`StartVmUnit`/`StopVmUnit`), and Linux update application | Privileged, bounded capabilities |
| `firecrab-api-types` | Shared REST request/response models | Shared crate |
| `firecrab-helper-protocol` | Typed envelopes, framing, and protocol version 2 | Shared crate |
| `firecrab-vm` shim | Owns one Firecracker process; serves its console, stop/kill requests, and exit status on `shim.sock`; writes `console.log` and `exit.json` | One process per running VM, same account as the API (`firecrab-api vm-shim`), in its own `firecrab-vm-<id>.service` unit |
| Firecracker | Boots a guest kernel and rootfs through KVM | One process per running VM, child of its shim |
| SQLite and filesystem | Durable resource records and artifacts | API-managed state |

| Resource | Meaning |
| --- | --- |
| MicroVM | A VM with image, CPU, RAM, disk, network, and egress policy |
| MicroNetwork | A named IPv4 subnet, bridge, DHCP settings, and internet policy |
| MicroStorage | A registered host directory for VM disks |
| M2Image | A verified kernel/rootfs template, optionally with initramfs |
| MicroRegistry | Image/kernel catalog and package source; custom image registrations are local |
| MicroBoot | API-only image bootstrap using a temporary builder VM |

Every VM selects one MicroNetwork and an allowed storage root; there is no hidden default network. VM requests cannot supply arbitrary host filesystem paths.

### VM start flow

The numbers match the diagram.

1. The client sends `POST /api/vms/{id}/start`.
2. The API records `starting` and responds immediately; startup continues in a background task.
3. It prepares the VM's writable rootfs `d/<generation-id>.ext4` from the M2Image.
4. It asks helper over the Unix socket to reconcile the MicroNetwork bridge, create the VM TAP, and apply firewall and DHCP state.
5. It writes runtime configuration `fc.json` and starts the VM's shim, which starts Firecracker; the API attaches to the shim's socket.
6. Firecracker boots the guest kernel/rootfs with KVM; the TAP connects the bridge to the guest's virtio network interface.
7. The guest confirms DHCP and DNS, then sends `FIRECRAB_NETWORK_READY` over serial. The shim forwards the console to the API's console broker, which reads it, and the API records `running`.

A host-side DHCP lease alone is not sufficient to mark a VM running. The startup timeline records disk preparation, configuration, process start, and readiness.
Failure rolls back the process, firewall policy, and TAP where possible; an exit monitor removes runtime network state. Disk preparation concurrency is bounded to protect host I/O.
Network mutations are serialized so an old firewall snapshot cannot remove newer policy.

## Image and kernel supply

A VM needs an installed M2Image. Three image routes converge on a registered template; kernels are managed independently.

![Catalog installation, OCI import, MicroBoot bootstrap, registration, and kernel pairing](../assets/architecture/image-kernel-supply.en.svg)

1. **Catalog install:** fetch the host-architecture `.tar.zst` package from MicroRegistry (default `registry.firecrab.dev`), verify its digests, unpack kernel/rootfs into the image root, and register the template.
2. **OCI import:** inspect platform compatibility, then run a background job: fetch verified blobs, decompress and verify layer diff IDs, perform safety checks, merge layers in manifest order with whiteouts, inject guest utilities/init, and pack ext4 with `mkfs.ext4 -d`. Stages publish atomically; caches live under `.oci/`. Supported native init is preserved, with BusyBox as fallback. Imports also generate SPDX SBOMs.
3. **MicroBoot bootstrap:** a temporary builder VM downloads distribution files and creates an ext4 rootfs. Stop it before reading and packaging its disk. Remove the builder after success, failure, or cancellation; only one bootstrap runs at a time. Its dashboard panel has been removed; the API remains.
4. **Kernel install:** fetch a digest-pinned kernel from MicroRegistry into `.oci/kernel/<architecture>/`. Kernels can be installed/deleted separately from images; OCI import reuses the same cache.
5. **Register:** each route registers an alias in the image root (`nginx:1.27` becomes `nginx-1.27`). Only installed images create VMs. Custom MicroRegistry registration saves a local package and its SHA-256 in SQLite; reinstall verifies the package and restores the template after deletion. Registration does not publish it remotely.
6. **Pair a kernel:** `PUT /api/images/{alias}/kernel` switches an image to an installed, verified managed kernel without changing its rootfs. The update is refused while an instance VM references that image.

The API confines artifact paths to the configured image root and verifies hashes before use.
See [Images](images.md), [OCI imports](oci.md), and [Kernels](kernels.md) for pipeline details.

## Guest features

Three flows connect running guests to the API and dashboard.
The numbering continues within the diagram across usage, shell scripts, and SSH.

![Guest usage telemetry, pinned shell injection, and per-VM SSH keys](../assets/architecture/guest-features.en.svg)

### Usage

1. The guest agent measures CPU and memory (`MemTotal - MemAvailable`) and emits `FIRECRAB_USAGE` lines on the serial console.
2. The API console broker separates these lines for parsing and filters them from interactive terminals and console logs.
3. The dashboard shows guest CPU, memory, and sparklines in lists, details, and terminals. Without an agent, metrics are `null`; VM startup still succeeds.

### Shell repository

4. Scripts have revisions. Selecting `shellIds` at VM creation pins each script's latest revision to that VM.
5. On start, the API injects the pinned scripts into `/var/lib/firecrab/shells` in the rootfs and enables a boot oneshot.
6. After the guest network is ready, the oneshot runs the scripts (OpenRC on Alpine, systemd on Ubuntu/Rocky catalog guests).

### SSH

7. VM creation generates an operator ed25519 key pair under the VM's `ssh/` directory.
8. On start, the API injects the public key into `/root/.ssh/authorized_keys` and per-VM host keys into `/etc/ssh/`, enabling key-only `sshd`.
9. The dashboard SSH panel provides `firecrab-<name>.pem` and the host-key fingerprint. Connect with `ssh -i`; proxy jumps and configured DNAT forwards (`PUT /api/vms/{id}/port-forwards`) are also supported.

See [Dashboard](dashboard.md) and [API](api.md) for shell revision, SSH, and usage endpoints.

## CLI and updates

Resource commands use the same API as the dashboard on Linux, macOS, and Windows.
Native Linux checks and updates operate on the local host; macOS/Windows managed-service commands use `firecrab service`.

![CLI remote API access, Linux host diagnostics, and helper-mediated updates](../assets/architecture/cli-updates.en.svg)

### Remote control

1. Read host profiles from `~/.firecrab/config.toml`. Endpoint precedence is `--api`, `FIRECRAB_API`, `--host`, `current_host`, then `http://127.0.0.1:5523`.
2. VM, network, image, and host commands call the selected API over REST and use dashboard-equivalent validation.
3. `firecrab vm console` attaches to a running VM's serial console over WebSocket. Press Ctrl+] to detach without stopping the VM.

Resource commands do not touch Firecracker or nftables directly.
On Windows, `~` means `%USERPROFILE%`; `FIRECRAB_CONFIG_DIR` overrides the profile directory.

### Checks

4. `firecrab doctor` reads the local Linux host and runs 13 checks: KVM, Firecracker, forwarding, nftables, dnsmasq, helper socket, UFW, data root, images, install tools, SELinux domain, registry egress, and reflink support.
5. Passing checks stay silent; failures/skips produce blocks, and any failure returns a nonzero exit code. `firecrab status` reports both systemd services and `GET /api/host`, even when the API is down.

### Update

6. `firecrab update --check` (also the default with no flag) compares the latest GitHub release tag without downloading a bundle or requiring elevated privileges.
7. `sudo firecrab update --apply` downloads the host bundle, verifies SHA-256, and stages it under `$DATADIR/updates/<uuid>`. The CLI does not write installation directories or call systemctl.
8. It sends one `ApplySelfUpdate` request. The helper re-verifies the checksum using its own file descriptor and writes only to installation paths derived from its service units.
9. The helper replaces binaries/dashboard and restarts both services. Unit files are preserved; rerun `install.sh` when release notes require unit changes. The dashboard update indicator uses the equivalent `GET /api/update` and `POST /api/update` flow.
   Running VMs keep running through the update: each shim stays on the previous binary until its VM restarts, and the restarted API re-adopts it, or stops through the helper a shim whose protocol it no longer speaks.

See [CLI](firecrab-cli.md) and [Operations](operations.md) for commands and failure handling.

## State, networking, and security

### Durable and runtime state

```text
<storage-root>/vms/<vm-id>/
  vm.lock
  d/<generation-id>.ext4
  r/<runtime-id>/
    fc.json
    fc.sock
    shim.sock
    console.log
    exit.json
    shim.err
  ssh/
```

Disk generations survive stop/start; runtime directories belong to individual start attempts.
MicroStorage registers an already mounted directory; Firecrab does not partition, format, or mount physical disks.

| State | Location | Recovery |
| --- | --- | --- |
| VM, network, storage, lease, and port-forward records | SQLite WAL | Loaded at API startup |
| M2Images and VM disks | Filesystem | Paths and hashes are verified |
| Shim connections | API memory | Re-attached at startup when the shim still runs |
| Job progress | API memory | Not recovered after restart |
| Bridge, TAP, nftables, and dnsmasq state | Linux runtime | Reconciled from desired state |

![VM lifetime: each VM in its own systemd unit, and how API startup settles each active VM](../assets/architecture/vm-lifetime.en.svg)

The helper starts each shim as a transient `firecrab-vm-<id>.service` unit owned by PID 1, so VMs keep running across API restarts and upgrades.
The unit runs the `firecrab-api` installed beside the helper as the API's user; the helper refuses a program or Firecracker binary that anyone but root could change.
When the API runs in `firecrab-api.service`, the unit gets that sandbox and shares its private `/tmp` (`JoinsNamespaceOf=`), so paths the API resolves mean the same files to the shim; an API run from a checkout has no sandbox, and neither do its units.
Each unit also gets host-side ceilings from the VM's RAM and vCPUs when it starts: `MemoryMax` is the guest RAM plus the larger of 256 MiB and an eighth of it, and `CPUQuota` is one core per vCPU plus one. A healthy VM stays well below them; they stop a runaway Firecracker or shim from starving the API and the other VMs.
A stop from outside the API (`systemctl stop` of the unit, a host shutdown) reaches the shim, which records it in `exit.json` and its final frame, so the VM is recorded `stopped`.
Only one shim can run a VM at a time: each holds a lock on `<vm-id>/vm.lock`.

At startup the API settles every VM its database calls active from what the previous run left behind ([issue #123](https://github.com/SteelCrab/firecrab/issues/123)):

| Record | Evidence | Result |
| --- | --- | --- |
| `running` | Shim answers on `shim.sock` | Re-adopted; console and stop work again |
| `stopping` | Shim answers | Re-adopted, and the stop finishes |
| `starting` | Shim answers | Killed and recorded `error`: its start checks never finished |
| Any | Shim answers but cannot be attached (another protocol version or VM) | Unit stopped, recorded `stopped`; `error` with its network kept if the stop fails |
| Any | `exit.json`: clean exit or a requested stop | `stopped` |
| Any | `exit.json`: crash | `error` |
| Any | Neither | `stopped` |

VMs that are not re-adopted have their unit stopped (a no-op when none runs) and lose their TAP and firewall policy; networks, policies, and the re-adopted VMs' TAPs are then re-applied.
`firecrab-vm-*` units still running no re-adopted VM are logged, never stopped.
SQLite and artifacts remain the durable source of truth.

### Networking

- Each MicroNetwork has an `mnb*` bridge; each VM has an `fct*` TAP. Names are derived from UUIDs by the helper.
- Persistent IPv4/MAC leases live in SQLite. Internet access requires both network and VM policies to allow it.
- Host-to-guest port forwarding uses nftables DNAT; different MicroNetworks are isolated by default.
- The current implementation also blocks L2 traffic between VM TAPs; same-network traffic is tracked by [issue #72](https://github.com/SteelCrab/firecrab/issues/72).
- Desired firewall state is applied as one nftables transaction. An unchanged snapshot is skipped only after reading matching Firecrab-owned tables/chains/maps/rules from the kernel; counters and handles do not count as drift. Unrelated host tables remain intact.
- Startup network recovery repairs TAP attachment/link state, bridge MTU/gateway prefixes, forwarding, owned nft rules, and DHCP files/process state, with up to three attempts. DHCP retains its newest reservation snapshot when recovering from an older request.
- Persistent failures appear as per-VM `networkFailed` diagnostics while the VM remains `running`. `POST /api/network/reconcile` retries after the host/helper is repaired and updates running VMs' results without restarting their processes. Manual Firecrab rules may be overwritten during reconciliation.

### Security boundaries

- The HTTP listener defaults to loopback at `127.0.0.1:5523`.
- Browser mutations must pass origin policy; REST requests have body-size, timeout, and concurrency limits.
- The API is unprivileged. The helper validates peer UID, protocol version 2, and request fields, and receives bounded Linux capabilities through systemd.
- The helper starts VM units for the API but takes nothing privileged from the request: it derives the unit name from the VM UUID, the uid/gid from the socket peer, and the program from the `firecrab-api` beside it, accepts only root-owned binaries nobody else can change, and runs each unit as the API user in the API's own sandbox.
- Image and VM paths are restricted to configured roots.
- Firecrab remains a single-host system without built-in multi-host scheduling, HA, or live migration.

Do not expose the unauthenticated API listener to an external network.
The diagrams retain the supplied artwork. Linux, Apple, and Debian logos are from simple-icons (CC0); line icons are from Lucide (ISC). Trademarks belong to their respective owners.

## Related

- [Core concepts](concepts.md)
- [Networking](networking.md)
- [Storage](storage.md)
- [Images](images.md), [OCI imports](oci.md), and [Kernels](kernels.md)
- [Dashboard](dashboard.md) and [API](api.md)
- [CLI](firecrab-cli.md) and [Operations](operations.md)
- [microManager on macOS](micromanager-macos.md) and [Windows](micromanager-windows.md)
