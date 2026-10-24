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
    await page.getByRole("button", { name: "Use VM", exact: true }).click();
    await expect(page.getByRole("alert")).toContainText("Unable to connect");
    await page.waitForTimeout(3200);
    await expect(page.getByRole("alert")).toContainText("Unable to connect");
    await page.reload();
    await page.getByRole("button", { name: "Use VM", exact: true }).click();
    await expect.poll(() => keys.length).toBe(2);
    expect(keys[0]).toMatch(/^[a-f0-9]{32}$/);
    expect(keys[1]).toBe(keys[0]);
    await expect(page.getByRole("button", { name: "Use VM", exact: true })).toBeEnabled();
    await page.getByRole("button", { name: "Use VM", exact: true }).click();
    await expect.poll(() => keys.length).toBe(3);
    expect(keys[2]).not.toBe(keys[0]);
  });

  for (const state of ["running", "stopped", "paused"] as const) {
    test(`keeps a ${state} pool member read-only in the new VM screen`, async ({ page }) => {
      const record = { ...member, state };
      await openDashboard(page, { vms: [record], routes: { [`/api/vms/${VM_ID}`]: record } });
      const row = page.getByRole("row").filter({ hasText: record.name });
      await expect(row.getByRole("button", { name: /^(Start|Stop|Pause|Resume|Delete)$/ })).toHaveCount(0);
      await row.getByRole("button", { name: record.name, exact: true }).click();
      await expect(page.getByRole("link", { name: /^Add schedule/ })).toHaveCount(0);
      const detail = page.locator(".detail-head");
      await expect(detail).toContainText("Pool");
      await expect(detail.getByRole("button", { name: /^(Start|Stop|Pause|Resume|Delete)$/ })).toHaveCount(0);
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
    await page.getByRole("button", { name: "ci-runners", exact: true }).click();
    await expect(page.getByRole("button", { name: "End session", exact: true })).toBeVisible();
    await expect(page.locator(".screen-header")).toContainText("Pools");
    if (process.env.FIRECRAB_POOL_SCREENSHOT) {
      await page.screenshot({ path: process.env.FIRECRAB_POOL_SCREENSHOT, fullPage: true });
    }
  });
  test("creates a pool with only a name and image, using safe defaults", async ({ page }) => {
    const pools: PoolResponse[] = [];
    let request: Record<string, unknown> | undefined;
    await openDashboard(page, { hash: "#/pools", routes: {
      ...routes,
      "/api/images": [{ alias: "ubuntu", installed: true, minDiskGb: 8 }, { alias: "__microboot", installed: true }],
      "/api/pools": async route => {
        if (route.request().method() === "GET") return route.fulfill({ json: pools });
        request = route.request().postDataJSON();
        const created = { ...pool, ...request, name: "build-runners", members: [] } as PoolResponse;
        pools.push(created);
        return route.fulfill({ status: 201, json: created });
      },
    } });
    const form = page.getByRole("region", { name: "Create a pool", exact: true });
    await expect(form.getByLabel("Image", { exact: true })).toHaveValue("ubuntu");
    await expect(form.getByLabel("Image", { exact: true }).locator("option")).toHaveCount(1);
    await expect(form.locator("input:visible, select:visible")).toHaveCount(2);
    await form.getByLabel("Name", { exact: true }).fill("build-runners");
    await form.getByRole("button", { name: "Create pool", exact: true }).click();
    await expect(page.getByRole("button", { name: "build-runners", exact: true })).toBeVisible();
    expect(request).toMatchObject({ template: "ubuntu", diskGb: 8, cpu: 1, ram: 512, minReady: 2, maxSize: 4, leaseTtlSeconds: 600, microNetworkId: "default" });
    await expect(form).toHaveCount(0);
  });

  test("ends a session without exposing VM lifecycle controls", async ({ page }) => {
    let released = false;
    await openDashboard(page, { hash: "#/pools", routes: {
      ...routes,
      [`/api/pools/${POOL_ID}/leases`]: route => route.fulfill({ json: [{ ...lease, state: released ? "released" : "active" }] }),
      [`/api/pools/${POOL_ID}/leases/${lease.id}`]: route => { released = true; return route.fulfill({ json: { ...lease, state: "released" } }); },
    } });
    await page.getByRole("button", { name: "ci-runners", exact: true }).click();
    await expect(page.getByRole("link", { name: "Open terminal", exact: true })).toHaveAttribute("href", new RegExp(VM_ID));
    await expect(page.getByRole("button", { name: "Delete pool", exact: true })).toBeHidden();
    await page.getByRole("button", { name: "End session", exact: true }).click();
    await expect(page.getByText("Nothing in use. Choose Use VM to get started.", { exact: true })).toBeVisible();
    expect(released).toBe(true);
  });

  test("keeps settings optional and sends session duration in seconds", async ({ page }) => {
    let updated: unknown;
    await openDashboard(page, { hash: "#/pools", routes: {
      ...routes,
      [`/api/pools/${POOL_ID}`]: route => { updated = route.request().postDataJSON(); return route.fulfill({ json: pool }); },
    } });
    await page.getByRole("button", { name: "ci-runners", exact: true }).click();
    await expect(page.getByLabel("Keep ready", { exact: true })).toBeHidden();
    await page.getByText("Pool settings", { exact: true }).click();
    await page.getByLabel("Session duration (minutes)", { exact: true }).fill("30");
    await page.getByRole("button", { name: "Save settings", exact: true }).click();
    await expect.poll(() => updated).toMatchObject({ leaseTtlSeconds: 1800 });
  });

  test("shows the simple Korean flow on desktop and mobile", async ({ page }) => {
    await page.setViewportSize({ width: 1280, height: 860 });
    await openDashboard(page, { hash: "#/pools", locale: "ko", routes });
    await page.getByRole("button", { name: "ci-runners", exact: true }).click();
    await expect(page.getByRole("button", { name: "사용 종료", exact: true })).toBeVisible();
    if (process.env.FIRECRAB_POOL_SCREENSHOT) await page.screenshot({ path: process.env.FIRECRAB_POOL_SCREENSHOT, fullPage: true });
    await page.getByRole("button", { name: "새 풀", exact: true }).click();
    await expect(page.locator(".pool-create input:visible, .pool-create select:visible")).toHaveCount(2);
    if (process.env.FIRECRAB_POOL_SCREENSHOT) await page.screenshot({ path: process.env.FIRECRAB_POOL_SCREENSHOT.replace(/\.png$/, "-create.png"), fullPage: true });
    await page.setViewportSize({ width: 390, height: 844 });
    const content = page.locator(".pools-screen");
    expect(await content.evaluate(element => element.scrollWidth <= element.clientWidth)).toBe(true);
    await expect(page.getByRole("button", { name: "VM 사용", exact: true })).toBeVisible();
  });

});
