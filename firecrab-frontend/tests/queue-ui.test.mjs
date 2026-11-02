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

for (const recoveryState of ["needs_recovery", "running", "unavailable"]) {
  test(`Queue engine uses the original operation when recovery is ${recoveryState}`, async () => {
    const fixture = await createFixtureServer(
      fileURLToPath(new URL("../dist", import.meta.url)),
      0,
    );
    const browser = await chromium.launch({ headless: true });
    const page = await browser.newPage();
    let phase = "failed";
    fixture.handlers.set("GET /api/queue/engine", ({ send }) =>
      send(200, {
        vmId: "original-broker",
        phase,
        error:
          phase === "failed" ? "The existing engine requires recovery" : null,
      }),
    );
    fixture.handlers.set("GET /api/queues", ({ send }) => send(200, []));
    fixture.handlers.set(
      "GET /api/iac/native-operations/resources/queue_engine/original-broker",
      ({ send }) => {
        if (recoveryState === "unavailable")
          return send(503, {
            error: { message: "Recovery status unavailable" },
          });
        send(200, [
          { operationId: "completed-old-action", state: "succeeded" },
          {
            operationId: "original-operation",
            state: recoveryState,
            executionGeneration: 3,
            ownership: { endpointId: "queue-endpoint" },
          },
        ]);
      },
    );
    fixture.handlers.set(
      "POST /api/iac/native-operations/original-operation/recover",
      ({ req, send }) => {
        if (
          req.headers["x-firecrab-endpoint-id"] !== "queue-endpoint" ||
          req.headers["x-firecrab-execution-generation"] !== "3"
        )
          throw new Error("missing original recovery preconditions");
        phase = "starting";
        send(200, { operationId: "original-operation", state: "running" });
      },
    );
    try {
      await page.goto(`${fixture.url}/#/queues`);
      if (recoveryState === "unavailable") {
        await expect(page.getByRole("alert")).toContainText(
          "Recovery status unavailable",
        );
        await expect(
          page.getByRole("button", { name: "Start / recover", exact: true }),
        ).toHaveCount(0);
      } else {
        const button = page.getByRole("button", {
          name: "Recover engine",
          exact: true,
        });
        if (recoveryState === "running") {
          await expect(button).toBeDisabled();
        } else {
          await expect(button).toBeEnabled();
          await button.click();
          await expect(
            page.getByRole("region", { name: "Queue engine" }),
          ).toContainText("Starting");
          assert.equal(
            fixture.requests.filter(
              ({ method, path }) =>
                method === "POST" &&
                path ===
                  "/api/iac/native-operations/original-operation/recover",
            ).length,
            1,
          );
        }
      }
      assert.equal(
        fixture.requests.filter(
          ({ method, path }) =>
            method === "POST" && path === "/api/queue/engine/start",
        ).length,
        0,
      );
    } finally {
      await page.close();
      await browser.close();
      await fixture.close();
    }
  });
}
