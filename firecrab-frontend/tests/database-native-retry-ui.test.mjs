import { test } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";
import { expect } from "@playwright/test";
import { createFixtureServer } from "./fixture-server.mjs";

const endpointId = "a628d361-67b2-4ca6-bc3c-7bdd2619769b";
const database = {
  id: "baf48bbe-f7d4-4563-a105-98932321b2a2",
  name: "Saved original connection",
  provider: "local",
  engine: "postgres",
  host: "localhost",
  port: 5432,
  database: "fixture",
  tlsMode: "disable",
  writeEnabled: false,
};
const uri =
  "postgresql://fixture:private-browser-fixture-password@localhost/fixture";
const prefix = "firecrab.native-mutation:";
async function setup(t) {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  page.setDefaultTimeout(5000);
  t.after(async () => {
    await page.close();
    await browser.close();
    await fixture.close();
  });
  fixture.handlers.set("GET /api/iac/identity", ({ send }) =>
    send(200, { endpointId }),
  );
  await page.addInitScript(() => localStorage.setItem("firecrab.locale", "en"));
  await page.goto(`${fixture.url}/#/pipe/database`);
  return { fixture, page };
}
async function fill(page, name = "Requested connection", passwordUri = uri) {
  await page.getByRole("button", { name: "Add database", exact: true }).click();
  await page.getByLabel("Connection name", { exact: true }).fill(name);
  await page.getByLabel("Database URI", { exact: true }).fill(passwordUri);
}
const submit = (page) =>
  page.getByRole("button", { name: "Save connection", exact: true }).click();
const pending = (page) =>
  page.evaluate(
    (prefix) =>
      Object.entries(sessionStorage).filter(([key]) => key.startsWith(prefix)),
    prefix,
  );
function receipt(id, state) {
  return {
    operationId: id,
    action: "create_database_connection",
    state,
    ownership: {
      endpointId,
      resource: { kind: "database_connection", id: database.id },
    },
  };
}

test("lost database creation acknowledgement resolves the original resource without another create", async (t) => {
  const { fixture, page } = await setup(t);
  let requests = 0;
  fixture.handlers.set("POST /api/pipe/databases", ({ req, send }) => {
    requests++;
    const id = req.headers["x-firecrab-operation-id"];
    assert.match(id, /^[a-f0-9-]{36}$/);
    assert.equal(req.headers["x-firecrab-endpoint-id"], endpointId);
    fixture.state.databases = [database];
    fixture.handlers.set(`GET /api/iac/native-operations/${id}`, ({ send }) =>
      send(200, receipt(id, "succeeded")),
    );
    send(503, { error: { message: "Creation acknowledgement lost" } });
  });
  await fill(page);
  await submit(page);
  await expect(page.getByRole("status")).toContainText(
    "The original request completed.",
  );
  await expect(
    page.getByRole("button", { name: `Browse ${database.name}` }),
  ).toBeVisible();
  await expect(page.getByLabel("Database URI", { exact: true })).toHaveCount(0);
  assert.equal(requests, 1);
  assert.equal(fixture.state.databases.length, 1);
  assert.deepEqual(await pending(page), []);
});

