# Firecrab WebAssembly frontend

The default Firecrab dashboard, written in Rust and compiled to WebAssembly. The WASM module owns routing,
application state, HTML rendering, delegated events, forms, validation, HTTP
requests, polling, and stale-response rejection. The repository no longer ships a React frontend.

The JavaScript entry point loads WASM and lazily loads the Ghostty adapter. Rust
owns WebSocket connections, retry state, preferences, clipboard access, downloads
and log bundles. The adapter only connects the Ghostty library to Rust. WebAssembly accesses browser APIs through
[`wasm-bindgen` and `web-sys`](https://rustwasm.github.io/docs/wasm-bindgen/examples/fetch.html).
CSS and public assets belong to this package.

## CODE workspaces

The CODE navigation group opens `#/code`. Saved workspaces can also be opened at
`#/code/{workspace-uuid}`. The dashboard searches names, repositories,
owners and branches. It reads the configuration and selected runtime every three
seconds, with navigation fences for late responses. A failed read removes stale
runtime readiness; access failures also remove previously displayed records.

The view shows current GitHub/AI AUTH account references. CODE hosts the existing
Pipe AI account manager without leaving the current setup or copying credentials.
ChatGPT login, API-key replacement, disconnect and pending-login cancellation
use that shared service. Account presence is not agent authentication.
**Choose AI AUTH** updates a saved workspace's agent/account reference with a
revision check and interrupted-save recovery. Removing the assignment keeps the
shared account connected. `#/code/{workspace-uuid}/editor` displays the same live
AUTH states. Its editor, terminal and agent controls remain disabled until their
runtime adapters are verified.
The [agent adapter check](../public-docs/guides/code-agent-auth.md) discovers fixed
CLI recipes and checks provider credentials plus isolated CLI configuration in a
ready MicroVM. Unsupported handoffs remain disabled. Disposable verification
does not enable execution or persist a login in the browser.
MicroVM readiness is not editor or agent readiness. **New workspace** opens
`#/code/new`: choose a GitHub repository/branch, runtime/resources, and an optional
agent with a compatible existing AI AUTH account. Catalog calls are read-only;
saving creates configuration with revision 0. Lost responses retain the same
UUID and request for reconciliation, including reload with session storage.
The detail and editor status views connect installed image/network/storage choices
to the revision-checked MicroVM provision/start/stop APIs. Saving configuration
does not provision a VM. Lost lifecycle responses retain the original request in
session storage; checking observes the saved operation before an explicit retry.
Conflicting configurations or operations block replay. Stop/start preserve the
same checkout. Editor and agent execution remain separate scope items in #560. See the
[CODE guide](../public-docs/guides/code-workspaces.md) for the current API.

Browser contracts run against the production WASM build without a live API:

```sh
FIRECRAB_MOCKED_E2E_PRODUCTION=1 npm run test:dashboard --prefix firecrab-e2e -- tests/code-workspaces.spec.ts tests/code-setup.spec.ts tests/code-ai-auth.spec.ts tests/code-auth-states.spec.ts tests/code-agent-auth.spec.ts tests/code-runtime-controls.spec.ts tests/pipe-ai.spec.ts
```

Run that command from the repository root. It covers English/Korean desktop and
mobile layouts, reload/history, late responses, outage recovery, disconnection,
operator access, missing runtimes, compatibility, pagination, late branches,
unknown save outcomes, reload recovery and escaped server values.

## Build and run

Requires the repository's pinned Rust toolchain, Node.js 22+, npm, and wasm-pack
0.14.0.

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-pack --version 0.14.0 --locked
npm ci --prefix firecrab-frontend
npm run build --prefix firecrab-frontend
npm run dev --prefix firecrab-frontend
```

Open `http://localhost:8080/`. The development server proxies `/api` and `/ws` to
`127.0.0.1:5523`. The development server builds Rust on startup and rebuilds when Rust sources change. Production output is
`firecrab-frontend/dist`; builds do not change an installed Firecrab service.
The standalone Cargo workspace avoids native API/helper dependencies in browser
builds. Its tests deserialize requests using the existing `firecrab-api-types`.

## Implemented flows

| Area | Rust/WASM client |
|---|---|
| MicroVM | List, state tabs, name/IP/image search, allocation totals, create, start/pause/resume/stop/delete, details, resources, egress, environment, port forwards, Shell assignments, storage assignment, logs, SSH key download |
| MicroService | AUTH-owned app/version selection, four-step setup, security/S3 bindings, deployment preflight, AUTH reconnection with restored placement, status and stop |
| Pools | Create/configure/delete, ready members, acquire with a persistent idempotency key, lease expiry/release, pool-managed VM protection |
| Schedules | Weekday/daily/weekly/custom cron and one-time actions, time zones, exact instant preservation, VM context links, enable/disable, run history |
| Security groups | Create/delete, ingress/egress IPv4 rules, service policy selection, deployment network enforcement, live replica inheritance and partial-failure reporting |
| Secrets | Metadata create/edit/delete, explicit reveal with automatic masking, provider credentials, import/export previews, explicit replacements, expiry, partial failures |
| Networks | List, create with IPv4/IPv6, details, internet/uplink changes, delete |
| Storage | List, create, inspect, delete |
| Images | List, catalog registration, OCI import, package download/install/delete, kernel assignment, progress |
| Kernels | List, install, delete, progress |
| Shells | List, create, inspect, publish revision, read revision, delete |
| Pipe | Routing and service controls, AWS/GitHub auth, registry login/browsing/pull, S3 connections and binary object transfers, database connections/table previews, Firecrab addresses, quick tunnels, Cloudflare connection |
| Host | Status, build information, update/start/progress, localhost microManager resource edit/apply with controller ID |
| Browser | Hash routing, back/forward, dirty-form confirmation, keyboard navigation, VM/destination palette, responsive sidebar, English/Korean forms and actions |
| Console | Lazy Ghostty WASM, serial WebSocket, bounded reconnect, session-ended close code 4000, disconnect/dispose on navigation, saved + live buffer log copy/export, preferences, inspector, SSH, terminal-only mode |

## Scope and validation

The previously omitted frontend workflows are implemented in Rust:

- Resizable, persistent detail panel; startup timeline and reconciliation diagnostics;
  network/environment/log tabs; masked secrets; copy actions; related VMs; lazy logs.
- VM command-palette search and keyboard selection; indexed Back/Forward cancellation;
  shared status tooltip; field-level errors; English/Korean forms and actions.
- Registry repository/tag pagination with opaque cursors, stale-response rejection,
  retry, cached inventory, pull status, and restoration of transfer jobs.
- Cloudflare zone selection from saved route hosts; quick-tunnel and fixed-address
  editing; application pickers; ownership tokens; public address copy actions.
- SSH key reveal/copy/download, generated connection and fingerprint commands, and
  SSH forwarding that preserves unrelated rules.
- Console themes/font size, persistent inspector, terminal-only mode, IME guard,
  binary UTF-8 input, reconnect/session-ended handling, and server + live-buffer logs.
- Release-note review, update PID tracking/retry/restart recovery, controller resource
  apply tracking; OCI inspection; database connection tests and bidirectional preview.

Filtering normalizes the query once and reuses cached DOM rows. Lists with more
than 256 VMs render a scrolling window with overscan. Other rows are created
on demand. Search and state filters still cover every VM. The table exposes its
full row count and each rendered row's index to assistive technology. Palette and
keyboard navigation reveal rows outside the current window.
Unchanged polls do not replace the list. Detail updates do not rebuild it.

The browser tests use deterministic HTTP fixtures. They verify frontend behavior
and request shapes, not real guest execution, credential validity, installation,
network changes, or service restarts. CI, installer payloads, and host release bundles build and ship this dashboard.
Building locally does not deploy or restart a running service.

## Validation

```sh
cargo fmt --manifest-path firecrab-frontend/Cargo.toml -- --check
cargo clippy --manifest-path firecrab-frontend/Cargo.toml --target wasm32-unknown-unknown --locked -- -D warnings
cargo test --manifest-path firecrab-frontend/Cargo.toml --locked
npm ci --prefix firecrab-e2e
npm run install-browsers --prefix firecrab-e2e
node --test firecrab-frontend/tests/frontend.test.mjs
```

The terminal uses the pinned `ghostty-web` VT parser and canvas renderer. It loads a separate WASM asset on demand. Compiled code is shared; each session owns its memory. Rust owns socket state and session retries. Browser adapters handle canvas, pointer, IME and clipboard events. Upstream build patches remain checked against the pinned revision. Terminal licenses ship in `public/terminal-licenses.txt`.

The Rust console negotiates `firecrab.console.v1`, reports local viewport dimensions, and renders the shared guest grid. See [guest resize support](../public-docs/development/console-resize.md) for kernel, guest helper and lifecycle requirements.
