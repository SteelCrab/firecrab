import { test } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";
import { expect } from "@playwright/test";
import { createFixtureServer } from "./fixture-server.mjs";

test("pool creation sends the selected image, network and resource settings", async () => {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  fixture.handlers.set("POST /api/pools", ({ body, send }) => {
    const pool = {
      ...body,
      id: "pool-1",
      templateVersion: "v1",
      storageRoot: "default",
      members: [],
      deleting: false,
      createdAtMs: 0,
    };
    fixture.state.pools.push(pool);
    send(201, pool);
  });
  fixture.handlers.set("GET /api/pools/pool-1/leases", ({ send }) =>
    send(200, []),
  );
  try {
    await page.goto(`${fixture.url}/#/pools`);
    await expect(
      page.getByRole("button", { name: "Create pool", exact: true }),
    ).toBeEnabled();
    await page.getByLabel("Name", { exact: true }).fill("workers");
    await page
      .getByRole("button", { name: "Create pool", exact: true })
      .click();
    await expect(
      page.getByText("workers", { exact: true }).first(),
    ).toBeVisible();
    assert.deepEqual(
      fixture.requests.find(
        (request) => request.method === "POST" && request.path === "/api/pools",
      ).body,
      {
        name: "workers",
        template: "ubuntu-24.04",
        cpu: 1,
        ram: 512,
        diskGb: 2,
        egressPolicy: "internet",
        microNetworkId: "network-1",
        storageRoot: null,
        minReady: 2,
        maxSize: 4,
        leaseTtlSeconds: 600,
      },
    );
  } finally {
    await page.close();
    await browser.close();
    await fixture.close();
  }
});
