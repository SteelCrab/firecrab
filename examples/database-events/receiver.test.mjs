import { test } from "node:test";
import assert from "node:assert/strict";
import { createHmac } from "node:crypto";
import { authenticateEvent, inboxTable } from "./receiver.mjs";

test("verify timestamp, event ID and the exact body", () => {
  const id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa:123";
  const body = Buffer.from(
    JSON.stringify({ id, change: { operation: "insert" } }),
  );
  const timestamp = "1792692000";
  const headers = {
    "x-firecrab-event-id": id,
    "x-firecrab-timestamp": timestamp,
    "x-firecrab-signature":
      "sha256=" +
      createHmac("sha256", "secret")
        .update(timestamp)
        .update(".")
        .update(body)
        .digest("hex"),
  };
  assert.equal(
    authenticateEvent(body, headers, "secret", Number(timestamp) * 1000).id,
    id,
  );
  assert.throws(() =>
    authenticateEvent(
      Buffer.from(body.toString().replace("insert", "delete")),
      headers,
      "secret",
      Number(timestamp) * 1000,
    ),
  );
  assert.throws(() =>
    authenticateEvent(
      body,
      headers,
      "secret",
      Number(timestamp) * 1000 + 301000,
    ),
  );
  assert.throws(() =>
    authenticateEvent(
      body,
      { ...headers, "x-firecrab-event-id": "other" },
      "secret",
      Number(timestamp) * 1000,
    ),
  );
});
test("only a generated inbox name can select the SQL table", () => {
  const id = "a".repeat(32);
  assert.equal(
    inboxTable(`"firecrab_${id}"."inbox_${id}"`, false),
    `"firecrab_${id}"."inbox_${id}"`,
  );
  assert.equal(
    inboxTable(`\`firecrab_${id}\`.\`inbox_${id}\``, true),
    `\`firecrab_${id}\`.\`inbox_${id}\``,
  );
  assert.throws(() =>
    inboxTable("app.receipts; DROP TABLE app.customers", false),
  );
});
