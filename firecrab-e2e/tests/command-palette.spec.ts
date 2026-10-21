import { expect, test, type Page } from "@playwright/test";
import type { VmResponse } from "../../firecrab-frontend/src/bindings";

function vm(name: string, id: string, state: VmResponse["state"]): VmResponse {
  return {
    id,
    name,
    state,
    reconciliation: null,
    template: "ubuntu-24.04",
    templateVersion: "2026.09.1",
    cpu: 2,
    ram: 1024,
    diskGb: 8,
    startupStep: null,
    startupTimeline: [],
    egressPolicy: "internet",
    ipv4: state === "running" ? "10.42.0.11" : null,
    ipv6: null,
    mac: null,
    hostname: name,
    microNetworkId: "default",
    storageRoot: "default",
    cpuUsagePercent: null,
    memoryUsedMib: null,
    memoryTotalMib: null,
    memoryUsedPercent: null,
    usageHistory: [],
    shellRefs: [],
    portForwards: [],
    env: {},
    sshHostFingerprint: null,
  };
}

const VMS = [
  vm("api-gateway", "7f3a9c2e-1b4d-4e8a-9c3f-2d1e0a5b6c7d", "running"),
  vm("postgres-main", "8d1f4a6c-2e3b-4c5d-9e8f-7a6b5c4d3e2f", "stopped"),
];

async function open(page: Page, locale = "en") {
  await page.addInitScript((language) => localStorage.setItem("firecrab.locale", language), locale);
  // Only the VM list matters here; every other API call gets an empty answer
  // so pages reached through the palette render without a backend.
  await page.route(/^https?:\/\/[^/]+\/api\//, async (route) => {
    const path = new URL(route.request().url()).pathname;
    if (path === "/api/vms") return route.fulfill({ json: VMS });
    if (path === "/api/update") return route.fulfill({ json: { current: "0.3.1", latest: "0.3.1", updateAvailable: false } });
    return route.fulfill({ status: 404, json: { error: { message: "not mocked" } } });
  });
  await page.goto("/#/vms");
  await expect(page.getByRole("button", { name: "api-gateway", exact: true })).toBeVisible();
}

const palette = (page: Page) => page.getByRole("dialog", { name: "Command palette", exact: true });
const search = (page: Page) => palette(page).getByRole("combobox");

test.describe("command palette", () => {
  test("opens with the shortcut and closes with Escape", async ({ page }) => {
    await open(page);
    await expect(palette(page)).toHaveCount(0);

    await page.keyboard.press("ControlOrMeta+k");
    await expect(palette(page)).toBeVisible();
    await expect(search(page)).toBeFocused();
    await expect(palette(page).getByRole("group", { name: "Go to" })).toBeVisible();

    await page.keyboard.press("Escape");
    await expect(palette(page)).toHaveCount(0);
  });

  test("the shortcut toggles the palette closed again", async ({ page }) => {
    await open(page);
    await page.keyboard.press("ControlOrMeta+k");
    await expect(palette(page)).toBeVisible();
    await page.keyboard.press("ControlOrMeta+k");
    await expect(palette(page)).toHaveCount(0);
  });

  test("opens from the navigation search button", async ({ page }) => {
    await open(page);
    await page.getByRole("button", { name: "Search and go", exact: true }).click();
    await expect(palette(page)).toBeVisible();
  });

  test("closes when clicking outside", async ({ page }) => {
    await open(page);
    await page.keyboard.press("ControlOrMeta+k");
    await page.mouse.click(10, 10);
    await expect(palette(page)).toHaveCount(0);
  });

  test("filters by text and goes to the chosen screen", async ({ page }) => {
    await open(page);
    await page.keyboard.press("ControlOrMeta+k");
    await search(page).fill("kern");

    const options = palette(page).getByRole("option");
    await expect(options).toHaveCount(1);
    await expect(options.first()).toContainText("Kernels");
    await expect(options.first()).toHaveAttribute("aria-selected", "true");

    await page.keyboard.press("Enter");
    await expect(palette(page)).toHaveCount(0);
    await expect(page).toHaveURL(/#\/kernels$/);
  });

  test("arrow keys move the selection and Enter runs it", async ({ page }) => {
    await open(page);
    await page.keyboard.press("ControlOrMeta+k");
    await search(page).fill("MicroVM");

    const selected = palette(page).getByRole("option", { selected: true });
    const first = await selected.textContent();
    await page.keyboard.press("ArrowDown");
    await expect(selected).not.toHaveText(first ?? "");
    await page.keyboard.press("ArrowUp");
    await expect(selected).toHaveText(first ?? "");
  });

  test("choosing a MicroVM opens its detail", async ({ page }) => {
    await open(page);
    await page.goto("/#/host");
    await page.keyboard.press("ControlOrMeta+k");
    await search(page).fill("postgres");
    await palette(page).getByRole("option", { name: /postgres-main/ }).click();

    await expect(palette(page)).toHaveCount(0);
    await expect(page).toHaveURL(/#\/vms$/);
    await expect(page.getByRole("dialog", { name: /VM — postgres-main/ })).toBeVisible();
  });

  test("New MicroVM goes to the create page", async ({ page }) => {
    await open(page);
    await page.keyboard.press("ControlOrMeta+k");
    await search(page).fill("new microvm");
    await page.keyboard.press("Enter");
    await expect(page).toHaveURL(/#\/vms\/new$/);
  });

  test("shows an empty result", async ({ page }) => {
    await open(page);
    await page.keyboard.press("ControlOrMeta+k");
    await search(page).fill("zzzz-nothing");
    await expect(palette(page).getByRole("option")).toHaveCount(0);
    await expect(palette(page)).toContainText("No results");
  });

  test("uses Korean labels", async ({ page }) => {
    await open(page, "ko");
    await page.getByRole("button", { name: "검색 및 이동", exact: true }).click();
    const dialog = page.getByRole("dialog", { name: "명령 팔레트", exact: true });
    await expect(dialog).toBeVisible();
    await expect(dialog.getByRole("group", { name: "이동" })).toBeVisible();
    await dialog.getByRole("combobox").fill("커널");
    await expect(dialog.getByRole("option")).toHaveCount(1);
  });
});
