# Internal service routing

After every replica passes its health check, Firecrab publishes `<name>.internal`
through the service MicroNetwork's existing dnsmasq. Its IPv4 A record points to
that network's gateway. Nginx listens on the gateway's port 80 and balances
requests between the healthy replica app ports. Unknown `.internal` names return
NXDOMAIN without being forwarded to public DNS. Local records have DNS TTL zero.

Apps in the same MicroNetwork can call `http://orders.internal` without knowing
replica addresses or app ports. Guests receive their gateway DNS through DHCP.
Internal listeners require an RFC1918 IPv4 gateway; discovery publishes A records.
HTTP/1.1, HTTP/2 prior knowledge and WebSocket clients are supported; Nginx forwards
to the application with HTTP/1.1. Another network's requests are denied. Private
routes are absent from public tunnel selection and the public loopback listener.

Omit `hostPort`, or send zero, to deploy without individual host ports. The CLI
accepts `firecrab microservice deploy SERVICE_UUID` and REST accepts `{}`.
The dashboard shows the internal address and accepts an empty host port.
Public domain routing remains optional and works alongside the private route.

A destination security group restricts internal callers by its TCP ingress rules
for the **app port**, using the original caller's IPv4 address. An empty group
returns HTTP 403. A caller with a security group also needs TCP **port 80 egress**
to its own gateway. Other MicroNetworks remain denied even with a wide peer rule.
Stop affected services before changing group rules, then redeploy.

Stop withdraws DNS and ingress before draining or deleting replicas. Lost helper
responses retain `internalRouteOwned`, so Stop can retry cleanup. An API restart
keeps established data-plane routes; DHCP reconciliation and helper restart
rebuild DNS from the persisted Pipe manifest. An interrupted deployment remains
failed and requires Stop before retrying.

Gateways keep their current MAC address when VM ports join or leave. Helper
reconciliation also pins an existing bridge without changing its address, so
surviving guests can continue using their cached gateway neighbor after Stop.

Install matching API, helper, CLI and the bundled HTTP/2 ingress engine together.
Existing ingress units need the updated `CAP_NET_BIND_SERVICE` capability for port
80: disable Pipe and apply its saved configuration once after upgrading. This
interrupts traffic during the update. Existing running deployments gain their
internal address after Stop and redeploy.
