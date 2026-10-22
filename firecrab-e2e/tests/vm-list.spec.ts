import { expect, test, type Page } from "@playwright/test";
import { openDashboard, vm } from "./support/mockApi";

const HOST = {
  loadAverage1m: 0.42,
  memoryTotalMib: 65536,
  memoryAvailableMib: 24903,
  diskTotalGib: 1000,
  diskAvailableGib: 620,
  uptimeSeconds: 1036800,
};

const now = Date.now();
const VMS = [
  vm("api-gateway", "7f3a9c2e-1b4d-4e8a-9c3f-2d1e0a5b6c7d", "running", { cpu: 2, ram: 1024, diskGb: 8, ipv4: "10.42.0.11" }),
  vm("postgres-main", "8d1f4a6c-2e3b-4c5d-9e8f-7a6b5c4d3e2f", "running", { cpu: 4, ram: 4096, diskGb: 40, ipv4: "10.42.0.12", template: "debian-12" }),
  vm("redis-cache", "5c6d7e8f-9a0b-4c1d-8e2f-3a4b5c6d7e8f", "stopped", { cpu: 1, ram: 512, diskGb: 10 }),
  vm("sandbox-dev", "e7f8a9b0-c1d2-4e3f-a4b5-c6d7e8f9a0b1", "error", {
    cpu: 2,
    ram: 512,
    diskGb: 10,
    startupTimeline: [{ step: "configuringNetwork", startedAtMs: now - 40_000, endedAtMs: now - 10_000, outcome: "failed", detail: "guest DHCP did not answer" }],
  }),
];

const rows = (page: Page) => page.locator("table.vm-list tbody tr");
const row = (page: Page, name: string) => rows(page).filter({ has: page.getByRole("button", { name, exact: true }) });
const panel = (page: Page) => page.getByRole("complementary", { name: "MicroVM detail" });
const summary = (page: Page, label: string) => page.locator(".vm-summary-item").filter({ hasText: label });
const resizer = (page: Page) => page.getByRole("separator", { name: "Resize detail panel" });
const panelWidth = (page: Page) => panel(page).evaluate((el) => el.getBoundingClientRect().width);
const mainWidth = (page: Page) => page.locator(".screen-main").evaluate((el) => el.getBoundingClientRect().width);

async function open(page: Page, options: Parameters<typeof openDashboard>[1] = {}) {
  await openDashboard(page, { vms: VMS, routes: { "/api/host": HOST }, ...options });
  await expect(rows(page)).toHaveCount(4);
}

