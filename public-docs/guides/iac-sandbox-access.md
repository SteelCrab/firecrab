# Shared plans for Sandbox access

The `sandbox_access` adapter uses the [shared plan API](iac-resource-plans.md).
Both Stack and Terraform ownership use the same operator-authorized policy
implementation. Full native YAML and the typed Terraform provider remain pending
in #548. Ownership support for every inventory kind is a separate prerequisite.

## Configuration

The plan's `desired` accepts:

```json
{
  "kind": "sandbox_access",
  "configuration": {
    "principal": "11111111-1111-4111-8111-111111111111",
    "policy": {
      "grants": [
        {
          "resource": {
            "kind": "project",
            "id": "22222222-2222-4222-8222-222222222222"
          },
          "permissions": ["read", "execute"]
        }
      ]
    }
  }
}
```

Replace the example UUIDs with the intended mTLS host and exact resource.
The identity is `sandbox_access/HOST_UUID` on the selected controller.
A grant does not enroll a host, create a project or connection, start a VM,
read source contents or grant provider inference permission.
The existing policy contract permits reserving a future project UUID.
Every actual tool operation still checks its required rights and runtime proof.

Project grants support `read`, `write`, `execute` and `apply`.
Connection and VM grants support `read` and `execute`.
Source grants use an exact canonical API-host directory and support `read`/`write`.
The policy accepts at most 256 grants with nonempty permission sets.
Duplicate resources, nil UUIDs, unsupported rights and unknown fields are errors.
Grant and permission ordering have set semantics; duplicates within a permission
set are normalized. Equivalent ordering produces a no-op.

## Original plan and authority

A new policy requires no existing live grants at the same principal identity.
An existing nonempty policy requires exact prior ownership adoption.
Display names and matching values never adopt another policy.

Planning captures canonical source directories' device, inode and birth time.
The frozen private proof and desired policy contribute to `planSha256`.
Public plans/receipts expose changed field names, never source paths or grants.
First execution rejects a missing, symlinked or physically replaced source.
Editing a source file does not change its containing directory's identity.
The policy never takes infrastructure ownership of borrowed source contents.

Execution holds the existing Sandbox admission boundary and rechecks the exact
observed configuration and owner inside the SQLite transaction.
Policy, ownership and original acknowledgement commit together.
The usual operator-only emergency revoke remains available.
Revocation advances authority and invalidates any older pending grant plan.
An old completion receipt never re-grants access after revocation.

## Removal and recovery

Set `desired` to null for the exact owned policy to remove its live grants.
The API keeps an explicit deny row for that certificate and releases ownership.
Deleting the row would let the certificate bypass the delegated API scope.
Projects, connections, VMs, source directories and application data remain intact.
Source directories need not still exist to remove a policy.

A released deny tombstone requires explicit new operator intent before creation.
Read its retained generation from the ownership API and set the configuration's
`revocationGeneration` to that exact value. Omission acknowledges only generation
zero. An earlier declaration cannot automatically re-grant a revoked policy.
The new plan captures new source proofs and does not reuse old authorization.

Completed receipts remain readable after source removal, expiry, ownership
change and SQLite reopen. Repeating one never changes the current policy.
Tests use actual SQLite and local authenticated-router fixtures, including a
delegated mTLS identity and physical directory replacement.
They do not claim live remote-worker admission or both-workflow every-row
acceptance. The #548 integration hold remains in effect.

[Exact Linux verification](../assets/iac-548-access-linux-evidence.json) and
[local API/browser evidence](../assets/iac-548-shared-plans-browser-evidence.json)
record their source and fixture limits separately.
