import { test } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";
import { expect } from "@playwright/test";
import { createFixtureServer } from "./fixture-server.mjs";

test("database row edits retry the same request and preserve optimistic row version", async () => {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  const browser = await chromium.launch({ headless: true }),
    page = await browser.newPage();
  const db = {
    id: "db-1",
    name: "Fixture DB",
    provider: "local",
    engine: "postgres",
    host: "localhost",
    port: 5432,
    database: "fixture",
    tlsMode: "require",
    writeEnabled: true,
  };
  fixture.state.databases = [db];
  fixture.handlers.set("GET /api/pipe/databases/db-1/tables", ({ send }) =>
    send(200, {
      tables: [{ schema: "public", name: "items" }],
      truncated: false,
    }),
  );
  fixture.handlers.set("POST /api/pipe/databases/db-1/rows", ({ send }) =>
    send(200, {
      definition: {
        transactional: true,
        primaryKey: ["id"],
        columns: [
          {
            name: "id",
            dataType: "text",
            nullable: false,
            defaultValue: null,
            generated: false,
            primaryKey: true,
            binary: false,
          },
          {
            name: "value",
            dataType: "text",
            nullable: false,
            defaultValue: null,
            generated: false,
            primaryKey: false,
            binary: false,
          },
        ],
      },
      rows: [
        {
          key: { id: "1" },
          version: "original-version",
          values: { id: "1", value: "old" },
          truncatedColumns: [],
        },
      ],
      offset: 0,
      hasMore: false,
    }),
  );
  let attempts = 0;
  fixture.handlers.set(
    "POST /api/pipe/databases/db-1/mutations",
    ({ send }) => {
      if (++attempts === 1)
        send(503, { error: { message: "unknown outcome" } });
      else send(200, { requestId: "mutation-1" });
    },
  );
  try {
    await page.addInitScript(() =>
      Object.defineProperty(crypto, "randomUUID", {
        value: () => "mutation-1",
      }),
    );
    await page.goto(`${fixture.url}/#/pipe/database`);
    await page.getByRole("button", { name: "Browse Fixture DB" }).click();
    await page.locator("#pipe-database-table").selectOption("0");
    await page.getByRole("button", { name: "Edit", exact: true }).click();
    const field = page.getByRole("group", { name: "value · text" });
    await field.locator("input").fill("new");
    await page.getByRole("button", { name: "Save", exact: true }).click();
    await expect(
      page.getByText("The result is unknown. Retry the same request."),
    ).toBeVisible();
    await page.getByRole("button", { name: "Retry", exact: true }).click();
    await expect.poll(() => attempts).toBe(2);
    const calls = fixture.requests.filter(
      (r) => r.method === "POST" && r.path.endsWith("/mutations"),
    );
    assert.deepEqual(calls[0].body, calls[1].body);
    assert.equal(calls[0].body.version, "original-version");
    assert.deepEqual(calls[0].body.values, { value: "new" });
  } finally {
    await page.close();
    await browser.close();
    await fixture.close();
  }
});

