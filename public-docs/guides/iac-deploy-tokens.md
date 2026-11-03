# IaC ownership of service Deploy tokens

This is an ownership and native-revocation prerequisite for [#548](https://github.com/SteelCrab/firecrab/issues/548).
Complete declarative provisioning, write-only delivery and rotation remain part of the shared lifecycle work.
A metadata ownership record does not establish Terraform or expanded native Stack support.

## Identity and transfer

The resource kind is `service_deploy_token`.
Its exact identity is `SERVICE_UUID:TOKEN_UUID`; normalize both non-nil UUIDs.
A token UUID alone does not identify its service scope.
The token must exist under that exact local service with a valid verifier record before adoption.
Controller-owned local service receipts cannot be adopted through this adapter.

Transfer retains the verifier hash, original token scope, issuance and expiry times.
It never creates a replacement credential or resets its lifetime.
The ownership API and operation acknowledgement contain neither the bearer secret nor its hash.

Use the existing ownership commands with the composite identity:

```sh
firecrab iac owner --kind service_deploy_token --id SERVICE_UUID:TOKEN_UUID
firecrab iac change-owner --file retained-transition.json
```

Retain the exact transition request when retrying a lost acknowledgement.
See [shared ownership](iac-ownership.md) for endpoint, owner and generation checks.

## Operator revocation

Verified operators can revoke a credential even when Terraform or Stack owns it.
An active Stack operation does not prevent emergency revocation.
Unverified callers cannot gain permission through a request header.

`DELETE /api/micro-services/{service}/deploy-tokens/{token}` accepts an optional
`x-firecrab-operation-id` UUID and returns that UUID with its `204` acknowledgement.
The server atomically removes the scoped verifier, releases ownership, advances authority,
retires the credential identity and saves a completed `revoke_deploy_token` native operation.
An acknowledgement failure rolls back the entire transaction.
Wrong-service revocation never removes another service's verifier or changes its owner.

Observe the acknowledgement at `GET /api/iac/native-operations/{operation}`.
After a lost response, discover it through
`GET /api/iac/native-operations/resources/service_deploy_token/{service}:{token}`.
An exact retry requires the same typed action and composite resource identity.
Recovering a completed operation only returns its immutable acknowledgement.
It does not revoke another credential or change newer authority.

The owned SQLite worker retains admission after its HTTP waiter disconnects.
Revocation preserves already accepted CI operation journals and their outcome receipts.
It rejects future authentication or fresh CI admissions with the revoked secret.
An earlier issuer cannot recreate a retired credential identity, even if issuance had not committed.
Late issuance also compares the parent service authority captured before validation.

## Parent removal and limits

A separately managed credential prevents deleting its parent service.
Explicitly release or revoke it first; parent deletion cannot silently erase another owner's configuration.
Revocation preserves the service, its runtime and other credentials.

Normal token issuance still follows the [CI Deploy](ci-deploy.md) running-service,
operator, registry, finite-lifetime and one-time secret-delivery rules.
These prerequisites do not add a credential reveal endpoint or a complete IaC token provisioning schema.
The full #548 implementation and verification merge hold remains in force.
