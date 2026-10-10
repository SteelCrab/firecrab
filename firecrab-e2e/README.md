# Browser E2E

Isolated Playwright suite.

- [#90](https://github.com/SteelCrab/firecrab/issues/90) OCI import
- [#108](https://github.com/SteelCrab/firecrab/issues/108) MicroRegistry register
- [#146](https://github.com/SteelCrab/firecrab/issues/146) MicroNetwork IPv6
- OCI DHCP boot (busybox `udhcpc`, same path as nginx-stable)
- [#303](https://github.com/SteelCrab/firecrab/issues/303) console session end: `exit` stops the web terminal at "Session ended"
- `npm run test:dashboard` runs only `@dashboard` specs, which fake the API or console and need no KVM (CI job `e2e-dashboard`)
- Local OCI registry fixture only — no Docker Hub
- Playwright is a test-only dependency of this package, not of `firecrab-frontend`

## Contents

- [What it covers](#what-it-covers)
- [Setup](#setup)
- [Run](#run)
- [Fixture](#fixture)
- [Environment](#environment)
- [Results and CI coverage](#results-and-ci-coverage)
- [Related](#related)

## What it covers

1. Type `127.0.0.1:15555/firecrab/e2e:ready` on Images
2. Inspect — host must accept the fixture architecture
3. Import — poll until the derived alias is registered
4. Create and start a VM from that alias
5. Assert `FIRECRAB_NETWORK_READY` and `FIRECRAB_OCI_E2E_READY` on the console
6. Networks: IPv6 select defaults to Off; create IPv4-only and auto-ULA dual-stack networks
7. OCI DHCP: import fixture → create network → VM with `80:18888/tcp` → start → `FIRECRAB_NETWORK_READY` and an IPv4 on the detail panel
8. MicroRegistry: register a custom image → delete its installed template → reinstall the local package → boot that image

- `FIRECRAB_E2E_SKIP_GUEST_BOOT=1`: skip guest-boot half
- Inspect and import still run
- Full suite: **17 cases, including 1 existing explicit skip**; import/form-only mode also skips guest boot checks
- The failed-register-job case is always skipped because no real dashboard/API failure trigger exists

## Setup

```sh
npm ci --prefix firecrab-frontend
npm ci --prefix firecrab-e2e
npm run install-browsers --prefix firecrab-e2e
```

- Chromium
- `python3`
- Fixture: `scripts/oci-e2e-registry.py` at the repo root

## Run

Host Firecrab panel only (H3, Linux/macOS/Windows):

```sh
npm run test:host --prefix firecrab-e2e
```

Expect **2 passed**: info fields/panel placement and API-unavailable messaging.
This command starts only Vite and mocks all API responses. It needs no running
API, management VM, or Docker. It fails if port 8080 is already occupied instead
of reusing another checkout's frontend. Stop Vite before this command.
Verify the real `/api/info` response separately for complete H3 runtime QA.

Inspect and import only:

```sh
FIRECRAB_E2E_SKIP_GUEST_BOOT=1 npm test --prefix firecrab-e2e
```

- Expect **1 passed, 1 skipped** for the import spec (other specs in `tests/` also run)

Full path (KVM, `firecracker` on `PATH`, live net helper):

```sh
./scripts/dev-net-helper.sh    # terminal session 1; socket /run/firecrab/net-helper.sock
npm test --prefix firecrab-e2e
```

On macOS, start the source-built services and use the management VM for the
Linux OCI fixture and IPv6 SSH connection:

```sh
./target/debug/firecrab service dev
FIRECRAB_MICROMANAGER_HOME="$HOME/Library/Application Support/Firecrab/micromanager" \
  ./scripts/ci-qa-macos-e2e.sh browser
```

Chromium and frontend dependencies must be installed first. The browser runs
on the Mac; guest boot and the registry run inside Debian. The fixture exits
when its controlling SSH connection closes.

On Windows, use PowerShell from the checkout root:

```powershell
cargo build -p firecrab-cli --locked
.\scripts\ci-qa-windows-e2e.ps1 -Phase browser -Cli .\target\debug\firecrab.exe -Source .
```

`-Source .` runs the capability/install gate and deploys the source API/helper before Chromium.
The browser, Vite, and registry run inside `firecrab-debian`; Windows `node_modules` are excluded and Linux dependencies are installed there.
The runner reuses the managed API with `FIRECRAB_E2E_REUSE_SERVER=1` and keeps guest boot enabled.
Workload IPv4 forwarding and direct IPv6 SSH use WSL routes without a management SSH proxy.
Use the separate `nginx` phase to verify the forwarded HTTP port from native Windows.
See [CI runtime commands](../public-docs/ci.md#windows-runtime-e2e) for all phases and options.

MicroRegistry register ([#108](https://github.com/SteelCrab/firecrab/issues/108)), skip guest boot:

```sh
FIRECRAB_E2E_SKIP_GUEST_BOOT=1 npm run test:register --prefix firecrab-e2e
```

- Expect **2 passed, 2 skipped** (import + register/409; guest boot is disabled and failed-job cleanup has API unit coverage)
- Without the guest-boot skip flag: **3 passed, 1 skipped**, including local package reinstall and actual guest boot
- Cleanup removes the imported disk, staged package, and this run's local catalog registration

MicroNetwork IPv6 ([#146](https://github.com/SteelCrab/firecrab/issues/146)), form only:

```sh
FIRECRAB_E2E_SKIP_GUEST_BOOT=1 npm run test:ipv6 --prefix firecrab-e2e
```

- Expect **1 passed, 1 skipped**

Create IPv4-only and auto-ULA dual-stack:

```sh
./scripts/dev-net-helper.sh    # terminal session 1
npm run test:ipv6 --prefix firecrab-e2e
```

- Expect **2 passed**
- `afterAll` deletes `ipv6-e2e-v4` and `ipv6-e2e-v6`
- Needs a helper the API process can connect to (`/run/firecrab/net-helper.sock`)
- systemd `firecrab-api` runs as user `firecrab`; a debug helper that recreates the socket as `root:pista` makes create return 500

OCI DHCP boot (busybox `udhcpc`, nginx-stable path), form only:

```sh
FIRECRAB_E2E_SKIP_GUEST_BOOT=1 npm --prefix firecrab-e2e run test:dhcp
```

- Expect **1 passed, 1 skipped**

Create a dedicated dual-stack network, start the imported guest with `80:18888/tcp`, and prove
IPv4 and IPv6 SSH authentication:

```sh
./scripts/dev-net-helper.sh    # terminal session 1
npm --prefix firecrab-e2e run test:dhcp
```

- Expect **2 passed**
- Guest boot opens the SSH tab, downloads the per-VM key, forwards host port `18022` to guest
  `22/tcp`, and proves key-only root login over both forwarded IPv4 and the guest's direct IPv6
- `afterAll` deletes `oci-e2e-dhcp` (VM, network, imported alias)
- An orphan dnsmasq holding `:67` fails the boot half with `FIRECRAB_NETWORK_FAILED no-ipv4-address`
- Needs a helper the API process can connect to (`/run/firecrab/net-helper.sock`)
- Needs the OpenSSH client (`ssh`), server (`/usr/sbin/sshd`), and `ldd` on the host; the local
  fixture packages the host server and its runtime libraries without contacting an external registry
- Requires working KVM on the Linux Firecrab host, directly or inside a capable macOS/Windows management guest
- Linux installer CI runs the complete Chromium suite; ARM64 KVM runtime still needs a separate capable host

Playwright:

- Starts `firecrab-api` on `:5523` and Vite on `:8080` if needed
- Reuses existing servers outside CI; inside CI, reuse requires `FIRECRAB_E2E_REUSE_SERVER=1`
- Dashboard origin: `http://localhost:8080`
- `127.0.0.1:8080` is a different CORS origin and fails
- `ensure-api.mjs` copies the Ubuntu catalog kernel into `images/kernel/` as a regular file (`O_NOFOLLOW`)
- Static busybox on disk: sets `FIRECRAB_OCI_TOOLBOX_PATH` so toolbox install does not reach a public registry

## Fixture

```sh
python3 scripts/oci-e2e-registry.py --port 15555
```

- First stdout line: JSON `reference`, `alias`, `ready`, `architecture`
- Announcement deadline: 60 seconds, including assembly of the host OpenSSH server and shared libraries
- Image entrypoint prints `FIRECRAB_OCI_E2E_READY` as a guest service, not PID 1
- SIGINT or SIGTERM: stop listener, delete scratch blobs
- Playwright `afterAll`: stop fixture; delete VM, imported template, or MicroNetwork this suite created

## Environment

| Variable | Default | Role |
| --- | --- | --- |
| `FIRECRAB_E2E_SKIP_GUEST_BOOT` | unset | Skip VM create/start when `1` / `true` / `yes` |
| `FIRECRAB_E2E_REUSE_SERVER` | unset | Reuse API/Vite even in CI when `1` / `true` |
| `FIRECRAB_E2E_REQUIRE_GUEST_BOOT` | unset | Reject a guest-boot skip when `1` / `true` / `yes` |
| `FIRECRAB_E2E_REQUIRE_RUNNING_API` | unset | Fail instead of starting a replacement API when `1` / `true` / `yes` |
| `FIRECRAB_OCI_E2E_PORT` | `15555` | Loopback registry port |
| `FIRECRAB_OCI_DHCP_E2E_PORT` | `15557` | DHCP-boot spec registry port |
| `FIRECRAB_E2E_BASE_URL` | `http://localhost:8080` | Dashboard origin |
| `FIRECRAB_E2E_API_URL` | `http://127.0.0.1:5523` | API used for cleanup |
| `FIRECRAB_QA_MANAGER_HOST` | unset | Management VM IP for OCI fixtures and IPv6 SSH |
| `FIRECRAB_QA_MANAGER_KEY` | unset | Management VM key; set together with the host |

- Suite does not infer `/dev/kvm`
- Unset the skip flag only on a host that can boot a guest

## Results and CI coverage

Native Windows `service shell` E2E lives in `firecrab-cli/tests/windows_service_shell.rs`:

```powershell
cargo test -p firecrab-cli --test windows_service_shell -- --ignored --test-threads=1
```

It uses the actual CLI and managed WSL distribution for eight root/argument/stdin/stdio/exit/recovery checks, with a 30-second limit per command. Set `FIRECRAB_WINDOWS_SHELL_CLI` to test another native executable. Hosted Windows CI compiles these tests; runtime execution requires an installed microManager.
The real console session test attaches two viewers, waits for the guest shell prompt, exits the shell, requires both viewers to stop, then reattaches and proves a new command executes while the VM remains running. Reattachment waits for a prompt after the new-session banner: a connected socket or banner alone does not mean the respawned shell can accept input.
The DHCP boot test also waits for a live host-key `match` before authenticating through the IPv4 port forward and over IPv6. DHCP readiness can precede sshd startup and host-key generation; the authentication checks retain their own deadlines.

Tests use one worker and zero retries; unavailable KVM does not automatically skip guest boot.
Every run writes `test-results/results.json` and `test-results/junit.xml`; failures also retain traces and screenshots.
Windows staging uses `/root/firecrab-qa/<run-id>` and retains a transcript, phase summary, and browser archive under `target/qa/windows/<run-id>` or `-ResultsDir`.
Linux installer CI uploads these results and shared QA logs for 14 days, even on failure.
Cleanup removes suite-owned VMs, networks, imported aliases, staged packages, and local catalog registrations.

The [2026-10-03 Windows source run](../public-docs/qa.md#windows-source-validation-2026-10-03) passed 9 cases with the single existing skip, including real boot and IPv4/IPv6 SSH.
This was Linux Chromium inside WSL on an x86_64 QEMU Windows host.
Public-image Ubuntu/Fedora SSH QA failed separately; the browser result does not establish a passing `-Phase all` run.
The [2026-10-04 continuation](../public-docs/micromanager-windows.md#source-qa-continuation-2026-10-04) passed that source snapshot’s 13-case suite: **12 passed, 1 existing explicit skip, 0 failed**, with one worker and zero retries. Console reattachment and IPv4/IPv6 SSH authentication passed. Public-image Alpine/Ubuntu/Fedora guest QA also passed separately; Windows `-Phase all` was not rerun.
The Windows runner now completes the remaining test phases after failures and reports an aggregate failure.
See the [CI guide](../public-docs/ci.md) for automated job coverage.

## Related

- [OCI images](../public-docs/oci.md)
- [Dashboard](../public-docs/dashboard.md)
- [Networking](../public-docs/networking.md)
- [API](../public-docs/api.md)