test("database retry keeps its endpoint and operation across reload without storing credentials", async (t) => {
  const { fixture, page } = await setup(t);
  const ids = [];
  let state = "needs_recovery";
  fixture.handlers.set("POST /api/pipe/databases", ({ req, send }) => {
    const id = req.headers["x-firecrab-operation-id"];
    ids.push(id);
    assert.equal(req.headers["x-firecrab-endpoint-id"], endpointId);
    fixture.handlers.set(`GET /api/iac/native-operations/${id}`, ({ send }) =>
      ids.length === 1
        ? send(503, { error: { message: "Receipt temporarily unavailable" } })
        : send(200, receipt(id, state)),
    );
    if (state === "succeeded") {
      fixture.state.databases = [database];
      send(200, database);
    } else send(503, { error: { message: "Database outcome unknown" } });
  });
  await fill(page);
  await submit(page);
  await expect(page.getByRole("alert")).toContainText(
    "Database outcome unknown",
  );
  const saved = await pending(page);
  assert.equal(saved.length, 1);
  assert.deepEqual(Object.keys(JSON.parse(saved[0][1])).sort(), [
    "endpointId",
    "operationId",
  ]);
  assert(!JSON.stringify(saved).includes("private-browser"));
  assert(!JSON.stringify(saved).includes("postgresql"));
  await page.reload();
  await fill(page);
  await submit(page);
  await expect(
    page.getByRole("link", { name: "Review original operation" }),
  ).toBeVisible();
  assert.equal(ids.length, 2);
  assert.equal(ids[0], ids[1]);
  assert.deepEqual(await pending(page), saved);
  state = "succeeded";
  await submit(page);
  await expect(page.getByRole("status")).toContainText(
    "The original request completed.",
  );
  assert.equal(new Set(ids).size, 1);
  assert.deepEqual(await pending(page), []);
});

test("a known no-effect database failure permits corrected input with a new operation", async (t) => {
  const { fixture, page } = await setup(t);
  const ids = [];
  fixture.handlers.set("POST /api/pipe/databases", ({ req, send }) => {
    const id = req.headers["x-firecrab-operation-id"];
    ids.push(id);
    if (ids.length === 1) {
      fixture.handlers.set(`GET /api/iac/native-operations/${id}`, ({ send }) =>
        send(200, receipt(id, "failed")),
      );
      send(502, { error: { message: "Database verification failed" } });
    } else {
      fixture.state.databases = [database];
      send(200, database);
    }
  });
  await fill(page);
  await submit(page);
  await expect(page.getByRole("alert")).toContainText(
    "Database verification failed",
  );
  assert.deepEqual(await pending(page), []);
  await page
    .getByLabel("Database URI", { exact: true })
    .fill("postgresql://fixture:corrected-private-password@localhost/fixture");
  await submit(page);
  await expect(
    page.getByRole("button", { name: `Browse ${database.name}` }),
  ).toBeVisible();
  assert.equal(ids.length, 2);
  assert.notEqual(ids[0], ids[1]);
  assert.deepEqual(await pending(page), []);
});

test("completion after leaving the database page remains unacknowledged until the original resource is shown", async (t) => {
  const { fixture, page } = await setup(t);
  const ids = [];
  let release;
  fixture.handlers.set("POST /api/pipe/databases", async ({ req, send }) => {
    const id = req.headers["x-firecrab-operation-id"];
    ids.push(id);
    if (ids.length === 1)
      await new Promise((resolve) => {
        release = resolve;
      });
    fixture.state.databases = [database];
    send(200, database);
  });
  await fill(page);
  await submit(page);
  await expect.poll(() => ids.length).toBe(1);
  page.once("dialog", (dialog) => dialog.accept());
  await page.goto(`${fixture.url}/#/images`);
  await expect(
    page.getByRole("heading", { name: "Images", exact: true }),
  ).toBeVisible();
  release();
  await expect.poll(() => fixture.state.databases.length).toBe(1);
  assert.equal((await pending(page)).length, 1);
  await page.goto(`${fixture.url}/#/pipe/database`);
  await fill(page);
  await submit(page);
  await expect(page.getByRole("status")).toContainText(
    "The original request completed.",
  );
  assert.equal(ids.length, 2);
  assert.equal(ids[0], ids[1]);
  assert.deepEqual(await pending(page), []);
});

