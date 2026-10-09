# Firecrab test checklist (English)

- Korean: [TEST.ko.md](TEST.ko.md)
- Scope: every named case in the [QA work list](qa.md), the cross-platform CLI verification checklist, and every browser case in the [Playwright suite](../firecrab-e2e/README.md).
- Record each applicable case as **PASS**, **FAILED**, or **WARNING**. Use **WARNING** for a skip or leftover; never count it as a pass.
- For resources, follow **create → use or change → delete → confirm no leftovers**. Prefix test names with the QA prefix and use a dedicated subnet per run.
- The shared API contract applies on Linux, macOS, and Windows.
- Apply an update only during U1. Delete only images installed during this run. Save and restore any previous Docker Hub login.
- API, CLI, and browser checks are separate runs. A browser guest-boot skip does not imply a successful guest boot.

```text
QA name prefix: qa-
API base URL:   http://127.0.0.1:5523
Update scope:   POST /api/update → U1 only
```

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
- [ ] **A20 — microManager settings and Sleepy:** validate settings save/discard and terminal restoration in a PTY; separately run `scripts/test-micromanager-sleepy.ps1 -CliPath ./firecrab.exe` on native Windows/WSL2 for actual idle shutdown, automatic wake, workload inhibition, and manual stop. See [settings and Sleepy](micromanager-settings.md). macOS runtime validation requires a native Mac.

Run workspace gates on Linux from the repository root.
The IDs show which checklist items each command covers:

```sh
# A1–A4
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --locked
cargo test -p firecrab-cli --locked
cargo llvm-cov --workspace --locked --lcov --output-path lcov.info

# A18 (Linux or macOS; uses stub build/service commands)
python3 scripts/test-micromanager-dev.py
python3 scripts/test-ci-qa-guest.py # A19

# A5
npm ci --prefix firecrab-frontend
npm run lint --prefix firecrab-frontend
npm run build --prefix firecrab-frontend

# A9, A14
python3 scripts/check-doc-links.py
python3 scripts/check-changelog.py
RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --document-private-items
python3 -m unittest scripts/test_check_changelog.py scripts/test_write_release_notes.py scripts/test_micromanager_pr_report.py
```

Installer and platform commands:

```sh
# A10, A15
shellcheck install.sh scripts/firecrab-release.sh
bash scripts/test-firecrab-release.sh
bash scripts/test-install-cli.sh
bash scripts/test-install-cli-release.sh
bash scripts/test-smoke-release-exec.sh
bash -n install.sh
./install.sh --help
./install.sh --check
./install.sh --doctor

# A11 (macOS)
swift test --package-path micromanager-macos --scratch-path target/swift-micromanager-tests

# A17 (run inside each supported distribution container)
./install.sh --check
./install.sh --deps-only
```

Native Windows CLI checks (A12), from PowerShell:

```powershell
cargo clippy -p firecrab-cli --all-targets -- -D warnings
cargo test -p firecrab-cli --locked
cargo build -p firecrab-cli --locked
pwsh -File scripts/test-install-cli.ps1
powershell.exe -NoProfile -ExecutionPolicy Bypass -File scripts/test-ci-qa-windows.ps1
.\target\debug\firecrab.exe service doctor --json
```

On macOS, run the same CLI Clippy/test commands and the Swift/helper checks in the [CI guide](ci.md).
API/helper workspace checks run on Linux; their source deployment builds inside the management guest.
Hosted doctor checks accept exit 0 or 1 for valid JSON; runtime G2/G3 still requires `ready: true`.

```text
A13 release targets:
x86_64-unknown-linux-gnu    x86_64-unknown-linux-musl
aarch64-unknown-linux-gnu   aarch64-unknown-linux-musl
aarch64-apple-darwin        x86_64-pc-windows-msvc
aarch64-pc-windows-msvc

A17 required tools: ip nft dnsmasq mkfs.ext4 firecracker sha256sum
```

## Platform and host

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
- [ ] **H1 — Update status:** read-only status check succeeds.
- [ ] **H2 — Host network:** an uplink is present; loopback and internal helper interfaces are absent from the picker.
- [ ] **U1 — Update apply (optional separate run):** apply update; host recovers and API returns 200.

