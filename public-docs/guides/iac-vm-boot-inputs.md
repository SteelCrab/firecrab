# Immutable VM boot inputs

This is a shared stage 2 prerequisite for [#548](https://github.com/SteelCrab/firecrab/issues/548).
Complete Stack/Terraform execution and all-service acceptance remain pending.

## Original content

New VMs retain the template version and SHA256 identities of the kernel, root filesystem, optional initrd and boot arguments.
An absent initrd is an explicit part of this proof.
The record survives API restart and does not depend on a mutable image alias or Stack ownership.
Normal VM updates cannot replace or clear it.

Every VM start verifies that original proof and the actual registered artifact files before network setup or process creation.
This also applies to unowned VMs and VMs that already have a prepared writable disk.
Controller and Sandbox identity comparisons include the initrd identity.
Changing an alias, adding/removing an initrd or changing its bytes cannot silently change an existing VM's boot content.
Restore the original registered artifacts when verification fails.

## Legacy records

Older VM records have kernel, rootfs and boot-argument hashes but lack initrd evidence.
Migration preserves that missing evidence instead of approving the current registry's initrd.
A legacy VM without an initrd can start only when all its recorded hashes match.
A legacy VM with an initrd requires an explicit review of that same original version.
The review does not change a template, disk, VM identity or running workload.

Use the existing authenticated API transport to inspect the exact VM:

```text
GET /api/vms/VM_UUID/boot-inputs
```

`approved` is the stored complete proof, or null for a legacy record.
`candidate` is the registered version's metadata, not an automatic approval.
`legacyReviewRequired` reports missing legacy initrd evidence.
Check the version and hashes against the original trusted image before approving them.

Submit the exact reviewed proof as a JSON request:

```text
PUT /api/vms/VM_UUID/boot-inputs
{
  "operationId": "ORIGINAL_NON_NIL_OPERATION_UUID",
  "expectedEndpointId": "API_ENDPOINT_UUID",
  "expectedGeneration": 0,
  "pin": {
    "version": "ORIGINAL_VERSION",
    "kernelSha256": "ORIGINAL_KERNEL_SHA256",
    "rootfsSha256": "ORIGINAL_ROOTFS_SHA256",
    "initrdSha256": "REVIEWED_ORIGINAL_INITRD_SHA256",
    "bootArgsSha256": "ORIGINAL_BOOT_ARGUMENT_SHA256"
  }
}
```

Use null for `initrdSha256` only when that original version has no initrd.
The operation requires operator access, the exact endpoint and observed ownership generation.
The VM must be inactive, independent and unowned, with no unfinished native or Stack reservation.
Managed/generated children keep their parent authority; this route grants no delegation.
The API verifies the original three recorded hashes, the exact reviewed initrd and all file bytes before committing.
A complete existing proof cannot be overwritten by another review.

## Retry and recovery

The proof and succeeded operation receipt commit in one SQLite transaction.
The same operation UUID and request recover that immutable acknowledgement after a lost response.
Changed contents with that UUID conflict.
A retry does not acquire fresh authority or require the current image files after completion.
Later management transfer and missing artifacts do not rewrite the original receipt.

Observe the original result through:

```text
GET /api/iac/native-operations/OPERATION_UUID
```

No VM start, restart, filesystem preparation or guest execution is part of the review.
The verification tests use temporary image bytes and SQLite; they are not Linux/KVM or complete Stack/Terraform acceptance.