async function accessPanel(fixture, page) {
  fixture.state.databases = [database];
  const base = `/api/pipe/databases/${database.id}`;
  const serviceId = "8f51d057-15ef-4212-b7e4-dcc8affb852d";
  fixture.handlers.set(`GET ${base}/tables`, ({ send }) =>
    send(200, { tables: [], truncated: false }),
  );
  fixture.handlers.set("GET /api/micro-services", ({ send }) =>
    send(200, [{ id: serviceId, name: "Handler" }]),
  );
  for (const [path, value] of [
    [`${base}/triggers`, []],
    [`${base}/jobs`, []],
    ["/api/pipe/database-rabbitmq", { configured: false }],
    ["/api/pipe/database-rabbitmq/managed", null],
    ["/api/pipe/database-maintenance/tools", { postgres: false, mysql: false }],
  ])
    fixture.handlers.set(`GET ${path}`, ({ send }) => send(200, value));
  await page.reload();
  await page.getByRole("button", { name: `Browse ${database.name}` }).click();
  await page
    .getByRole("button", { name: "Service access and events", exact: true })
    .click();
  await page
    .locator("form")
    .filter({
      has: page.getByRole("button", { name: "Grant access", exact: true }),
    })
    .getByRole("combobox")
    .first()
    .selectOption(serviceId);
  return { base, serviceId };
}

test("binding creation keeps its account UUID and operation UUID across browser reload", async (t) => {
  const { fixture, page } = await setup(t);
  let bindings = [];
  const ids = [];
  const base = `/api/pipe/databases/${database.id}`;
  fixture.handlers.set(`GET ${base}/bindings`, ({ send }) =>
    send(200, bindings),
  );
  fixture.handlers.set(`POST ${base}/bindings`, ({ req, body, send }) => {
    const id = req.headers["x-firecrab-operation-id"];
    ids.push(id);
    assert.equal(body.requestId, id);
    assert.equal(req.headers["x-firecrab-endpoint-id"], endpointId);
    fixture.handlers.set(`GET /api/iac/native-operations/${id}`, ({ send }) =>
      send(200, {
        ...receipt(id, "needs_recovery"),
        action: "create_database_binding",
        ownership: { endpointId, resource: { kind: "database_binding", id } },
      }),
    );
    if (ids.length < 3)
      send(503, { error: { message: "Binding outcome unknown" } });
    else {
      const saved = {
        id,
        serviceId: body.serviceId,
        access: body.access,
        ready: true,
      };
      bindings = [saved];
      send(200, saved);
    }
  });
  await accessPanel(fixture, page);
  await page.getByRole("button", { name: "Grant access", exact: true }).click();
  await expect(
    page.getByRole("link", { name: "Review original operation" }),
  ).toBeVisible();
  const saved = await pending(page);
  assert.equal(saved.length, 1);
  await accessPanel(fixture, page);
  await page.getByRole("button", { name: "Grant access", exact: true }).click();
  await expect(
    page.getByRole("link", { name: "Review original operation" }),
  ).toBeVisible();
  assert.equal(ids.length, 2);
  assert.equal(ids[0], ids[1]);
  assert.deepEqual(await pending(page), saved);
  await page.getByRole("button", { name: "Grant access", exact: true }).click();
  await expect(page.getByRole("status")).toContainText("Review service access");
  assert.equal(new Set(ids).size, 1);
  assert.equal(bindings.length, 1);
  assert.deepEqual(await pending(page), []);
});

test("lost binding revocation acknowledgement resolves its original receipt without another revoke", async (t) => {
  const { fixture, page } = await setup(t);
  const id = "ddf309ed-96b9-4d27-9541-fc8a90d5e252";
  let bindings = [
    {
      id,
      serviceId: "8f51d057-15ef-4212-b7e4-dcc8affb852d",
      access: "read",
      ready: true,
    },
  ];
  let deletes = 0;
  const base = `/api/pipe/databases/${database.id}`;
  fixture.handlers.set(`GET ${base}/bindings`, ({ send }) =>
    send(200, bindings),
  );
  fixture.handlers.set(
    `DELETE /api/pipe/database-bindings/${id}`,
    ({ req, send }) => {
      deletes++;
      bindings = [];
      const operationId = req.headers["x-firecrab-operation-id"];
      assert.equal(req.headers["x-firecrab-endpoint-id"], endpointId);
      fixture.handlers.set(
        `GET /api/iac/native-operations/${operationId}`,
        ({ send }) =>
          send(200, {
            operationId,
            action: "delete_database_binding",
            state: "succeeded",
            ownership: {
              endpointId,
              resource: { kind: "database_binding", id },
            },
          }),
      );
      send(503, { error: { message: "Revocation acknowledgement lost" } });
    },
  );
  await accessPanel(fixture, page);
  page.once("dialog", (dialog) => dialog.accept());
  await page.getByRole("button", { name: "Revoke", exact: true }).click();
  await expect(page.getByRole("status")).toContainText("Review service access");
  await expect(
    page.getByRole("button", { name: "Revoke", exact: true }),
  ).toHaveCount(0);
  assert.equal(deletes, 1);
  assert.deepEqual(await pending(page), []);
});

