# Publish apps and Nginx routes with Quick Tunnels

Cloudflare Quick Tunnel creates temporary HTTPS addresses such as
`https://random-name.trycloudflare.com`. A Cloudflare account, API token, and
personal domain are not required. The address changes when its connector is
started again. See [Cloudflare Quick Tunnels](https://developers.cloudflare.com/tunnel/get-started/quick-tunnels/).

Firecrab manages a list of tunnels. Each entry has a name, a target, its own
connector service, and its own current URL.

## Requirements

- A running Firecrab Linux host or managed guest with Nginx installed.
- For VM app targets, running VMs with IPv4 addresses and registered TCP ports.
- For Nginx route targets, a saved domain/path route group.
- Outbound access for Cloudflare Tunnel. Firecrab installs its pinned,
  checksum-verified cloudflared connector if it is missing.

On macOS and Windows, the connector runs inside the managed Linux guest.

## Existing connectors and Firecrab tokens

Use **Pipe → Cloudflare Quick Tunnel** to create, edit and control temporary
connectors. In **Pipe → Firecrab Tunnel**, paste a token
from [the Firecrab dashboard](https://firecrab.dev/dashboard/) and select an
**VM apps** or saved **Nginx routes**. See the
[Firecrab Tunnel guide](firecrab-tunnel.md) for token issuance and revocation.

VM app selection publishes the chosen running apps and TCP ports. Nginx selection
publishes every saved path of its selected domain. Firecrab Tunnel manages its
own connector for either target. Temporary Cloudflare Quick Tunnels remain
independent of the fixed-address target selection.

Use the API below to provision or manage independent Quick Tunnels. Each has a
name, target, unit and temporary public URL. Saving an existing ID changes only
that connector. Omitting the ID creates one. Targets are VM app addresses or a
saved Nginx hostname. A route group must retain at least one saved path while
referenced by its connector.

Stop/start/restart operate on one saved connector. Starting it again changes its
temporary URL. Other connectors and Nginx routes remain. An exclusively owned
Firecrab Nginx connector is managed through its Firecrab address; user connectors
referenced by an address cannot be deleted directly.

## Troubleshooting

| Result | Action |
| --- | --- |
| Create is unavailable | Enter a name and select running app ports or a saved route domain. |
| No VM choices | Check VM state, IPv4, TCP ports, and API availability. |
| No route domains | Save a Nginx domain/path route before selecting this target mode. |
| Base link returns 404 | Append a path listed for that route domain, such as `/api`. |
| Preparing URL | Check that row's service state and logs. Wait for the next poll. |
| Cloudflare rate limited URL creation (HTTP 429) | Wait for automatic retries. Repeated manual restarts can prolong the limit. Use **Stop** to pause retries. |
| Old link fails after restart | Copy the new URL from that tunnel's row. |
| Removing a domain is rejected | Change or delete tunnels targeting that domain first. |

Failed provisioning retries start after 60 seconds. On systemd 254 or later,
the delay increases to at most 15 minutes. Older systemd versions use the
60-second delay. Connector failures also appear on any Firecrab address that
depends on that connector. Once a URL is issued, the address updater retries
its registration automatically.

## API

| Endpoint | Request | Result |
| --- | --- | --- |
| `POST /api/pipe/quick-tunnels` | `{id: null, name, target}` | Create an entry with a generated UUID |
| `POST /api/pipe/quick-tunnels` | `{id, name, target}` | Update that entry |
| `DELETE /api/pipe/quick-tunnels/{id}` | None | Delete that entry |
| `POST /api/pipe/quick-tunnels/{id}/service` | `{action: "start", "stop" or "restart"}` | Control that connector |

An app target is `{type: "apps", upstreams: ["VM_IP:PORT"]}`. A route target is
`{type: "routes", host: "pipe.test"}`. These operations use the server's saved
configuration and retain unrelated tunnels and routes.

`GET /api/pipe` includes `quickTunnels` entries with their name, target, unit
state, and current `publicUrl`. A URL can be null while it is being issued.
An optional `error` contains a sanitized provisioning failure for the current
connector invocation. Stopped connectors do not retain earlier failures.
The saved manifest stores definitions in `config.quickTunnels`. The legacy
`quickTunnel` field remains compatible.

The endpoints use the existing authentication, origin controls, and fixed helper
operations. Automated browser tests use API fixtures. Real external HTTPS
reachability requires a separate check against each generated URL.

## Use a stable Cloudflare domain

The separate **Cloudflare Tunnel** panel publishes saved Nginx routes using a
Cloudflare domain you own. Its address remains the same when its connector
restarts. One regular connector can run alongside the Quick Tunnel list.
If a pre-list Quick Tunnel is still configured, open its **Edit** form and select
**Save** once first. This migration changes its temporary URL once and enables
regular tunnel publishing. API token input and domain lookup are available before
this migration.

1. Register your domain in Cloudflare and complete nameserver activation.
2. Save an Nginx route for an unused hostname in that domain, such as
   `app.example.com`. Add the required path routes and VM apps.
3. Find the API token input in **Cloudflare Tunnel**. It is visible whenever no
   regular tunnel is connected, including while a pre-list Quick Tunnel is running.
4. Open **How to create an API token**. Create a scoped token with Account /
   Cloudflare Tunnel / Edit, Zone / DNS / Edit, and Zone / Zone / Read.
5. Paste the token and select **Load domains**.
6. Select the Cloudflare domain and the saved **Route domain**.
7. Select **Connect and publish**. Firecrab creates a tunnel and DNS record,
   installs its verified connector if necessary, and checks its connection.
8. Wait for DNS to become available, then open the fixed HTTPS address. Use the
   adjacent copy icon to copy it. Existing DNS records are never overwritten.

The API token stays in the setup request and is cleared from the form after
success or **Clear input**. Only the connector credentials remain protected on
the Linux host. See [Cloudflare setup](https://developers.cloudflare.com/tunnel/get-started/).

Use the Cloudflare Tunnel service row's **Start**, **Stop**, and **Restart**
actions to control the regular connector. **Disconnect** removes its local unit
and credentials while preserving Nginx routes and other Quick Tunnels. Remote
Cloudflare DNS and tunnel records remain in the account for separate management.

`POST /api/pipe/cloudflare/zones` lists available domains using `{apiToken}`.
`POST /api/pipe/cloudflare/connect` accepts `{apiToken, zoneId, hostname, config}`.
The helper retains the latest saved listener, unrelated routes, and Quick Tunnels.
`POST /api/pipe/cloudflare/disconnect` removes only the local regular connector.
The setup endpoints retain authentication and origin checks while allowing their
bounded installation and provider operations to complete.

Real account and owned-domain acceptance checks require a user-provided token.
Automated tests use provider fixtures and do not create resources in user accounts.
