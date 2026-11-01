import { test } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";
import { expect } from "@playwright/test";
import { createFixtureServer } from "./fixture-server.mjs";

test("Pipe Auth provides setup steps for each provider and connects Firecrab", async () => {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  fixture.state.auth = [];
  fixture.handlers.set("PUT /api/pipe/auth/firecrab", ({ send }) =>
    send(200, {
      provider: "firecrab",
      account: "test@example.com",
      accountId: "account-1",
      registry: null,
      region: null,
      services: ["secrets"],
    }),
  );
  try {
    await page.goto(`${fixture.url}/#/pipe/auth`);
    for (const provider of ["GitHub", "AWS", "Firecrab"]) {
      await page
        .getByRole("button", { name: `Connect ${provider}`, exact: true })
        .click();
      await expect(
        page.getByRole("heading", { name: `${provider} connection steps` }),
      ).toBeVisible();
      await expect(page.locator("form ol li")).toHaveCount(4);
      if (provider !== "Firecrab")
        await page.getByRole("button", { name: "Cancel", exact: true }).click();
    }
    await page
      .getByLabel("Account integration token")
      .fill(`fca_${"a".repeat(64)}`);
    await page.getByRole("button", { name: "Verify and connect" }).click();
    await expect(page.getByText("test@example.com").first()).toBeVisible();
    assert.deepEqual(
      fixture.requests.find(
        (r) => r.method === "PUT" && r.path === "/api/pipe/auth/firecrab",
      ).body,
      { provider: "firecrab", token: `fca_${"a".repeat(64)}` },
    );
  } finally {
    await page.close();
    await browser.close();
    await fixture.close();
  }
});
