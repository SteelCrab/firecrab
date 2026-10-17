# Independent service data plane

The data-plane slice of [#335](https://github.com/SteelCrab/firecrab/issues/335)
adds independent Nginx PipeIngress and cloudflared PipeTunnel systemd units.

```text
Public request → Cloudflare → firecrab-tunnel.service
               → firecrab-ingress.service (Nginx) → app in a MicroVM
```

The API manages configuration. systemd owns both traffic processes. API
stop/restart does not stop either unit. Both units start at boot after an explicit
apply and restart after failure. Neither has an API `PartOf`, `BindsTo`, or
`Requires` dependency. Stopping the whole host or management VM interrupts traffic.

VM continuity still requires a compatible VM shim protocol from #123. Startup
reconciliation stops incompatible shims. This does not guarantee traffic across
incompatible VM protocol upgrades.

This slice routes to existing reachable app addresses. The MicroService model,
app deployment automation, secret store, and automatic VM replacement are separate
#335 work. Guided Cloudflare setup creates DNS for one chosen public hostname.

## Requirements

- Linux with systemd 247 or later. On macOS/Windows, a running managed Linux guest.
- Matching Firecrab API, helper, and CLI builds for dashboard controls.
- Nginx installed by the host's package manager. The CLI searches `/usr/sbin/nginx`,
  `/usr/bin/nginx`, then PATH. `apply --nginx` accepts an external executable.
- Reachable app ports. Existing VM port forwards can provide loopback upstreams
  such as `127.0.0.1:18081`.
- For advanced manual public access, a locally managed named Cloudflare Tunnel and its JSON
  credentials. Omit `tunnel` for ingress-only operation.

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

## Dashboard

Select **Pipe** in the existing dashboard menu. The page uses the dashboard's
shared panels, form fields, tables, service badges, colors, fonts, and navigation.

1. Enter the domain, path prefix, and comma-separated app `IP:port` addresses.
2. Add or edit the route in the draft table.
3. Select **Save and apply** to validate and apply the whole draft.
4. Select **Stop** to stop the traffic units while keeping saved routes.

Removing the last route and saving produces HTTP 404 for all external requests.
Editing routes does not apply them until Save and apply. Polling preserves local
edits. API connection failure clears stale service badges.

Cloudflare Tunnel is optional. Use **Connect Cloudflare Tunnel** for guided setup:
follow the token issuance instructions, load domains, choose a hostname and app
addresses, then select **Connect and publish**. Firecrab creates the tunnel and
DNS and installs the connector if missing. The setup API token is not saved.
See [Cloudflare token setup and connection](cloudflare-tunnel.md).

**Advanced tunnel configuration** retains the UUID and credential JSON flow.
Leave credentials blank to retain the saved credential. The input clears after
saving. GET responses never include credentials. Manual tunnels and additional
hostnames require their own Cloudflare DNS records.

The API uses the authenticated helper socket for apply, inspect, and disable.
It does not accept executable paths or arbitrary commands from dashboard requests.
The helper invokes the matching root-owned CLI. This keeps the CLI and dashboard
on the same validation, persistence, reload, and rollback path.

| Endpoint | Action |
| --- | --- |
| `GET /api/pipe` | Saved manifest, engine availability, and live unit states |
| `PUT /api/pipe` | Apply `{config, credentials}`; null credentials retains the saved file |
| `POST /api/pipe/disable` | Stop both units and retain saved config |

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

Changing only app addresses or routes under existing tunnel hostnames keeps the
connector PID. Changing hostnames, listener address, tunnel UUID, or credentials
restarts the connector and can interrupt its connections. Removing `tunnel`
disables the connector and deletes managed credentials. Disable first before
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

For a locally managed tunnel, create account credentials and DNS routes before
applying the pipe:

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
  passive failure exclusion, in-flight requests across reload, and rejected config.
- `test-pipe-cli.py`: real systemd apply, stdin bundles, rollback, connector lifetime,
  private credentials, tunnel removal, and disable. Tunnel connections use a fixture.
- `test-pipe-api.py`: real unprivileged API → helper → CLI → Nginx, traffic and stable
  ingress PID across actual API stop/restart, persisted routes, and API disable.
- `firecrab-e2e/tests/pipe.spec.ts`: desktop/mobile dashboard editing, save, stop,
  rejected changes, credential clearing, and stale-state handling with API fixtures.

The privileged scripts refuse existing Firecrab traffic units and require a
disposable host. Actual public Cloudflare connectivity still needs acceptance
with real account credentials. These tests do not claim that external connection.

References: [Nginx reload](https://nginx.org/en/docs/control.html),
[Nginx upstreams](https://nginx.org/en/docs/http/ngx_http_upstream_module.html),
and [locally managed Cloudflare tunnels](https://developers.cloudflare.com/cloudflare-one/networks/connectors/cloudflare-tunnel/configure-tunnels/local-management/).
