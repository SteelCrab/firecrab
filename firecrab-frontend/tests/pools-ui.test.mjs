import { test } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";
import { expect } from "@playwright/test";
import { createFixtureServer } from "./fixture-server.mjs";

for (const existing of [false, true]) {
  test(`pool creation waits for the initial ${existing ? "populated" : "empty"} list`, async () => {
    const fixture = await createFixtureServer(
      fileURLToPath(new URL("../dist", import.meta.url)),
      0,
    );
    const browser = await chromium.launch({ headless: true });
    const page = await browser.newPage();
    let release;
    const pending = new Promise((resolve) => {
      release = resolve;
    });
    fixture.handlers.set("GET /api/pools", async ({ send }) => {
      await pending;
      send(
        200,
        existing
          ? [
              {
                id: "pool-1",
                name: "workers",
                template: "ubuntu-24.04",
                templateVersion: "v1",
                cpu: 1,
                ram: 512,
                diskGb: 2,
                egressPolicy: "internet",
                microNetworkId: "network-1",
                storageRoot: "default",
                minReady: 0,
                maxSize: 1,
                leaseTtlSeconds: 600,
                members: [],
                deleting: false,
                createdAtMs: 0,
              },
            ]
          : [],
      );
    });
    try {
      await page.goto(`${fixture.url}/#/pools`);
      await expect(
        page.getByText("Loading pools…", { exact: true }),
      ).toBeVisible();
      const button = page.getByRole("button", {
        name: "New pool",
        exact: true,
      });
      const form = page.getByRole("region", {
        name: "Create a pool",
        exact: true,
      });
      await expect(button).toBeHidden();
      await expect(form).toBeHidden();
      release();
      if (existing) await button.click();
      await expect(form).toBeVisible();
      await expect(
        page.getByRole("button", { name: "Create pool", exact: true }),
      ).toBeEnabled();
    } finally {
      release();
      await page.close();
      await browser.close();
      await fixture.close();
    }
  });
}

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
