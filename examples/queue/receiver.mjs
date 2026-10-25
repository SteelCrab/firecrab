import amqp from "amqplib";

const url = process.env.FIRECRAB_QUEUE_NOTIFICATIONS_URL;
const queue = process.env.FIRECRAB_QUEUE_NOTIFICATIONS_NAME;
if (!url || !queue) throw new Error("Set the NOTIFICATIONS Queue binding environment variables");

// Reconnect after broker/network loss and register a new consumer on the new channel.
for (;;) {
  let connection;
  try {
    connection = await amqp.connect(url);
    connection.on("error", () => {});
    const closed = new Promise(resolve => connection.once("close", resolve));
    const channel = await connection.createChannel();
    channel.on("error", () => {});
    await channel.prefetch(1);
    await channel.consume(queue, message => {
      if (!message) return;
      try {
        const event = JSON.parse(message.content.toString());
        // This example logs the event. Real apps must make their effect idempotent
        // using message.properties.messageId and durable application storage.
        console.log(JSON.stringify({ messageId: message.properties.messageId, event }));
        channel.ack(message);
      } catch {
        // On RabbitMQ 4.3, reject(requeue=true) counts a failed delivery.
        // The configured delivery limit eventually moves repeated failures to .failed.
        channel.reject(message, true);
      }
    }, { noAck: false });
    console.log("Waiting for Queue messages");
    await closed;
  } catch {
    console.error("Queue connection unavailable; reconnecting");
  } finally {
    if (connection) { try { await connection.close(); } catch {} }
  }
  await new Promise(resolve => setTimeout(resolve, 2000));
}
