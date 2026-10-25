// Used by the disposable Linux/KVM CI fixture. Never print connection errors.
import assert from "node:assert/strict";
import amqp from "amqplib";
const mode = process.argv[2];
let connection;
try {
  connection = await amqp.connect(process.env.FIRECRAB_QA_AMQP_URL);
  connection.on("error", () => {});
  const channel = await connection.createConfirmChannel();
  channel.on("error", () => {});
  const queue = process.env.FIRECRAB_QA_QUEUE;
  if (mode === "seed") {
    let returned = false;
    channel.on("return", () => { returned = true; });
    channel.sendToQueue(queue, Buffer.from("persistent-runtime-probe"), {
      persistent: true, mandatory: true, messageId: process.env.FIRECRAB_QA_MESSAGE_ID,
    });
    await channel.waitForConfirms();
    assert.equal(returned, false);
  } else if (mode === "read") {
    let message;
    for (let attempt = 0; attempt < 100 && !message; attempt++) {
      message = await channel.get(queue, { noAck: false });
      if (!message) await new Promise(resolve => setTimeout(resolve, 100));
    }
    assert.ok(message);
    assert.equal(message.content.toString(), process.env.FIRECRAB_QA_EXPECT_BODY);
    if (process.env.FIRECRAB_QA_MESSAGE_ID) {
      assert.equal(message.properties.messageId, process.env.FIRECRAB_QA_MESSAGE_ID);
    }
    channel.ack(message);
    await channel.close();
  } else { throw new Error("Unknown fixture operation"); }
} catch {
  console.error("FAIL: RabbitMQ runtime assertion failed");
  process.exitCode = 1;
} finally {
  if (connection) { try { await connection.close(); } catch {} }
}