Hosted macOS and Windows runners do build, unit, and diagnostic checks.
Runtime E2E needs a native M3-or-later Mac or a Windows host with WSL2, and doctor must report ready.
The QA wait factor extends nginx/SSH guest waits on slow first boots; it does not extend package-install or Playwright deadlines.

```sh
# G1: Linux
firecrab doctor
firecrab service install
firecrab service start
firecrab service status

# G2/G3: native macOS or Windows host
firecrab service doctor
firecrab service install
firecrab service status

# G6: native macOS or Windows host
firecrab service shell -- id -un                                     # root
firecrab service shell -- printf '[%s]\n' "it's here" 'a b' '$HOME'   # [it's here] [a b] [$HOME]
firecrab service shell -- sh -c 'exit 7'; echo $?                    # 7
firecrab service shell                                               # root login; `exit` leaves
```

Windows G6a–G6h: `cargo test -p firecrab-cli --test windows_service_shell -- --ignored --test-threads=1` on an installed WSL2 microManager host. The default-shell checks use redirected stdin; terminal keyboard/resize/Ctrl-C require a separate interactive pass.

```text
Runtime gate: doctor → ready: true
Slow guest:   FIRECRAB_QA_WAIT_FACTOR
H2 excludes:  lo, fct*, mnb*
```

Check the shared API before resource tests:

```sh
API=http://127.0.0.1:5523
curl -i "$API/api/host"             # G4: 200
curl -i "$API/"                    # G4: HTML 200
curl -i "$API/api/no-such-route"    # G5: JSON 404 with requestId
curl -i "$API/api/update"           # H1: read-only check
curl -i "$API/api/network"          # H2: uplink list
```

## MicroNetwork

- [ ] **N1 — IPv4 create/delete:** creation returns 201; deletion returns 204; detail then returns 404.
- [ ] **N2 — List/detail:** both views contain the created network.
- [ ] **N3 — Internet toggle:** disable, then re-enable internet access.
- [ ] **N4 — IPv6 SLAAC:** create a ULA /64 network with SLAAC, then delete it.
- [ ] **N5 — Invalid uplink:** an empty uplink is rejected with 400 and leaves no row.
- [ ] **N6 — Busy delete:** deleting a network attached to a VM returns 409; delete the VM, then the network.

Example N1–N3 lifecycle on an unused test subnet (change the CIDR if it overlaps an existing network):

```sh
API=http://127.0.0.1:5523
NET_ID=$(curl -fsS -X POST "$API/api/micro-networks" \
  -H "Origin: $API" -H 'content-type: application/json' \
  -d '{"name":"qa-doc-net","subnetCidr":"172.31.230.0/24","internetEnabled":true}' \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')
curl -fsS "$API/api/micro-networks/$NET_ID"
curl -fsS -X PATCH "$API/api/micro-networks/$NET_ID" \
  -H "Origin: $API" -H 'content-type: application/json' \
  -d '{"internetEnabled":false}'
curl -fsS -X PATCH "$API/api/micro-networks/$NET_ID" \
  -H "Origin: $API" -H 'content-type: application/json' \
  -d '{"internetEnabled":true}'
curl -sS -o /dev/null -w '%{http_code}\n' -X DELETE "$API/api/micro-networks/$NET_ID"
```

Other network requests and expected responses:

```http
N4  POST /api/micro-networks
    {"name":"qa-ipv6","subnetCidr":"172.31.231.0/24","internetEnabled":true,"ipv6AddressMode":"slaac"}
    → 201; IPv6 ULA /64
N5  POST /api/micro-networks with "uplink":"" → 400; no row
N6  DELETE /api/micro-networks/{id} with attached VM → 409
```

## MicroStorage

- [ ] **S1 — Roots:** root list includes the default pool.
- [ ] **S2 — Devices:** host device list loads.
- [ ] **S3 — Pool lifecycle:** register an absolute host path (201), delete it (204), and confirm detail returns 404. On macOS/Windows, use a path inside the Debian management guest.

