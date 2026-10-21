# Connect public addresses to Nginx

Firecrab connects an existing public domain or a Cloudflare Quick Tunnel URL to
Nginx. Nginx forwards requests to VM apps by hostname and path and balances
requests across the selected apps.

```text
Public domain ─── DNS / Cloudflare Tunnel ─┐
                                         ├→ Nginx → VM apps
Temporary trycloudflare.com URL ──────────┘
```

For a fixed `*.firecrab.dev` subdomain, use [Firecrab Tunnel](firecrab-tunnel.md).
The steps below cover user-owned public domains and temporary Quick Tunnel URLs.
User-owned domains use the existing Cloudflare Tunnel flow.

## Use an existing domain

1. Open **Pipe → Routes → Add route**.
2. Enter the full domain, for example `my-app.example.com`.
3. Enter the path, select the VM app ports, and click **Save**.
4. Add another path under the same hostname if needed. Selecting multiple apps
   balances requests across them.
5. Connect the domain to Nginx:
   - With a separately configured public ingress, point DNS to that ingress and
     configure HTTPS there.
   - For the default private management-VM listener, use the **Cloudflare Tunnel**
     panel. Follow its API-token instructions, load your domains, select the saved
     route domain, and click **Connect and publish**.
6. Open the public hostname with each saved path and check the app response.

Saving a route configures Nginx. It does not by itself create DNS records or make
a loopback listener reachable from the Internet. The Cloudflare setup uses the
real Cloudflare API for domain lookup, tunnel creation and DNS registration.
Existing DNS records are preserved. A real account token and domain permissions
are required for that provider-managed setup.

## Connect VM apps to a fixed address

Create and manage temporary connectors in **Pipe → Cloudflare Quick Tunnel**.
To give VM apps a fixed address, get a connection token from
[the dashboard](https://firecrab.dev/dashboard/), then open **Pipe → Firecrab
Tunnel → Add fixed address**. Paste the token and choose **VM apps**
in **Connection target**, or choose **Nginx routes** to publish a saved domain/path group through
an automatically managed connector.

The token supplies the fixed public address; no subdomain input is required.
See [Firecrab Tunnel](firecrab-tunnel.md) for issuance, connection and revocation,
and the [Quick Tunnel API](cloudflare-tunnel.md#api) for existing connector controls.

## CLI and verification

`firecrab pipe status` shows the independent Nginx and connector services. On
macOS and Windows these services run inside the management VM.

A request such as the following checks the Nginx route inside that VM:

```sh
./target/debug/firecrab service shell curl -fsS \
  -H 'Host: my-app.example.com' http://127.0.0.1:8080/
```

This checks routing, not public DNS or HTTPS. Verify the actual public hostname
separately after its DNS and connection path are configured. See the
[Cloudflare and Quick Tunnel guide](cloudflare-tunnel.md) for provider setup and
the [Pipe guide](pipe.md) for unit ownership and traffic continuity.
