import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";
import { expect } from "@playwright/test";
import { createFixtureServer } from "./fixture-server.mjs";

test("Stack setup automatically updates and distinguishes missing and unavailable source", async () => {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  let stacks = [];
  fixture.handlers.set("GET /api/stacks", ({ send }) => send(200, stacks));
  fixture.handlers.set("GET /api/stacks/one/source", ({ send }) =>
    send(404, { error: { message: "not found" } }),
  );
  fixture.handlers.set("GET /api/stacks/two/source", ({ send }) =>
    send(500, { error: { message: "unavailable" } }),
  );
  try {
    await page.goto(`${fixture.url}/#/stacks`);
    await expect(
      page.getByRole("heading", { name: "Set up a Stack" }),
    ).toBeVisible();
    await expect(
      page.getByRole("button", { name: "Refresh", exact: true }),
    ).toHaveCount(0);
    stacks = [{ name: "one" }, { name: "two" }];
    await expect(
      page.getByRole("heading", { name: "one", exact: true }),
    ).toBeVisible({ timeout: 10000 });
    await expect(page.getByText("Git source is not configured.")).toBeVisible();
    await expect(page.getByRole("alert")).toContainText("Source unavailable");
  } finally {
    await page.close();
    await browser.close();
    await fixture.close();
  }
});