```http
S1  GET    /api/storage                         → contains "default"
S2  GET    /api/storage/devices
S3  POST   /api/micro-storages                  → 201
    DELETE /api/micro-storages/{id}             → 204
    GET    /api/micro-storages/{id}             → 404
```

## Shells

- [ ] **L1 — Create/delete:** create a named POSIX shell (201), then delete it (204).
- [ ] **L2 — Revision:** add a new shell revision.
- [ ] **L3 — Read:** fetch the shell and its revision body.
- [ ] **L4 — Pin to VM:** assign a shell on VM creation or update; confirm its executable file inside the guest.

```http
L1  POST   /api/shells                           → 201; body uses /bin/sh
    DELETE /api/shells/{id}                     → 204
L2  POST   /api/shells/{id}/revisions
L3  GET    /api/shells/{id} and its revision
L4  VM create: shellIds
    PUT    /api/vms/{id}/shells
    Guest:  /var/lib/firecrab/shells/00.sh
```

## Images, kernels, OCI, and registry

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

```http
I1   GET    /api/images | /api/kernels | /api/microregistry
I2   POST   /api/kernels/{version}/install       → succeeded
     DELETE paired kernel                      → 409 in_use
I3   POST   /api/images/{alias}/package, then /install
I4   PUT    /api/images/{alias}/kernel           → 409 kernel_required if cache missing
I5   GET    /api/oci/inspect?reference=…
I6   POST   /api/oci/import; poll /import/{alias}
     DELETE /api/images/{alias}
I7   POST   /api/microregistry/register
I8   Docker Hub: GET; optional PUT, then DELETE → configured=false or prior login
I9   GET    /api/images/does-not-exist           → 404
I10  POST   /api/images/{alias}/bootstrap
     DELETE /api/images/bootstrap/{id}          → builder VM removed
```

```text
I11 OCI references: alpine:3.21 | ubuntu:24.04 | fedora:42
OCI ext4 packing: fakeroot required on the Firecrab host
```

I2, I3, I4, I7, I10, and U1 are outside GitHub Ubuntu CI.

## MicroVM and SSH

Create an installed template (I3/I6) and network (N1) before the VM lifecycle.

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

```http
V1   POST   /api/vms                         → name, template, cpu, ram, disk, microNetworkId, env
V2   GET    /api/vms; GET /api/vms/{id}      → created
V3   Create or PUT env                       → guest /etc/firecrab/vm.env
V4   Create or PUT portForwards              → curl host port after start
V5   Create or PUT shellIds                  → guest /var/lib/firecrab/shells/00.sh
V6   PUT    /api/vms/{id}/storage
V7   POST   /api/vms/{id}/start              → running; FIRECRAB_NETWORK_READY
V8a  GET    /api/vms/{id}/ssh-key            → firecrab-<name>.pem
V8b  GET    /api/vms/{id}/ssh-host-key       → fingerprint, publicKey
V8c  GET    /api/vms/{id}/ssh-host-key/check → status=match
V9   GET    /ws/vms/{id}/console             → 101
V10  PUT    env while running                → services.d/app restarts
V11  POST   /api/vms/{id}/stop               → stopped
V12  DELETE /api/vms/{id} while running      → rejected
V13  DELETE /api/vms/{id}                    → 204; later GET → 404
V14  POST   /api/vms without microNetworkId → 400
```

```sh
# V8d: replace the key path and guest address with values from V8a and V7
GUEST_IP=172.31.230.2  # replace with the guest's IPv4 address
ssh -i firecrab-qa-vm.pem -o IdentitiesOnly=yes "root@$GUEST_IP" true
ssh -i firecrab-qa-vm.pem -o IdentitiesOnly=yes "root@$GUEST_IP" uname -s
```

CPU, RAM, disk, and egress edits apply only in created, stopped, or error. Environment edits also apply in running.

## VM lifetime

Run these on a running VM (V7) with root on the API host; on macOS, run the host commands in the management VM over SSH.
Every VM runs in its own `firecrab-vm-<simple id>.service` unit.

