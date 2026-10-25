# Queue: internal RabbitMQ

Queue connects applications through a RabbitMQ engine running inside Firecrab.
It is a separate dashboard resource, not a Pipe connector.
One stateful MicroVM hosts multiple application queues.
Applications connect directly with an AMQP 0-9-1 client library.
The Firecrab API manages configuration and does not relay application messages.

## Install the engine

Open **Queue** and choose an existing MicroNetwork.
Choose CPU, memory and disk capacity, then select **Install RabbitMQ**.
Defaults are four CPUs, 4096 MiB RAM and an 8 GiB disk.
Small development environments can choose less memory; size production workloads from measurements.
The first installation imports the official RabbitMQ 4.3.7 management image pinned by its multi-platform SHA-256 digest.
Image installation requires internet access; normal messaging does not require an external queue service or cloud account.

The engine keeps the same VM identifier, node name and writable disk across stop/start.
The broker's data stays inside that VM's persistent disk generation.
**Stop** pauses messaging without deleting queues, users or messages.
**Start / recover** starts the existing VM and verifies RabbitMQ's management API.
An interrupted initial installation can be retried with the same operation.
A missing previously created VM is an error; Firecrab never silently replaces it with an empty broker.

The VM appears in MicroVM with purpose `queue`.
Ordinary VM mutation routes return `409 queue_owned`.
Use Queue controls to preserve its lifecycle and data.
The console and SSH remain available to the host administrator.
Engine deletion, automatic upgrades and multi-node clustering are not included.

## Create a queue

Select **Create Queue** and enter a name such as `notifications`.
Names contain 1–40 lowercase ASCII letters, digits or hyphens.
Firecrab provisions a private RabbitMQ virtual host, a main queue, a failure queue and separate sender/receiver credentials.
Queue creation runs asynchronously; wait for **Ready** before connecting apps.
A failed declaration can be retried without changing its identifiers or passwords.

Both queues are durable single-node quorum queues.
They are not exclusive or auto-delete queues.
Disconnecting an application does not delete them.
The default delivery limit is five failed redeliveries.
Repeated failures go to `notifications.failed` through an at-least-once dead-letter policy.
The delivery limit counts RabbitMQ failures; it is not an application retry scheduler.
On RabbitMQ 4.3, use `basic.reject` with requeue for a failed attempt that should count toward the limit.
`basic.nack` requeue does not necessarily increase that counter.

The default capacity is 64 MiB for each main/failure queue.
A full queue rejects new publications instead of removing the oldest messages.
Applications must use publisher confirms to detect rejection.
RabbitMQ may briefly exceed a quorum queue's byte limit while publications are in flight.
No message expiry is configured by default.
Operators can inspect the failure queue using the receiver connection and its `.failed` queue name.

Deletion is refused while a MicroService references the queue or either queue contains messages.
Active consumers also prevent deletion.
Firecrab does not offer a destructive purge operation.

## Bind a MicroService

In the MicroService form, open **Resources → Advanced settings → Queue connections**.
Choose an alias such as `NOTIFICATIONS`, select the queue and choose **Send** or **Receive**.
A service supports up to eight bindings.
Aliases use 1–24 uppercase letters, digits or underscores and start with a letter.
The service uses the engine's MicroNetwork automatically unless explicitly configured.
An explicitly selected different network is rejected.
Security groups must permit the required AMQP traffic to the engine on TCP port 5672.

Service JSON contains references only:

```json
{
  "queues": {
    "NOTIFICATIONS": {
      "queueId": "00000000-0000-0000-0000-000000000001",
      "access": "publish"
    }
  }
}
```

At boot, Firecrab injects `FIRECRAB_QUEUE_NOTIFICATIONS_URL` and `FIRECRAB_QUEUE_NOTIFICATIONS_NAME`.
These generated names cannot also be defined in plain environment variables or Secrets.
Connection credentials do not appear in service configuration, deployment snapshots or VM read responses.
The guest receives them through its root-owned credential file with mode `0600`.
The guest and its administrators can access them.
Engine and role passwords are stored in Firecrab's owner-only SQLite database; they are not application-encrypted.
Protect database and VM-disk backups as credentials and application data.

