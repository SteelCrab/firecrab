import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";
import type { MicroBlockStorageResponse, VmState } from "../types/bindings";
import { openDashboard, vm } from "./support/mockApi";
import type { MockRoute } from "./support/mockApi";

const VM = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const VOLUME = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const seed: MicroBlockStorageResponse = {
  id: VOLUME, name: "app-data", sizeGib: 10, storageRoot: "default",
  state: "available", attachedVmId: null, mountPath: null,
};

async function open(page: Page, options: { hash?: string; ko?: boolean; failCreate?: boolean; empty?: boolean; routes?: Record<string, MockRoute> } = {}) {
  const state = {
    volumes: options.empty ? [] as MicroBlockStorageResponse[] : [{ ...seed }],
    vmState: "stopped" as VmState,
    writes: [] as { method: string; body: Record<string, unknown> }[],
  };
  await openDashboard(page, {
    hash: options.hash ?? "#/volumes", locale: options.ko ? "ko" : "en",
    vms: () => [vm("database", VM, state.vmState)],
    routes: {
      "/api/host": { loadAverage1m: 0, memoryTotalMib: 8192, memoryAvailableMib: 4096, diskTotalGib: 100, diskAvailableGib: 80, uptimeSeconds: 100 },
      "/api/update/progress": { phase: "idle", percent: 0, updatedAtMs: 0 },
      "/api/storage": [{ id: "default", name: "Local storage", path: "/var/lib/firecrab", availableGib: 80, totalGib: 100 }],
      "/api/micro-networks": [], "/api/shells": [],
      [`/api/vms/${VM}`]: route => route.fulfill({ json: vm("database", VM, state.vmState) }),
      "/api/microblockstorage": route => {
        if (route.request().method() === "GET") return route.fulfill({ json: state.volumes });
        const body = route.request().postDataJSON();
        state.writes.push({ method: "create", body });
        if (options.failCreate) return route.fulfill({ status: 409, json: {
          error: { code: "volume_conflict", message: "volume name is already registered" },
        } });
        const result = { ...seed, ...body, storageRoot: body.storageRoot ?? "default" };
        state.volumes = [result];
        return route.fulfill({ status: 201, json: result });
      },
      [`/api/microblockstorage/${VOLUME}`]: route => {
        state.writes.push({ method: "delete", body: {} });
        state.volumes = [];
        return route.fulfill({ status: 204 });
      },
      [`/api/microblockstorage/${VOLUME}/attach`]: route => {
        const body = route.request().postDataJSON();
        state.writes.push({ method: "attach", body });
        const result = { ...seed, state: "attached" as const, attachedVmId: body.vmId, mountPath: body.mountPath };
        state.volumes = [result];
        return route.fulfill({ json: result });
      },
      [`/api/microblockstorage/${VOLUME}/detach`]: route => {
        state.writes.push({ method: "detach", body: {} });
        state.volumes = [{ ...seed }];
        return route.fulfill({ json: seed });
      },
      ...options.routes,
    },
  });
  return state;
}

