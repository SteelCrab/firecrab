import { expect, test, type Page, type Route } from "@playwright/test";
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
  vm("api-gateway", "7f3a9c2e-1b4d-4e8a-9c3f-2d1e0a5b6c7d", "running", {
    cpu: 2,
    ram: 1024,
    diskGb: 8,
    ipv4: "10.42.0.11",
  }),
  vm("postgres-main", "8d1f4a6c-2e3b-4c5d-9e8f-7a6b5c4d3e2f", "running", {
    cpu: 4,
    ram: 4096,
    diskGb: 40,
    ipv4: "10.42.0.12",
    template: "debian-12",
  }),
  vm("redis-cache", "5c6d7e8f-9a0b-4c1d-8e2f-3a4b5c6d7e8f", "stopped", {
    cpu: 1,
    ram: 512,
    diskGb: 10,
  }),
  vm("sandbox-dev", "e7f8a9b0-c1d2-4e3f-a4b5-c6d7e8f9a0b1", "error", {
    cpu: 2,
    ram: 512,
    diskGb: 10,
    startupTimeline: [
      {
        step: "configuringNetwork",
        startedAtMs: now - 40_000,
        endedAtMs: now - 10_000,
        outcome: "failed",
        detail: "guest DHCP did not answer",
      },
    ],
  }),
];

const many = Array.from({ length: 20 }, (_, i) =>
  vm(
    `vm-${String(i).padStart(2, "0")}`,
    `00000000-0000-4000-8000-${String(i).padStart(12, "0")}`,
    "running",
  ),
);
// More than the 256 rows the list renders in full: it then draws a window of rows between two spacers.
const windowed = Array.from({ length: 300 }, (_, i) =>
  vm(
    `vm-${String(i).padStart(3, "0")}`,
    `00000000-0000-4000-8000-${String(i).padStart(12, "0")}`,
    "running",
    { ipv4: `10.42.${Math.floor(i / 250)}.${(i % 250) + 2}` },
  ),
);

const rows = (page: Page) => page.locator("table.vm-list tbody tr");
const row = (page: Page, name: string) =>
  rows(page).filter({ has: page.getByRole("button", { name, exact: true }) });
const panel = (page: Page) =>
  page.getByRole("complementary", { name: "MicroVM detail" });
const summary = (page: Page, label: string) =>
  page.locator(".vm-summary-item").filter({ hasText: label });
const resizer = (page: Page) =>
  page.getByRole("separator", { name: "Resize detail panel" });
const panelWidth = (page: Page) =>
  panel(page).evaluate((el) => el.getBoundingClientRect().width);
const mainWidth = (page: Page) =>
  page
    .locator(".screen-main")
    .evaluate((el) => el.getBoundingClientRect().width);

async function open(
  page: Page,
  options: Parameters<typeof openDashboard>[1] = {},
) {
  await openDashboard(page, {
    vms: VMS,
    routes: { "/api/host": HOST },
    ...options,
  });
  await expect(rows(page)).toHaveCount(4);
}

