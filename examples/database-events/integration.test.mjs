// Disposable databases only. Do not point this test at a production server.
import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { randomUUID } from "node:crypto";
import pg from "pg";
import mysql from "mysql2/promise";
import { receiveMysql, receivePostgres } from "./receiver.mjs";

const fixture = process.env.FIRECRAB_DB_SERVICES_QA_JSON;
test(
  "inbox and business effects commit together on PostgreSQL and MySQL",
  { skip: !fixture },
  async () => {
    for (const input of JSON.parse(await readFile(fixture, "utf8"))) {
      const isMysql = input.uri.startsWith("mysql:");
      const url = new URL(input.uri);
      const config = {
        host: url.hostname,
        port: Number(url.port || (isMysql ? 3306 : 5432)),
        user: decodeURIComponent(url.username),
        password: decodeURIComponent(url.password),
        database: decodeURIComponent(url.pathname.slice(1)),
        ...(isMysql ? { connectionLimit: 8 } : { max: 8 }),
        ...(input.caCertificate
          ? {
              ssl: {
                ca: input.caCertificate,
                rejectUnauthorized: true,
                ...(isMysql ? { verifyIdentity: true } : {}),
              },
            }
          : {}),
      };
      const pool = isMysql ? mysql.createPool(config) : new pg.Pool(config);
      const schema = `firecrab_${randomUUID().replaceAll("-", "")}`;
      const quote = (name) => (isMysql ? `\`${name}\`` : `"${name}"`);
      const inbox = `${quote(schema)}.${quote("inbox")}`,
        effect = `${quote(schema)}.${quote("effects")}`;
      const receive = isMysql ? receiveMysql : receivePostgres;
      const query = (sql, values) => pool.query(sql, values);
      try {
        await query(
          `CREATE ${isMysql ? "DATABASE" : "SCHEMA"} ${quote(schema)}`,
        );
        await query(
          `CREATE TABLE ${inbox}(event_id varchar(128) PRIMARY KEY) ${isMysql ? "ENGINE=InnoDB" : ""}`,
        );
        await query(
          `CREATE TABLE ${effect}(id integer PRIMARY KEY,total integer NOT NULL) ${isMysql ? "ENGINE=InnoDB" : ""}`,
        );
        await query(`INSERT INTO ${effect} VALUES(1,0)`);
        const apply = async (client) => {
          await client.query(`UPDATE ${effect} SET total=total+1 WHERE id=1`);
        };
        const event = { id: `${randomUUID()}:1` };
        const results = await Promise.all(
          Array.from({ length: 8 }, () => receive(pool, inbox, event, apply)),
        );
        assert.equal(results.filter(Boolean).length, 1);
        const failed = { id: `${randomUUID()}:2` };
        await assert.rejects(
          receive(pool, inbox, failed, async (client) => {
            await apply(client);
            throw new Error("simulated_crash_before_commit");
          }),
        );
        assert.equal(await receive(pool, inbox, failed, apply), true);
        assert.equal(await receive(pool, inbox, failed, apply), false);
        const result = await query(`SELECT total FROM ${effect}`);
        assert.equal(Number((isMysql ? result[0] : result.rows)[0].total), 2);
      } finally {
        await query(
          `DROP ${isMysql ? "DATABASE" : "SCHEMA"} IF EXISTS ${quote(schema)} ${isMysql ? "" : "CASCADE"}`,
        );
        await pool.end();
      }
    }
  },
);
