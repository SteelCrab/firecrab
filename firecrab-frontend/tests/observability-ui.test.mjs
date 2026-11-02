import { test } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";
import { expect } from "@playwright/test";
import { createFixtureServer } from "./fixture-server.mjs";

const base = "/api/observability/signoz";
const ids = [
  "00000000-0000-4000-8000-000000000101",
  "00000000-0000-4000-8000-000000000102",
];
const accepted = () => ({
  timestampMs: Date.now(),
  records: 1,
  delivered: 1,
  rejected: 0,
  status: "delivered",
  httpStatus: 200,
});

async function fixture() {
  const server = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  let config = null;
  let history = [];
  const snapshot = () => ({
    observedAtMs: Date.now(),
    provider: "signoz",
    config,
    sources: ids.map((id, index) => ({
      id,
      name: ["api-gateway", "background-worker"][index],
      selected: config?.serviceIds.includes(id) ?? false,
      available: true,
      status: config?.serviceIds.includes(id)
        ? {
            state: config.paused
              ? "paused"
              : history.length
                ? "delivered"
                : "pending",
            paused: config.paused,
            queued: 0,
            inFlight: 0,
            delivered: history.length,
            dropped: 0,
            rejected: 0,
            failedBatches: 0,
            lastDeliveredAtMs: history[0]?.timestampMs ?? null,
            history,
          }
        : null,
    })),
    history: history.map((batch) => ({
      ...batch,
      serviceId: ids[0],
      serviceName: "api-gateway",
    })),
    historyLimitPerService: 128,
    displayHistoryLimit: 200,
    queueLimitPerService: 128,
    queuePersistent: false,
  });
  server.handlers.set(`GET ${base}`, ({ send }) => send(200, snapshot()));
  server.handlers.set(`PUT ${base}`, ({ body, send }) => {
    config = body;
    send(200, snapshot());
  });
  server.handlers.set(`DELETE ${base}`, ({ send }) => {
    config = null;
    history = [];
    send(200, snapshot());
  });
  server.handlers.set(`POST ${base}/test`, ({ send }) =>
    send(200, { ...accepted(), records: 0, delivered: 0 }),
  );
  server.handlers.set(`POST ${base}/test-log`, ({ send }) => {
    const result = accepted();
    history.unshift(result);
    send(200, result);
  });
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  return {
    server,
    page,
    errors,
    close: async () => {
      await browser.close();
      await server.close();
    },
  };
}

test("SigNoz connection validates input, tests explicitly and preserves canceled edits", async () => {
  const { server, page, errors, close } = await fixture();
  try {
    await page.goto(`${server.url}/#/observability`);
    await page
      .getByRole("button", { name: "Connect SigNoz", exact: true })
      .click();
    await page
      .getByLabel("OTLP collector endpoint")
      .fill("http://user:secret@collector.test:4318");
    await page.getByRole("button", { name: "Save connection" }).click();
    await expect(page.getByRole("alert")).toContainText("without credentials");
    assert.equal(server.requests.filter((r) => r.method === "PUT").length, 0);
    await page
      .getByLabel("OTLP collector endpoint")
      .fill("http://collector.test:4318");
    await page.getByRole("button", { name: "Save connection" }).click();
    await expect(page.getByRole("alert")).toContainText(
      "Select at least one service",
    );
    await page.getByRole("checkbox", { name: /api-gateway/ }).check();
    await page
      .getByRole("button", { name: "Test connection", exact: true })
      .click();
    await expect(page.getByRole("status")).toContainText(
      "Collector accepted the empty OTLP test batch",
    );
    assert.deepEqual(
      server.requests.find((r) => r.path === `${base}/test`).body,
      { endpoint: "http://collector.test:4318" },
    );
    await page.getByRole("button", { name: "Save connection" }).click();
    await expect(page.getByRole("dialog")).toHaveCount(0);
    await expect(page.locator(".obs-provider-details code")).toHaveText(
      "http://collector.test:4318",
    );
    assert.deepEqual(
      server.requests.find((r) => r.method === "PUT").body.serviceIds,
      [ids[0]],
    );
    await page
      .getByRole("button", { name: "Connection settings", exact: true })
      .click();
    await page
      .getByLabel("OTLP collector endpoint")
      .fill("http://unsaved.test");
    await page.keyboard.press("Escape");
    await expect(page.locator(".obs-provider-details code")).toHaveText(
      "http://collector.test:4318",
    );
    assert.equal(server.requests.filter((r) => r.method === "PUT").length, 1);
    assert.deepEqual(errors, []);
  } finally {
    await close();
  }
});

