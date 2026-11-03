# Native operation recovery and volume attachments

Issue [#548](https://github.com/SteelCrab/firecrab/issues/548) remains in stage 2.
Ownership adapters and the native operations below are prerequisites for the full
43-kind contract. The expanded Stack manifest, dedicated Terraform provider and
all-service acceptance remain incomplete. Keep the implementation unmerged.

Native operations, ownership changes and Stack applies share one durable UUID namespace.
An ownership intent and its final receipt use the same ownership UUID.
Another operation family cannot reuse it, even after its original rows are removed.
SQLite enforces this across API processes. Existing ambiguous cross-family history is rejected without rewriting receipts.
Initialization acquires its SQLite writer before reading history, so concurrent API startup does not race a later lock upgrade.
Image and kernel leases explicitly unlock before descriptor close; unrelated forked children cannot extend a completed lease.
This does not turn request-ID headers or unrelated transient API actions into managed execution authority.

## Volume attachment identity

`volume_attachment` identifies exactly `VOLUME_UUID:VM_UUID` on one endpoint.
Both UUIDs are canonicalized. A different VM is a different attachment identity.
Adoption requires the exact existing mount, a ready volume and an independent
user VM. Controller and generated VMs cannot become independent attachments.
Active VM transitions, native cleanup and Stack work block admission.

Ownership changes preserve the mount path, disk identity, filesystem and data.
A connection transfers separately from its volume and VM configuration.
The native attach/detach API captures all three ownership snapshots before disk
verification and compares them in the SQLite write transaction. A late writer
cannot cross release/reclaim or a parent ownership change.
VM creation also checks borrowed volume and attachment writers at commit.

A parent VM cannot implicitly detach an independently managed connection or
volume. An independently managed attachment also prevents volume removal.
Release or explicitly remove that configuration before removing its parent.
Existing stopped-VM, exclusive-volume and non-overlapping-mount rules apply.

## VM cleanup

Every VM start verifies its original kernel, root filesystem, optional initrd and boot arguments before runtime effects.
New VMs retain that complete proof across API restart, independently of Stack ownership.
See [VM boot inputs](iac-vm-boot-inputs.md) for inspection and explicit legacy initrd review.

Native VM deletion reserves the exact VM UUID and records its disk policy
before log draining, host teardown or filesystem dispatch. Cleanup continues
when an HTTP waiter disconnects. The API returns `x-firecrab-operation-id`.
Supply that header on the original request to retain a caller-known retry UUID.

```sh
curl -X DELETE "$FIRECRAB_API/api/vms/$VM_UUID" \
  -H "x-firecrab-operation-id: $OPERATION_UUID"
```

Use the host's existing authenticated transport configuration with this example.
The UUID selects a receipt; it grants no authority over managed or generated VMs.
A completed retry acknowledges the original deletion. An unfinished retry
requires observation or explicit recovery and dispatches no additional cleanup.
Parent requests that remove several replicas receive separate child receipts.

VM artifact retention is immutable for the operation. The ordinary VM DELETE
API removes private VM artifacts. Existing Stack lifecycle policy retains them
by default. Borrowed data-volume files survive either policy.
`retainDisks` in a VM cleanup status reports the recorded policy.
The [physical cleanup proof](iac-vm-storage.md) also retains the exact storage scope and disk generation across recovery.

The host must confirm firewall-policy and TAP removal before the VM address can
be reused. Metadata removal, shell/port cleanup, lease release and the succeeded
receipt commit together. An interrupted or failed cleanup retains the original
UUID, ownership, VM row, address and policy until the original operation recovers.
An already completed filesystem effect is preserved during recovery.
A completed deletion retires the UUID against late creation and lease allocation.
Generated VM cleanup pins its service, pool or Sandbox parent ownership through
completion or recovery. Parent configuration and ownership writers wait for these
leases; existing runtime observations remain available. An older worker cannot
restore a VM cache entry after a newer recovery removed the durable row.

## Saved database connection operations

Connection creation (`POST /api/pipe/databases`), replacement of saved settings
(`PUT /api/pipe/databases/UUID`) and write-access policy changes
(`PUT /api/pipe/databases/UUID/access`) retain their immutable request before
connection verification or namespace setup. Supply `x-firecrab-operation-id` and,
for a pinned remote host, `x-firecrab-endpoint-id`. Existing mTLS authentication
still applies. These headers select an operation and endpoint; they grant no
management authority.

A completed retry returns its original public connection summary even after
later edits or an ownership transfer. Private URIs, passwords and CA material
remain in the host's existing owner-only database. Public operation metadata
contains only identities, state and static failure categories.

A verification failure before the first possible managed SQL effect records
`failed`. Corrected input needs a new operation UUID. An uncertain setup records
`needs_recovery` and keeps its original resource UUID, saved configuration and
database-target reservation. Recover that exact operation through the endpoint
below. Recovery advances the execution generation and rejects older callbacks.
Connection metadata, the succeeded receipt and reservation release commit in
one SQLite transaction. HTTP disconnection does not cancel admitted work.
Disabling writes needs no database connection, after retained bindings and
triggers are checked.

Connections configured with the same engine, host, port and database share the
reservation, including maintenance job admission and ownership changes. This
matches configured endpoint identities; DNS aliases and different proxy addresses
are not proof of one physical database. The existing external migration ledger
and database locks remain necessary for SQL execution.

Maintenance claims move atomically from queued to running. A second API process
cannot execute the same attempt. Recovery invalidates older progress writes.
The internal safety snapshot must carry its exact running parent attempt.
These are shared lifecycle prerequisites. Managed RabbitMQ, declarative migration
and complete cross-workflow database contracts are still under implementation in
stage 2; this connection work does not establish full IaC coverage.

## Database binding accounts

`database_binding` uses the exact binding UUID and retains its database/service
association. Adoption requires the existing account record to be ready and its
configured target to have no pending maintenance. Transfer preserves the generated
URI, account name, permissions and table list. It does not contact the database.

Native binding creation and revocation retain a private immutable request before
remote work. Creation defaults its operation ID to the existing `requestId`;
revocation accepts `x-firecrab-operation-id`. The response reports that header.
An exact completed retry returns the original summary or revocation receipt,
including after later account removal. A retired binding UUID cannot be recreated.

The service must be stopped for native account changes. The operation reserves
its service, connection and configured database target. Generate and persist the
private account credential once before provisioning. Failed setup, lost receipt
commits and restart keep that same credential and operation for recovery. Older
attempts cannot commit. PostgreSQL and MySQL use the same account-specific SQL
session lock for provisioning and revocation. The server rechecks the attempt
after acquiring that lock, before changing grants. A delayed older connection
cannot re-enable an account after a newer recovery or revocation.
Account revocation preserves application tables and rows;
PostgreSQL generated roles retain their ownership marker with login disabled.

Event setup cannot implicitly broaden a Terraform-owned account's inbox grants.
It checks this before declaring broker resources. A maintenance job may refresh
only derived table grants under its original running execution generation; it
cannot change account credentials, access policy or association. Active jobs
also prevent ownership transfer. Full declarative execution and expanded Stack/Terraform execution remain pending.
See [migration ownership and recovery](iac-database-migrations.md).

## Database event capture

`database_events` identifies a capture by `database_uuid:trigger_uuid`. Native
create, pause, detach and removal retain immutable private requests and original
receipts. Creation defaults the operation UUID to `requestId`; controls accept
`x-firecrab-operation-id`. Retrying a removed capture returns its original removal
receipt. Reusing a retired capture UUID for another creation is rejected.

Setup reserves the stopped handler, database, account and broker configuration.
A borrowed account with existing inbox grants can remain independent. Otherwise,
changing its grants requires the current writer. Generated account credentials
are prepared once and kept across recovery. The worker continues after its HTTP
caller disconnects. Its source SQL session lock remains held through broker work,
account setup, source capture and the final local receipt.

Preparation pauses local delivery. A verified failure before external changes
restores the previous metadata or removes new staging and releases reservations.
An uncertain remote change keeps the capture paused and reserves the original
resources for recovery. Recovery rejects older execution generations.

Detach stops new capture and retains the source outbox. Removal requires a
detached, drained capture. If retained pending or failed events prevent removal,
the operation restores delivery instead of leaving it paused. Successful removal
deletes its owned broker queues and local capture record. Application rows and
source outbox evidence remain. Migration jobs may record derived detachment only
under their original running execution generation.

## Managed database broker safeguards

Managed database RabbitMQ supervision retains a captured owner, configuration
generation and checkpoint. Explicit configuration changes invalidate old workers
even if the values later change back. A dedicated OS file lock excludes another
API supervisor without using the scheduler's database-file lock.

The complete broker UUID set is saved before any broker VM insertion. A retry
uses those same UUIDs. Insertion rechecks its private parent grant inside SQLite.
Generated broker VMs cannot be independently adopted or edited through the VM
API. Missing recorded VMs, disks or addresses require restoration of their
original resources; they do not permit an empty replacement. Rejecting a
duplicate VM creation preserves its existing record and address.

External broker selection retains a private immutable request before its
read-only handshake. It accepts `x-firecrab-operation-id` and the endpoint header.
HTTP disconnect does not cancel verification. Selection, disabling managed
supervision and the original success receipt commit together. Known verification
failure preserves the saved configuration. Restart recovery reuses the original
credentials; older attempts cannot commit. A completed retry acknowledges its
original selection without overwriting any newer configuration.

Managed preparation, automatic repair and disconnection retain their original
operation before image import or guest changes. Complete UUID sets and generated
TLS material survive restart. Recovery reuses the pinned image, addresses and
existing disk generations. A changed or missing retained disk cannot activate a
replacement client. Final readiness, client selection and the success receipt
commit together. A no-effect failure restores the previous configuration;
uncertain work retains its original resources and recovery fence.

`DELETE /api/pipe/database-rabbitmq` stops owned guests and disconnects the client.
It retains VM records, disks and broker credentials. Event captures must be
removed before preparation or disconnection. A narrowly scoped repair may restore
transport for unfinished capture work only with its existing pinned image,
addresses and credentials. It cannot rotate configuration under that capture.

The exact `database_rabbitmq/default` slot supports explicit ownership transfer.
Transfer preserves external credentials and managed guest identities. Terraform
ownership blocks ordinary external selection, preparation, disconnection and
controller repair. Releasing and reclaiming ownership invalidates older workers
even when settings are unchanged. These are shared prerequisites; complete
replacement/data policies and expanded Stack/Terraform execution remain pending.

## Dashboard retries

The database dashboard retains the operation UUID and endpoint UUID before sending
connection, binding, event or broker changes. It stores no URI, password, certificate or request
body in browser storage. Reloading the page reuses the original identity.
A failed receipt lookup does not allocate a new create operation.

After a lost acknowledgement the dashboard observes the original operation. A
completed request displays its existing resource and a review notice. Pending or
uncertain work links to Stack operations. Only a known failure before managed
effects permits corrected input to use a new operation. A completion received
after leaving the page remains unacknowledged until the resource can be shown.
Legacy pending bindings and captures retry setup using their existing UUID.
Event receipts must match both the database UUID and capture UUID.

## Observe and recover

The Stack dashboard lists running and uncertain native work, including resources
without an IaC owner. It displays the resource, operation UUID, action, recovery
attempt and VM artifact policy. Recovery uses that exact operation UUID.
After a lost acknowledgement it observes the original receipt before continuing.
A failed lookup disables recovery until observation succeeds again.

```text
GET  /api/iac/native-operations?limit=50
GET  /api/iac/native-operations?limit=50&before=OPERATION_UUID
GET  /api/iac/native-operations/OPERATION_UUID
GET  /api/iac/native-operations/resources/KIND/RESOURCE_ID
POST /api/iac/native-operations/OPERATION_UUID/recover
```

Unfinished recovery requires `x-firecrab-endpoint-id` and the observed
`x-firecrab-execution-generation`. The API compares the generation again inside
typed SQLite recovery admission. A delayed request cannot advance a newer
uncertain attempt. The CLI observes the original operation before sending these
preconditions. The dashboard sends the generation displayed in its current card.
Terminal receipts remain readable without dispatching more work. Project cleanup
also binds its final metadata and failure callbacks to the original attempt.

The active list returns `operations` and `nextBefore`. Its limit is 1–100.
UUID ordering remains stable across database maintenance. Completed receipts are
available through the exact operation and resource routes.
A `failed` receipt means verification ended before any managed change.
It releases the active fence so corrected configuration can use a new operation.
It never means the requested configuration was applied. Unknown outcomes keep
`needs_recovery` and retain their original identity and fence instead.
Database mutation adapters that use this distinction are still in development.
The dashboard can load more active operations and refreshes loaded pages.

Recovery requires an authenticated operator. The dashboard sends
`x-firecrab-endpoint-id`; a different endpoint rejects the request.
The original owner and execution attempt are checked before VM cleanup dispatch
and metadata completion. Recovery advances the attempt; an older worker cannot
commit completion or overwrite the newer attempt with failure.
Existing Native Stack work cannot start while an owned native operation is
running or uncertain, including facets absent from its next plan.

See [ownership transitions](iac-ownership.md),
[Queue engine recovery](iac-ownership.md#queue-engine-admission-and-recovery),
and the [complete target contract](../reference/iac.md).

CI deployment admission also checks the service dependency fence. An unfinished VM cleanup cannot admit replacement replicas through a valid CI token.