test.describe("MicroVM list", () => {
  test.beforeEach(async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 900 });
  });

  test("filters by state and text", async ({ page }) => {
    await open(page);
    await expect(page.getByRole("tab", { name: "All 4", exact: true })).toHaveAttribute("aria-selected", "true");
    await expect(page.getByRole("tab", { name: "Running 2", exact: true })).toBeVisible();
    await expect(page.getByRole("tab", { name: "Stopped 1", exact: true })).toBeVisible();
    await expect(page.getByRole("tab", { name: "Error 1", exact: true })).toBeVisible();

    await page.getByRole("tab", { name: "Running 2", exact: true }).click();
    await expect(rows(page)).toHaveCount(2);

    await page.getByRole("tab", { name: "All 4", exact: true }).click();
    await page.getByRole("searchbox", { name: "Filter MicroVMs" }).fill("10.42.0.12");
    await expect(rows(page)).toHaveCount(1);
    await expect(rows(page).first()).toContainText("postgres-main");
  });

  test("shows the summary", async ({ page }) => {
    await open(page);
    await expect(summary(page, "Running")).toContainText("2 / 4");
    await expect(summary(page, "vCPU")).toContainText("9");
    await expect(summary(page, "Memory")).toContainText("6 / 64 GiB");
    await expect(summary(page, "Disk")).toContainText("68 / 1000 GB");
  });

  test("shows the summary without host totals", async ({ page }) => {
    await openDashboard(page, { vms: VMS });
    await expect(rows(page)).toHaveCount(4);
    await expect(summary(page, "Memory")).toContainText("6 GiB");
    await expect(summary(page, "Memory")).not.toContainText("/");
    await expect(summary(page, "Disk")).toContainText("68 GB");
    await expect(summary(page, "Memory").locator(".vm-summary-bar")).toHaveCount(0);
  });

  test("selects a row and opens the panel", async ({ page }) => {
    await open(page);
    await expect(panel(page)).toContainText("Select an item to see its details.");
    await row(page, "postgres-main").click();
    await expect(panel(page).getByRole("heading", { name: "postgres-main" })).toBeVisible();
    await expect(row(page, "postgres-main")).toHaveAttribute("aria-selected", "true");
    await expect(row(page, "api-gateway")).toHaveAttribute("aria-selected", "false");
  });

  test("remembers a closed panel", async ({ page }) => {
    await open(page);
    await panel(page).getByRole("button", { name: "Close detail" }).click();
    await expect(panel(page)).toHaveCount(0);
    await page.reload();
    await expect(rows(page)).toHaveCount(4);
    await expect(panel(page)).toHaveCount(0);
    await row(page, "api-gateway").click();
    await expect(panel(page).getByRole("heading", { name: "api-gateway" })).toBeVisible();
  });

  test("overlays the panel on narrower screens", async ({ page }) => {
    await page.setViewportSize({ width: 1100, height: 800 });
    await open(page);
    await expect(panel(page)).toHaveCount(0);
    const before = await page.locator("table.vm-list").evaluate((table) => table.getBoundingClientRect().width);
    await row(page, "api-gateway").click();
    await expect(panel(page)).toHaveCSS("position", "fixed");
    const after = await page.locator("table.vm-list").evaluate((table) => table.getBoundingClientRect().width);
    expect(after).toBe(before);
    await page.keyboard.press("Escape");
    await expect(panel(page)).toHaveCount(0);
  });

  test("moves the selection with the keyboard", async ({ page }) => {
    await open(page);
    await row(page, "api-gateway").click();
    await page.keyboard.press("ArrowDown");
    await page.keyboard.press("ArrowDown");
    await expect(row(page, "redis-cache")).toHaveAttribute("aria-selected", "true");

    const filter = page.getByRole("searchbox", { name: "Filter MicroVMs" });
    await filter.focus();
    await page.keyboard.press("ArrowUp");
    await expect(row(page, "redis-cache")).toHaveAttribute("aria-selected", "true");
  });

  test("clears the selection when the VM disappears", async ({ page }) => {
    let gone = false;
    await open(page, { vms: () => (gone ? VMS.filter((item) => item.name !== "redis-cache") : VMS) });
    await row(page, "redis-cache").click();
    await expect(panel(page).getByRole("heading", { name: "redis-cache" })).toBeVisible();
    gone = true;
    await expect(rows(page)).toHaveCount(3, { timeout: 10_000 });
    await expect(panel(page)).toContainText("Select an item to see its details.");
  });

  test("truncates long values", async ({ page }) => {
    const longName = "n".repeat(64);
    await open(page, { vms: [...VMS.slice(0, 3), vm(longName, "11111111-2222-4333-8444-555555555555", "running", { ipv4: "10.42.0.99" })] });
    const longRow = row(page, longName);
    await longRow.click();
    expect(await longRow.evaluate((tr) => tr.getBoundingClientRect().height)).toBe(54);
    expect(await longRow.locator(".vm-row-name").evaluate((el) => el.scrollWidth > el.clientWidth)).toBe(true);
    expect(await panel(page).evaluate((el) => el.getBoundingClientRect().width)).toBe(384);
  });

  test("shows the error summary and start step in the second line", async ({ page }) => {
    const starting = vm("worker-02", "6f7a8b9c-0d1e-4f2a-b3c4-d5e6f7a8b9c0", "starting", { startupStep: "startingProcess" });
    await open(page, { vms: [...VMS.slice(1), starting] });
    await expect(row(page, "sandbox-dev").locator(".vm-row-sub")).toHaveText("guest DHCP did not answer");
    await expect(row(page, "worker-02").locator(".vm-row-sub")).toHaveText("Starting process…");
    await expect(row(page, "postgres-main").locator(".vm-row-sub")).toHaveText("debian-12 · 4 vCPU · 4 GiB · 40 GiB");
  });

  test("shows a start step it does not know by its name", async ({ page }) => {
    // A newer API may report a step this dashboard has no label for.
    const future = vm("worker-03", "7a8b9c0d-1e2f-4a3b-8c4d-e5f6a7b8c9d0", "starting", { startupStep: "warmingCache" as never });
    await open(page, { vms: [...VMS.slice(1), future] });
    await expect(row(page, "worker-03").locator(".vm-row-sub")).toHaveText("warmingCache…");
    await row(page, "worker-03").click();
    await expect(panel(page).getByRole("heading", { name: "worker-03" })).toBeVisible();
  });

  test("Esc on a row menu or a tooltip leaves the panel open", async ({ page }) => {
    await open(page);
    await row(page, "api-gateway").click();
    await expect(panel(page).getByRole("heading", { name: "api-gateway" })).toBeVisible();
    // One list poll first: it re-renders the screen and used to reorder the key listeners.
    await page.waitForTimeout(3500);

    await row(page, "redis-cache").getByRole("button", { name: "Actions", exact: true }).click();
    await expect(page.getByRole("menu")).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(page.getByRole("menu")).toHaveCount(0);
    await expect(panel(page).getByRole("heading", { name: "api-gateway" })).toBeVisible();

    await row(page, "redis-cache").locator(".vm-status-label").hover();
    await expect(page.getByRole("tooltip")).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(page.getByRole("tooltip")).toHaveCount(0);
    await expect(panel(page).getByRole("heading", { name: "api-gateway" })).toBeVisible();
  });

  test("arrow keys inside the panel keep the selected VM", async ({ page }) => {
    await open(page);
    await row(page, "api-gateway").click();
    await panel(page).getByRole("tab", { name: "Log" }).click();
    await page.keyboard.press("ArrowDown");
    await expect(panel(page).getByRole("heading", { name: "api-gateway" })).toBeVisible();
    await expect(panel(page).getByRole("tab", { name: "Log" })).toHaveAttribute("aria-selected", "true");
  });

  test("shows at least as many full rows as the old table at 1440×900", async ({ page }) => {
    // The old dashboard showed 13 complete rows of 30 VMs at this size.
    const many = Array.from({ length: 30 }, (_, index) =>
      vm(`vm-${String(index).padStart(2, "0")}`, `00000000-0000-4000-8000-${String(index).padStart(12, "0")}`, "running", { ipv4: `10.42.0.${index + 10}` }));
    await openDashboard(page, { vms: many, routes: { "/api/host": HOST } });
    await expect(rows(page)).toHaveCount(30);
    const full = await page.evaluate(() => {
      const box = document.querySelector(".vm-list-scroll")!.getBoundingClientRect();
      return [...document.querySelectorAll("table.vm-list tbody tr")].filter((tr) => {
        const rect = tr.getBoundingClientRect();
        return rect.top >= box.top && rect.bottom <= box.bottom;
      }).length;
    });
    expect(full).toBeGreaterThanOrEqual(13);
  });

  test("drags the panel edge to resize it and remembers the width", async ({ page }) => {
    await open(page);
    expect(await panelWidth(page)).toBe(384);
    const mainBefore = await mainWidth(page);
    const box = (await resizer(page).boundingBox())!;
    const y = box.y + box.height / 2;
    await page.mouse.move(box.x + box.width / 2, y);
    await page.mouse.down();
    await page.mouse.move(box.x + box.width / 2 - 120, y, { steps: 6 });
    await page.mouse.up();
    expect(await panelWidth(page)).toBe(504);
    expect(await mainWidth(page)).toBe(mainBefore - 120);
    await expect(resizer(page)).toHaveAttribute("aria-valuenow", "504");

    await page.reload();
    await expect(rows(page)).toHaveCount(4);
    expect(await panelWidth(page)).toBe(504);
  });

  test("resizes the panel with the keyboard within limits", async ({ page }) => {
    await open(page);
    await row(page, "api-gateway").click();
    const handle = resizer(page);
    await expect(handle).toHaveAttribute("aria-valuenow", "384");
    await handle.focus();
    await page.keyboard.press("ArrowLeft");
    await expect(handle).toHaveAttribute("aria-valuenow", "400");
    await page.keyboard.press("ArrowRight");
    await page.keyboard.press("ArrowRight");
    await expect(handle).toHaveAttribute("aria-valuenow", "368");
    await page.keyboard.press("Home");
    await expect(handle).toHaveAttribute("aria-valuenow", "320");
    await page.keyboard.press("End");
    // 1440 − 236 sidebar − 480 kept for the list = 724, capped at 720.
    await expect(handle).toHaveAttribute("aria-valuenow", "720");
    expect(await panelWidth(page)).toBe(720);
    // Arrow keys on the handle never move the list selection.
    await expect(row(page, "api-gateway")).toHaveAttribute("aria-selected", "true");
  });

  test("double-click on the panel edge restores the default width", async ({ page }) => {
    await page.addInitScript(() => localStorage.setItem("firecrab.detailPanelWidth", "560"));
    await open(page);
    expect(await panelWidth(page)).toBe(560);
    await resizer(page).dblclick();
    expect(await panelWidth(page)).toBe(384);
  });

  test("keeps room for the list when a remembered width no longer fits", async ({ page }) => {
    await page.addInitScript(() => localStorage.setItem("firecrab.detailPanelWidth", "700"));
    await page.setViewportSize({ width: 1280, height: 800 });
    await open(page);
    // 1280 − 236 sidebar − 480 for the list.
    expect(await panelWidth(page)).toBe(564);
  });

  test("the overlay panel uses the same width", async ({ page }) => {
    await page.addInitScript(() => localStorage.setItem("firecrab.detailPanelWidth", "480"));
    await page.setViewportSize({ width: 1100, height: 800 });
    await open(page);
    await row(page, "api-gateway").click();
    await expect(panel(page)).toHaveCSS("position", "fixed");
    expect(await panelWidth(page)).toBe(480);
  });

  test("the phone panel has no resize handle", async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await open(page);
    await page.getByRole("button", { name: "api-gateway", exact: true }).click();
    await expect(panel(page).getByRole("heading", { name: "api-gateway" })).toBeVisible();
    await expect(resizer(page)).toBeHidden();
  });

  test("keeps the row actions menu", async ({ page }) => {
    await open(page);
    await row(page, "redis-cache").getByRole("button", { name: "Actions", exact: true }).click();
    const menu = page.getByRole("menu");
    await expect(menu.getByRole("menuitem", { name: "SSH connect" })).toBeVisible();
    await expect(menu.getByRole("menuitem", { name: "start" })).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(menu).toHaveCount(0);
  });
});
