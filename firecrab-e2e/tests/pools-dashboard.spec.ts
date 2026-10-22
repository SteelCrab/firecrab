import { expect, test } from "@playwright/test";
import { openDashboard, vm } from "./support/mockApi";
import type { PoolLeaseResponse, PoolResponse } from "../../firecrab-frontend/src/bindings";

const POOL_ID = "11111111-1111-4111-8111-111111111111";
const VM_ID = "22222222-2222-4222-8222-222222222222";
const member = vm("pool-ci-runners-7f39b4a1", VM_ID, "running", { purpose: "pool" });
const pool: PoolResponse = {
  id: POOL_ID, name: "ci-runners", template: "alpine-3.21", templateVersion: "alpine-3.21-v1",
  cpu: 2, ram: 512, diskGb: 2, egressPolicy: "internet", microNetworkId: "default",
  storageRoot: "default", minReady: 2, maxSize: 4, leaseTtlSeconds: 600, deleting: false,
  members: [
    { vmId: VM_ID, state: "leased", createdAtMs: 1 },
    { vmId: "33333333-3333-4333-8333-333333333333", state: "ready", createdAtMs: 2 },
    { vmId: "44444444-4444-4444-8444-444444444444", state: "ready", createdAtMs: 3 },
  ], createdAtMs: 1,
};
const lease: PoolLeaseResponse = {
  id: "55555555-5555-4555-8555-555555555555", poolId: POOL_ID, vmId: VM_ID,
  state: "active", acquiredAtMs: Date.now(), expiresAtMs: Date.now() + 600_000, vm: member,
};
const routes = {
  "/api/pools": [pool],
  [`/api/pools/${POOL_ID}/leases`]: [lease],
  "/api/images": [{ alias: "alpine-3.21", installed: true }],
  "/api/micro-networks": [{ id: "default", name: "ci-network", subnetCidr: "10.42.0.0/24" }],
};

test.describe("pool dashboard @dashboard", () => {
  test.use({ timezoneId: "Asia/Seoul" });
  test("keeps the acquire key after a lost response, polling, and reload", async ({ page }) => {
    const keys: string[] = [];
    await page.addInitScript(() => Object.defineProperty(crypto, "randomUUID", { value: undefined }));
    await openDashboard(page, { hash: "#/pools", routes: {
      ...routes,
      [`/api/pools/${POOL_ID}/acquire`]: async route => {
        keys.push(route.request().headers()["idempotency-key"]);
        if (keys.length === 1) return route.abort("failed");
        return route.fulfill({ json: lease });
      },
    } });
    await page.getByRole("button", { name: "Acquire", exact: true }).click();
    await expect(page.getByRole("alert")).toContainText("Unable to connect");
    await page.waitForTimeout(3200);
    await expect(page.getByRole("alert")).toContainText("Unable to connect");
    await page.reload();
    await page.getByRole("button", { name: "Acquire", exact: true }).click();
    await expect.poll(() => keys.length).toBe(2);
    expect(keys[0]).toMatch(/^[a-f0-9]{32}$/);
    expect(keys[1]).toBe(keys[0]);
    await expect(page.getByRole("button", { name: "Acquire", exact: true })).toBeEnabled();
    await page.getByRole("button", { name: "Acquire", exact: true }).click();
    await expect.poll(() => keys.length).toBe(3);
    expect(keys[2]).not.toBe(keys[0]);
  });

  for (const state of ["running", "stopped"] as const) {
    test(`keeps a ${state} pool member read-only in the new VM screen`, async ({ page }) => {
      const record = { ...member, state };
      await openDashboard(page, { vms: [record], routes: { [`/api/vms/${VM_ID}`]: record } });
      const row = page.getByRole("row").filter({ hasText: record.name });
      await expect(row.getByRole("button", { name: /^(Start|Stop|Delete)$/ })).toHaveCount(0);
      await row.getByRole("button", { name: record.name, exact: true }).click();
      const detail = page.locator(".detail-head");
      await expect(detail).toContainText("Pool");
      await expect(detail.getByRole("button", { name: /^(Start|Stop|Delete)$/ })).toHaveCount(0);
      await expect(page.getByRole("button", { name: /^Edit / })).toHaveCount(0);
      await page.getByRole("tab", { name: "Environment", exact: true }).click();
      await expect(page.getByRole("button", { name: /^Edit / })).toHaveCount(0);
      await page.getByRole("tab", { name: "Network", exact: true }).click();
      await expect(page.getByRole("button", { name: /^Edit / })).toHaveCount(0);
    });
  }

  test("shows the pool, ready members, and active lease", async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 1100 });
    await openDashboard(page, { hash: "#/pools", routes });
    await page.getByRole("cell", { name: "ci-runners", exact: true }).click();
    await expect(page.getByRole("button", { name: "Release", exact: true })).toBeVisible();
    await expect(page.locator(".screen-header")).toContainText("Pools");
    if (process.env.FIRECRAB_POOL_SCREENSHOT) {
      await page.screenshot({ path: process.env.FIRECRAB_POOL_SCREENSHOT, fullPage: true });
    }
  });
});