test("SigNoz delivery history, pause/resume and confirmed disconnect use the API", async () => {
  const { server, page, errors, close } = await fixture();
  try {
    await page.goto(`${server.url}/#/observability`);
    await page
      .getByRole("button", { name: "Connect SigNoz", exact: true })
      .click();
    await page
      .getByLabel("OTLP collector endpoint")
      .fill("http://collector.test:4318");
    await page.getByRole("checkbox", { name: /api-gateway/ }).check();
    await page.getByRole("button", { name: "Save connection" }).click();
    await page
      .getByRole("button", { name: "Send test log", exact: true })
      .click();
    await expect(
      page.locator(".observability-page > .obs-notice"),
    ).toContainText("1/1 records accepted");
    assert.deepEqual(
      server.requests.find((r) => r.path === `${base}/test-log`).body,
      { serviceId: ids[0] },
    );
    await page.getByRole("tab", { name: "Delivery history" }).click();
    await expect(page.locator("#obs-history tbody")).toContainText(
      "api-gateway",
    );
    await expect(page.locator("#obs-history tbody")).toContainText("200");
    await page
      .getByRole("tab", { name: "Delivery history" })
      .press("ArrowLeft");
    await page.getByRole("button", { name: "Pause", exact: true }).click();
    await expect(
      page.getByRole("button", { name: "Resume", exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("button", { name: "Send test log", exact: true }),
    ).toBeDisabled();
    await page.getByRole("button", { name: "Resume", exact: true }).click();
    await expect(
      page.getByRole("button", { name: "Send test log", exact: true }),
    ).toBeEnabled();
    await page
      .getByRole("button", { name: "Connection settings", exact: true })
      .click();
    await page.getByRole("button", { name: "Disconnect", exact: true }).click();
    await page.getByRole("button", { name: "Cancel", exact: true }).click();
    assert.equal(
      server.requests.filter((r) => r.method === "DELETE").length,
      0,
    );
    await page.getByRole("button", { name: "Disconnect", exact: true }).click();
    await page
      .getByRole("dialog", { name: "Disconnect SigNoz?" })
      .getByRole("button", { name: "Disconnect", exact: true })
      .click();
    await expect(
      page.getByRole("button", { name: "Connect SigNoz", exact: true }),
    ).toBeVisible();
    assert.equal(
      server.requests.filter((r) => r.method === "DELETE").length,
      1,
    );
    assert.deepEqual(errors, []);
  } finally {
    await close();
  }
});

test("SigNoz settings show per-service failures and discard stale connection test results", async () => {
  const { server, page, close } = await fixture();
  try {
    await page.goto(`${server.url}/#/observability`);
    await page
      .getByRole("button", { name: "Connect SigNoz", exact: true })
      .click();
    await page
      .getByLabel("OTLP collector endpoint")
      .fill("http://old.test:4318");
    await page.getByRole("checkbox", { name: /api-gateway/ }).check();
    server.handlers.set(`POST ${base}/test`, async ({ send }) => {
      await new Promise((resolve) => setTimeout(resolve, 250));
      send(200, accepted());
    });
    await page
      .getByRole("button", { name: "Test connection", exact: true })
      .click();
    await page
      .getByLabel("OTLP collector endpoint")
      .fill("http://new.test:4318");
    await page.waitForTimeout(350);
    await expect(page.locator(".obs-settings .obs-notice")).toHaveCount(0);
    server.handlers.set(`PUT ${base}`, ({ send }) =>
      send(400, {
        error: {
          code: "validation_failed",
          message: "request validation failed",
          fields: {
            [ids[0]]:
              "Service already has an independently configured exporter",
          },
        },
      }),
    );
    await page.getByRole("button", { name: "Save connection" }).click();
    await expect(page.locator(".obs-settings [role=alert]")).toContainText(
      "api-gateway: Service already has",
    );
    await expect(page.getByLabel("OTLP collector endpoint")).toHaveValue(
      "http://new.test:4318",
    );
    await page.getByRole("button", { name: "Close settings" }).click();
    await expect(
      page.getByRole("button", { name: "Connect SigNoz", exact: true }),
    ).toBeVisible();
  } finally {
    await close();
  }
});

test("SigNoz Korean mobile overview and settings fit the viewport", async () => {
  const { server, page, errors, close } = await fixture();
  try {
    await page.setViewportSize({ width: 390, height: 844 });
    await page.goto(`${server.url}/#/observability`);
    await page.evaluate(() => localStorage.setItem("firecrab.locale", "ko"));
    await page.reload();
    await page
      .getByRole("button", { name: "SigNoz 연결", exact: true })
      .click();
    await expect(
      page.getByRole("dialog", { name: "SigNoz 연결 설정" }),
    ).toBeVisible();
    await page
      .getByLabel("OTLP 수집기 주소")
      .fill("http://collector.test:4318");
    await page.getByRole("checkbox", { name: /api-gateway/ }).check();
    await page.getByRole("button", { name: "연결 저장", exact: true }).click();
    for (const width of [360, 390, 768, 1440]) {
      await page.setViewportSize({ width, height: 900 });
      assert.ok(
        await page.evaluate(
          () => document.documentElement.scrollWidth <= innerWidth,
        ),
        `overflow at ${width}`,
      );
    }
    assert.deepEqual(errors, []);
  } finally {
    await close();
  }
});
