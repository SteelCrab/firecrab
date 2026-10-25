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

async function open(page: Page, options: { hash?: string; ko?: boolean; failCreate?: boolean; failAttach?: boolean; empty?: boolean; routes?: Record<string, MockRoute> } = {}) {
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
        if (options.failAttach) return route.fulfill({ status: 409, json: {
          error: { code: "volume_conflict", message: "volume is already attached" },
        } });
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
    await expect(panel.getByLabel("Available volume", { exact: true })).toHaveCount(0);
    await expect(panel.getByRole("button", { name: "Close detail", exact: true })).toHaveCount(1);
    await panel.getByRole("button", { name: "Attach volume", exact: true }).click();
    await expect(panel.getByLabel("Available volume", { exact: true })).toHaveValue(VOLUME);
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
    expect(state.writes).toHaveLength(1);
    await expect(panel.getByText("Detach this volume? Its data will be kept.")).toBeVisible();
    await panel.getByRole("button", { name: "Detach volume", exact: true }).click();
    await expect(panel.getByText("No attached volumes.", { exact: true })).toBeVisible();
    expect(state.writes.at(-1)?.method).toBe("detach");
  });

  test("keeps unavailable actions out of the empty volume flow", async ({ page }) => {
    await open(page, { hash: "#/vms?vm=" + VM, empty: true });
    const panel = page.getByRole("complementary", { name: "MicroVM detail", exact: true });
    await panel.getByRole("tab", { name: "Volumes", exact: true }).click();
    await expect(panel.getByText("No available volumes.", { exact: true })).toBeVisible();
    await expect(panel.getByRole("link", { name: "Create volume", exact: true })).toHaveAttribute("href", "#/volumes");
    await expect(panel.getByLabel("Available volume", { exact: true })).toHaveCount(0);
    await expect(panel.getByRole("button", { name: "Attach volume", exact: true })).toHaveCount(0);
  });

  test("supports keyboard tabs and dismisses More without closing the panel", async ({ page }) => {
    await open(page, { hash: "#/vms?vm=" + VM });
    const panel = page.getByRole("complementary", { name: "MicroVM detail", exact: true });
    await expect.poll(async () => {
      const start = await panel.getByRole("button", { name: "Start", exact: true }).boundingBox();
      const more = await panel.getByLabel("More VM actions", { exact: true }).boundingBox();
      return start && more ? Math.abs(start.y - more.y) : Infinity;
    }).toBeLessThan(2);
    await panel.getByLabel("More VM actions", { exact: true }).click();
    await expect(panel.getByRole("button", { name: "Resources", exact: true })).toBeVisible();
    // A refresh must not collapse a menu while it is in use.
    await page.waitForTimeout(3500);
    await expect(panel.getByRole("button", { name: "Resources", exact: true })).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(panel.getByRole("button", { name: "Resources", exact: true })).toBeHidden();
    await expect(panel).toBeVisible();
    const overview = panel.getByRole("tab", { name: "Overview", exact: true });
    await overview.focus();
    await page.keyboard.press("ArrowRight");
    await expect(panel.getByRole("tab", { name: "Network", exact: true })).toBeFocused();
    await page.keyboard.press("ArrowRight");
    await expect(panel.getByRole("tab", { name: "Volumes", exact: true })).toHaveAttribute("aria-selected", "true");
  });

  test("preserves the attachment draft after an API error and cancels without a write", async ({ page }) => {
    const state = await open(page, { hash: "#/vms?vm=" + VM, failAttach: true });
    const panel = page.getByRole("complementary", { name: "MicroVM detail", exact: true });
    await panel.getByRole("tab", { name: "Volumes", exact: true }).click();
    await panel.getByRole("button", { name: "Attach volume", exact: true }).click();
    await panel.getByRole("button", { name: "Cancel", exact: true }).click();
    expect(state.writes).toHaveLength(0);
    await expect(panel.getByRole("button", { name: "Attach volume", exact: true })).toBeFocused();
    await panel.getByRole("button", { name: "Attach volume", exact: true }).click();
    await panel.getByLabel("Mount path", { exact: true }).fill("/var/lib/postgresql/data");
    await panel.getByRole("button", { name: "Attach volume", exact: true }).click();
    await expect(panel.getByRole("alert")).toContainText("volume is already attached");
    await page.waitForTimeout(3500);
    await expect(panel.getByLabel("Mount path", { exact: true })).toHaveValue("/var/lib/postgresql/data");
    await expect(panel.getByLabel("Available volume", { exact: true })).toHaveValue(VOLUME);
    await expect(page.getByText("Saved.", { exact: true })).toHaveCount(0);
  });

  test("fits the attachment editor and connected volume in a phone viewport", async ({ page }, testInfo) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await open(page, { hash: "#/vms?vm=" + VM, ko: true });
    const panel = page.getByRole("complementary", { name: "MicroVM 상세", exact: true });
    await expect(page.getByRole("button", { name: "Open menu", exact: true })).toBeHidden();
    await panel.getByRole("tab", { name: "볼륨", exact: true }).click();
    await panel.getByRole("button", { name: "볼륨 연결", exact: true }).click();
    await panel.getByLabel("마운트 경로", { exact: true }).fill("/var/lib/postgresql/data");
    expect(await panel.evaluate(el => el.scrollWidth <= el.clientWidth)).toBe(true);
    await page.screenshot({ path: testInfo.outputPath("volumes-attach-mobile.png"), fullPage: true });
    await panel.getByRole("button", { name: "볼륨 연결", exact: true }).click();
    await expect(panel.getByText("/var/lib/postgresql/data", { exact: true })).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    await page.screenshot({ path: testInfo.outputPath("volumes-attached-mobile.png"), fullPage: true });
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.screenshot({ path: testInfo.outputPath("volumes-attached-default.png"), fullPage: true });
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
