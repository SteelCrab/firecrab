# Original VM cleanup storage

This is a shared lifecycle prerequisite for [#548](https://github.com/SteelCrab/firecrab/issues/548).
Full Stack/Terraform execution and every-resource acceptance remain incomplete.

## Admission and physical identity

VM cleanup records its exact resource UUID, storage ID, disk generation and retention policy before effects.
It also records the physical storage root, the existing `vms` directory and the original VM directory.
Directory identities include the original path, device, inode and creation time.
An unavailable stable directory identity fails before admission; no observation creates a directory.
The selected storage registration must exist. Removal never falls back to the default disk.

Cleanup checks this proof before host teardown and filesystem effects.
API restart, a changed storage mapping or a replacement directory cannot redirect the original operation.
An original directory that is already missing is a no-op.
A newly created directory at a previously absent VM identity is not claimed or removed.
Other VM directories remain outside the operation's authority.
Registered MicroStorage ownership/configuration waits for the cleanup's parent lease.
Controller retries acknowledge an existing completed native receipt without dispatching filesystem cleanup again.
Legacy worker paths without a native receipt still require the remaining shared parent-execution contracts in #548.

The disk generation remains an immutable input even while an operation needs recovery.
An older worker cannot remove artifacts or commit metadata after a newer attempt has been admitted.
Resource locks and durable reservations exclude competing API writers.
These checks do not grant untrusted access to the host filesystem or defend against a privileged administrator bypassing the API.

## Retained identity

Use the existing authenticated operator transport:

```text
GET /api/iac/native-operations/OPERATION_UUID/vm-storage
```

`approved` contains the original storage ID, VM UUID, disk generation, physical proof hash and retention policy.
It remains readable after VM metadata removal, independent of later filesystem changes.
`artifactDirectoryExistedAtObservation` describes the original observation point, not current existence.
The projection excludes private host paths and credential values.
The server retains its original physical locator privately.
The operation's ordinary succeeded receipt remains available through `GET /api/iac/native-operations/OPERATION_UUID`.

Retaining artifacts preserves their bytes, including the VM's writable disk and private artifact tree.
Borrowed data-volume files remain subject to their independent attachment/ownership rules.
The original retention policy cannot change during a retry or recovery.

## Missing legacy proof

Older unfinished cleanup records have a storage name but no original physical proof.
Migration preserves that absence. It never silently adopts the current directory as the old target.
Recovery remains blocked until an operator inspects and explicitly confirms the original storage.
Completed legacy receipts stay unchanged; `originalStorageUnrecorded` reports their unavailable historical proof.

For an unfinished record, the inspection response has `reviewRequired: true`, `approved: null` and a `candidate`.
The candidate is an observation of the currently selected filesystem.
It is not evidence that this is the original target.
Verify it against the original host/storage configuration and retained VM identity before confirming it.

Submit the exact candidate hash and observed operation generation:

```text
POST /api/iac/native-operations/OPERATION_UUID/vm-storage
{
  "expectedEndpointId": "ORIGINAL_API_ENDPOINT_UUID",
  "expectedExecutionGeneration": 0,
  "physicalSha256": "EXACT_CANDIDATE_SHA256",
  "confirmOriginalStorage": true
}
```

The operation must need recovery and retain its original ownership, VM storage ID and disk generation.
The proof is saved once. An already approved physical target cannot be replaced through this route.
Review does not remove files, alter retention or start cleanup.
It advances the execution fence, so workers from before the review cannot resume.
Read the original native operation again to obtain its new execution generation before requesting recovery.
Normal endpoint and `x-firecrab-execution-generation` recovery preconditions still apply.

The exact review request recovers its immutable acknowledgement after response loss or API restart.
This also works after cleanup completes or another directory appears at the retired VM UUID.
A changed hash or review generation conflicts instead of acquiring fresh authority.

## Verification limits

Tests exercise actual temporary directories, real SQLite migrations and actual HTTP handlers.
They cover remapped roots, replacement directories, absent originals, retained bytes, original receipts and stale disk generations.
Network helper replies are simulated; no live TAP, firewall, guest, Stack apply or Terraform execution is claimed.
