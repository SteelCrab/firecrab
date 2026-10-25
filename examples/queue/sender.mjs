import amqp from "amqplib";
import { randomUUID } from "node:crypto";

const url = process.env.FIRECRAB_QUEUE_NOTIFICATIONS_URL;
const queue = process.env.FIRECRAB_QUEUE_NOTIFICATIONS_NAME;
if (!url || !queue) throw new Error("Set the NOTIFICATIONS Queue binding environment variables");
const connection = await amqp.connect(url);
connection.on("error", () => console.error("Queue connection failed; retry unconfirmed work after reconnecting"));
try {
  const channel = await connection.createConfirmChannel();
  let returned = false;
  channel.on("return", () => { returned = true; });
  const messageId = process.env.MESSAGE_ID || randomUUID();
  channel.sendToQueue(queue, Buffer.from(JSON.stringify({ type: "user.created", userId: process.argv[2] || "123" })), {
    persistent: true, mandatory: true, contentType: "application/json", messageId,
  });
  await channel.waitForConfirms();
  if (returned) throw new Error("Queue did not route the message");
  console.log(`RabbitMQ accepted ${messageId}; application processing is still pending`);
  await channel.close();
} finally { await connection.close(); }