- [ ] **R1 — Shim in its unit:** `firecrab-api vm-shim --vm-id <id>` is Firecracker's parent and the main process of an active `firecrab-vm-<simple id>.service`; its parent is PID 1, it runs as the API user, the runtime directory has `shim.sock` and `console.log`, and the unit's `MemoryMax` is the guest RAM plus the larger of 256 MiB and an eighth of it and its `CPUQuota` is one core per vCPU plus one.
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
VM=<vm id>
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

`scripts/ci-qa-lifetime.sh [OCI reference]` runs R1–R7 and X7 and imports the image when it is missing.

## nginx combined scenario

Run the nginx QA script to cover OCI, shell, VM, environment, forwarding, and SSH together.

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

```sh
scripts/ci-qa-nginx.sh nginx:1.27-alpine
curl -i http://127.0.0.1:18080/  # NGX5 → 200
```

```text
NGX3:  QA_NGINX=ci; shellIds; portForwards 18080→80/tcp
NGX4:  FIRECRAB_NETWORK_READY
NGX6b: cat /etc/firecrab/vm.env → QA_NGINX=ci
NGX7:  test -x /var/lib/firecrab/shells/00.sh
NGX8:  PUT env QA_NGINX=two → guest file updates
```

On macOS, verify NGX5 both inside the management VM and on the Mac loopback relay; proxy NGX6–NGX8 SSH through that VM.

## CLI

- [ ] **C1 — VM:** list, create, start, stop, and delete; follow V* rules and end with no test VM.
- [ ] **C2 — Console:** attach and detach cleanly.
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

Linux-only commands run on Linux or inside the management guest. Mark them inapplicable on macOS/Windows host CLI.

```text
C1   firecrab vm list | create | start | stop | delete
C2   firecrab vm console
C3   firecrab network list | create | delete
C4   firecrab image list | inspect | import | import-status
C5   firecrab host add | list | use | show | remove
C6   Unix: ~/firecrab/config.toml
     Windows: %USERPROFILE%\firecrab\config.toml
     Override: FIRECRAB_CONFIG_DIR
C8   --api versus --host
C9   macOS: service install | start | stop | status | reinstall | uninstall
     Linux only: doctor | info | status | update --check | update --apply
     Linux systemd: service start | stop | restart | enable | disable
C11  SHA-256 verification; safe installer symlink replacement; user-writable install; PATH
C12  config: current_host = "dev"
```

For C12, replace the SSH host address before running:

```sh
# On the Linux host
sudo systemctl status firecrab-api firecrab-helper
curl -fsS http://127.0.0.1:5523/api/host

# On a Linux, macOS, or Windows client with SSH
ssh -fN -L 15523:127.0.0.1:5523 user@firecrab-host
firecrab host add dev http://127.0.0.1:15523 --use
firecrab host show
firecrab vm list
firecrab host list
```

## Browser E2E: every Playwright case

Use a local registry fixture; Docker Hub is not required. The suite starts the API and Vite if needed.

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

The import/form-only mode skips B2, B6, B8, B10, and B13; B5 remains product-blocked: **11 passed, 6 skipped**.
The full guest path expects **16 passed, 1 existing explicit skip** and needs KVM, Firecracker, a working network helper socket, and SSH tools for B10.
Cleanup removes suite-owned VMs, networks, aliases, packages, and local catalog registrations; a remaining registration fails setup.
Tests use one worker and zero retries; guest boot is skipped only by explicit configuration.

```text
Dashboard origin: http://localhost:8080
API:              http://127.0.0.1:5523
B2/B6:            FIRECRAB_NETWORK_READY
B2:               FIRECRAB_OCI_E2E_READY
B4:               409 alias_collision on duplicate registration
B8:               ULA /64; SLAAC; NAT66
B10:              80:18888/tcp
```

Run the import/form cases first:

```sh
npm ci --prefix firecrab-e2e
npm run test:runner --prefix firecrab-e2e # A19
npm ci --prefix firecrab-frontend
npm run install-browsers --prefix firecrab-e2e
FIRECRAB_E2E_SKIP_GUEST_BOOT=1 npm test --prefix firecrab-e2e
```