test.describe("MicroVM list", () => {
  test.beforeEach(async ({ page }) => {
    await page.setViewportSize({ width: 1920, height: 900 });
  });

  test("filters by state and text", async ({ page }) => {
    await open(page);
    await expect(
      page.getByRole("tab", { name: "All 4", exact: true }),
    ).toHaveAttribute("aria-selected", "true");
    await expect(
      page.getByRole("tab", { name: "Running 2", exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("tab", { name: "Stopped 1", exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("tab", { name: "Error 1", exact: true }),
    ).toBeVisible();

    await page.getByRole("tab", { name: "Running 2", exact: true }).click();
    await expect(rows(page)).toHaveCount(2);

    await page.getByRole("tab", { name: "All 4", exact: true }).click();
    await page
      .getByRole("searchbox", { name: "Filter MicroVMs" })
      .fill("10.42.0.12");
    await expect(rows(page)).toHaveCount(1);
    await expect(rows(page).first()).toContainText("postgres-main");
  });

  test("shows the summary", async ({ page }) => {
    await open(page);
    await expect(
      summary(page, "Running").locator(".vm-summary-value"),
    ).toHaveText("2 / 4");
    await expect(
      summary(page, "Running").locator(".vm-summary-value small"),
    ).toHaveText("/ 4");
    await expect(summary(page, "vCPU")).toContainText("9");
    await expect(summary(page, "Memory")).toContainText("6 / 64 GiB");
    await expect(summary(page, "Disk")).toContainText("68 / 1000 GB");
    // Only the memory and disk items have a bar, and the items carry no icon or note.
    await expect(page.locator(".vm-summary-bar")).toHaveCount(2);
    await expect(
      page.locator("#vm-summary svg, #vm-summary .vm-summary-note"),
    ).toHaveCount(4);
  });

  test("lays the summary out as four readable allocation counters", async ({
    page,
  }) => {
    await open(page);
    const items = page.locator(".vm-summary-item");
    await expect(items).toHaveCount(4);
    const bounds = await items.evaluateAll((nodes) =>
      nodes.map((node) => node.getBoundingClientRect().toJSON()),
    );
    for (const box of bounds) {
      expect(box.width).toBeGreaterThan(200);
      expect(box.y).toBe(bounds[0].y);
    }
    for (const value of await items.locator(".vm-summary-value").all())
      expect(
        await value.evaluate((node) =>
          parseFloat(getComputedStyle(node).fontSize),
        ),
      ).toBeGreaterThanOrEqual(27);
    await expect(
      summary(page, "Memory").locator(".vm-summary-bar"),
    ).toHaveCount(1);
    await expect(summary(page, "Disk").locator(".vm-summary-bar")).toHaveCount(
      1,
    );
  });

  test("turns a summary bar amber from 80 percent", async ({ page }) => {
    // 56 of 64 GiB is 87%; 6 of 1000 GB disk stays dark.
    const heavy = [
      vm("big", "9a9a9a9a-0000-4000-8000-000000000001", "running", {
        ram: 57344,
      }),
    ];
    await openDashboard(page, { vms: heavy, routes: { "/api/host": HOST } });
    await expect(rows(page)).toHaveCount(1);
    await expect(
      page.locator(".vm-summary-bar").first().locator("i"),
    ).toHaveCSS("background-color", "rgb(217, 119, 6)");
    await expect(page.locator(".vm-summary-bar").nth(1).locator("i")).toHaveCSS(
      "background-color",
      "rgb(82, 82, 91)",
    );
  });

  test("shows the summary without host totals", async ({ page }) => {
    await openDashboard(page, { vms: VMS });
    await expect(rows(page)).toHaveCount(4);
    await expect(
      summary(page, "Memory").locator(".vm-summary-value"),
    ).toHaveText("6 GiB");
    await expect(summary(page, "Memory")).not.toContainText("/");
    await expect(summary(page, "Disk").locator(".vm-summary-value")).toHaveText(
      "68 GB",
    );
    await expect(
      summary(page, "Memory").locator(".vm-summary-bar"),
    ).toHaveCount(0);
  });

  test("opens the panel only for a selected row", async ({ page }) => {
    await open(page);
    await expect(page.locator("#detail-panel")).toBeHidden();
    await row(page, "postgres-main")
      .getByRole("button", { name: "postgres-main", exact: true })
      .click();
    await expect(
      panel(page).getByRole("heading", { name: "postgres-main" }),
    ).toBeVisible();
    await expect(row(page, "postgres-main")).toHaveAttribute(
      "aria-selected",
      "true",
    );
  });

  test("names every lifecycle control in the panel head", async ({ page }) => {
    await open(page);
    await page
      .getByRole("button", { name: "api-gateway", exact: true })
      .click();
    const head = panel(page).locator(".detail-head");
    for (const button of await head.getByRole("button").all())
      expect(
        (await button.getAttribute("aria-label")) ||
          (await button.innerText()).trim(),
      ).toBeTruthy();
    await expect(
      head.getByRole("button", { name: "Stop", exact: true }),
    ).toBeVisible();
    await expect(
      head.getByRole("button", { name: "Pause", exact: true }),
    ).toBeVisible();
  });

  test("groups VM properties, startup and usage into readable sections", async ({
    page,
  }) => {
    await open(page);
    await page
      .getByRole("button", { name: "postgres-main", exact: true })
      .click();
    const detail = panel(page);
    await expect(detail.locator(".detail-panel-kind")).toContainText("MicroVM");
    await expect(detail.locator(".detail-title")).toHaveCSS(
      "font-size",
      "27px",
    );
    for (const name of ["Properties", "Startup", "This VM: CPU and memory"])
      await expect(
        detail.getByRole("region", { name, exact: true }),
      ).toBeVisible();
    await expect(
      detail.getByRole("tab", { name: "Overview", exact: true }),
    ).toHaveAttribute("aria-selected", "true");
  });

  test("closes with the X button and with Escape and clears the selection", async ({
    page,
  }) => {
    await open(page);
    await row(page, "postgres-main")
      .getByRole("button", { name: "postgres-main", exact: true })
      .click();
    await panel(page).getByRole("button", { name: "Close detail" }).click();
    await expect(page.locator("#detail-panel")).toBeHidden();
    await expect(row(page, "postgres-main")).toHaveAttribute(
      "aria-selected",
      "false",
    );
    await row(page, "postgres-main")
      .getByRole("button", { name: "postgres-main", exact: true })
      .click();
    await page.keyboard.press("Escape");
    await expect(page.locator("#detail-panel")).toBeHidden();
  });

  test("closes the panel when the selected VM disappears", async ({ page }) => {
    let list = [...VMS];
    await openDashboard(page, { vms: () => list });
    await row(page, "postgres-main")
      .getByRole("button", { name: "postgres-main", exact: true })
      .click();
    await expect(page.locator("#detail-panel")).toBeVisible();
    list = list.filter((item) => item.name !== "postgres-main");
    await expect(page.locator("#detail-panel")).toBeHidden({ timeout: 10_000 });
  });

  test("keeps a dirty panel form on Escape and asks before X closes it", async ({
    page,
  }) => {
    await openDashboard(page, {
      vms: VMS,
      hash: "#/networks",
      routes: { "/api/micro-networks": [] },
    });
    await page
      .locator(".screen-header")
      .getByRole("button", { name: /^(Create|New network)$/ })
      .click();
    const detail = page.locator("#detail-panel");
    await detail.getByLabel("Name", { exact: true }).fill("draft");
    await page.keyboard.press("Escape");
    await expect(detail).toBeVisible();
    page.once("dialog", (dialog) => dialog.dismiss());
    await detail.getByRole("button", { name: "Close detail" }).click();
    await expect(detail.getByLabel("Name", { exact: true })).toHaveValue(
      "draft",
    );
  });

  test("overlays the panel on narrower screens", async ({ page }) => {
    await page.setViewportSize({ width: 1100, height: 800 });
    await open(page);
    await expect(panel(page)).toHaveCount(0);
    const before = await page
      .locator("table.vm-list")
      .evaluate((table) => table.getBoundingClientRect().width);
    await row(page, "api-gateway")
      .getByRole("button", { name: "api-gateway", exact: true })
      .click();
    await expect(panel(page)).toHaveCSS("position", "fixed");
    const after = await page
      .locator("table.vm-list")
      .evaluate((table) => table.getBoundingClientRect().width);
    expect(after).toBe(before);
    await page.keyboard.press("Escape");
    await expect(panel(page)).toHaveCount(0);
  });

  test("moves the selection with the keyboard", async ({ page }) => {
    await open(page);
    await row(page, "api-gateway")
      .getByRole("button", { name: "api-gateway", exact: true })
      .click();
    await page.keyboard.press("ArrowDown");
    await page.keyboard.press("ArrowDown");
    await expect(row(page, "redis-cache")).toHaveAttribute(
      "aria-selected",
      "true",
    );

    const filter = page.getByRole("searchbox", { name: "Filter MicroVMs" });
    await filter.focus();
    await page.keyboard.press("ArrowUp");
    await expect(row(page, "redis-cache")).toHaveAttribute(
      "aria-selected",
      "true",
    );
  });

  test("truncates long values", async ({ page }) => {
    const longName = "n".repeat(64);
    await open(page, {
      vms: [
        ...VMS.slice(0, 3),
        vm(longName, "11111111-2222-4333-8444-555555555555", "running", {
          ipv4: "10.42.0.99",
        }),
      ],
    });
    const longRow = row(page, longName);
    await longRow.click();
    expect(
      await longRow.evaluate((tr) => tr.getBoundingClientRect().height),
    ).toBe(75);
    expect(
      await longRow
        .locator(".vm-row-name")
        .evaluate((el) => el.scrollWidth > el.clientWidth),
    ).toBe(true);
    expect(
      await panel(page).evaluate((el) => el.getBoundingClientRect().width),
    ).toBe(576);
  });

  test("shows the error summary and start step in the second line", async ({
    page,
  }) => {
    const starting = vm(
      "worker-02",
      "6f7a8b9c-0d1e-4f2a-b3c4-d5e6f7a8b9c0",
      "starting",
      { startupStep: "startingProcess" },
    );
    await open(page, { vms: [...VMS.slice(1), starting] });
    await expect(row(page, "sandbox-dev").locator(".vm-row-sub")).toHaveText(
      "guest DHCP did not answer",
    );
    await expect(row(page, "worker-02").locator(".vm-row-sub")).toHaveText(
      "Starting process…",
    );
    await expect(row(page, "postgres-main").locator(".vm-row-sub")).toHaveText(
      "debian-12 · 4 vCPU · 4 GiB · 40 GiB",
    );
  });

  test("shows a start step it does not know by its name", async ({ page }) => {
    // A newer API may report a step this dashboard has no label for.
    const future = vm(
      "worker-03",
      "7a8b9c0d-1e2f-4a3b-8c4d-e5f6a7b8c9d0",
      "starting",
      { startupStep: "warmingCache" as never },
    );
    await open(page, { vms: [...VMS.slice(1), future] });
    await expect(row(page, "worker-03").locator(".vm-row-sub")).toHaveText(
      "warmingCache…",
    );
    await row(page, "worker-03")
      .getByRole("button", { name: "worker-03", exact: true })
      .click();
    await expect(
      panel(page).getByRole("heading", { name: "worker-03" }),
    ).toBeVisible();
  });

  test("Esc on a row menu or a tooltip leaves the panel open", async ({
    page,
  }) => {
    await open(page);
    await row(page, "api-gateway")
      .getByRole("button", { name: "api-gateway", exact: true })
      .click();
    await expect(
      panel(page).getByRole("heading", { name: "api-gateway" }),
    ).toBeVisible();
    // One list poll first: it re-renders the screen and used to reorder the key listeners.
    await page.waitForTimeout(3500);

    await row(page, "redis-cache")
      .getByRole("button", { name: "Actions", exact: true })
      .click();
    await expect(page.getByRole("menu")).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(page.getByRole("menu")).toHaveCount(0);
    await expect(
      panel(page).getByRole("heading", { name: "api-gateway" }),
    ).toBeVisible();

    await row(page, "redis-cache").locator(".vm-status-label").hover();
    await expect(page.getByRole("tooltip")).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(page.getByRole("tooltip")).toHaveCount(0);
    await expect(
      panel(page).getByRole("heading", { name: "api-gateway" }),
    ).toBeVisible();
  });

  test("arrow keys inside the panel keep the selected VM", async ({ page }) => {
    await open(page);
    await row(page, "api-gateway")
      .getByRole("button", { name: "api-gateway", exact: true })
      .click();
    await panel(page).getByRole("tab", { name: "Log" }).click();
    await page.keyboard.press("ArrowDown");
    await expect(
      panel(page).getByRole("heading", { name: "api-gateway" }),
    ).toBeVisible();
    await expect(panel(page).getByRole("tab", { name: "Log" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
  });

  test("shows at least seven readable full rows at 1440×900", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 1440, height: 900 });
    const many = Array.from({ length: 30 }, (_, index) =>
      vm(
        `vm-${String(index).padStart(2, "0")}`,
        `00000000-0000-4000-8000-${String(index).padStart(12, "0")}`,
        "running",
        { ipv4: `10.42.0.${index + 10}` },
      ),
    );
    await openDashboard(page, { vms: many, routes: { "/api/host": HOST } });
    await expect(rows(page)).toHaveCount(30);
    const full = await page.evaluate(() => {
      const box = document
        .querySelector(".vm-list-scroll")!
        .getBoundingClientRect();
      return [...document.querySelectorAll("table.vm-list tbody tr")].filter(
        (tr) => {
          const rect = tr.getBoundingClientRect();
          return rect.top >= box.top && rect.bottom <= box.bottom;
        },
      ).length;
    });
    expect(full).toBeGreaterThanOrEqual(7);
  });

  test("drags the panel edge to resize it and remembers the width", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 1920, height: 1000 });
    await open(page);
    await row(page, "api-gateway")
      .getByRole("button", { name: "api-gateway", exact: true })
      .click();
    expect(await panelWidth(page)).toBe(576);
    const mainBefore = await mainWidth(page);
    const box = (await resizer(page).boundingBox())!;
    const y = box.y + box.height / 2;
    await page.mouse.move(box.x + box.width / 2, y);
    await page.mouse.down();
    await page.mouse.move(box.x + box.width / 2 - 120, y, { steps: 6 });
    await page.mouse.up();
    expect(await panelWidth(page)).toBe(696);
    expect(await mainWidth(page)).toBe(mainBefore - 120);
    await expect(resizer(page)).toHaveAttribute("aria-valuenow", "696");

    await page.reload();
    await expect(rows(page)).toHaveCount(4);
    await row(page, "api-gateway")
      .getByRole("button", { name: "api-gateway", exact: true })
      .click();
    expect(await panelWidth(page)).toBe(696);
  });

  test("resizes the panel with the keyboard within limits", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 1920, height: 1000 });
    await open(page);
    await row(page, "api-gateway")
      .getByRole("button", { name: "api-gateway", exact: true })
      .click();
    const handle = resizer(page);
    await expect(handle).toHaveAttribute("aria-valuenow", "576");
    await handle.focus();
    await page.keyboard.press("ArrowLeft");
    await expect(handle).toHaveAttribute("aria-valuenow", "592");
    await page.keyboard.press("ArrowRight");
    await page.keyboard.press("ArrowRight");
    await expect(handle).toHaveAttribute("aria-valuenow", "560");
    await page.keyboard.press("Home");
    await expect(handle).toHaveAttribute("aria-valuenow", "320");
    await page.keyboard.press("End");
    // A wide workspace supports the full 960px detail panel.
    await expect(handle).toHaveAttribute("aria-valuenow", "1080");
    expect(await panelWidth(page)).toBe(1080);
    // Arrow keys on the handle never move the list selection.
    await expect(row(page, "api-gateway")).toHaveAttribute(
      "aria-selected",
      "true",
    );
  });

  test("double-click on the panel edge restores the default width", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 1920, height: 1000 });
    await page.addInitScript(() =>
      localStorage.setItem("firecrab.detailPanelWidth", "560"),
    );
    await open(page);
    await row(page, "api-gateway")
      .getByRole("button", { name: "api-gateway", exact: true })
      .click();
    expect(await panelWidth(page)).toBe(560);
    await resizer(page).dblclick();
    expect(await panelWidth(page)).toBe(576);
  });

  test("keeps room for the list when a remembered width no longer fits", async ({
    page,
  }) => {
    await page.addInitScript(() =>
      localStorage.setItem("firecrab.detailPanelWidth", "900"),
    );
    await page.setViewportSize({ width: 1600, height: 800 });
    await open(page);
    await row(page, "api-gateway")
      .getByRole("button", { name: "api-gateway", exact: true })
      .click();
    // 1600 minus 312px for navigation and 520px for the list.
    expect(await panelWidth(page)).toBe(840);
  });

  test("the overlay panel uses the same width", async ({ page }) => {
    await page.addInitScript(() =>
      localStorage.setItem("firecrab.detailPanelWidth", "480"),
    );
    await page.setViewportSize({ width: 1100, height: 800 });
    await open(page);
    await row(page, "api-gateway")
      .getByRole("button", { name: "api-gateway", exact: true })
      .click();
    await expect(panel(page)).toHaveCSS("position", "fixed");
    expect(await panelWidth(page)).toBe(480);
  });

  test("the phone panel has no resize handle", async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await open(page);
    await page
      .getByRole("button", { name: "api-gateway", exact: true })
      .click();
    await expect(
      panel(page).getByRole("heading", { name: "api-gateway" }),
    ).toBeVisible();
    await expect(resizer(page)).toBeHidden();
  });

  test("shows at least seven MicroVM rows at 1440x900", async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 900 });
    await openDashboard(page, { vms: many });
    await expect(page.locator("#vm-rows tr")).toHaveCount(20);
    const visible = await page
      .locator("#vm-rows tr")
      .evaluateAll(
        (rows) =>
          rows.filter(
            (row) => row.getBoundingClientRect().bottom <= window.innerHeight,
          ).length,
      );
    expect(visible).toBeGreaterThanOrEqual(7);
    await expect(page.locator("#vm-rows tr").first()).toHaveCSS(
      "height",
      "75px",
    );
    await expect(page.locator(".vm-overview")).toHaveCount(0);
  });

  test("reveals row actions on hover and keeps long names inside the row", async ({
    page,
  }) => {
    // 80 characters fit the name column at 1440px, so the name is longer than the brief's 80.
    const long = vm(
      "a".repeat(120),
      "11111111-1111-4111-8111-111111111111",
      "running",
    );
    await openDashboard(page, { vms: [long, ...VMS] });
    const first = page.locator("#vm-rows tr").first();
    // The Task 7 rules fade the buttons, not their cell.
    const action = first.locator(".vm-row-actions .icon-btn").first();
    await expect(action).toHaveCSS("opacity", "0");
    await first.hover();
    await expect(action).toHaveCSS("opacity", "1");
    expect(
      await first
        .locator(".vm-row-name")
        .evaluate((el) => el.scrollWidth > el.clientWidth),
    ).toBe(true);
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= window.innerWidth,
      ),
    ).toBe(true);
    await page.setViewportSize({ width: 390, height: 844 });
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= window.innerWidth,
      ),
    ).toBe(true);
  });

  test("keeps names readable on phones and exposes controls in the detail panel", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await open(page);
    for (const selector of [".vm-col-ip", ".vm-col-cpu", ".vm-col-mem"])
      for (const cell of await page
        .locator(`table.vm-list th${selector},table.vm-list td${selector}`)
        .all())
        await expect(cell).toBeHidden();
    await expect(
      page.getByRole("button", { name: "api-gateway", exact: true }),
    ).toBeVisible();
    await page
      .getByRole("button", { name: "api-gateway", exact: true })
      .click();
    await expect(
      panel(page).getByRole("button", { name: "Stop", exact: true }),
    ).toBeVisible();
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    ).toBe(true);
  });

  test("turns the memory meter amber from 80 percent", async ({ page }) => {
    const busy = vm("busy", "22222222-2222-4222-8222-222222222222", "running", {
      memoryUsedPercent: 85,
    });
    const idle = vm("idle", "33333333-3333-4333-8333-333333333333", "running", {
      memoryUsedPercent: 40,
    });
    await openDashboard(page, { vms: [busy, idle] });
    await expect(row(page, "busy").locator(".vm-meter")).toHaveClass(
      /\bwarn\b/,
    );
    await expect(row(page, "busy").locator(".vm-meter i")).toHaveCSS(
      "background-color",
      "rgb(217, 119, 6)",
    );
    await expect(row(page, "idle").locator(".vm-meter")).not.toHaveClass(
      /\bwarn\b/,
    );
  });

  test("keeps running VM execution, console and SSH controls together", async ({
    page,
  }) => {
    await open(page);
    await page
      .getByRole("button", { name: "api-gateway", exact: true })
      .click();
    const head = panel(page).locator(".detail-head");
    for (const name of ["Stop", "Pause", "SSH"])
      await expect(
        head.getByRole("button", { name, exact: true }),
      ).toBeVisible();
    const consoleLink = head.getByRole("link", {
      name: "Open console",
      exact: true,
    });
    await expect(consoleLink).toHaveAttribute("target", "_blank");
    await expect(consoleLink).toHaveAttribute(
      "href",
      new RegExp(`#/console/${VMS[0].id}$`),
    );
    for (const name of ["Delete", "Start"])
      await expect(head.getByRole("button", { name, exact: true })).toHaveCount(
        0,
      );
  });

  test("puts Start and Delete in a stopped VM's panel head and keeps Delete out of More", async ({
    page,
  }) => {
    const stopped = VMS[2];
    const deletes: string[] = [];
    await open(page, {
      routes: {
        "/api/host": HOST,
        [`/api/vms/${stopped.id}`]: (route) => {
          if (route.request().method() !== "DELETE")
            return route.fulfill({ json: stopped });
          deletes.push(new URL(route.request().url()).pathname);
          return route.fulfill({ status: 204 });
        },
      },
    });
    await row(page, "redis-cache")
      .getByRole("button", { name: "redis-cache", exact: true })
      .click();
    const head = panel(page).locator(".detail-actions");
    await expect(
      head.getByRole("button", { name: "Start", exact: true }),
    ).toBeVisible();
    const remove = head.getByRole("button", { name: "Delete", exact: true });
    await expect(remove).toBeVisible();
    await expect(remove).toHaveAttribute("title", "Delete");
    await expect(remove).toHaveClass(/\bdanger\b/);
    await expect(
      head.getByRole("link", { name: "Open console", exact: true }),
    ).toHaveCount(0);
    await expect(
      head.getByRole("button", { name: "Stop", exact: true }),
    ).toHaveCount(0);

    await expect(
      panel(page).getByRole("button", { name: "Edit spec", exact: true }),
    ).toBeVisible();
    await expect(
      panel(page).getByRole("button", { name: "Delete", exact: true }),
    ).toHaveCount(1);
    // The confirm step and the DELETE request stay as they were.
    let asked = "";
    page.once("dialog", (dialog) => {
      asked = dialog.message();
      void dialog.dismiss();
    });
    await remove.click();
    await expect.poll(() => asked).toBe("Delete this VM record and its disk?");
    expect(deletes).toEqual([]);
    page.once("dialog", (dialog) => void dialog.accept());
    await remove.click();
    await expect.poll(() => deletes).toEqual([`/api/vms/${stopped.id}`]);
  });

  test("keeps the panel scroll position across polls and starts at the top for another VM", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 1440, height: 600 });
    let tick = 0;
    // Every poll answers a fresh object, so the screen and the panel render again.
    const current = () =>
      VMS.map((item) =>
        item.state === "running" ? { ...item, cpuUsagePercent: ++tick } : item,
      );
    await openDashboard(page, {
      vms: current,
      routes: {
        "/api/host": HOST,
        ...Object.fromEntries(
          VMS.map((item) => [
            `/api/vms/${item.id}`,
            (route: Route) =>
              route.fulfill({
                json: current().find((vmItem) => vmItem.id === item.id),
              }),
          ]),
        ),
      },
    });
    await row(page, "api-gateway")
      .getByRole("button", { name: "api-gateway", exact: true })
      .click();
    await expect(
      panel(page).getByRole("heading", { name: "api-gateway" }),
    ).toBeVisible();
    const body = panel(page).locator(".detail-body");
    await expect
      .poll(() => body.evaluate((el) => el.scrollHeight - el.clientHeight))
      .toBeGreaterThan(150);
    await body.evaluate((el) => {
      el.scrollTop = 150;
      el.dataset.probe = "old";
    });
    expect(await body.evaluate((el) => el.scrollTop)).toBe(150);
    const seen = tick;
    await expect.poll(() => tick, { timeout: 10000 }).toBeGreaterThan(seen);
    await expect(body).toHaveAttribute("data-probe", "old");
    expect(await body.evaluate((el) => el.scrollTop)).toBe(150);

    await row(page, "postgres-main")
      .getByRole("button", { name: "postgres-main", exact: true })
      .click();
    await expect(
      panel(page).getByRole("heading", { name: "postgres-main" }),
    ).toBeVisible();
    expect(await body.evaluate((el) => el.scrollTop)).toBe(0);
  });

  test("scrolls a list of more than 256 MicroVMs to its last row", async ({
    page,
  }) => {
    await openDashboard(page, { vms: windowed });
    const scroll = page.locator("#vm-scroll");
    await expect(page.locator("table.vm-list")).toHaveAttribute(
      "data-visible-count",
      "300",
    );
    expect(await rows(page).count()).toBeLessThan(100);
    // Spacers and rows add up to the real height: a 51px head and 300 rows of 75px.
    const headerHeight = await page
      .locator("table.vm-list thead")
      .evaluate((node) => node.getBoundingClientRect().height);
    expect(
      Math.abs(
        (await scroll.evaluate((el) => el.scrollHeight)) -
          (headerHeight + 300 * 75),
      ),
    ).toBeLessThanOrEqual(2);
    await scroll.evaluate((el) => {
      el.scrollTop = el.scrollHeight;
    });
    const last = page.getByRole("button", { name: "vm-299", exact: true });
    await expect(last).toBeVisible();
    const inside = await last.evaluate((el) => {
      const box = document.querySelector("#vm-scroll")!.getBoundingClientRect();
      const rect = el.closest("tr")!.getBoundingClientRect();
      return rect.top >= box.top && rect.bottom <= box.bottom + 1;
    });
    expect(inside).toBe(true);
    await last.click();
    await expect(
      panel(page).getByRole("heading", { name: "vm-299" }),
    ).toBeVisible();
    await expect(row(page, "vm-299")).toHaveAttribute("aria-selected", "true");
  });

  test("keeps the row actions menu", async ({ page }) => {
    await open(page);
    await row(page, "redis-cache")
      .getByRole("button", { name: "Actions", exact: true })
      .click();
    const menu = page.getByRole("menu");
    await expect(
      menu.getByRole("menuitem", { name: "SSH connect" }),
    ).toBeVisible();
    await expect(menu.getByRole("menuitem", { name: "start" })).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(menu).toHaveCount(0);
  });
});

test.describe("MicroVM list on a touch screen", () => {
  test.use({
    viewport: { width: 390, height: 844 },
    hasTouch: true,
    isMobile: true,
  });

  test("keeps lifecycle and console controls available on touch screens", async ({
    page,
  }) => {
    await openDashboard(page, { vms: VMS, routes: { "/api/host": HOST } });
    await expect(rows(page)).toHaveCount(4);
    expect(await page.evaluate(() => matchMedia("(hover:none)").matches)).toBe(
      true,
    );
    await page
      .getByRole("button", { name: "api-gateway", exact: true })
      .click();
    for (const name of ["Stop", "Pause", "SSH"])
      await expect(
        panel(page).getByRole("button", { name, exact: true }),
      ).toBeVisible();
    await expect(
      panel(page).getByRole("link", { name: "Open console", exact: true }),
    ).toBeVisible();
    await panel(page)
      .getByRole("button", { name: "Close detail", exact: true })
      .click();
    await page
      .getByRole("button", { name: "redis-cache", exact: true })
      .click();
    await expect(
      panel(page).getByRole("button", { name: "Start", exact: true }),
    ).toBeVisible();
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    ).toBe(true);
  });
});
