# Browser E2E

Isolated Playwright suite for [issue #90](https://github.com/SteelCrab/firecrab/issues/90) (OCI import) and [issue #108](https://github.com/SteelCrab/firecrab/issues/108) (MicroRegistry register).
It drives the dashboard against a **local** OCI registry fixture.
Nothing is pulled from Docker Hub.

Playwright is a test-only dependency of this package.
It is not added to `firecrab-frontend`.

## What it covers

1. Type `127.0.0.1:15555/firecrab/e2e:ready` on Images.
2. Inspect — the host must accept the fixture architecture.
3. Import — poll until the derived alias is registered.
4. (Optional) Create and start a VM from that alias.
5. (Optional) Assert `FIRECRAB_NETWORK_READY` and `FIRECRAB_OCI_E2E_READY` on the console.

The guest-boot half is skipped when `FIRECRAB_E2E_SKIP_GUEST_BOOT=1`.
Inspect and import still run.

## Setup

```sh
npm install --prefix firecrab-e2e
npm run install-browsers --prefix firecrab-e2e
```

Chromium and `python3` are required.
The fixture script is `scripts/oci-e2e-registry.py` at the repo root.

## Run

Inspect and import only:

```sh
FIRECRAB_E2E_SKIP_GUEST_BOOT=1 npm test --prefix firecrab-e2e
```

Expect **1 passed, 1 skipped**.

Full path (KVM, `firecracker` on `PATH`, and a live net helper):

```sh
./scripts/dev-net-helper.sh    # other terminal; socket /run/firecrab/net-helper.sock
npm test --prefix firecrab-e2e
```

MicroRegistry register ([#108](https://github.com/SteelCrab/firecrab/issues/108)), skip guest boot:

```sh
FIRECRAB_E2E_SKIP_GUEST_BOOT=1 npm run test:register --prefix firecrab-e2e
```

Expect **2 passed, 2 skipped** (import + register/409; failed-job and reinstall/boot are product-gated).
A leftover `127.0.0.1-15556-firecrab-e2e-ready` catalog row fails `beforeAll` until L3 grows a DELETE.

Playwright starts `firecrab-api` on `:3000` and Vite on `:8080` unless those
ports already answer.
Open the dashboard as `http://localhost:8080`.
`127.0.0.1:8080` is a different CORS origin and will fail.

The API helper copies the Ubuntu catalog kernel into `images/kernel/` as a
regular file (import opens it with `O_NOFOLLOW`).
If a static busybox is already on disk it sets `FIRECRAB_OCI_TOOLBOX_PATH`
so toolbox install does not reach a public registry.

## Fixture

```sh
python3 scripts/oci-e2e-registry.py --port 15555
```

The first stdout line is JSON: `reference`, `alias`, `ready`, `architecture`.
The image entrypoint prints `FIRECRAB_OCI_E2E_READY` as a guest service,
not as PID 1.

SIGINT or SIGTERM stops the listener and deletes scratch blobs.
The Playwright `afterAll` hook also stops the fixture and deletes any VM,
imported template, or MicroNetwork this suite created.

## Environment

| Variable | Default | Role |
| --- | --- | --- |
| `FIRECRAB_E2E_SKIP_GUEST_BOOT` | unset | Skip VM create/start when `1` / `true` / `yes` |
| `FIRECRAB_OCI_E2E_PORT` | `15555` | Loopback registry port |
| `FIRECRAB_E2E_BASE_URL` | `http://localhost:8080` | Dashboard origin |
| `FIRECRAB_E2E_API_URL` | `http://127.0.0.1:3000` | API used for cleanup |

The suite does not infer `/dev/kvm`.
Unset the skip flag only on a host that can actually boot a guest.

## Related

- [OCI images](../public-docs/oci.md)
- [Dashboard](../public-docs/dashboard.md)
- [API](../public-docs/api.md)
