# Database event handler

A small Node handler for a Firecrab **write** database binding. Firecrab injects `DATABASE_URL`, `FIRECRAB_INBOX_TABLE`, `FIRECRAB_EVENT_SECRET` and the CA path. No credentials are baked into the image.

Create this application table through a schema job before binding the service:

```sql
CREATE TABLE received_changes (
  event_id varchar(128) PRIMARY KEY,
  operation varchar(16) NOT NULL
);
```

Build the image from this directory and register it as a MicroService on port 3000. Select that stopped service in the database binding and event settings, then deploy it. Health checks use `/health`; event delivery uses `/events/database`.

Replace the `apply` callback in `server.mjs` with application work. Keep its DB changes on the supplied transaction connection. Return success only after commit. The generated inbox prevents repeated committed events from applying the callback twice. External requests need a separate idempotency key.

For verified MySQL TLS, this example requires a certificate-matching DNS endpoint. IP endpoints fail closed because of the example driver's hostname verification behavior.

Event SQL numbers are strings. Native JSON columns can still contain arbitrary-precision JSON numbers; use an appropriate parser if your application depends on them. Do not log connection strings, signing keys or complete sensitive payloads.

Run `npm test` for authentication checks. Set `FIRECRAB_DB_SERVICES_QA_JSON` to an explicit disposable PostgreSQL/MySQL fixture and run `node --test integration.test.mjs` for transaction and duplicate-delivery checks. Never use production credentials for these tests.
