# Shared Secret lifecycle

The stage 2 `secret` configuration adapter uses the same frozen plan for both
manager identities. It manages host-local PipeSecrets. Full Stack YAML and the
Terraform provider remain separate unfinished stages of [#548](../reference/iac.md).

## Declare metadata and private input

Use the authenticated operator [resource planning API](iac-resource-plans.md).
The desired configuration contains metadata and an input descriptor:

```json
{
  "kind": "secret",
  "configuration": {
    "name": "APPLICATION_TOKEN",
    "description": "Application credential",
    "value": {"kind": "write_only", "version": 1}
  }
}
```

After reviewing the plan, send its private value separately:

```text
POST /api/iac/resource-plans/PLAN_UUID/credentials
```

The JSON body contains `expectedEndpointId`, `planSha256`, `version` and `value`.
Send private bytes over the existing authenticated transport. A successful upload
returns HTTP 204 without a value. It creates no Secret and grants no ownership.
The server binds encrypted input to the original operation, endpoint, target,
owner and plan hash. Repeating identical input is safe. Substituting different
bytes under the same plan is rejected. Public plans, receipts, SQLite journals
and ordinary logs contain no value or value fingerprint.

Alternatively, use `{"kind":"secret","id":"SOURCE_UUID","version":3}` as
the input descriptor. First admission copies that exact existing host Secret
version. A different source version or authority requires a new plan. The source
remains borrowed and is never deleted by the target's lifecycle.

An existing target may omit `value` to keep its current bytes. A new target must
declare an input. Existing targets require exact UUID-based adoption before
configuration. Name matches never imply adoption.

## Update and readiness

Saving completes when the exact encrypted target version and its SQLite
ownership/receipt are durable. It does not wait for a dependent application's
next automatic deployment. Existing service rotation status remains observable
through the ordinary service APIs.

Name and description edits preserve the UUID and credential bytes. An unchanged
declaration is a no-op and needs no new private upload. A changed write-only
version requests rotation. The marker is public intent, not a value checksum.
Native metadata edits preserve acknowledged input provenance. A native value
write or import invalidates it. Reusing the old marker then fails with
`iac_credential_version_required`; choose a new marker or explicitly omit `value`
to retain the current value. Refresh never guesses private bytes.

## Interruption and recovery

Repeat the original `/apply` request after a timeout. An admitted operation keeps
its original encrypted input across API restart. It fences the exact resource,
desired logical address and Stack registration until acknowledgement completes.
Expired admission deadlines do not abandon accepted work.

The file effect and SQL acknowledgement have separate durable phases. If the
file commits but SQL fails, status remains `needs_recovery`. Repeating the exact
apply request acknowledges the original version. It does not reread a newer
source, reprovision a value or allocate another Secret. The generic native
recovery endpoint does not replace this resource-plan recovery request.

Completed receipts return before opening the current Vault. Later rotation,
deletion, management transfer or an unavailable Vault cannot replay their effect.
Consumed private input is removed after the effect commits. Recovery metadata is
pruned after its public receipt becomes durable.

An API maintenance loop checks expired input once per minute. New private uploads
also check for expired input. Only original scopes with no admitted operation are
expired at their plan deadline. Accepted or uncertain effects keep their recovery
material after expiry. Completed receipts permit removal of leftover private
metadata. Missing plans, a different database identity or unknown effects retain
their material for explicit recovery. Ordinary Secrets are never removed by this
cleanup.

## Remove or retain

Set `desired` to null for the exact owned target. The default policy releases
ownership and retains the Secret and its value. The public plan reports this
choice. Retained data can be adopted explicitly later.

To remove the actual value, also set `removal` to `{"deleteData":true}` on the
plan request. Service references reject this plan and are checked again before
execution. Database guards prevent another API process from adding a reference
while the effect is uncertain or after that UUID has been deleted. Other Secrets,
borrowed input sources and external stores remain intact.

## Verification scope

The [clean Linux Release record](../assets/iac-548-secret-linux-evidence.json)
pins the source, compiler-reported executables, test totals and verification logs.

Tests use actual encrypted files, independent SQLite connections and HTTP routes.
They cover both manager identities, private-input scope, no-ops, metadata edits,
rotation, default retention, exact data deletion, concurrent callers, source
drift, expiry and a deliberately failed SQL receipt write after file commit.
They do not establish full YAML/provider or every-service live acceptance.
