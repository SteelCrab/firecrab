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
  fixture.handlers.set("GET /api/iac/native-operations", ({ send }) =>
    send(200, { operations: [], nextBefore: null }),
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

test("Stack resource sections preserve state boundaries and follow an ownership handover", async () => {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  fixture.handlers.set("GET /api/iac/native-operations", ({ send }) =>
    send(200, { operations: [], nextBefore: null }),
  );
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  const owned = (manager, id, kind, resource, key) => ({
    resource: { kind, id: resource },
    owner: { manager, id, key },
    generation: 1,
  });
  let owners = [
    owned("stack", "native", "network", "shared", "native-network"),
    owned("terraform", "state-a", "network", "net-a", "network.primary"),
    owned("terraform", "state-a", "volume", "vol-a", "volume.primary"),
    owned("terraform", "state-b", "network", "net-b", "network.other"),
  ];
  fixture.handlers.set("GET /api/stacks", ({ send }) =>
    send(200, [
      {
        id: "native",
        name: "Application",
        bindings: [
          { kind: "network", resourceId: "shared", key: "old-native-snapshot" },
        ],
      },
    ]),
  );
  fixture.handlers.set("GET /api/iac/owners", ({ send }) => send(200, owners));
  const card = (id) => page.locator(`.stack-card[data-owner-id="${id}"]`);
  try {
    await page.goto(`${fixture.url}/#/stacks`);
    await expect(
      card("state-a").getByText("network.primary", { exact: true }),
    ).toBeVisible();
    await expect(
      card("state-a").getByText("volume.primary", { exact: true }),
    ).toBeVisible();
    await expect(
      card("state-a").getByText("network.other", { exact: true }),
    ).toHaveCount(0);
    await expect(
      card("state-b").getByText("network.other", { exact: true }),
    ).toBeVisible();
    await expect(
      card("native").locator('.stack-resource[data-resource-id="shared"]'),
    ).toHaveCount(1);
    await expect(
      card("state-a").locator(
        '.stack-resource-group[data-kind="network"] .stack-resource-icon svg',
      ),
    ).toHaveCount(1);
    await expect(
      card("state-a").locator(
        '.stack-resource-group[data-kind="volume"] .stack-resource-count',
      ),
    ).toHaveText("1");
    owners = [
      ...owners.slice(1),
      owned("terraform", "state-b", "network", "shared", "network.transferred"),
    ];
    await expect(
      card("native").locator('.stack-resource[data-resource-id="shared"]'),
    ).toHaveCount(0, { timeout: 10000 });
    await expect(
      card("state-b").getByText("network.transferred", { exact: true }),
    ).toBeVisible();
    await page.setViewportSize({ width: 375, height: 812 });
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    ).toBe(true);
  } finally {
    await page.close();
    await browser.close();
    await fixture.close();
  }
});
