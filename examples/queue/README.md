# RabbitMQ application example

Create a Queue in Firecrab and wait for Ready.
Bind the sender MicroService with alias `NOTIFICATIONS` and access `publish`.
Bind the receiver with the same alias and access `consume`.
For manually managed VMs, obtain the corresponding role connection with `firecrab queue connection`.
Set the matching `FIRECRAB_QUEUE_NOTIFICATIONS_URL` and `FIRECRAB_QUEUE_NOTIFICATIONS_NAME` environment variables privately.
Do not print connection URLs or store them in source control.

Install the pinned client:

```sh
npm ci
```

In the receiving app, run `npm run receive`.
In the sending app, run `npm run send -- 123`.
The sender uses persistent messages, mandatory routing and publisher confirms.
The receiver uses manual acknowledgements and reconnects when RabbitMQ becomes unavailable.
It logs an event and acknowledges it; replace that example effect with your application's processing.
For MicroServices, run the consumer alongside an HTTP server that satisfies the existing health check.

A confirm means RabbitMQ accepted the message, not that the receiver completed it.
If publishing fails without a confirm, preserve the work in durable application storage and retry with the same `MESSAGE_ID`.
A receiver can execute a task twice if its acknowledgement is lost.
Use the message ID and durable application state to prevent duplicate effects.
The example does not implement a business database or a transactional outbox.

See [Queue operations](../../public-docs/guides/queues.md) for failure queues, limits and recovery.

## Disconnect integration test

Install this package with `npm ci`.
Use an isolated RabbitMQ 4.3.7 management broker with private test credentials.
Set the following environment variables without logging their values:

- `FIRECRAB_TEST_RABBITMQ_HTTP`: the HTTP origin, such as `http://127.0.0.1:15672`.
- `FIRECRAB_TEST_RABBITMQ_AMQP`: the AMQP host and port, such as `127.0.0.1:5672`.
- `FIRECRAB_TEST_RABBITMQ_USER` and `FIRECRAB_TEST_RABBITMQ_PASSWORD`: disposable administrator credentials.

From the repository root, run:

```sh
cargo test -p firecrab-api rabbitmq_live_disconnects -- --ignored --nocapture
```

The test provisions a random virtual host through the real Firecrab RabbitMQ client.
It verifies confirmed sends survive sender disconnects, unacknowledged deliveries return after receiver crashes, ACKs remove messages, repeated failures are dead-lettered and role permissions are separate.
It removes only its random test virtual host and users.
