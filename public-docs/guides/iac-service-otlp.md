# Shared service OTLP plans

The `service_otlp` adapter uses the [shared planning API](iac-resource-plans.md)
for both Stack and Terraform manager identities. Full YAML and provider coverage
remain separate unfinished #548 stages.

```json
{
  "kind": "service_otlp",
  "configuration": {
    "serviceId": "11111111-1111-4111-8111-111111111111",
    "endpoint": "https://collector.example.test/otlp"
  }
}
```

The existing local service UUID is both the setting identity and a borrowed
dependency. The plan pins its authority and rejects controller/worker services.
It never creates, deploys or deletes the parent service. Existing disabled
settings can be explicitly adopted by their exact service UUID.

The endpoint field is required. Explicit `null` disables export while retaining
the setting's IaC ownership. Null `desired` removes the setting and releases its
ownership. Neither action deletes local logs or borrowed service data.
An independently owned setting blocks deletion of its parent service until
the setting is removed or explicitly released.

Only credential-free HTTP(S) base URLs are accepted. User information, queries
and fragments are rejected before admission. Collector authentication headers
remain outside the existing [platform OTLP contract](otlp.md). Do not put
credentials in a URL path. Plan and receipt views expose changed field names,
not endpoint values or application output.

The operator-only `GET /api/micro-services/SERVICE_UUID/observability/otlp`
returns `serviceId`, `endpoint`, `enabledAtMs` and exact `ownership` metadata.
It does not read or return logs, events, alerts or deployment snapshots, and
does not start an exporter. This view supports configuration refresh without
copying application data into IaC state.

Changed settings prepare an empty exporter before committing configuration,
authority and the original receipt atomically. A failed SQL commit leaves no
setting or active exporter. Saved and active service environments are checked
inside that transaction. Service edits and new deployment admissions recheck
the current OTLP setting in their own commit transaction.

An unchanged plan preserves the original consent boundary and active exporter.
An old completed apply returns its original receipt after later changes without
restoring its endpoint or re-enabling export. Late activation cannot replace
a newer committed configuration. Background reconciliation also compares the
consent boundary, including when a later opt-in uses the same URL.

The receipt acknowledges saved validated configuration. Collector delivery is
observed separately through the existing service observation route. Enabling
does not require the collector to acknowledge a record, and saving a setting
does not restart running guests. The platform queue is bounded and volatile.
Re-enabling starts after the new consent boundary and current history cursors;
disabled history is not replayed. See the existing OTLP guide for retry limits,
clock limitations and application instrumentation requirements.

Focused tests cover both manager identities, immutable receipts, disabled
adoption, borrowed-parent retention, environment conflicts in both writer orders,
SQL rollback and real loopback HTTP delivery. These are local API fixtures,
not evidence of completed YAML/provider or every-row Linux/KVM acceptance.
