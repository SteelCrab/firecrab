# Host Ingress ownership

This is a stage 2 prerequisite for [#548](https://github.com/SteelCrab/firecrab/issues/548).
It transfers existing Ingress ownership metadata. It does not complete declarative
Ingress provisioning, the Stack adapter, or the Terraform provider.

## Identity and observation

The `ingress` resource uses `listener` or the exact independent route key,
such as `route:app.example.com/` or `route:app.example.com/api`.
The route key removes trailing slashes, matching native route uniqueness.
Existing paths whose complete key exceeds 512 bytes use
`route:sha256:HEX`, derived from the canonical complete key.
`firecrab_helper_protocol::host_iac::ingress_route_key` produces that identity.
An arbitrary digest never aliases a shorter literal route.

Use the ordinary `/api/iac/owners/{kind}/{id}` observation and ownership-change
API. Percent-encode the complete route ID when using it in a URL path.
Existing API authorization and remote mTLS remain required.
The privileged host broker supplies physical host identity, directory identity,
current ownership, configuration digest, and controller ownership.
Observation does not create infrastructure or claim an unused installation.
A missing or incompatible helper fails closed. It does not return invented
guest-local host state.

## Durable transfer and recovery

The API first reserves the exact resource, request and desired logical address.
The host then checks the original configuration digest and owner generation.
Its transaction holds the same cross-process Pipe lock used by native changes.
Queued systemd jobs and transitional ingress states also block new transfers.
The host retains its receipt and reservation across disconnection or API exit.
The API commits the verified receipt to its ordinary ownership journal.
Only acknowledgement of that durable mirror releases the host reservation.

Keep the same operation UUID after any timeout or lost response.
Retry the original change or inspect `/api/iac/owner-changes/UUID`.
Recovery reuses the admitted host proof and original result.
It never follows a replacement configuration directory or endpoint.
Completed results remain readable after later changes or helper outages.
No-op transfers preserve the current ownership generation.

A known no-effect rejection has its own immutable host result.
It releases the API reservation without changing resource ownership.
Inspect the current owner and configuration before submitting a new operation.
An unavailable transport does not count as a known no-effect rejection.

## Native mutation rules

Ownership applies to the privileged CLI, including when the API is stopped.
Whole-manifest changes must preserve managed routes and their listener.
Unrelated routes can reconcile an already running listener.
They cannot start, enable or replace another workflow's managed ingress unit.
Ingress stop/control and whole-Pipe disable reject managed or unconfirmed slots.
Release retains configuration and bytes. Its reservation remains until confirmed.
Service-generated routes stay with their service controller and cannot be
adopted independently.

The host namespace is bound to a stable Linux machine identity, canonical
configuration root, directory device/inode and API endpoint UUID.
Its retained metadata uses private root-owned files and protected directories.
Owner metadata contains no credential values and never authorizes arbitrary
configuration writes through request headers.

## Verification scope

The portable tests cover restart recovery, lost target and confirmation replies,
exact retry identity, stale proofs, logical-address reservations, no-effect
rejections, generated routes, and changed helper targets.
`scripts/test-host-iac.py` runs the actual Release CLI inside a disposable Linux
container. It checks retained files, native rejection, unsafe-directory rejection,
corrupt-manifest recovery and the real cross-process Pipe lock.
The fixture does not run systemd, Nginx, KVM guests, Stack or Terraform.
Its systemd observations are explicit fixtures, including a pending stop job.
Full lifecycle, runtime readiness, credential/delegation contracts and both
declarative workflows remain required before the #548 merge hold can end.
