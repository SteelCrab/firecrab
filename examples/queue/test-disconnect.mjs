// Called by the ignored Rust integration test against a disposable RabbitMQ instance.
import assert from "node:assert/strict";
import amqp from "amqplib";
import { randomUUID } from "node:crypto";

const pub = process.env.FIRECRAB_TEST_AMQP_PUBLISH_URL;
const sub = process.env.FIRECRAB_TEST_AMQP_CONSUME_URL;
const queue = process.env.FIRECRAB_TEST_QUEUE;
const connections = new Set();
async function connect(url) {
  const connection = await amqp.connect(url);
  connection.on("error", () => {});
  connections.add(connection);
  return connection;
}
async function close(connection) {
  try { await connection.close(); } finally { connections.delete(connection); }
}
async function crash(connection) {
  const closed = new Promise(resolve => connection.once("close", resolve));
  connection.connection.stream.destroy();
  await closed;
  connections.delete(connection);
}
async function receive(channel) {
  for (let attempt = 0; attempt < 200; attempt++) {
    const message = await channel.get(queue, { noAck: false });
    if (message) return message;
    await new Promise(resolve => setTimeout(resolve, 50));
  }
  throw new Error("Expected a queued message");
}
try {
  const producer = await connect(pub);
  const publishing = await producer.createConfirmChannel();
  publishing.on("error", () => {});
  let returned = false;
  publishing.on("return", () => { returned = true; });
  const messageId = randomUUID();
  publishing.sendToQueue(queue, Buffer.from("kept-after-A-disconnect"), { persistent: true, mandatory: true, messageId });
  await publishing.waitForConfirms();
  assert.equal(returned, false);
  await crash(producer);
  console.log("PASS: confirmed messages survive A disconnecting");

  const first = await connect(sub);
  const firstChannel = await first.createChannel();
  const before = await receive(firstChannel);
  assert.equal(before.properties.messageId, messageId);
  // Simulate B stopping after reading the job, before its ACK reaches RabbitMQ.
  await crash(first);
  const second = await connect(sub);
  const secondChannel = await second.createChannel();
  const after = await receive(secondChannel);
  assert.equal(after.properties.messageId, messageId);
  assert.equal(after.fields.redelivered, true);
  secondChannel.ack(after);
  await secondChannel.close();
  await close(second);
  console.log("PASS: unacknowledged messages are redelivered after B disconnects");

  const third = await connect(sub);
  const thirdChannel = await third.createChannel();
  assert.equal(await thirdChannel.get(queue, { noAck: false }), false);
  await close(third);
  console.log("PASS: acknowledged messages are removed");

  const sender = await connect(pub);
  const senderChannel = await sender.createConfirmChannel();
  senderChannel.on("error", () => {});
  const failedId = randomUUID();
  senderChannel.sendToQueue(queue, Buffer.from("repeated-failure"), { persistent: true, mandatory: true, messageId: failedId });
  await senderChannel.waitForConfirms();
  await close(sender);
  const receiver = await connect(sub);
  const receiverChannel = await receiver.createChannel();
  for (let index = 0; index < 3; index++) {
    const message = await receive(receiverChannel);
    assert.equal(message.properties.messageId, failedId);
    receiverChannel.reject(message, true);
  }
  await receiverChannel.close();
  await close(receiver);
  console.log("PASS: repeated failures reach the configured delivery limit");

  const deniedSender = await connect(pub);
  const deniedChannel = await deniedSender.createChannel();
  deniedChannel.on("error", () => {});
  await assert.rejects(deniedChannel.get(queue, { noAck: false }));
  await close(deniedSender);
  const deniedReceiver = await connect(sub);
  const deniedPublishing = await deniedReceiver.createConfirmChannel();
  deniedPublishing.on("error", () => {});
  deniedPublishing.sendToQueue(queue, Buffer.from("must-not-publish"), { persistent: true });
  await assert.rejects(deniedPublishing.waitForConfirms());
  await close(deniedReceiver);
  console.log("PASS: sender and receiver permissions are separate");
} catch {
  // Connection URLs contain credentials: never print library errors or full stacks.
  console.error("FAIL: RabbitMQ disconnect or permission assertion failed");
  process.exitCode = 1;
} finally {
  for (const connection of connections) { try { await connection.close(); } catch {} }
}
