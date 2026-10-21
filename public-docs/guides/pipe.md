# Independent service data plane

The data-plane slice of [#335](https://github.com/SteelCrab/firecrab/issues/335)
adds independent Nginx PipeIngress and cloudflared PipeTunnel systemd units.

```text
Public domain or Quick Tunnel URL → Cloudflare connector / public ingress
  → firecrab-ingress.service (Nginx) → app in a MicroVM
```

The API manages configuration. systemd owns Nginx and each tunnel connector. API
stop/restart does not stop those units. They start at boot after an explicit
apply and restart after failure. They have no API `PartOf`, `BindsTo`, or
`Requires` dependency. Stopping the whole host or management VM interrupts traffic.

VM continuity still requires a compatible VM shim protocol from #123. Startup
reconciliation stops incompatible shims. This does not guarantee traffic across
incompatible VM protocol upgrades.

This slice routes to existing reachable app addresses. The MicroService model,
app deployment automation, secret store, and automatic VM replacement are separate
#335 work. Firecrab Tunnel uses a dashboard token to connect VM apps
or saved Nginx domain/path groups to a fixed public address.

## Public address connections

An existing public domain or a generated Quick Tunnel URL connects to the local
Nginx ingress. Nginx applies the saved domain/path routes and app pools.
[Public address setup](public-addresses.md) covers direct DNS with an existing
public ingress and real Cloudflare Tunnel setup for the private listener.
[Firecrab Tunnel](firecrab-tunnel.md) provides fixed `*.firecrab.dev` subdomains
through its separate registration/forwarding service. It supports both existing
VM apps and saved Nginx routes. The operator must deploy that service.

## Requirements

- Linux with systemd 247 or later. On macOS/Windows, a running managed Linux guest.
- Matching Firecrab API, helper, and CLI builds for dashboard controls.
- Nginx installed by the host's package manager. The CLI searches `/usr/sbin/nginx`,
  `/usr/bin/nginx`, then PATH. `apply --nginx` accepts an external executable.
- Reachable app ports. Existing VM port forwards can provide loopback upstreams
  such as `127.0.0.1:18081`.
- For temporary public access, a reachable Cloudflare Quick Tunnel connection.
  A Cloudflare account, API token, and domain are not required.
- Existing locally managed named tunnels remain compatible through CLI manifests.
  Omit `tunnel` and `quickTunnel`, and leave `quickTunnels` empty, for ingress-only operation.

The listener is HTTP on loopback, on port >= 1024. Public TLS terminates at
Cloudflare. Direct public HTTPS and MicroNetwork listeners are subsequent work.

## Install and configure

```sh
# Linux example. Other distributions use their own Nginx package.
sudo apt-get install nginx
sudo firecrab pipe install

# macOS/Windows: install Nginx in the managed guest first, then run:
firecrab pipe install
```

`pipe install` requires an installed Nginx and downloads the pinned cloudflared
release. It verifies SHA-256 before replacing the executable and skips unchanged
verified downloads. Installing does not start Firecrab traffic units or apply
routes. `install.sh --with-pipe` also installs the OS Nginx package.

Cloudflared versions and release digests are in
[pipe-engines.json](../../packaging/pipe-engines.json). Run `pipe install` explicitly
to update the connector. Manage Nginx updates through the OS package manager.
Firecrab uses its own Nginx config and unit. It does not edit the host's nginx.conf.

Copy [the manifest example](../../packaging/pipe.example.json). Set the hostnames,
app addresses, and optional tunnel UUID and credentials file. Relative credential
paths resolve beside the manifest.

```sh
# Render without changing services.
firecrab pipe render --config pipe.json
firecrab pipe render --config pipe.json --tunnel

# Linux: validate, persist config, enable/start units and apply routes.
sudo firecrab pipe apply --config pipe.json
firecrab pipe status
sudo firecrab pipe disable
```

On macOS/Windows, run apply/status/disable without host sudo. The native CLI
reads the manifest and credential file, then streams them through stdin to the
managed guest over SSH or WSL. Executable overrides refer to guest paths.
`firecrab service dev` builds and installs the matching guest CLI.

`pipe status` shows a table with the Nginx and Cloudflare Tunnel runtime states,
PIDs, and executable availability. An installed connector can be available while
its tunnel is not configured. Status works without the API and does not read
the protected manifest. View saved routes in the Pipe dashboard.

## Dashboard

Select **Pipe** in the existing dashboard menu. The page uses the dashboard's
shared panels, form fields, tables, service badges, colors, fonts, and navigation.

Open **⋯** in the Nginx row and select **Edit** to expand **Nginx settings**
directly below that row. Its listen address, **Save**, and **Cancel** are grouped
inside the editor. Select **Save** to apply or **Cancel** to discard the address
change. Either action closes the editor. Cancelling preserves pending route edits.
The listen address stays hidden when the editor is closed.

1. Select **Add route** to show the domain, path prefix, and VM app choices.
2. Enter the domain and path, then select one or more registered TCP app ports.
3. Select **Save** once to validate and apply the route. The editor closes on success.

Use each service row's **⋯** menu for **Start**, **Stop**, and **Restart**. These
controls use the saved configuration. They do not save unfinished address, route,
or tunnel changes. The page reports that the request was accepted, then polls the
unit state. A start or restart may take a few seconds.

Stopping Nginx stops request routing. Stopping Cloudflare Tunnel stops public
access while keeping Nginx available for local requests. Starting or restarting
the tunnel can also start Nginx because its unit depends on the ingress service.
Stopping Nginx does not stop a running tunnel connector, but public requests need
Nginx to succeed. Stop the tunnel first when pausing both services.

Start and restart are unavailable until the service has saved configuration and
its executable is installed. Start is unavailable while already active. Stop is
unavailable while inactive. Lifecycle actions are unavailable during a transition.
The service controls retain saved routes and tunnel credentials.

Removing the last explicit route removes that domain/path forwarding. App-target
Quick Tunnels retain their separate app pools. A domain referenced by a
route-target tunnel must retain at least one route.
Select **Edit** on an existing row to open its editor, or **Cancel** to discard
the editor's inputs. Removed routes stay in the local draft until **Save**.
Polling preserves local edits. API connection failure clears stale service badges.

Open [the connection dashboard](https://firecrab.dev/dashboard/) to issue a token.
Under **Firecrab Tunnel → Add fixed address**, paste it and select
**VM apps** or saved **Nginx routes** in **Connection target**. There is no
subdomain input or separate Quick Tunnel input in Firecrab Tunnel. Use the
**Cloudflare Quick Tunnel** panel to create and manage temporary connectors.

The public address is assigned by the token service. Appending a saved path uses
that route group; if it has `/api` but no `/`, the base address can return 404.
Saved legacy Quick Tunnels remain available in their Cloudflare panel. See
[Firecrab Tunnel](firecrab-tunnel.md) for issuance, revocation and continuity.
The **Cloudflare Tunnel** panel also connects an owned Cloudflare domain using
a scoped API token and a saved Nginx route. It creates DNS and retains a stable
public address. Its connector runs alongside the existing Quick Tunnels.
Existing named tunnel manifests remain compatible with the CLI.

The API uses the authenticated helper socket for apply, inspect, service control, and disable.
It does not accept executable paths or arbitrary commands from dashboard requests.
The helper invokes the matching root-owned CLI. This keeps the CLI and dashboard
on the same validation, persistence, reload, and rollback path.

| Endpoint | Action |
| --- | --- |
| `GET /api/pipe` | Saved manifest, engine availability, unit states, and Quick Tunnel list with current URLs |
| `PUT /api/pipe` | Apply `{config, credentials}`; null credentials retains the saved file |
| `POST /api/pipe/quick-tunnels` | Create or edit `{id, name, target}` while retaining other definitions |
| `DELETE /api/pipe/quick-tunnels/{id}` | Stop and remove one Quick Tunnel |
| `POST /api/pipe/quick-tunnels/{id}/service` | Start, stop, or restart one Quick Tunnel |
| `POST /api/pipe/service` | Request `{service: "ingress" or "tunnel", action: "start", "stop" or "restart"}` for one unit |
| `POST /api/pipe/disable` | Stop traffic units and retain saved config |

Manifests and dashboard requests are limited to 48 KiB for the helper's bounded protocol frames.

## Routing and reload

Hosts are lowercase literal DNS names. `/api` matches `/api` and `/api/users`,
but not `/apiculture`. Nginx selects the longest matching path. Original request
paths and queries are forwarded. Unknown hosts and unmatched paths return 404.
WebSocket upgrade headers and forwarded client addresses are preserved.

Nginx uses round-robin across each route's app addresses. Passive checks remember
failed connections, timeouts, and configured 5xx responses. `max_fails=1` and
`fail_timeout=30s` exclude a failing peer without involving the API. A single-peer
upstream has no alternative and Nginx ignores passive exclusion for that case.
Periodic active HTTP health checks are not provided by this OSS Nginx slice.
The former Caddy `healthPath` field is not accepted.

Apply validates the candidate before changing files. It writes config atomically,
then asks systemd to reload Nginx. Nginx replaces workers while keeping its master
PID and processing existing requests. A private Unix status endpoint confirms
that the new generation was accepted. A failed apply restores persisted config
and enabled states. The CLI reports any rollback failure. Old workers have a
five-minute shutdown limit for long-lived connections.

Changing only app addresses or routes under existing named tunnel hostnames keeps
the connector PID. Updating a Quick Tunnel target or its referenced paths reloads
Nginx without changing that connector's URL. Each list entry uses its own systemd
unit. Removing an entry stops only that connector. Listener or connector changes
can interrupt connections. Named tunnel credentials remain managed separately. Disable first before
changing the Nginx executable path or config directory of an active pipe.

## Credentials and operations

Non-secret Nginx config is root-owned and readable by its dynamic service user,
so reload reads the latest file. The saved manifest is root-only. Tunnel secrets
live in `$CONFDIR/pipe/secrets` with directory mode 0700 and file mode 0600.
systemd passes the connector its credentials through `LoadCredential`. Secrets
are not placed in command arguments. The local status socket only returns a generation hash. Its runtime directory
is searchable but cannot be modified by other users. Nginx temporary files remain
private. A directory bind gives Nginx read-only access to its route config while
retaining the installed config directory permissions. Apply and disable share a host lock.

For an existing locally managed named tunnel, the CLI still accepts account
credentials and DNS routes prepared outside the dashboard:

```sh
cloudflared tunnel login
cloudflared tunnel create firecrab-services
cloudflared tunnel route dns <tunnel-uuid> app.example.com
sudo journalctl -u firecrab-ingress -u firecrab-tunnel -b
```

The credential's `TunnelID` must match `tunnel.id`. Linux service restart,
reinstall, and self-update operate on API/helper units and preserve traffic units.
Native `service dev restart` restarts only the guest API/helper. Whole management
VM restart interrupts traffic. Linux uninstall removes pipe units and managed
cloudflared binaries. Saved config remains until purge. OS Nginx and external
executables are not removed.

## Validation

- `test-pipe-runtime.py`: real Nginx domain/path boundaries, request distribution,
  passive failure exclusion, multiple isolated Quick hosts, cloned route paths,
  target update/removal, public Host/HTTPS forwarding, and in-flight reloads.
- `test-pipe-cli.py`: real systemd apply, stdin bundles, rollback, connector lifetime,
  private credentials, tunnel removal, and disable. Tunnel connections use a fixture.
- `test-pipe-api.py`: real unprivileged API → helper → CLI → Nginx, traffic and stable
  ingress PID across actual API stop/restart, persisted routes, individual ingress
  stop/start/restart, and API disable.
- `firecrab-e2e/tests/pipe.spec.ts`: desktop/mobile dashboard editing, local saves,
  independent service menus, disabled states, rejected requests, draft preservation,
  Quick Tunnel CRUD, independent URL lifecycle, app/route targets, domain removal
  guards, and stale-state handling with API fixtures.

The privileged scripts refuse existing Firecrab traffic units and require a
disposable host. Actual public Quick Tunnel connectivity still needs acceptance with external
HTTPS requests. These local tests do not claim that external connection.

References: [Nginx reload](https://nginx.org/en/docs/control.html),
[Nginx upstreams](https://nginx.org/en/docs/http/ngx_http_upstream_module.html),
and [locally managed Cloudflare tunnels](https://developers.cloudflare.com/cloudflare-one/networks/connectors/cloudflare-tunnel/configure-tunnels/local-management/).
