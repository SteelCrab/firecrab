# Connect Cloudflare Tunnel

Firecrab creates a Cloudflare Tunnel and a public app address from the Pipe
dashboard. The normal flow uses a scoped Cloudflare API token. You do not need
to create a tunnel, copy its UUID or credential JSON, or run cloudflared commands.

## Requirements

- An active domain in your Cloudflare account. Complete its nameserver setup.
- A running app reachable from the Firecrab Linux host or managed guest.
- Nginx installed on that host. Firecrab installs the pinned, checksum-verified
  cloudflared connector during setup if it is missing.
- Outbound HTTPS access to Cloudflare's API and tunnel access on port 7844.

On macOS, `127.0.0.1` in an app address refers to the management VM, rather than
the Mac. VM port forwards can provide reachable app addresses. The Mac's dashboard
and the guest's Nginx listener can both use port 8080 on their respective machines.

## Create an API token

1. Open [Cloudflare API Tokens](https://dash.cloudflare.com/profile/api-tokens).
2. Select **Create Token**, then **Create Custom Token**.
3. Name the token `Firecrab` and add these permissions:

| Scope | Resource | Permission |
| --- | --- | --- |
| Account | Cloudflare Tunnel | Edit |
| Zone | DNS | Edit |
| Zone | Zone | Read |

4. Under **Account Resources**, include the account that owns your domain.
5. Under **Zone Resources**, include the domain you want to publish.
6. Select **Continue to summary**, then **Create Token**. Copy the token.

Use the API token, rather than a tunnel token or an installation command.
Cloudflare documents the permissions for
[creating tunnels](https://developers.cloudflare.com/cloudflare-one/networks/connectors/cloudflare-tunnel/get-started/create-remote-tunnel-api/)
and [listing zones](https://developers.cloudflare.com/api/resources/zones/methods/list/).

## Connect from Firecrab

1. Open **Pipe**. Expand **How to create an API token** for the instructions above.
2. Paste the token into **Cloudflare API token** and select **Load domains**.
3. Select your **Cloudflare domain**.
4. Enter an unused **Subdomain**, such as `app` for `app.example.com`.
5. Enter **Public app addresses**, such as `127.0.0.1:18081`. Separate multiple
   addresses with commas to distribute requests across them.
6. Select **Connect and publish**.

Firecrab adds a `/` route for this hostname, creates a locally managed named
tunnel and a proxied CNAME, applies the independent units, and checks that
Cloudflare reports a connected connector. The API token input clears on success.
The result links to the public HTTPS address.

DNS propagation and HTTPS provisioning can take time after the connector connects.
The app must already be running; tunnel creation does not deploy it.

## Credentials and changes

Firecrab uses the API token only during setup. It does not save it, return it in
GET responses, or put it in process arguments. The tunnel-specific credential
is stored with root-only permissions and supplied through systemd `LoadCredential`.
Revoking the setup API token does not revoke that independent credential. See
[Cloudflare tunnel permissions](https://developers.cloudflare.com/tunnel/features/locally-managed-tunnels/tunnel-permissions/).

Guided setup creates DNS for the chosen hostname. Editing other hostnames in the
route table does not create additional DNS records. Configure those in Cloudflare.
Existing locally managed tunnels can use **Advanced tunnel configuration** with
a UUID and credential JSON.

**Stop** stops the local traffic units and keeps saved configuration. Removing
the tunnel in advanced settings removes its local credentials. These operations
do not delete Cloudflare DNS records or the Cloudflare tunnel. Delete those in
Cloudflare when no longer needed.

## Troubleshooting

| Result | Action |
| --- | --- |
| No active domains | Complete nameserver activation and include the domain in Zone Resources. |
| Token rejected while loading domains | Add Zone / Zone / Read and check resource scope. |
| Token rejected while connecting | Add Cloudflare Tunnel / Edit and DNS / Edit. Account and domain resources must match. |
| DNS record already exists | Choose an unused subdomain. Existing records are never replaced. |
| Connector download fails | Check outbound HTTPS access. Downloads are verified before installation. |
| Connector does not connect | Allow outbound port 7844 and inspect `firecrab-tunnel.service` logs. |
| Cleanup incomplete | Inspect the hostname and tunnel ID named in the error before retrying. |

A failed local apply or connectivity check restores previous local routing and
attempts to remove only the Cloudflare resources created by that setup. A lost
response during remote creation can require checking Cloudflare for a `firecrab-`
tunnel before retrying. Credentials are not included in errors.

## API

| Endpoint | Request | Result |
| --- | --- | --- |
| `POST /api/pipe/cloudflare/zones` | `{apiToken}` | Accessible active domains, without the token |
| `POST /api/pipe/cloudflare/connect` | `{apiToken, zoneId, hostname, config}` | Updated Pipe snapshot, without secrets |

Both endpoints use existing dashboard authentication and origin controls. Setup
runs through the helper's fixed CLI command. Nginx and the tunnel remain owned by
systemd across API restarts.

Tests use a local Cloudflare API fixture. Public DNS, HTTPS, and real account
permissions still require acceptance with the user's Cloudflare account.
