# Browser E2E

Isolated Playwright suite.

- [#90](https://github.com/SteelCrab/firecrab/issues/90) OCI import
- [#108](https://github.com/SteelCrab/firecrab/issues/108) MicroRegistry register
- [#146](https://github.com/SteelCrab/firecrab/issues/146) MicroNetwork IPv6
- OCI DHCP boot (busybox `udhcpc`, same path as nginx-stable)
- Local OCI registry fixture only — no Docker Hub
- Playwright is a test-only dependency of this package, not of `firecrab-frontend`

## Contents

- [What it covers](#what-it-covers)
- [Setup](#setup)
- [Run](#run)
- [Fixture](#fixture)
- [Environment](#environment)
- [Related](#related)

## What it covers

1. Type `127.0.0.1:15555/firecrab/e2e:ready` on Images
2. Inspect — host must accept the fixture architecture
3. Import — poll until the derived alias is registered
4. Optional: create and start a VM from that alias
5. Optional: assert `FIRECRAB_NETWORK_READY` and `FIRECRAB_OCI_E2E_READY` on the console
6. Networks: IPv6 select defaults to Off; optional create of IPv4-only and auto-ULA dual-stack
7. OCI DHCP: import fixture → create network → VM with `80:18888/tcp` → start → `FIRECRAB_NETWORK_READY` and an IPv4 on the detail panel

- `FIRECRAB_E2E_SKIP_GUEST_BOOT=1`: skip guest-boot half
- Inspect and import still run

## Setup

```sh
npm install --prefix firecrab-e2e
npm run install-browsers --prefix firecrab-e2e
```

- Chromium
- `python3`
- Fixture: `scripts/oci-e2e-registry.py` at the repo root

## Run

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

MicroRegistry register ([#108](https://github.com/SteelCrab/firecrab/issues/108)), skip guest boot:

```sh
FIRECRAB_E2E_SKIP_GUEST_BOOT=1 npm run test:register --prefix firecrab-e2e
```

- Expect **2 passed, 2 skipped** (import + register/409; failed-job and reinstall/boot are product-gated)
- Leftover `127.0.0.1-15556-firecrab-e2e-ready` catalog row fails `beforeAll` until L3 grows a DELETE

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
- Requires a native KVM host for each architecture; scheduled CI runs this path on x86_64 and on
  the gated `self-hosted,linux,arm64,kvm` runner when `ENABLE_M2_SELF_HOSTED=true`

Playwright:

- Starts `firecrab-api` on `:5523` unless it already answers
- Starts Vite on `:8080` unless it already answers
- Dashboard origin: `http://localhost:8080`
- `127.0.0.1:8080` is a different CORS origin and fails
- `ensure-api.mjs` copies the Ubuntu catalog kernel into `images/kernel/` as a regular file (`O_NOFOLLOW`)
- Static busybox on disk: sets `FIRECRAB_OCI_TOOLBOX_PATH` so toolbox install does not reach a public registry

## Fixture

```sh
python3 scripts/oci-e2e-registry.py --port 15555
```

- First stdout line: JSON `reference`, `alias`, `ready`, `architecture`
- Image entrypoint prints `FIRECRAB_OCI_E2E_READY` as a guest service, not PID 1
- SIGINT or SIGTERM: stop listener, delete scratch blobs
- Playwright `afterAll`: stop fixture; delete VM, imported template, or MicroNetwork this suite created

## Environment

| Variable | Default | Role |
| --- | --- | --- |
| `FIRECRAB_E2E_SKIP_GUEST_BOOT` | unset | Skip VM create/start when `1` / `true` / `yes` |
| `FIRECRAB_OCI_E2E_PORT` | `15555` | Loopback registry port |
| `FIRECRAB_OCI_DHCP_E2E_PORT` | `15557` | DHCP-boot spec registry port |
| `FIRECRAB_E2E_BASE_URL` | `http://localhost:8080` | Dashboard origin |
| `FIRECRAB_E2E_API_URL` | `http://127.0.0.1:5523` | API used for cleanup |

- Suite does not infer `/dev/kvm`
- Unset the skip flag only on a host that can boot a guest

## Related

- [OCI images](../public-docs/oci.md)
- [Dashboard](../public-docs/dashboard.md)
- [Networking](../public-docs/networking.md)
- [API](../public-docs/api.md)