test("database maintenance requires the database name and external-writer confirmation", async () => {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  const browser = await chromium.launch({ headless: true }),
    page = await browser.newPage();
  fixture.state.databases = [
    {
      id: "db-1",
      name: "Fixture DB",
      provider: "local",
      engine: "postgres",
      host: "localhost",
      port: 5432,
      database: "fixture",
      tlsMode: "require",
      writeEnabled: false,
    },
  ];
  fixture.handlers.set("GET /api/pipe/databases/db-1/tables", ({ send }) =>
    send(200, { tables: [], truncated: false }),
  );
  for (const path of [
    "/api/pipe/databases/db-1/bindings",
    "/api/pipe/databases/db-1/triggers",
    "/api/pipe/databases/db-1/jobs",
  ])
    fixture.handlers.set(`GET ${path}`, ({ send }) => send(200, []));
  fixture.handlers.set("GET /api/pipe/database-rabbitmq", ({ send }) =>
    send(200, { configured: false, brokers: [], tls: true }),
  );
  fixture.handlers.set("GET /api/pipe/database-rabbitmq/managed", ({ send }) =>
    send(200, { phase: "idle", error: null }),
  );
  fixture.handlers.set("POST /api/pipe/databases/db-1/migrations", ({ send }) =>
    send(202, { id: "job-1" }),
  );
  try {
    await page.addInitScript(() =>
      Object.defineProperty(crypto, "randomUUID", {
        value: () => "job-request-1",
      }),
    );
    await page.goto(`${fixture.url}/#/pipe/database`);
    await page.getByRole("button", { name: "Browse Fixture DB" }).click();
    await page.getByRole("button", { name: "Backup and schema" }).click();
    await page.getByLabel("Version", { exact: true }).fill("v1");
    await page.getByLabel("SQL", { exact: true }).fill("SELECT 1");
    await page.getByRole("button", { name: "Apply schema change" }).click();
    await expect(page.getByRole("alert")).toContainText(
      "Type the database name",
    );
    assert.equal(
      fixture.requests.filter((r) => r.path.endsWith("/migrations")).length,
      0,
    );
    await page.getByLabel("Confirm database name").fill("fixture");
    await page.getByLabel("All external writers are stopped.").check();
    await page.getByRole("button", { name: "Apply schema change" }).click();
    await expect
      .poll(() => fixture.requests.some((r) => r.path.endsWith("/migrations")))
      .toBe(true);
    const body = fixture.requests.find((r) =>
      r.path.endsWith("/migrations"),
    ).body;
    assert.deepEqual(body, {
      requestId: "job-request-1",
      version: "v1",
      sql: "SELECT 1",
      confirmDatabase: "fixture",
      externalWritersStopped: true,
    });
  } finally {
    await page.close();
    await browser.close();
    await fixture.close();
  }
});

test("database jobs remain visible when another operations endpoint fails", async () => {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  const browser = await chromium.launch({ headless: true }),
    page = await browser.newPage();
  fixture.state.databases = [
    {
      id: "db-1",
      name: "Fixture DB",
      provider: "local",
      engine: "postgres",
      host: "localhost",
      port: 5432,
      database: "fixture",
      tlsMode: "require",
      writeEnabled: false,
    },
  ];
  fixture.handlers.set("GET /api/pipe/databases/db-1/tables", ({ send }) =>
    send(200, { tables: [], truncated: false }),
  );
  fixture.handlers.set("GET /api/pipe/databases/db-1/bindings", ({ send }) =>
    send(503, { error: { message: "binding service unavailable" } }),
  );
  fixture.handlers.set("GET /api/pipe/databases/db-1/triggers", ({ send }) =>
    send(200, []),
  );
  fixture.handlers.set("GET /api/pipe/databases/db-1/jobs", ({ send }) =>
    send(200, [
      {
        id: "job-1",
        kind: "backup",
        status: "completed",
        error: null,
        createdAt: "2026-01-01",
      },
    ]),
  );
  fixture.handlers.set("GET /api/pipe/database-rabbitmq", ({ send }) =>
    send(200, { configured: false, brokers: [], tls: true }),
  );
  fixture.handlers.set("GET /api/pipe/database-rabbitmq/managed", ({ send }) =>
    send(200, { phase: "idle", error: null }),
  );
  try {
    await page.goto(`${fixture.url}/#/pipe/database`);
    await page.getByRole("button", { name: "Browse Fixture DB" }).click();
    await page.getByRole("button", { name: "Backup and schema" }).click();
    await expect(page.getByText(/backup · completed/)).toBeVisible();
    await expect(page.getByRole("alert")).toContainText(
      "binding service unavailable",
    );
  } finally {
    await page.close();
    await browser.close();
    await fixture.close();
  }
});
