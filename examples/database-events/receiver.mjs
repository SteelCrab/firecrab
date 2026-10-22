// Verify the immutable HTTP body before parsing or opening a transaction.
import { createHmac, timingSafeEqual } from "node:crypto";

export function authenticateEvent(body, headers, secret, now = Date.now()) {
  const timestamp = String(headers["x-firecrab-timestamp"] ?? "");
  const supplied = String(headers["x-firecrab-signature"] ?? "");
  if (
    !secret ||
    !/^\d{10}$/.test(timestamp) ||
    Math.abs(now / 1000 - Number(timestamp)) > 300 ||
    !/^sha256=[0-9a-f]{64}$/.test(supplied)
  )
    throw new Error("invalid_event_signature");
  const expected = createHmac("sha256", secret)
    .update(timestamp)
    .update(".")
    .update(body)
    .digest();
  if (!timingSafeEqual(expected, Buffer.from(supplied.slice(7), "hex")))
    throw new Error("invalid_event_signature");
  const event = JSON.parse(body.toString("utf8"));
  if (
    !/^[0-9a-f-]{36}:\d+$/.test(event.id) ||
    event.id !== headers["x-firecrab-event-id"]
  )
    throw new Error("invalid_event_id");
  return event;
}

// The receipt INSERT and application callback share this transaction. A crash
// before COMMIT rolls both back. A repeated committed event skips the callback.
// External HTTP/payment effects need their own idempotency keys as well.
export async function receivePostgres(pool, table, event, apply) {
  const client = await pool.connect();
  try {
    await client.query("BEGIN");
    const receipt = await client.query(
      `INSERT INTO ${table}(event_id) VALUES ($1) ON CONFLICT DO NOTHING`,
      [event.id],
    );
    if (receipt.rowCount === 1) await apply(client, event);
    await client.query("COMMIT");
    return receipt.rowCount === 1;
  } catch (error) {
    await client.query("ROLLBACK");
    throw error;
  } finally {
    client.release();
  }
}

export async function receiveMysql(pool, table, event, apply) {
  const client = await pool.getConnection();
  try {
    await client.beginTransaction();
    let inserted;
    try {
      await client.execute(`INSERT INTO ${table}(event_id) VALUES (?)`, [
        event.id,
      ]);
      inserted = true;
    } catch (error) {
      if (error.code !== "ER_DUP_ENTRY") throw error;
      inserted = false;
    }
    if (inserted) await apply(client, event);
    await client.commit();
    return inserted;
  } catch (error) {
    await client.rollback();
    throw error;
  } finally {
    client.release();
  }
}

// Only use the platform-generated inbox identifier. Request data cannot select SQL.
export function inboxTable(value, mysql) {
  const pattern = mysql
    ? /^`firecrab_[0-9a-f]{32}`\.`inbox_[0-9a-f]{32}`$/
    : /^"firecrab_[0-9a-f]{32}"\."inbox_[0-9a-f]{32}"$/;
  if (!pattern.test(value ?? ""))
    throw new Error("missing_write_database_binding");
  return value;
}