test("legacy pending binding setup retries its existing account instead of allocating another account UUID", async (t) => {
  const { fixture, page } = await setup(t);
  const id = "457cd429-cc1d-4d33-b11c-ec229468a3dd";
  const serviceId = "8f51d057-15ef-4212-b7e4-dcc8affb852d";
  let bindings = [
    { id, serviceId, access: "read", envName: "DATABASE_URL", ready: false },
  ];
  let requests = 0;
  const base = `/api/pipe/databases/${database.id}`;
  fixture.handlers.set(`GET ${base}/bindings`, ({ send }) =>
    send(200, bindings),
  );
  fixture.handlers.set(`POST ${base}/bindings`, ({ req, body, send }) => {
    requests++;
    assert.equal(body.requestId, id);
    assert.notEqual(req.headers["x-firecrab-operation-id"], id);
    assert.equal(body.serviceId, serviceId);
    assert.equal(body.envName, "DATABASE_URL");
    bindings = [{ ...bindings[0], ready: true }];
    send(200, bindings[0]);
  });
  await accessPanel(fixture, page);
  await page.getByRole("button", { name: "Retry setup", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "Retry setup", exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "Revoke", exact: true }),
  ).toBeVisible();
  assert.equal(requests, 1);
  assert.equal(bindings[0].id, id);
  assert.deepEqual(await pending(page), []);
});

async function eventPanel(fixture, page, triggers) {
  fixture.handlers.set(
    `GET /api/pipe/databases/${database.id}/bindings`,
    ({ send }) => send(200, []),
  );
  const { base, serviceId } = await accessPanel(fixture, page);
  fixture.handlers.set(`GET ${base}/tables`, ({ send }) =>
    send(200, {
      tables: [{ schema: "public", name: "orders" }],
      truncated: false,
    }),
  );
  fixture.handlers.set(`GET ${base}/triggers`, ({ send }) =>
    send(200, triggers()),
  );
  fixture.handlers.set("GET /api/pipe/database-rabbitmq", ({ send }) =>
    send(200, { configured: true }),
  );
  await page.reload();
  await page.getByRole("button", { name: `Browse ${database.name}` }).click();
  await page
    .getByRole("button", { name: "Service access and events", exact: true })
    .click();
  await page
    .getByRole("combobox", { name: "Source table", exact: true })
    .selectOption(JSON.stringify(["public", "orders"]));
  await page
    .getByRole("combobox", { name: "Handler service", exact: true })
    .selectOption(serviceId);
  return { base, serviceId };
}

function eventReceipt(
  operationId,
  state,
  id,
  action = "create_database_events",
) {
  return {
    operationId,
    state,
    action,
    ownership: {
      endpointId,
      resource: { kind: "database_events", id: `${database.id}:${id}` },
    },
  };
}

