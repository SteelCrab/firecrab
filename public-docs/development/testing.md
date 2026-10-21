# Test checklist

[Korean](testing.ko.md) · [QA scripts](qa.md)

Record each applicable case as **PASS**, **FAILED**, or **WARNING**.
A skip or leftover is WARNING.
Use `qa-` names and a dedicated subnet; create, exercise, delete, and check leftovers.
Apply updates only for U1; delete only images installed by this run and restore prior Docker Hub login.
API, CLI, and browser checks are separate evidence.

## Contents

- [Automated checks](#automated-checks-and-prerequisites)
- [Platform and host](#platform-and-host)
- [MicroNetwork](#micronetwork)
- [MicroStorage](#microstorage)
- [Shells](#shells)
- [Images, kernels, OCI, and registry](#images-kernels-oci-and-registry)
- [MicroVM and SSH](#microvm-and-ssh)
- [VM lifetime](#vm-lifetime)
- [nginx combined scenario](#nginx-combined-scenario)
- [CLI](#cli)
- [Browser E2E](#browser-e2e-every-playwright-case)
- [Uninstall](#uninstall)
- [Cleanup and CI coverage](#cleanup-and-ci-coverage)

## Automated checks and prerequisites

Commands: [CI local checks](ci.md#local-checks) and [installer/platform checks](ci.md#installer-and-platform-checks).

- [ ] **A1 — Rust formatting:** no formatting diff.
- [ ] **A2 — Rust lint:** zero warnings across all targets.
- [ ] **A3 — Rust tests:** workspace tests pass; run the CLI subset when needed.
- [ ] **A4 — Coverage:** generate a workspace coverage report where the tooling is available.
- [ ] **A5 — Frontend:** install dependencies, lint, typecheck, and build.
- [ ] **A6 — Browser setup:** install the isolated E2E package and Chromium.
- [ ] **A7 — Browser import pass:** run without guest boot; record guest cases as skipped.
- [ ] **A8 — Browser full pass:** run with KVM, Firecracker, and the network helper.
- [ ] **A9 — Documentation:** check links, changelog shape, and rustdoc warnings.
- [ ] **A10 — Installer scripts:** run shell lint and release/CLI installer checks on applicable OSes.
- [ ] **A11 — Swift/macOS:** run unit tests, verify the signed helper and CLI, and inspect diagnostic JSON.
- [ ] **A12 — Windows build:** run CLI lint, tests, build, and diagnostic JSON check.
- [ ] **A13 — Native release targets:** test and build all seven release targets listed below.
- [ ] **A14 — Documentation unit checks:** run changelog, release-note, and PR-report tests; check the rustdoc coverage floor in CI.
- [ ] **A15 — Installer diagnostics:** syntax, smoke, help, check, and doctor checks pass; check and doctor leave host state unchanged.
- [ ] **A16 — Installer lifecycle:** verify installation, daemon/socket permissions, KVM group, CLI, doctor, safe reinstall, uninstall, and purge.
- [ ] **A17 — Distribution dependencies:** check and install dependencies on Debian 12, Fedora, Arch, and openSUSE Tumbleweed.
- [ ] **A18 — Development deployment contract:** run shared guest build/deploy/rollback regression checks; record live `service dev` validation separately.
- [ ] **A19 — QA runner contracts:** verify guest failure/cleanup, native Windows phase aggregation, and browser mode/API guards without a VM.

## Platform and host

Use the [platform setup guides](../README.md#get-started), [repair QA](macos-repair.md), and [Windows shell tests](ci.md#windows-runtime-e2e).
Runtime doctor must report `ready: true`; hosted build/unit jobs do not prove runtime success.

- [ ] **G1 — Linux:** doctor, service install, start, and status succeed.
- [ ] **G2 — macOS:** service doctor, install, and status succeed with nested virtualization.
- [ ] **G3 — Windows:** service doctor, install, and status succeed with WSL2 nested virtualization.
- [ ] **G4 — All:** host API returns 200; dashboard returns HTML 200.
- [ ] **G5 — All:** unknown route returns JSON 404 with a request ID.
- [ ] **G6 — macOS/Windows:** `service shell` runs a command in the managed Debian guest as root with each argument unchanged and returns its exit code; without a command it opens a root login.
- [ ] **G6a — Windows:** commands run as root with `/root` as the working directory.
- [ ] **G6b — Windows:** the default shell reads stdin and returns exit 19.
- [ ] **G6c — Windows:** the `service run` alias still opens that shell.
- [ ] **G6d — Windows:** Unicode, quotes, empty strings, spaces, dollar signs, backslashes, and shell metacharacters remain literal arguments.
- [ ] **G6e — Windows:** stdout/stderr remain separate and exit 23 reaches the host.
- [ ] **G6f — Windows:** command stdin preserves Unicode and lines; EOF terminates the command.
- [ ] **G6g — Windows:** a missing command fails visibly; a subsequent shell succeeds.
- [ ] **G6h — Windows:** shell exit leaves API/helper active and the host API responsive.
- [ ] **G7 — macOS repair:** restore registration and the API without replacing binaries, data, keys, or dev overrides.
  Back up and check a detached OS copy before verified repair commit.
  Missing prerequisites fail before stopping; invalid boot reports/API checks fail nonzero.
  Follow [repair QA](macos-repair.md) for preservation checks.
- [ ] **G7a — macOS recovery advice:** service runtime failures suggest `firecrab service repair` without running it. Lost registration in a completed installation suggests repair; a new host suggests install. Source/compiler failures keep their own errors, and `debug --json` stays valid JSON with advice inside its fields.
- [ ] **G7b — macOS repair progress:** repair prints validation, shutdown/helper cleanup, OS backup/repair, registration, startup, and guest-boot/API-check stages before each operation. Pending VM/API and shutdown waits report elapsed time and current state every ten seconds. Local VM/API readiness times out after 120 seconds; shutdown keeps its forty-second deadline. Health checks and failure exit codes stay unchanged.
- [ ] **G7c — macOS OS filesystem repair:** a tiny unattached-inode fixture is corrected and passes a second read-only check. Clean checks keep the original OS inode. Failed checks and timeouts keep the original, backup, data disk, and SSH assets. Offline checks use a separate 120-second deadline.
- [ ] **G7d — macOS helper cleanup:** stale helpers are matched by owner, executable, and managed home. Helpers for another home survive. A per-home lock prevents simultaneous management VMs and OS commit.
- [ ] **G7e — Missing Debian files:** restore only verified missing packaged files and matching-version links/manifests.
  Preserve existing files, `/etc`, app data, and OS/data/SSH/reference assets.
  Reject failed verification; a second repair restores zero files.
  The repaired copy must boot with SSH, dbus, networking, and API ready within 120 seconds.
- [ ] **G7f — macOS SSH paths:** default `Application Support` paths remain one OpenSSH known-hosts path. Check the real `ssh -G` parser, wrapper tunnel/reconnect, shell, dev, and port-forward arguments. Repair verifies the current-boot guest report without an SSH command; missing, inactive, duplicate, or mismatched fields fail. Retain existing host pins and reject a changed host key.
- [ ] **G8 — Windows repair:** back up the WSL disk, check a detached copy in a separate WSL2 rescue distribution, and replace only verified repairs.
  Recreate the task/XML and restart installed API/helper, including legacy helper names.
  Preserve binaries, provisioning, data, and dev overrides; no downloads, reimport, or global WSL shutdown.
  Missing Administrator rights or rescue tools fail before stopping; every failed stage returns nonzero.
  Require live services and Windows API within the five-minute readiness deadline.
- [ ] **H1 — Update status:** `GET /api/update` succeeds; `GET /api/update/progress` reports `idle` or the last run.
- [ ] **H2 — Host network:** an uplink is present; loopback and internal helper interfaces are absent from the picker.
- [ ] **H3 — Firecrab info:** the API reports build/install info, the Firecracker version, and API/helper states. The Host panel shows and refreshes them; an API outage must not retain stale active states.
- [ ] **U1 — Update apply (optional separate run):** apply update; host recovers and API returns 200.
  The dashboard dialog lists release notes, then shows a gauge, percent, and stage until the new API answers.

## MicroNetwork

Requests: [Networking](../guides/networking.md) and [API field rules](../reference/api.md#micronetwork).

- [ ] **N1 — IPv4 create/delete:** creation returns 201; deletion returns 204; detail then returns 404.
- [ ] **N2 — List/detail:** both views contain the created network.
- [ ] **N3 — Internet toggle:** disable, then re-enable internet access.
- [ ] **N4 — IPv6 SLAAC:** create a ULA /64 network with SLAAC, then delete it.
- [ ] **N5 — Invalid uplink:** an empty uplink is rejected with 400 and leaves no row.
- [ ] **N6 — Busy delete:** deleting a network attached to a VM returns 409; delete the VM, then the network.

## MicroStorage

Requests and placement limits: [Storage](../guides/storage.md).

- [ ] **S1 — Roots:** root list includes the default pool.
- [ ] **S2 — Devices:** host device list loads.
- [ ] **S3 — Pool lifecycle:** register an absolute host path (201), delete it (204), and confirm detail returns 404. On macOS/Windows, use a path inside the Debian management guest.

## Shells

Routes: [API](../reference/api.md#other-endpoints); guest behavior: [shell revisions](../reference/guest-runtime.md#shell-revisions).

- [ ] **L1 — Create/delete:** create a named POSIX shell (201), then delete it (204).
- [ ] **L2 — Revision:** add a new shell revision.
- [ ] **L3 — Read:** fetch the shell and its revision body.
- [ ] **L4 — Pin to VM:** assign a shell on VM creation or update; confirm its executable file inside the guest.

## Images, kernels, OCI, and registry

Procedures: [Images](../guides/images.md), [Kernels](../guides/kernels.md), and [OCI](../guides/oci.md).
I11 references: `alpine:3.21`, `ubuntu:24.04`, `fedora:42`.
I2/I3/I4/I7/I10/U1 require separate checks outside GitHub Ubuntu CI.

- [ ] **I1 — Catalogs:** image, kernel, and registry catalogs load.
- [ ] **I2 — Kernel lifecycle:** installation succeeds; unused kernel deletes; a paired kernel returns a conflict.
- [ ] **I3 — M2Image lifecycle:** package, install, then remove staging and only the image installed in this run.
- [ ] **I4 — Kernel pairing:** pair an image; missing cache returns a conflict.
- [ ] **I5 — OCI inspect:** inspect a reference without storing blobs.
- [ ] **I6 — OCI import:** import, poll status, then delete the imported alias.
- [ ] **I7 — Registry:** register a custom installed alias and clean it up with I6.
- [ ] **I8 — Docker Hub:** read without exposing the secret; optionally set and remove a fake login, restoring the original state.
- [ ] **I9 — Missing image:** unknown alias returns 404.
- [ ] **I10 — Bootstrap (separate slow pass):** start bootstrap, then delete its job and builder VM.
- [ ] **I11 — OCI distro boot matrix:** inspect, import, start, stop, and delete all three references below. Preserve preinstalled aliases and mark unperformed imports **WARNING**.

## MicroVM and SSH

Install a template and create a network first.
Use the [VM API](../reference/api.md#vm-endpoints); CPU/RAM/disk/egress edits require an inactive VM.
SSH requires real authentication, including the management-VM proxy on macOS.

- [ ] **V1 — Create:** submit name, installed template, CPU, RAM, disk, network ID, and environment.
- [ ] **V2 — List/detail:** both views show the new VM in the created state.
- [ ] **V3 — Stopped VM environment:** set values on create or update; verify the guest file.
- [ ] **V4 — Port forwards:** set rules on create or update; curl the host port after start.
- [ ] **V5 — Shell pins:** assign shell IDs and verify the guest file.
- [ ] **V6 — Storage:** update VM storage successfully.
- [ ] **V7 — Start:** reach running state with the network-ready log marker.
- [ ] **V8 — SSH:** complete all four subchecks once the VM is running.
  - [ ] **V8a — Private key:** download the per-VM OpenSSH PEM.
  - [ ] **V8b — Host key:** receive fingerprint and public key.
  - [ ] **V8c — Host-key check:** poll until the key matches.
  - [ ] **V8d — Login:** key-only root login succeeds; OS name is nonempty. On macOS, use the management VM SSH proxy.
- [ ] **V9 — Console:** WebSocket upgrade returns 101.
- [ ] **V10 — Running VM environment:** update values while running; guest file changes and app service restarts.
- [ ] **V11 — Stop:** reach stopped state.
- [ ] **V12 — Running delete:** deletion is rejected until the VM stops.
- [ ] **V13 — Stopped delete:** deletion returns 204, detail returns 404, and the VM leaves the list.
- [ ] **V14 — Missing network:** creation returns 400 and leaves no row.
- [ ] **V15 — Console logout:** `exit` closes the socket with `4000 session_ended`; VM stays running.

## VM lifetime

Run on the API host as root with a running QA VM.
On macOS, run host commands through management VM SSH.

- [ ] **R1 — Shim unit:** shim is Firecracker's parent and the unit main process, owned by PID 1 and running as the API user.
  Require `shim.sock` and `console.log`.
  `MemoryMax` is guest RAM plus max(256 MiB, RAM/8); `CPUQuota` is vCPUs plus one core.
- [ ] **R2 — Stop from outside the API:** `systemctl stop` of the unit records `stopped`, `exit.json` has `"stop_requested":true`, and the TAP is gone.
- [ ] **R3 — Quick restart:** a start right after a stop reaches `running`.
- [ ] **R4 — API restart:** shim and Firecracker PIDs are unchanged, the VM stays `running`, the journal reports `adopted=1`, the TAP stays on its bridge, and V9 and V11 still work.
- [ ] **R4b — nft drift:** with the helper alive and API down, damage the QA VM's DNAT/egress/L2 rules. API startup restores intended policy; a foreign table survives; guest ping, SSH and forwarded HTTP work; VM PIDs are unchanged.
- [ ] **R4c — TAP/bridge drift:** detach/down TAP, down bridge, wrong MTU, gateway prefix and forwarding. Startup repairs all with unchanged VM PIDs and working guest traffic.
- [ ] **R4d — DHCP drift:** corrupt hosts/base configuration and kill dnsmasq. The same lease revision restores service; the guest obtains its reserved IP again and traffic works.
- [ ] **R4e — recovery retry:** helper outage reports `networkFailed`; retry returns 503. Start helper and `POST /api/network/reconcile` returns 204, refreshes results to `reconnected`, retains VM PIDs and restores traffic.
- [ ] **R4f — helper rename (disposable Linux CI only):** migrate a real legacy unit with a running VM; both unit names resolve to the same helper PID, the old executable path and drop-in work, VM PIDs are unchanged, and guest ping/SSH/forwarded HTTP recover.
- [ ] **R5 — Crash while the API is down:** after `kill -9` of Firecracker, the started API records `error`, removes the TAP and nft rules, and the VM keeps its IPv4 address.
- [ ] **R6 — Interrupted start:** restarting the API after the shim appears but before `running` records `error` and leaves no unit.
- [ ] **R7 — Normal stop:** V11 records `stopped`, `exit.json` has `"stop_requested":true`, and no `firecrab-vm-*` unit is failed.

```sh
# R1: the shim, its parent, and its unit
VM=VM_ID
ps -o pid,ppid,user,args -p "$(pgrep -f "[v]m-shim --vm-id $VM")"
systemctl list-units --all --plain --no-legend 'firecrab-vm-*'
systemctl show -p MemoryMax -p CPUQuotaPerSecUSec "firecrab-vm-$(echo "$VM" | tr -d -).service"

# R2: stop the VM from outside the API
sudo systemctl stop "firecrab-vm-$(echo "$VM" | tr -d -).service"

# R4: restart the API and compare PIDs
pgrep -f "[v]m-shim --vm-id $VM"
sudo systemctl restart firecrab-api
pgrep -f "[v]m-shim --vm-id $VM"
journalctl -u firecrab-api -b | grep 'startup reconciliation finished' | tail -1

# R5: crash the VM while the API is down
sudo systemctl stop firecrab-api
sudo pkill -9 -f "firecracker --api-sock .*/$(echo "$VM" | tr -d -)/"
sudo systemctl start firecrab-api

# R7, X7
systemctl --failed --plain --no-legend | grep firecrab-vm- || echo none
```

Run `scripts/ci-qa-lifetime.sh [OCI reference]` for the automated lifetime pass.

## nginx combined scenario

Run `scripts/ci-qa-nginx.sh nginx:1.27-alpine`.
Use `QA_NGINX=ci`, a pinned POSIX shell, and TCP `18080 → 80`; update env to `QA_NGINX=two`.
Check `/etc/firecrab/vm.env` and executable `/var/lib/firecrab/shells/00.sh` over SSH.
On macOS, HTTP must work inside Debian and on the Mac relay; Windows must also return HTTP 200 on native localhost.

- [ ] **NGX1:** inspect and import the nginx image; alias becomes installed.
- [ ] **NGX2:** create a POSIX shell; receive 201 and a shell ID.
- [ ] **NGX3:** create a VM with an environment value, pinned shell, and HTTP port forward; receive 201.
- [ ] **NGX4:** start VM; state is running with IPv4; ping or the network-ready marker succeeds.
- [ ] **NGX5:** HTTP request through the host port returns 200.
- [ ] **NGX6:** V8a–V8d pass: PEM, matching host key, and root SSH login.
- [ ] **NGX6b:** SSH confirms the initial environment value inside the guest.
- [ ] **NGX7:** SSH confirms the pinned shell is executable.
- [ ] **NGX8:** update the environment while running; guest file changes.
- [ ] **NGX9:** stop/delete VM, delete imported alias, shell, and network; no leftovers.

## CLI

Commands, config paths, and precedence: [CLI reference](../reference/cli.md).
C12 uses an SSH tunnel to the remote loopback API; see [host profiles](../reference/cli.md#host-profiles).

- [ ] **C1 — VM:** list, create, start, stop, and delete; follow V* rules and end with no test VM.
- [ ] **C2 — Console:** attach and detach cleanly.
- [ ] **C2b — Console logout:** CLI prints `guest session ended`, exits 0, and leaves VM running.
- [ ] **C3 — Network:** list, create, and delete; follow N* rules and end with no test network.
- [ ] **C4 — Image:** list, inspect, import, check status, and delete the imported alias.
- [ ] **C5 — Host profiles:** add, list, use, show, and remove a test profile.
- [ ] **C6 — Config path:** check Unix/Windows default paths and the directory override.
- [ ] **C7 — Profile validation:** serialized profile mutations reject unknown keys.
- [ ] **C8 — Endpoint selection:** verify explicit API and saved-host precedence.
- [ ] **C9 — Platform commands:** verify Linux-only host administration and Apple silicon macOS service lifecycle.
- [ ] **C10 — Terminal:** raw terminal mode is restored on every platform after console exit.
- [ ] **C11 — Installer integrity:** verify release hash, safe symlink replacement, writable install, and executable search path.
- [ ] **C12 — Remote manual flow:** confirm Linux daemons and API; forward its loopback API over SSH from any client OS; use a saved host profile and confirm the config.

## Browser E2E: every Playwright case

Commands and fixtures: [Browser E2E](../../firecrab-e2e/README.md).
Use one worker and zero retries.
Import/form mode: **11 passed, 6 skipped** (B2/B6/B8/B10/B13 plus product-blocked B5).
Full runtime mode: **16 passed, 1 explicit skip** (B5).
Save traces and guest logs on failure; cleanup must remove suite-owned registrations and resources.

- [ ] **B1 — OCI inspect/import:** enter the local reference, inspect architecture compatibility, import, and confirm the registered installed alias.
- [ ] **B2 — OCI guest boot:** create/start a VM from the imported image; verify network and guest-service markers.
- [ ] **B3 — MicroRegistry import:** inspect/import the fixture as an installed image.
- [ ] **B4 — MicroRegistry register/conflict:** register it, confirm one versioned catalog row, then verify duplicate registration returns a conflict.
- [ ] **B5 — MicroRegistry failed job:** verify no current catalog row after a failed register job. **Product-blocked skip:** no real failure trigger exists yet.
- [ ] **B6 — MicroRegistry reinstall/boot:** delete the template, reinstall from the local row, and boot until the network-ready marker.
- [ ] **B7 — IPv6 form:** IPv6 fields stay hidden while Off; enabling them shows address/mode with SLAAC selected.
- [ ] **B8 — IPv6 networks:** create IPv4-only and auto-ULA dual-stack networks; verify IPv4-only has no IPv6 settings and dual-stack has ULA /64, SLAAC, and NAT66.
- [ ] **B9 — DHCP fixture import:** inspect/import the local DHCP-boot image.
- [ ] **B10 — DHCP dual-stack guest:** boot with a forwarded HTTP port; verify network-ready log, both IP families, and key-only root SSH over forwarded IPv4 and direct IPv6.
- [ ] **B11 — Console session end:** a simulated guest exit leaves the terminal at Session ended; New session attaches again.
- [ ] **B12 — Console reconnect:** other simulated socket closures reconnect automatically.
- [ ] **B13 — Real console reattachment:** exit ends both viewers; a new session executes a command while the VM stays running.
- [ ] **B14 — English reconciliation UI:** list and detail show all six API results, tooltips, timestamps, and unchecked VMs.
- [ ] **B15 — Korean reconciliation UI:** verify the same list/detail results and diagnostics in Korean.
- [ ] **B16 — Reconciliation polling:** a new start clears the previous API result in the list and detail.
- [ ] **B17 — Narrow reconciliation panel:** Korean diagnostics wrap without overflow and status tooltips remain accessible.

## Uninstall

Use a disposable Linux host with UFW, one network, and a running VM.
Compare ACLs, forwarding, and firewall rules with a preinstall snapshot.

- [ ] **UN1:** `firecrab service uninstall` or `./install.sh --uninstall` stops the VM first; no `firecracker` or `firecrab-api` process and no `firecrab-*` unit remains.
- [ ] **UN2:** `/usr/local/lib/firecrab` and `/usr/local/bin/firecrab` are gone.
- [ ] **UN3:** no `mnb*`, `fct*`, or `fcbr0` link, no firecrab nftables table, and no MASQUERADE rule for a MicroNetwork subnet.
- [ ] **UN4:** `ufw status numbered` names no firecrab bridge and keeps the rules that were there before.
- [ ] **UN5:** `/etc/firecrab/host-baseline.env` is gone, `getfacl -p /dev/kvm` matches the host before the install, and forwarding is restored unless another bridge or container network exists.
- [ ] **UN6:** without `--purge` the data and config stay and a reinstall finds the networks and VMs `stopped`; with `--purge` they are gone.
- [ ] **UN7:** a second uninstall changes nothing and exits `0`.

```sh
sudo firecrab service uninstall            # or: sudo ./install.sh --uninstall
pgrep -x firecracker || echo none          # UN1
pgrep -x firecrab-api || echo none
systemctl list-units --all --plain --no-legend 'firecrab-*' | grep . || echo none
ls /usr/local/lib/firecrab 2>&1 | head -1  # UN2: No such file or directory
ip -br link | grep -E '^(mnb|fct|fcbr)' || echo none                        # UN3
sudo iptables -t nat -S POSTROUTING | grep MASQUERADE || echo none
sudo ufw status numbered | grep -E 'mnb[0-9a-f]{12}' || echo none          # UN4
getfacl -p /dev/kvm; sysctl net.ipv4.ip_forward net.ipv6.conf.all.forwarding # UN5
```

## Cleanup and CI coverage

Inspect VM, network, storage, and shell lists after cleanup.
Keep logs and summaries; see [QA script coverage](qa.md#script-coverage).

- [ ] **X1:** no QA VM remains.
- [ ] **X2:** no QA network remains.
- [ ] **X3:** no QA storage pool remains.
- [ ] **X4:** no QA shell remains.
- [ ] **X5:** custom OCI alias is gone; retain catalog fixtures only by explicit choice.
- [ ] **X6:** no QA Docker Hub secret remains; restore any prior login.
- [ ] **X7:** no `firecrab-vm-*` unit remains for a QA VM.
- [ ] **X8:** forwarded TCP ports close after stopping/deleting QA VMs.
- [ ] **X9:** restore manually changed QA DNS/management SSH settings and save logs and result summaries.

## Related

- [CI](ci.md)
- [Validation history](validation-history.md)
- [Contributing](../../CONTRIBUTING.md)
