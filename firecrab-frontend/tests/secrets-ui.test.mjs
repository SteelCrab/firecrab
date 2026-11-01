import { test } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";
import { expect } from "@playwright/test";
import { createFixtureServer } from "./fixture-server.mjs";

test("Secrets create and reveal values explicitly, then delete on an empty response", async () => {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  fixture.handlers.set("POST /api/secrets", ({ body, send }) => {
    fixture.state.secrets.push({
      id: "secret-1",
      name: body.name,
      description: body.description,
      version: 1,
      createdAt: 0,
      updatedAt: 0,
      source: null,
    });
    send(201, fixture.state.secrets[0]);
  });
  fixture.handlers.set("POST /api/secrets/secret-1/reveal", ({ send }) =>
    send(200, { value: "fixture-secret", version: 1 }),
  );
  fixture.handlers.set("DELETE /api/secrets/secret-1", ({ send }) => {
    fixture.state.secrets = [];
    send(204, null);
  });
  try {
    await page.goto(`${fixture.url}/#/secrets`);
    await page.getByRole("button", { name: "New secret", exact: true }).click();
    await page.getByLabel("Secret name").fill("DB_PASSWORD");
    await page.getByLabel("Secret value").fill("fixture-secret");
    await page.getByRole("button", { name: "Save secret" }).click();
    await expect(
      page.getByRole("button", { name: "Reveal DB_PASSWORD" }),
    ).toBeVisible();
    assert.equal(
      fixture.requests.find(
        (request) =>
          request.method === "POST" && request.path === "/api/secrets",
      ).body.value,
      "fixture-secret",
    );
    await expect(page.locator("body")).not.toContainText("fixture-secret");
    await page.getByRole("button", { name: "Reveal DB_PASSWORD" }).click();
    await expect(page.locator("body")).toContainText("fixture-secret");
    await page.getByRole("button", { name: "Hide DB_PASSWORD" }).click();
    await expect(page.locator("body")).not.toContainText("fixture-secret");
    page.once("dialog", (dialog) => dialog.accept());
    await page.getByRole("button", { name: "Delete DB_PASSWORD" }).click();
    await expect(
      page.getByRole("button", { name: "Reveal DB_PASSWORD" }),
    ).toHaveCount(0);
    await expect(page.getByRole("alert")).toHaveCount(0);
  } finally {
    await page.close();
    await browser.close();
    await fixture.close();
  }
});
