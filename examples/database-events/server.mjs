import { isIP } from "node:net";
import { readFileSync } from "node:fs";
import { createServer } from "node:http";
import { Pool } from "pg";
import { createPool } from "mysql2/promise";
import {
  authenticateEvent,
  inboxTable,
  receiveMysql,
  receivePostgres,
} from "./receiver.mjs";

const uri = new URL(process.env.DATABASE_URL);
const mysql = uri.protocol === "mysql:";
const mode = mysql ? uri.searchParams.get("ssl-mode") : process.env.PGSSLMODE;
// mysql2's hostname verification uses DNS server names. Fail closed for IP
// endpoints instead of accepting a certificate without the intended identity.
if (
  mysql &&
  mode !== "disable" &&
  mode !== "require" &&
  isIP(uri.hostname.replace(/^\[|\]$/g, ""))
) {
  throw new Error("Use a certificate-matching DNS name for this MySQL example");
}
const caPath = mysql ? process.env.MYSQL_SSL_CA : process.env.PGSSLROOTCERT;
const ssl =
  mode === "disable"
    ? undefined
    : {
        ...(caPath ? { ca: readFileSync(caPath, "utf8") } : {}),
        rejectUnauthorized: mode !== "require",
        ...(mysql ? { verifyIdentity: mode !== "require" } : {}),
      };
const options = {
  host: uri.hostname,
  port: Number(uri.port || (mysql ? 3306 : 5432)),
  user: decodeURIComponent(uri.username),
  password: decodeURIComponent(uri.password),
  database: decodeURIComponent(uri.pathname.slice(1)),
  ssl,
};
const pool = mysql
  ? createPool({
      ...options,
      connectionLimit: 5,
      supportBigNumbers: true,
      bigNumberStrings: true,
    })
  : new Pool({ ...options, max: 5 });
const inbox = inboxTable(process.env.FIRECRAB_INBOX_TABLE, mysql);
const receive = mysql ? receiveMysql : receivePostgres;

// Replace this callback with application SQL. The example needs this table:
// CREATE TABLE received_changes(event_id varchar(128) PRIMARY KEY, operation varchar(16) NOT NULL);
// Create it before adding the service binding, so its grants are automatic.
async function apply(client, event) {
  const sql = mysql
    ? "INSERT INTO received_changes(event_id,operation) VALUES (?,?)"
    : "INSERT INTO received_changes(event_id,operation) VALUES ($1,$2)";
  await client.query(sql, [event.id, event.change.operation]);
}

createServer(async (request, response) => {
  if (request.url === "/health" && request.method === "GET") {
    try {
      await pool.query("SELECT 1");
      response.writeHead(200).end("ok");
    } catch {
      response.writeHead(503).end();
    }
    return;
  }
  if (request.url !== "/events/database" || request.method !== "POST") {
    response.writeHead(404).end();
    return;
  }
  try {
    const chunks = [];
    let length = 0;
    for await (const chunk of request) {
      length += chunk.length;
      if (length > 1024 * 1024) {
        response.writeHead(413).end();
        return;
      }
      chunks.push(chunk);
    }
    let event;
    try {
      event = authenticateEvent(
        Buffer.concat(chunks),
        request.headers,
        process.env.FIRECRAB_EVENT_SECRET,
      );
    } catch {
      response.writeHead(401).end();
      return;
    }
    await receive(pool, inbox, event, apply);
    response.writeHead(204).end();
  } catch {
    response.writeHead(503).end();
  }
}).listen(Number(process.env.PORT || 3000), "0.0.0.0");
