# Service OTLP export

Service OTLP export is disabled until an operator configures a collector.
Choose a collector reachable from the Firecrab host and from service guests.
The endpoint is an OTLP base URL, for example
`https://collector.example.test/otlp`. User information, query parameters,
and fragments are rejected. Do not put credentials in the URL path.
Collector authentication headers are not supported for platform export.
Protect the collector with network controls and use HTTPS when the connection
crosses an untrusted network.
Platform export connects directly and does not use host proxy settings.

The operator can read app logs, lifecycle events and alert state with
`GET /api/micro-services/{id}/observability`. Configure the base URL with
`PUT /api/micro-services/{id}/observability/otlp` and a JSON body such as
`{"endpoint":"https://collector.example.test/otlp"}`. Set `endpoint` to
`null` to disable export. These endpoints require operator access. They support only services managed on
this host. Controller-managed and multi-host services cannot use these routes;
their observations and settings are not forwarded across hosts.

When enabled, platform events and isolated application output are sent as
OTLP/HTTP JSON to the base URL plus `/v1/logs`. Application output can contain
sensitive data. Enabling export sends that output to the chosen collector.
Each record identifies its service. Application output also identifies the VM
and its stdout or stderr stream.

Local history retains up to 2,000 application output chunks and 1,000 events
per service for seven days. The observation API returns the latest 200 of
each. Output is stored in chunks, which need not be complete lines. During an
API outage each VM keeps two files of up to 1 MiB each. Older buffered output
can be lost when these files fill; the service view reports a gap.
Expired history is pruned at startup and hourly. Expired shim log files are
removed only when the runtime is verified as stopped; uncertain files are kept.

The export queue holds at most 128 records per service. Each record is limited
to 8,192 UTF-8 bytes, and each request contains at most 16 records. A full
queue drops the new record. Transient network errors and HTTP 429, 502, 503,
and 504 are retried up to three attempts. Other failures drop the batch.
A collector's partial-success response is not retried. Status reports queued,
in-flight, delivered, dropped, rejected, and failed-batch counts without
returning collector response bodies. A lost collector acknowledgement can
cause a retried record to appear more than once.

The export queue and counters are in memory. An API restart loses queued and
in-flight exports and resets these counters. Status reports `queuePersistent`
as `false` so this limitation is visible. Disabling or changing the endpoint
drops pending exports. A request already transmitted may still reach the old
collector. Local observation history has its own retention policy and is not
replayed into this queue. The opt-in boundary uses the host clock; a large
backward clock change can make the age of buffered output ambiguous.

At the next guest boot, Firecrab adds these variables to the service process:

- `OTEL_EXPORTER_OTLP_ENDPOINT`: the configured base URL.
- `OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf`.
- `OTEL_SERVICE_NAME`: the service name.

The platform uses HTTP/JSON for its own log export. Guest SDKs use
HTTP/protobuf, so the collector must accept both encodings. Firecrab rejects a
conflicting service environment variable or Secret binding instead of replacing
it. Signal-specific OTLP endpoint and protocol overrides in those settings also
conflict. Image-baked signal-specific overrides are not checked; remove them
from the image because an SDK may otherwise send to the image's endpoint.

An application must already use an OpenTelemetry SDK or instrumentation agent
to create metrics and traces. Setting these environment variables does not
instrument an application. Existing running VMs keep their current environment
until their next start or replacement.

The collector endpoint is an independent operator setting. It is not part of a
Deploy release snapshot. A rollback restores the release configuration, but
the next boot uses the current OTLP endpoint.
