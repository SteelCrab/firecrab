# Native operation recovery and volume attachments

Issue [#548](https://github.com/SteelCrab/firecrab/issues/548) remains in stage 2.
Ownership adapters and the native operations below are prerequisites for the full
43-kind contract. The expanded Stack manifest, dedicated Terraform provider and
all-service acceptance remain incomplete. Keep the implementation unmerged.

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

The active list returns `operations` and `nextBefore`. Its limit is 1–100.
UUID ordering remains stable across database maintenance. Completed receipts are
available through the exact operation and resource routes.
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
