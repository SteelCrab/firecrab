import { test } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";
import { expect } from "@playwright/test";
import { createFixtureServer } from "./fixture-server.mjs";

test("Queue setup keeps edits across polling and validation, then creates the engine", async () => {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  let engine = null;
  let queues = [];
  fixture.handlers.set("GET /api/queue/engine", ({ send }) =>
    send(200, engine),
  );
  fixture.handlers.set("GET /api/queues", ({ send }) => send(200, queues));
  fixture.handlers.set("POST /api/queue/engine", ({ body, send }) => {
    if (body.diskGb === 8)
      return send(400, {
        error: {
          message: "request validation failed",
          fields: { storageRoot: "not enough free space" },
        },
      });
    engine = { phase: "installing", log: "" };
    return send(202, engine);
  });
  fixture.handlers.set("GET /api/queues/one/metrics", ({ send }) =>
    send(200, { ready: 1, unacknowledged: 0, consumers: 1, failed: 0 }),
  );
  fixture.handlers.set("GET /api/queues/two/metrics", ({ send }) =>
    send(200, { ready: 0, unacknowledged: 0, consumers: 0, failed: 0 }),
  );
  fixture.handlers.set("POST /api/queues/one/connection/publish", ({ send }) =>
    send(200, { url: "amqp://secret-for-one" }),
  );
  try {
    await page.goto(`${fixture.url}/#/queues`);
    const network = page.getByLabel("MicroNetwork");
    await expect(network).toBeVisible();
    await network.selectOption("network-1");
    await page.getByLabel("CPU", { exact: true }).fill("2");
    await page.getByLabel("Memory (MiB)").fill("2048");
    const poll = page.waitForResponse((response) =>
      response.url().endsWith("/api/queue/engine"),
    );
    await poll;
    await expect(network).toHaveValue("network-1");
    await expect(page.getByLabel("CPU", { exact: true })).toHaveValue("2");
    await page.getByRole("button", { name: "Install RabbitMQ" }).click();
    await expect(page.getByRole("alert")).toContainText(
      "storageRoot: not enough free space",
    );
    await expect(network).toHaveValue("network-1");
    await expect(page.getByLabel("Memory (MiB)")).toHaveValue("2048");
    await page.getByLabel("Disk (GiB)").fill("4");
    await page.getByRole("button", { name: "Install RabbitMQ" }).click();
    await expect(
      page.getByRole("region", { name: "Queue engine" }),
    ).toContainText("Installing");
    assert.deepEqual(
      fixture.requests
        .filter(
          (request) =>
            request.path === "/api/queue/engine" && request.method === "POST",
        )
        .at(-1).body,
      { microNetworkId: "network-1", cpu: 2, ram: 2048, diskGb: 4 },
    );
    engine = { phase: "running", log: "" };
    queues = ["one", "two"].map((id) => ({
      id,
      name: id,
      phase: "ready",
      deliveryLimit: 5,
      maxBytes: 67108864,
      services: [],
    }));
    await expect(
      page.getByRole("button", { name: "one", exact: true }),
    ).toBeVisible({ timeout: 10000 });
    await page.getByRole("button", { name: "one", exact: true }).click();
    await page.getByRole("button", { name: "Show sender connection" }).click();
    await expect(page.locator("pre")).toContainText("amqp://secret-for-one");
    await page.evaluate(() => window.dispatchEvent(new Event("blur")));
    await expect(page.locator("body")).not.toContainText(
      "amqp://secret-for-one",
    );
    await page.getByRole("button", { name: "Show sender connection" }).click();
    await expect(page.locator("pre")).toContainText("amqp://secret-for-one");
    await page.getByRole("button", { name: "two", exact: true }).click();
    await expect(page.locator("body")).not.toContainText(
      "amqp://secret-for-one",
    );
  } finally {
    await page.close();
    await browser.close();
    await fixture.close();
  }
});
