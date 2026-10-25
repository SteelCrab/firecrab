# Databases used by services

Firecrab connects MySQL and PostgreSQL with verified TLS, creates separate service accounts, captures committed row changes and delivers them through RabbitMQ. Users select a database, table and handler service. Firecrab generates broker credentials, certificates, queues, signing keys and retry schedules.

## Service access and duplicate protection

A database binding injects `DATABASE_URL`, its CA file and an inbox table into a stopped service on its next deployment. Read bindings grant SELECT on application tables. Write bindings grant row operations without schema or account administration. Enabling events creates a read-only binding if necessary and grants INSERT only on that service's generated inbox. It preserves an existing binding's business-table privileges.

Use `examples/database-events/receiver.mjs` to verify the signed HTTP body. It inserts the stable event ID into the inbox and runs the application callback in the same database transaction. The handler returns success after COMMIT. Concurrent duplicates execute the callback once. A failed transaction rolls back both the receipt and the callback. External HTTP, payment and email effects require their own idempotency keys. An arbitrary HTTP handler does not receive automatic exactly-once business semantics.

Changing table grants requires a stopped service and a refreshed binding. Credentials stay in private host state and the guest's private environment file. VM and service summaries do not expose them. Removing a binding revokes its owned account.

## Delivery and recovery

A source row change and its outbox entry commit in one database transaction. Rolled-back changes produce no committed event. Each trigger owns a durable quorum delivery queue and a separate quorum failed-event queue.

The host publishes persistent messages with mandatory routing and waits for publisher confirmation. It then receives the RabbitMQ delivery without automatic acknowledgement. After the handler succeeds, it records completion in the source database and acknowledges the broker message. A crash or uncertain HTTP result can cause redelivery with the same ID. The transactional receiver inbox prevents a repeated business effect.

The source database retains event payloads and delivery receipts. A missing broker copy can be published again. This also repairs an interrupted broker rebuild. Broker acknowledgements do not delete source rows. One source advisory lock serializes each trigger. Delivery follows committed outbox IDs. This is not a global transaction-commit order across concurrent writers or different triggers.

Failures use persisted exponential backoff from 2 seconds to 5 minutes. After 12 unsuccessful handler attempts, the event stays failed in the source and is rejected to the failed-event queue. Quorum dead-lettering uses at-least-once transfer and reject-publish overflow. A failed event blocks later source events until replay or an explicit skip. Replay keeps the original ID. Skip retains the payload and records the operator's decision. Oversized payloads remain failed in the source without publication. Source rows are capped at 512 KiB per event envelope.

Pause stops new attempts after an in-flight operation finishes. Detach removes source capture and drains retained events. Remove trigger metadata only after pending and failed events are resolved; owned queues are removed as part of that cleanup. Retry timing and failed payloads survive API restarts.

PostgreSQL capture uses row triggers. MySQL requires InnoDB and a primary key. Foreign-key CASCADE/SET NULL, generated columns and binary columns are rejected where row triggers cannot capture them safely. Unsupported CDC workloads require a separate CDC connector. Host database credentials must be authorized to install owned triggers and namespaces.

## Managed RabbitMQ

The default uses three isolated MicroVMs, each with 1024 MiB RAM and a 10 GiB persistent disk. Preparation checks host memory and disk capacity. The official RabbitMQ image is resolved to an immutable digest before import. Client and peer traffic use TLS. Generated broker keys are readable only by the guest broker account. The private management endpoint also requires verified mutual TLS.

Firecrab waits for all owned cluster members before enabling delivery. Queues use three replicas in the default profile. A majority is necessary for availability. The supervisor checks broker health and restarts owned brokers with their existing disks. API restart resumes supervision. A single-node profile is available only as an explicit development option.

All managed nodes share the API host. This tolerates a broker process or MicroVM failure, but provides no independent-host failover. Keep the source database available and backed up. Host loss can require rebuilding the brokers and replaying retained source events. Use an external cluster across independent hosts when that availability boundary is required.

External RabbitMQ configuration is in advanced settings. Production connections require verified TLS, credentials and three failover endpoints. Operators must provide a three-node cluster with quorum capacity and permission to declare owned queues. Multiple endpoint names alone do not prove independent replicas. External virtual hosts default to `firecrab`.

## Backups and schema changes

Firecrab runs daily logical backups for writable databases and supports manual backup, checksum-verified restore and immutable versioned schema jobs. Archives are stored beside private API state. Download a copy for off-host storage. Native PostgreSQL/MySQL client tools must support the target server version.

Before restore or schema changes, stop bound services, pause and drain triggers, confirm the database name and stop external writers. Firecrab creates a safety backup first. Queued maintenance blocks console writes and service deployment across saved connections to the same database. PostgreSQL restore is transactional. MySQL DDL and restore can commit partly; interrupted work stays in `needs_recovery`.

Restore rewinds source outbox, inbox and audit state. Coordinate broker deliveries and external effects when restoring an earlier point. Logical backups do not replace provider HA, off-host backups or point-in-time recovery. External broker state is outside a database snapshot.

## Validation

`durable_events_real_rabbitmq` uses real TLS PostgreSQL/MySQL connections and a three-node RabbitMQ cluster. It covers source capture, publisher confirms, ordered failure blocking, broker-copy loss, API-state reopening, stable-ID redelivery and terminal failure recovery. `FIRECRAB_QA_RESTART_GATE` allows the QA controller to restart both source databases and brokers at a persisted checkpoint.

`examples/database-events/integration.test.mjs` checks concurrent duplicates and callback rollback against both engines. Browser tests cover minimal managed defaults, advanced TLS settings, replay controls and confirmed restore. Real ext4 tests check private guest credentials, public CA readability and broker key ownership. The disposable QA runner cleans only resources it owns.