For guest-boot cases, start the helper in one terminal, then run the suite in another:

```sh
# Terminal 1
./scripts/dev-net-helper.sh

# Terminal 2 (with KVM and Firecracker available)
npm test --prefix firecrab-e2e
```

On Windows, run source deployment and browser E2E from PowerShell:

```powershell
cargo build -p firecrab-cli --locked
.\scripts\ci-qa-windows-e2e.ps1 -Phase browser -Cli .\target\debug\firecrab.exe -Source .
.\scripts\ci-qa-windows-e2e.ps1 -Phase nginx -Cli .\target\debug\firecrab.exe -Source . -WaitFactor 3
```

The browser, Vite, and fixture run inside managed WSL with real guest boot and API reuse enabled.
nginx separately requires HTTP 200 from the native Windows forwarded port.
Windows `all` requires gate/setup, then collects API → nginx → guest → browser results even after test failures, and fails overall if any phase fails.
The guest script also checks later image references after a failed reference.
The Windows transcript, summary, and browser archive are retained under `target/qa/windows/<run-id>` or `-ResultsDir`.
For macOS browser commands and management SSH settings, see the [E2E guide](../firecrab-e2e/README.md).

## Uninstall

On a disposable Linux host with UFW active, one MicroNetwork, and one running MicroVM, compare the host after the uninstall with the host before the install.

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

- [ ] **X1:** no QA VM remains.
- [ ] **X2:** no QA network remains.
- [ ] **X3:** no QA storage pool remains.
- [ ] **X4:** no QA shell remains.
- [ ] **X5:** custom OCI alias is gone; retain catalog fixtures only by explicit choice.
- [ ] **X6:** no QA Docker Hub secret remains; restore any prior login.
- [ ] **X7:** no `firecrab-vm-*` unit remains for a QA VM.
- [ ] **X8:** forwarded TCP ports close after stopping/deleting QA VMs.
- [ ] **X9:** restore manually changed QA DNS/management SSH settings and save logs and result summaries.

Inspect the four resource lists after cleanup:

```sh
API=http://127.0.0.1:5523
curl -fsS "$API/api/vms"
curl -fsS "$API/api/micro-networks"
curl -fsS "$API/api/micro-storages"
curl -fsS "$API/api/shells"
```

CI script coverage:

```text
scripts/ci-qa-api.sh: G4–G5 H1–H2 N1–N5 S1–S3 L1–L3 I1 I8–I9 V14 C3 C5 X1–X4 X6
scripts/ci-qa-nginx.sh: NGX1–NGX9, including V8a–V8d
scripts/ci-qa-ssh.sh: V8a–V8d from guest and nginx runs
scripts/ci-qa-guest.sh: I5–I6 V1–V2 V6–V9 V11–V13 N6 C1–C2 C4 X5
scripts/ci-qa-lifetime.sh: R1–R7 X7, root commands on the API host (macOS: management VM SSH)
scripts/ci-qa-macos-e2e.sh: native-Mac manual gate/shell/browser/API/nginx/guest/lifetime phases (G6 included)
scripts/ci-qa-windows-e2e.ps1: manual Windows gate, optional source deployment, G6 shell, WSL browser/API/nginx/guest; native HTTP
```

Expanded guest checks run on the first OCI reference.
The current CI workflow runs Linux API/nginx/public-image guest QA and complete Chromium E2E; ARM64 KVM runtime has no registered job.
Linux CI retains logs, JSON/JUnit results, and failure traces for 14 days.
The [Windows source validation snapshot](qa.md#windows-source-validation-2026-10-03) records 9 browser passes, the existing B5 skip, and separate Ubuntu/Fedora SSH failures.
That result does not establish a passing Windows `all` run or stock pinned-release runtime.

## Related

- [QA source list](qa.md)
- [CI and runtime E2E](ci.md)
- [Browser E2E setup](../firecrab-e2e/README.md)
- [Contributing](../CONTRIBUTING.md)

`FIRECRAB_QA_HELPER_UPGRADE=1` enables the R4f installer migration in the
Linux CI lifetime job; it requires a disposable GitHub Actions host.