The sender can publish but cannot consume or configure queues.
The receiver can consume its main/failure queues but cannot publish or configure them.
Apps need code to send and handle messages; a connection binding does not convert an existing HTTP app automatically.
Current MicroServices still require an HTTP app port and health endpoint.
A consumer can run alongside that app's HTTP server.
For an independently managed MicroVM, explicitly reveal the sender/receiver connection in Queue or with the CLI.
Browser reveals use `Cache-Control: no-store` and hide on focus loss or after 30 seconds.

## When an app disconnects

| Situation | Result |
| --- | --- |
| Sender A disconnects after a positive publisher confirm | RabbitMQ retains the accepted message until it is acknowledged, dead-lettered, or explicitly deleted |
| Sender A disconnects before the confirm arrives | Acceptance is uncertain; the app reconnects and retries unconfirmed work with the same message ID |
| Receiver B disconnects before its manual acknowledgement | RabbitMQ detects the closed connection and requeues the unacknowledged delivery |
| B finishes its work but its acknowledgement is lost | The message can be delivered again; B must prevent duplicate application effects |
| B stays offline | Accepted work waits, subject to queue capacity and available broker disk space |
| B repeatedly crashes or rejects the same work | The configured delivery limit eventually moves the message to the failure queue |

Use manual acknowledgements after completing the application effect.
Automatic acknowledgements can lose work when the receiving app crashes.
A publisher confirm means broker acceptance, not that B completed its task.
Heartbeats help detect broken connections; recovery is not instantaneous.
Client applications must reconnect, reopen channels and restore consumers.
An offline sender also needs application-side durable storage if it must accept new work while RabbitMQ is unavailable.
A transactional outbox can coordinate an application database change with eventual publication.

RabbitMQ provides at-least-once delivery with these settings, not exactly-once business effects.
Use message IDs and durable application state for duplicate handling.
A single Queue VM does not provide high availability for host or disk failure.
After an API restart, compatible VM shims keep RabbitMQ running independently.
After a host restart, start the existing engine to resume messaging.

See RabbitMQ's [acknowledgements](https://www.rabbitmq.com/docs/confirms),
[reliability](https://www.rabbitmq.com/docs/reliability) and
[quorum queue delivery limits](https://www.rabbitmq.com/docs/quorum-queues).

## CLI

All commands honor the normal `--host` and `--api` options.
Create/start/stop and queue declarations return their accepted state; inspect status until complete.

```sh
firecrab queue engine create --network NETWORK_UUID
firecrab queue engine status
firecrab queue engine start
firecrab queue engine stop
firecrab queue create --name notifications
firecrab queue list
firecrab queue metrics QUEUE_UUID
firecrab queue retry QUEUE_UUID
firecrab queue connection QUEUE_UUID --access publish
firecrab queue connection QUEUE_UUID --access consume
firecrab queue delete QUEUE_UUID
```

Connection commands print credentials deliberately; do not put their output in shared logs.
See the [sender and receiver example](../../examples/queue/README.md).

## REST

| Method | Path | Operation |
| --- | --- | --- |
| GET, POST | `/api/queue/engine` | Read or install the single internal engine |
| POST | `/api/queue/engine/start` | Start/recover without replacing the disk |
| POST | `/api/queue/engine/stop` | Stop messaging while preserving data |
| GET, POST | `/api/queues` | List or declare application queues |
| POST | `/api/queues/{id}/retry` | Recover failed provisioning |
| GET | `/api/queues/{id}/metrics` | Waiting, unacknowledged, consumer and failure counts |
| POST | `/api/queues/{id}/connection/{publish|consume}` | Explicitly reveal role credentials |
| DELETE | `/api/queues/{id}` | Delete an empty, unused, unbound queue |

Application messages use AMQP directly, not the management REST API.
The RabbitMQ management listener is private to the guest network on port 15672.

## Runtime acceptance

The Linux installer CI runs `scripts/ci-qa-queue.py --disposable-host --restart-api`.
It imports the pinned image, boots the real Queue MicroVM, sends confirmed work,
restarts the API and engine, then verifies the same message, VM ID and credentials.
It also exercises sender/receiver disconnects, permissions, failure retention and safe deletion.
Results are saved to `target/qa/queue.json` in the Linux runtime QA artifact.
Run this script only on a fresh disposable localhost Linux/KVM installation.
The stateful engine and its network remain stopped until that installation is purged.
A fixture test or a Docker broker test does not verify MicroVM persistence.