test.describe("Persistent volumes @dashboard", () => {
  test("creates and explicitly deletes an independent data volume", async ({ page }, testInfo) => {
    const state = await open(page, { empty: true });
    await page.getByRole("button", { name: "Create volume", exact: true }).click();
    await page.getByLabel("Name", { exact: true }).fill("uploads");
    await page.getByLabel("Size (GiB)", { exact: true }).fill("4");
    await page.locator("#editor").getByRole("button", { name: "Create volume", exact: true }).click();
    await expect(page.getByRole("cell", { name: /^uploads/ })).toBeVisible();
    expect(state.writes[0]).toEqual({ method: "create", body: { name: "uploads", sizeGib: 4, storageRoot: null } });
    await page.screenshot({ path: testInfo.outputPath("volumes-desktop.png"), fullPage: true });
    page.once("dialog", dialog => dialog.accept());
    await page.getByRole("button", { name: "Delete", exact: true }).click();
    await expect(page.getByText("No data volumes yet.", { exact: true })).toBeVisible();
    expect(state.writes.at(-1)?.method).toBe("delete");
  });

  test("attaches to a stopped VM and disables detach while paused", async ({ page }, testInfo) => {
    await page.setViewportSize({ width: 1600, height: 1000 });
    await page.addInitScript(() => localStorage.setItem("firecrab.detailPanelWidth", "600"));
    const state = await open(page, { hash: "#/vms?vm=" + VM });
    const panel = page.getByRole("complementary", { name: "MicroVM detail", exact: true });
    await panel.getByRole("tab", { name: "Volumes", exact: true }).click();
    await panel.getByLabel("Available volume", { exact: true }).selectOption(VOLUME);
    await panel.getByLabel("Mount path", { exact: true }).fill("/var/lib/postgresql/data");
    await panel.getByRole("button", { name: "Attach volume", exact: true }).click();
    await expect(panel.getByText("/var/lib/postgresql/data", { exact: true })).toBeVisible();
    expect(state.writes[0]).toEqual({ method: "attach", body: { vmId: VM, mountPath: "/var/lib/postgresql/data" } });
    await page.screenshot({ path: testInfo.outputPath("volumes-vm-attached.png"), fullPage: true });
    state.vmState = "paused";
    await expect(panel.getByRole("button", { name: "Detach", exact: true })).toBeDisabled({ timeout: 10000 });
    state.vmState = "stopped";
    await expect(panel.getByRole("button", { name: "Detach", exact: true })).toBeEnabled({ timeout: 10000 });
    await panel.getByRole("button", { name: "Detach", exact: true }).click();
    await expect(panel.getByText("No attached volumes.", { exact: true })).toBeVisible();
    expect(state.writes.at(-1)?.method).toBe("detach");
  });

  test("shows create errors in the Korean mobile flow", async ({ page }, testInfo) => {
    await page.setViewportSize({ width: 390, height: 844 });
    const state = await open(page, { ko: true, failCreate: true, empty: true });
    await page.getByRole("button", { name: "볼륨 생성", exact: true }).click();
    await page.getByLabel("이름", { exact: true }).fill("existing");
    await page.locator("#editor").getByRole("button", { name: "볼륨 생성", exact: true }).click();
    await expect(page.getByRole("alert").filter({ hasText: "volume name is already registered" })).toBeVisible();
    expect(state.volumes).toHaveLength(0);
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    await page.screenshot({ path: testInfo.outputPath("volumes-ko-mobile.png"), fullPage: true });
  });

  test("keeps a populated volume table inside the mobile viewport", async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await open(page, { ko: true });
    await expect(page.getByText("app-data", { exact: true })).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  });

  test("selects a volume and guest path during VM creation", async ({ page }, testInfo) => {
    await page.setViewportSize({ width: 1440, height: 1100 });
    const writes: Record<string, unknown>[] = [];
    await open(page, { hash: "#/vms/new", routes: {
      "/api/images": [{ alias: "alpine-test", installed: true, minDiskGb: 1, rootfsSizeBytes: 1024 ** 3 }],
      "/api/micro-networks": [{ id: VM, name: "test-net", subnetCidr: "172.31.1.0/24" }],
      "/api/vms": route => {
        if (route.request().method() === "GET") return route.fulfill({ json: [] });
        writes.push(route.request().postDataJSON());
        return route.fulfill({ status: 201, json: vm("volume-vm", VM, "created") });
      },
    } });
    await page.locator("#field-name").fill("volume-vm");
    await page.getByRole("button", { name: "Add volume", exact: true }).click();
    await page.getByLabel("Volume", { exact: true }).selectOption(VOLUME);
    await page.getByLabel("Mount path", { exact: true }).fill("/data");
    await page.screenshot({ path: testInfo.outputPath("volumes-create-vm.png"), fullPage: true });
    await page.locator("#editor button[type=submit]").click();
    await expect.poll(() => writes.length).toBe(1);
    expect(writes[0].blockStorages).toEqual([{ volumeId: VOLUME, mountPath: "/data" }]);
  });
});