test("event setup retains its qualified trigger identity across reload and resolves a lost acknowledgement", async (t) => {
  const { fixture, page } = await setup(t);
  let triggers = [];
  const ids = [];
  let completed = false;
  const base = `/api/pipe/databases/${database.id}`;
  fixture.handlers.set(`POST ${base}/triggers`, ({ req, body, send }) => {
    const id = req.headers["x-firecrab-operation-id"];
    ids.push(id);
    assert.equal(body.requestId, id);
    assert.equal(req.headers["x-firecrab-endpoint-id"], endpointId);
    fixture.handlers.set(`GET /api/iac/native-operations/${id}`, ({ send }) =>
      send(
        200,
        eventReceipt(id, completed ? "succeeded" : "needs_recovery", id),
      ),
    );
    if (completed)
      triggers = [
        { ...body, id, installed: true, paused: false, detached: false },
      ];
    send(503, { error: { message: "Event acknowledgement unavailable" } });
  });
  await eventPanel(fixture, page, () => triggers);
  await page
    .getByRole("button", { name: "Enable events", exact: true })
    .click();
  await expect(
    page.getByRole("link", { name: "Review original operation" }),
  ).toBeVisible();
  const saved = await pending(page);
  assert.equal(saved.length, 1);
  await eventPanel(fixture, page, () => triggers);
  completed = true;
  await page
    .getByRole("button", { name: "Enable events", exact: true })
    .click();
  await expect(page.getByRole("status")).toContainText(
    "Review event configuration",
  );
  await expect(
    page.getByRole("button", { name: "Detach capture", exact: true }),
  ).toBeVisible();
  assert.equal(ids.length, 2);
  assert.equal(new Set(ids).size, 1);
  assert.equal(triggers.length, 1);
  assert.deepEqual(await pending(page), []);
});

test("lost event removal acknowledgement resolves its qualified receipt without repeating deletion", async (t) => {
  const { fixture, page } = await setup(t);
  const id = "a079dc53-16f3-4e5f-87d5-7312bb6b3a6f";
  let triggers = [
    {
      id,
      schema: "public",
      table: "orders",
      serviceId: "8f51d057-15ef-4212-b7e4-dcc8affb852d",
      detached: true,
      installed: true,
      paused: false,
      pending: 0,
      failed: 0,
    },
  ];
  let deletes = 0;
  fixture.handlers.set(
    `DELETE /api/pipe/database-triggers/${id}`,
    ({ req, send }) => {
      deletes++;
      triggers = [];
      const operation = req.headers["x-firecrab-operation-id"];
      fixture.handlers.set(
        `GET /api/iac/native-operations/${operation}`,
        ({ send }) =>
          send(
            200,
            eventReceipt(operation, "succeeded", id, "delete_database_events"),
          ),
      );
      send(503, { error: { message: "Removal acknowledgement lost" } });
    },
  );
  await eventPanel(fixture, page, () => triggers);
  page.once("dialog", (dialog) => dialog.accept());
  await page
    .getByRole("button", { name: "Remove trigger", exact: true })
    .click();
  await expect(page.getByRole("status")).toContainText(
    "Review event configuration",
  );
  await expect(
    page.getByRole("button", { name: "Remove trigger", exact: true }),
  ).toHaveCount(0);
  assert.equal(deletes, 1);
  assert.deepEqual(await pending(page), []);
});

test("a receipt for another database cannot acknowledge event setup", async (t) => {
  const { fixture, page } = await setup(t);
  fixture.handlers.set(
    `POST /api/pipe/databases/${database.id}/triggers`,
    ({ req, send }) => {
      const id = req.headers["x-firecrab-operation-id"];
      const receipt = eventReceipt(id, "succeeded", id);
      receipt.ownership.resource.id = `9f9a4772-7e77-47ea-a39c-38497e64520f:${id}`;
      fixture.handlers.set(`GET /api/iac/native-operations/${id}`, ({ send }) =>
        send(200, receipt),
      );
      send(503, { error: { message: "Event acknowledgement lost" } });
    },
  );
  await eventPanel(fixture, page, () => []);
  await page
    .getByRole("button", { name: "Enable events", exact: true })
    .click();
  await expect(
    page.getByRole("link", { name: "Review original operation" }),
  ).toBeVisible();
  assert.equal((await pending(page)).length, 1);
  await expect(
    page.getByRole("button", { name: "Detach capture", exact: true }),
  ).toHaveCount(0);
});
