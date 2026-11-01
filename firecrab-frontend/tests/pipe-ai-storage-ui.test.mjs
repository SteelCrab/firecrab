import { test } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";
import { expect } from "@playwright/test";
import { createFixtureServer } from "./fixture-server.mjs";

test("Pipe AI saves API credentials and Pipe Storage creates an S3 connection", async () => {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  fixture.handlers.set(
    "PUT /api/pipe/ai/connections/test-id",
    ({ body, send }) =>
      send(200, {
        id: "test-id",
        name: body.name,
        provider: body.provider,
        authentication: "api_key",
        models: [],
        endpoint: "",
        streaming: true,
        toolCalls: true,
      }),
  );
  fixture.handlers.set("POST /api/pipe/storage", ({ body, send }) =>
    send(201, { id: "s3-1", ...body, services: [] }),
  );
  try {
    await page.addInitScript(() => {
      Object.defineProperty(crypto, "randomUUID", { value: () => "test-id" });
    });
    await page.goto(`${fixture.url}/#/pipe/ai`);
    await page.getByRole("button", { name: "Use API key" }).click();
    await page.getByLabel("Provider").selectOption("anthropic");
    await page.getByLabel("Anthropic API key").fill("fixture-key");
    await page.getByRole("button", { name: "Verify and save" }).click();
    await expect(
      page.getByRole("button", { name: "Replace API key" }),
    ).toBeVisible();
    await expect(
      page.getByText("Account verified and saved.", { exact: true }),
    ).toHaveCount(0);
    assert.deepEqual(
      fixture.requests.find(
        (r) =>
          r.method === "PUT" && r.path === "/api/pipe/ai/connections/test-id",
      ).body,
      { name: "Anthropic", provider: "anthropic", apiKey: "fixture-key" },
    );
    await page.goto(`${fixture.url}/#/pipe/storage`);
    await page
      .getByRole("button", { name: "Connect bucket", exact: true })
      .first()
      .click();
    await page.getByLabel("Bucket", { exact: true }).fill("fixture-bucket");
    await page
      .getByRole("button", { name: "Connect bucket", exact: true })
      .last()
      .click();
    await expect(page.getByText("fixture-bucket").first()).toBeVisible();
    await expect(
      page.getByText("Connection verified and saved.", { exact: true }),
    ).toHaveCount(0);
    const request = fixture.requests.find(
      (r) => r.method === "POST" && r.path === "/api/pipe/storage",
    );
    assert.equal(request.body.bucket, "fixture-bucket");
    assert.equal(request.body.credentialRef, "pipeAuthAws");
  } finally {
    await page.close();
    await browser.close();
    await fixture.close();
  }
});
