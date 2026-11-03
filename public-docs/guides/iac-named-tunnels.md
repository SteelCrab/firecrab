# Named Tunnel ownership

An independent configured named connector uses `named_tunnel` and its exact
non-nil tunnel UUID. Ownership transfer preserves the hostname, published routes,
listener, connector credentials and native runtime configuration. It creates no
Cloudflare objects. The privileged Pipe ledger and API mirror retain the original
operation UUID through acknowledgement loss and API restart.

Adoption pins the physical host, configuration directory, API endpoint, connector
projection and validated credential content. Credential values never appear in
observations or ownership intents. Pending native unit jobs block adoption. Retry
the original request or read `/api/iac/owner-changes/UUID` after a lost reply.

Native configuration applies cannot replace a managed connector, change its
routes or credentials, or substitute the legacy anonymous connector on its shared
unit. Cloudflare setup checks the retained slot before contacting Cloudflare or
installing an engine. Disconnect and shared-unit controls obey the same owner.
External configuration deletion does not release that owner. An explicit release
retains its target reservation until the API acknowledges its durable mirror.

This adapter provides ownership prerequisites. Existing imported local manifests
do not retain a provider zone or DNS-record receipt. They cannot prove remote DNS
ownership. Durable remote provisioning, original credential references, readiness,
scoped retirement and both complete declarative workflows remain required in
#548. Disconnect retains remote provider objects under the existing native
contract. It does not claim a complete Terraform destroy or Stack prune.

## Verification scope

Portable tests cover original-operation recovery and API restart. The actual
Linux root CLI fixture checks credential drift, native credential replacement,
shared controls, externally deleted configuration and rejection before remote
provisioning. Its systemd observations are fixtures. No real Cloudflare request,
systemd job, Nginx process, KVM guest, Stack or Terraform provider runs in that
fixture. These checks do not complete #548 or remove its integration hold.
