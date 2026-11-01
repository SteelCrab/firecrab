# SigNoz integration

Firecrab 0.6.0 connects selected local MicroServices to one self-hosted SigNoz
collector. The integration exports isolated stdout/stderr and service lifecycle
and health events as OTLP log records. It does not install or operate SigNoz.

## Connect

1. Install SigNoz using its [self-hosted installation guide](https://signoz.io/docs/install/docker/).
2. Open **System → Observability → Connect SigNoz** in Firecrab.
3. Enter the OTLP/HTTP **base URL**, for example `http://signoz.internal:4318`.
   Do not enter the UI URL or append `/v1/logs`. A reverse-proxy base path is allowed.
4. Select one or more locally managed MicroServices, up to 100.
5. Select **Test connection** to send an empty OTLP batch and inspect the actual
   response. This action does not enable continuous export.
6. Select **Save connection** to enable export of new records from the selected
   sources. Open **Send test log** to send one explicit test event from the first
   selected service. Inspect its batch acknowledgement and **Delivery history**.
7. Find the records in SigNoz Logs Explorer. Filter by `service.name` or
   `firecrab.service.id`. Application output also carries `service.instance.id`
   and `log.iostream`. The test event body contains a unique request UUID.

The endpoint must be reachable from Firecrab. Existing guest SDK configuration
also uses the service endpoint on the next VM boot, as described in
[Service OTLP export](otlp.md). Collector authentication headers, URL credentials,
query parameters, and fragments are not supported. This initial provider targets
self-hosted SigNoz rather than SigNoz Cloud.

## Source ownership

The integration owns the OTLP setting of each selected service. Configure these
services from Observability; the service detail links back to this page.
Services with independent OTLP exporters cannot be adopted implicitly. Remove
their independent configuration explicitly before selecting them.

Configuration updates validate every selected source before writing. A missing,
busy, remotely managed, or conflicting service produces a per-service error.
The previous configuration remains in place. Deselection or disconnect removes
only the integration's service bindings. Deleting a service removes its binding.

## Delivery and pause behavior

**Pause** prevents new collector requests after any current request finishes.
Records continue to enter the bounded queue; overflow drops new records.
**Resume** preserves the current exporter, counters, and opt-in boundary, then
sends queued records. Pause state and source selections survive an API restart.

The queue holds up to 128 records per service, plus an active batch of up to
16 records. The queue, counters, and recent delivery history are in memory.
They do not survive an API restart or exporter replacement. The next process
starts from current observation cursors and does not replay historical records.
An endpoint change replaces the affected exporters and drops their pending data.
Disconnect stops new exports and drops pending data. Records already accepted
by SigNoz remain in SigNoz.

Pause and disconnect control Firecrab's platform exporter. They do not change
the environment of running applications or stop an application's own SDK export.

## Counters and history

- Accepted, rejected, dropped, and failed-batch counters cover the current
  exporter session. They are not hourly totals.
- Each service retains the most recent 128 completed batches. The history tab
  displays the most recent 200 across selected services.
- The volume chart shows accepted records from retained batches in the last
  60 minutes. Older batches can leave retained history before an hour passes.
- HTTP status is the collector's actual status, or unavailable for transport
  errors. Response bodies and log contents are not included in delivery history.
- `pending` means there is no completed delivery yet. `delivered` means the
  latest batch was fully accepted. `partial`, `retrying`, and `failed` need
  inspection. Collector acceptance does not guarantee indexing or searchability.

## API

All routes require verified operator access and return `Cache-Control: no-store`.

| Method | Route                                | Purpose                                                                                       |
| ------ | ------------------------------------ | --------------------------------------------------------------------------------------------- |
| GET    | `/api/observability/signoz`          | Connection, sources, delivery status and recent history                                       |
| PUT    | `/api/observability/signoz`          | Save `{ "endpoint": "http://signoz.internal:4318", "serviceIds": ["UUID"], "paused": false }` |
| DELETE | `/api/observability/signoz`          | Disconnect the integration                                                                    |
| POST   | `/api/observability/signoz/test`     | Test `{ "endpoint": "http://signoz.internal:4318" }` with an empty OTLP batch                 |
| POST   | `/api/observability/signoz/test-log` | Send one test record using `{ "serviceId": "UUID" }` from an active selected source           |

Metrics/traces management, additional providers, authentication headers, and
historical backfill are outside this first integration.
