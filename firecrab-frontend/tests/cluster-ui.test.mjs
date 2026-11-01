import { test } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";
import { expect } from "@playwright/test";
import { createFixtureServer } from "./fixture-server.mjs";

test("cluster placement retries the same idempotent request after reload", async () => {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  const host = {
    online: true,
    draining: false,
    reserved: { cpu: 0, ramMib: 0 },
    report: {
      id: "host-1",
      name: "worker",
      capacity: { cpu: 4, ramMib: 4096 },
      networks: [{ id: "network-1", name: "default" }],
      storages: [{ id: "disk-1" }],
      images: [{ alias: "nginx", version: "1", rootfsSha256: "sha256:abc" }],
    },
  };
  fixture.handlers.set("GET /api/cluster/hosts", ({ send }) =>
    send(200, [host]),
  );
  fixture.handlers.set("GET /api/cluster/placements", ({ send }) =>
    send(200, []),
  );
  fixture.handlers.set("GET /api/cluster/services", ({ send }) =>
    send(200, []),
  );
  let tries = 0;
  fixture.handlers.set("POST /api/cluster/placements", ({ send }) => {
    if (++tries === 1) send(503, { error: { message: "unknown outcome" } });
    else send(200, { id: "placement-1" });
  });
  try {
    await page.goto(`${fixture.url}/#/cluster`);
    await expect(page.getByRole("heading", { name: "worker" })).toBeVisible();
    await page.getByRole("button", { name: "VM placements" }).click();
    await page.getByLabel("VM name").fill("web");
    await page.getByRole("button", { name: "Place VM", exact: true }).click();
    await expect(
      page.getByRole("button", { name: "Retry", exact: true }),
    ).toBeVisible();
    await page.reload();
    await page.getByRole("button", { name: "VM placements" }).click();
    await page.getByRole("button", { name: "Retry", exact: true }).click();
    await expect(
      page.getByRole("button", { name: "Retry", exact: true }),
    ).toHaveCount(0);
    const requests = fixture.requests.filter(
      (r) => r.method === "POST" && r.path === "/api/cluster/placements",
    );
    assert.equal(requests.length, 2);
    assert.equal(
      requests[0].headers["idempotency-key"],
      requests[1].headers["idempotency-key"],
    );
    assert.deepEqual(requests[0].body, requests[1].body);
    assert.equal(requests[0].body.name, "web");
    assert.deepEqual(requests[0].body.targets, [
      {
        network: { hostId: "host-1", resourceId: "network-1" },
        storage: { hostId: "host-1", resourceId: "disk-1" },
      },
    ]);
  } finally {
    await page.close();
    await browser.close();
    await fixture.close();
  }
});
