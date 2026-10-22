import { expect, test, type Page } from "@playwright/test";
import { openDashboard, vm } from "./support/mockApi";

const VMS = [
  vm("api-gateway", "7f3a9c2e-1b4d-4e8a-9c3f-2d1e0a5b6c7d", "running"),
  vm("postgres-main", "8d1f4a6c-2e3b-4c5d-9e8f-7a6b5c4d3e2f", "stopped"),
  vm("sandbox-dev", "e7f8a9b0-c1d2-4e3f-a4b5-c6d7e8f9a0b1", "error"),
];

const HOST = {
  loadAverage1m: 0.42,
  memoryTotalMib: 65536,
  memoryAvailableMib: 24903,
  diskTotalGib: 1000,
  diskAvailableGib: 620,
  uptimeSeconds: 1036800,
  platform: { os: "macos", name: "macOS 26", version: null, architecture: "arm64", virtualization: "vz", system: "Ubuntu 24.04", kernel: "6.8" },
};

const nav = (page: Page) => page.getByRole("navigation", { name: "Main navigation" });
const hostCard = (page: Page) => page.getByRole("region", { name: "Host status" });

test.describe("dashboard frame", () => {
  test("uses the redesign tokens", async ({ page }) => {
    await openDashboard(page, { vms: VMS });
    await expect(page.getByText("api-gateway", { exact: true })).toBeVisible();

    const body = page.locator("body");
    await expect(body).toHaveCSS("font-family", /Pretendard/);
    await expect(body).toHaveCSS("background-color", "rgb(255, 255, 255)");
    await expect(page.locator("#vm-list-add")).toHaveCSS("background-color", "rgb(24, 24, 27)");
  });

  test("leaves controls nested in a composite control to their own rules", async ({ page }) => {
    await openDashboard(page, { hash: "#/vms/new" });
    // The RAM stepper draws one border around its input and buttons.
    await expect(page.locator(".ram-stepper .ram-stepper-value")).toHaveCSS("border-top-width", "0px");
  });

  test("groups the navigation", async ({ page }) => {
    await openDashboard(page, { vms: VMS });
    for (const group of ["Compute", "Network", "Data", "System"]) {
      await expect(nav(page).getByText(group, { exact: true })).toBeVisible();
    }
    for (const name of ["MicroVM", "MicroService", "Networks", "Pipe", "Secrets", "Storage", "Images", "Kernels", "Shells", "Host"]) {
      await expect(nav(page).getByRole("button", { name, exact: true })).toBeVisible();
    }
  });

  test("shows the MicroVM count and error badge", async ({ page }) => {
    await openDashboard(page, { vms: VMS });
    const item = nav(page).getByRole("button", { name: "MicroVM", exact: true });
    await expect(item.locator(".sidebar-count")).toHaveText("3");
    await expect(item.locator(".sidebar-alert")).toHaveText("1");
  });

  test("shows host status and falls back when it fails", async ({ page }) => {
    await openDashboard(page, { vms: VMS, routes: { "/api/host": HOST } });
    await expect(hostCard(page)).toContainText("0.42");
    await expect(hostCard(page)).toContainText("62%");
    await expect(hostCard(page)).toContainText("38%");
    await expect(nav(page)).toContainText("macOS 26 · arm64");
  });

  test("shows dashes when host status cannot be read", async ({ page }) => {
    await openDashboard(page, { vms: VMS });
    await expect(hostCard(page).locator(".host-meter-value")).toHaveText(["—", "—", "—"]);
  });

  test("puts the banner under the screen header", async ({ page }) => {
    await openDashboard(page, {
      routes: { "/api/vms": (route) => route.fulfill({ status: 500, json: { error: { message: "VM list unavailable" } } }) },
    });
    const header = page.locator(".screen-header");
    const banner = page.getByRole("alert").filter({ hasText: "VM list unavailable" });
    await expect(banner).toBeVisible();
    await expect(page.locator(".screen-header + [role=alert]")).toHaveCount(1);
    await expect(header).toContainText("API unavailable — retrying every 15s", { timeout: 20_000 });
  });

  test("keeps the language select", async ({ page }) => {
    await openDashboard(page, { vms: VMS });
    await page.getByRole("combobox", { name: "Language", exact: true }).selectOption("ko");
    await expect(page.locator(".screen-header")).toContainText("컴퓨팅");
  });

  test("uses a drawer on small screens", async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await openDashboard(page, { vms: VMS });
    await expect(nav(page)).toBeHidden();
    await page.getByRole("button", { name: "Open menu", exact: true }).click();
    await expect(nav(page)).toBeVisible();
    await nav(page).getByRole("button", { name: "Networks", exact: true }).click();
    await expect(page).toHaveURL(/#\/networks$/);
    await expect(nav(page)).toBeHidden();
  });

  test("the console terminal starts with the mono token font", async ({ page }) => {
    const target = VMS[0];
    await openDashboard(page, { vms: VMS, hash: `#/console/${target.id}`, routes: { [`/api/vms/${target.id}`]: target } });
    const rows = page.locator(".console-page .xterm-rows");
    await expect(rows).toBeAttached();
    await expect(rows).toHaveCSS("font-family", /^"JetBrains Mono"/);
  });
});
