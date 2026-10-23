import { expect, test, type Page } from "@playwright/test";
import type { VmResponse } from "../../firecrab-frontend/src/bindings";
import { openDashboard, vm } from "./support/mockApi";

const now = Date.now();
const STOPPED = vm("redis-cache", "5c6d7e8f-9a0b-4c1d-8e2f-3a4b5c6d7e8f", "stopped", {
  template: "alpine-3.20",
  templateVersion: "2026.09.0",
  cpu: 1,
  ram: 1024,
  diskGb: 8,
  startupTimeline: [
    { step: "preparingDisk", startedAtMs: now - 9000, endedAtMs: now - 7800, outcome: "succeeded", detail: null },
    { step: "generatingConfig", startedAtMs: now - 7800, endedAtMs: now - 7500, outcome: "succeeded", detail: null },
  ],
});
const RUNNING = vm("postgres-main", "8d1f4a6c-2e3b-4c5d-9e8f-7a6b5c4d3e2f", "running", {
  cpu: 4,
  ram: 4096,
  diskGb: 40,
  ipv4: "10.42.0.12",
  env: { POSTGRES_DB: "app", POSTGRES_PASSWORD: "pg-very-secret" },
});
const RECONCILED = vm("vm-network", "a1b2c3d4-0000-4000-8000-000000000001", "error", {
  reconciliation: { outcome: "networkFailed", checkedAtMs: now - 60_000, detail: "bridge is missing" },
  startupTimeline: [{ step: "configuringNetwork", startedAtMs: now - 40_000, endedAtMs: now - 10_000, outcome: "failed", detail: "guest DHCP did not answer" }],
});

const panel = (page: Page) => page.getByRole("complementary", { name: "MicroVM detail" });
const section = (page: Page, name: string) => panel(page).getByRole("region", { name, exact: true });

interface Put { path: string; body: Record<string, unknown> }

async function open(page: Page, vms: VmResponse[] | (() => VmResponse[]), select: string) {
  const puts: Put[] = [];
  const list = () => (typeof vms === "function" ? vms() : vms);
  await page.setViewportSize({ width: 1440, height: 900 });
  await openDashboard(page, {
    vms,
    routes: Object.fromEntries(list().flatMap((item) => [
      [`/api/vms/${item.id}`, async (route) => {
        if (route.request().method() === "PUT") {
          const body = route.request().postDataJSON() as Record<string, unknown>;
          puts.push({ path: `/api/vms/${item.id}`, body });
          return route.fulfill({ json: { ...item, ...body } });
        }
        return route.fulfill({ json: list().find((candidate) => candidate.id === item.id) ?? item });
      }],
      [`/api/vms/${item.id}/log`, { consoleLog: "[    0.000] firecrab-init: ready", truncated: false }],
    ])),
  });
  await page.getByRole("button", { name: select, exact: true }).click();
  await expect(panel(page).getByRole("heading", { name: select })).toBeVisible();
  return puts;
}

test.describe("MicroVM detail panel", () => {
  test("shows the overview", async ({ page }) => {
    await open(page, [STOPPED], "redis-cache");
    const properties = section(page, "Properties");
    await expect(properties).toContainText(STOPPED.id);
    await expect(properties).toContainText("alpine-3.20");
    await expect(properties).toContainText("2026.09.0");
    await expect(properties).toContainText("1 vCPU · 1 GiB · 8 GiB");
    await expect(properties).toContainText("MicroNetwork");
    await expect(properties).toContainText("MicroStorage");
    const startup = section(page, "Startup");
    await expect(startup.getByRole("listitem")).toHaveCount(4);
    await expect(startup.getByRole("listitem").first()).toContainText("Preparing disk");
    await expect(startup.getByRole("listitem").first()).toContainText("1s");
  });

  test("edits only the spec section", async ({ page }) => {
    const puts = await open(page, [STOPPED], "redis-cache");
    await section(page, "Properties").getByRole("button", { name: "Edit spec" }).click();
    await expect(panel(page).getByRole("button", { name: "Save" })).toHaveCount(1);
    await panel(page).getByLabel("vCPU", { exact: true }).fill("4");
    await panel(page).getByRole("button", { name: "Save" }).click();
    await expect.poll(() => puts.length).toBe(1);
    expect(puts[0].body).toEqual({ cpu: 4, ram: 1024, diskGb: 8, egressPolicy: "internet" });
    await expect(panel(page).getByRole("button", { name: "Save" })).toHaveCount(0);
  });

  test("locks the spec while running", async ({ page }) => {
    await open(page, [RUNNING], "postgres-main");
    const properties = section(page, "Properties");
    await expect(properties.getByRole("button", { name: "Edit spec" })).toHaveCount(0);
    await expect(properties).toContainText("Stop to change");
    await panel(page).getByRole("tab", { name: /Network/ }).click();
    await expect(section(page, "Port forwarding").getByRole("button", { name: "Edit port forwarding" })).toBeVisible();
    await panel(page).getByRole("tab", { name: /Environment/ }).click();
    await expect(section(page, "Environment").getByRole("button", { name: "Edit environment" })).toBeVisible();
  });

  test("saves env with the current resources", async ({ page }) => {
    const puts = await open(page, [RUNNING], "postgres-main");
    await panel(page).getByRole("tab", { name: /Environment/ }).click();
    await section(page, "Environment").getByRole("button", { name: "Edit environment" }).click();
    await panel(page).getByRole("button", { name: "Add variable" }).click();
    await panel(page).getByLabel("Key 3").fill("PGPORT");
    await panel(page).getByLabel("Value 3").fill("5432");
    await panel(page).getByRole("button", { name: "Save" }).click();
    await expect.poll(() => puts.length).toBe(1);
    expect(puts[0].body).toEqual({
      cpu: 4,
      ram: 4096,
      diskGb: 40,
      egressPolicy: "internet",
      env: { POSTGRES_DB: "app", POSTGRES_PASSWORD: "pg-very-secret", PGPORT: "5432" },
    });
  });

  test("keeps the draft across polling", async ({ page }) => {
    let tick = 0;
    // Every poll answers a fresh object for the same VM.
    await open(page, () => [{ ...RUNNING, cpuUsagePercent: ++tick }], "postgres-main");
    await panel(page).getByRole("tab", { name: /Environment/ }).click();
    await section(page, "Environment").getByRole("button", { name: "Edit environment" }).click();
    const value = panel(page).getByLabel("Value 1");
    await value.fill("draft-value");
    const seen = tick;
    await expect.poll(() => tick, { timeout: 10_000 }).toBeGreaterThan(seen + 1);
    await expect(value).toHaveValue("draft-value");
  });

  test("Esc while a section is being edited keeps the draft", async ({ page }) => {
    await open(page, [RUNNING], "postgres-main");
    await panel(page).getByRole("tab", { name: /Environment/ }).click();
    await section(page, "Environment").getByRole("button", { name: "Edit environment" }).click();
    await panel(page).getByLabel("Value 1").fill("draft-value");
    await panel(page).getByRole("button", { name: "Add variable" }).focus();
    await page.keyboard.press("Escape");
    await expect(panel(page).getByLabel("Value 1")).toHaveValue("draft-value");
  });

  test("reads the console log only while the Log tab is open", async ({ page }) => {
    let logReads = 0;
    let vmReads = 0;
    page.on("request", (request) => {
      const path = new URL(request.url()).pathname;
      if (path === `/api/vms/${RUNNING.id}/log`) logReads += 1;
      if (path === `/api/vms/${RUNNING.id}`) vmReads += 1;
    });
    await open(page, [RUNNING], "postgres-main");
    const before = vmReads;
    await page.waitForTimeout(3200);
    expect(logReads).toBe(0);
    // A settled VM is refreshed at the list's pace, not every 750ms.
    expect(vmReads - before).toBeLessThanOrEqual(2);
    await panel(page).getByRole("tab", { name: "Log" }).click();
    await expect.poll(() => logReads).toBeGreaterThan(0);
  });

  test("masks secret values", async ({ page }) => {
    await open(page, [RUNNING], "postgres-main");
    await panel(page).getByRole("tab", { name: /Environment/ }).click();
    const env = section(page, "Environment");
    await expect(env).toContainText("POSTGRES_DB");
    await expect(env).toContainText("app");
    await expect(env).toContainText("••••••••");
    await expect(env).not.toContainText("pg-very-secret");
  });

  test("shows API status and a failed start", async ({ page }) => {
    await open(page, [RECONCILED], "vm-network");
    await expect(panel(page).getByRole("heading", { name: "API-STATUS", exact: true })).toBeVisible();
    await expect(panel(page).locator(".reconciliation-detail .reconciliation-diagnostic")).toHaveText("bridge is missing");
    const failure = panel(page).getByRole("alert");
    await expect(failure).toContainText("guest DHCP did not answer");
    await failure.getByRole("button", { name: "View log" }).click();
    await expect(panel(page).getByRole("tab", { name: "Log" })).toHaveAttribute("aria-selected", "true");
    await expect(panel(page).locator("pre.detail-log")).toContainText("firecrab-init: ready");
  });

  test("shows the duplicate env key error", async ({ page }) => {
    await open(page, [RUNNING], "postgres-main");
    await panel(page).getByRole("tab", { name: /Environment/ }).click();
    await section(page, "Environment").getByRole("button", { name: "Edit environment" }).click();
    await panel(page).getByLabel("Key 2").fill("POSTGRES_DB");
    await panel(page).getByRole("button", { name: "Save" }).click();
    await expect(section(page, "Environment")).toContainText("Environment keys must be unique.");
  });

  test("truncates long property values", async ({ page }) => {
    const long = vm("long-values", "22222222-3333-4444-8555-666666666666", "running", {
      ipv6: "fd42:dead:beef:cafe:0123:4567:89ab:cdef",
      storageRoot: "/var/lib/firecrab/storage/".concat("deep/".repeat(20)),
      env: { LONG: "v".repeat(200) },
    });
    await open(page, [long], "long-values");
    expect(await panel(page).evaluate((el) => el.getBoundingClientRect().width)).toBe(384);
    const storage = section(page, "Properties").locator(".prop-value").filter({ hasText: "deep/" });
    expect(await storage.evaluate((el) => el.scrollWidth > el.clientWidth)).toBe(true);
    await panel(page).getByRole("tab", { name: /Environment/ }).click();
    const value = section(page, "Environment").locator(".kv-value").first();
    expect(await value.evaluate((el) => el.scrollWidth > el.clientWidth)).toBe(true);
    expect(await panel(page).evaluate((el) => el.getBoundingClientRect().width)).toBe(384);
  });
});

test.describe("MicroVM pause and resume", () => {
  for (const locale of ["en", "ko"] as const) {
    test(`preserves a paused VM and resumes from the detail panel (${locale})`, async ({ page }) => {
      const current = { ...RUNNING, env: {} };
      const actions: string[] = [];
      await openDashboard(page, {
        locale,
        vms: () => [current],
        routes: {
          [`/api/vms/${current.id}`]: () => undefined,
          [`/api/vms/${current.id}/pause`]: async (route) => {
            actions.push("pause");
            current.state = "paused";
            await route.fulfill({ json: current });
          },
          [`/api/vms/${current.id}/resume`]: async (route) => {
            actions.push("resume");
            current.state = "running";
            await route.fulfill({ json: current });
          },
        },
      });
      // The detail endpoint always returns the latest execution state.
      await page.route(`**/api/vms/${current.id}`, (route) => route.fulfill({ json: current }));
      await page.getByRole("button", { name: current.name, exact: true }).click();
      const detail = page.getByRole("complementary", { name: locale === "en" ? "MicroVM detail" : "MicroVM 상세", exact: true });
      await detail.getByRole("button", { name: locale === "en" ? "Pause" : "일시정지", exact: true }).click();
      await expect(detail.getByRole("button", { name: locale === "en" ? "Resume" : "재개", exact: true })).toBeVisible();
      await expect(detail).toContainText(locale === "en" ? "Memory stays allocated" : "메모리는 계속 점유합니다");
      await expect(detail.getByRole("button", { name: locale === "en" ? "Start" : "시작", exact: true })).toHaveCount(0);
      await expect(detail.getByRole("button", { name: locale === "en" ? "Delete" : "삭제", exact: true })).toHaveCount(0);
      await expect(detail.getByRole("button", { name: locale === "en" ? "Edit spec" : "사양 수정", exact: true })).toHaveCount(0);
      await expect(detail.getByRole("link", { name: locale === "en" ? "Console" : "콘솔", exact: true })).toHaveCount(0);
      expect(current.ram).toBe(RUNNING.ram);
      expect(current.ipv4).toBe(RUNNING.ipv4);
      await detail.getByRole("button", { name: locale === "en" ? "Resume" : "재개", exact: true }).click();
      await expect(detail.getByRole("button", { name: locale === "en" ? "Pause" : "일시정지", exact: true })).toBeVisible();
      expect(actions).toEqual(["pause", "resume"]);
      const row = page.getByRole("row").filter({ has: page.getByRole("button", { name: current.name, exact: true }) });
      await row.getByRole("button", { name: locale === "en" ? "Pause" : "일시정지", exact: true }).click();
      await expect(row.getByRole("button", { name: locale === "en" ? "Resume" : "재개", exact: true })).toBeVisible();
      await page.getByRole("tab", { name: locale === "en" ? /^Paused/ : /^일시정지됨/ }).click();
      await expect(row).toBeVisible();
    });
  }

  test("an unconfirmed pause exposes recovery and stop actions", async ({ page }) => {
    const pending = { ...RUNNING, state: "pausing" as const };
    await open(page, [pending], pending.name);
    await expect(panel(page).getByRole("button", { name: "Retry pause", exact: true })).toBeVisible();
    await expect(panel(page).getByRole("button", { name: "Retry resume", exact: true })).toBeVisible();
    await expect(panel(page).getByRole("button", { name: "Stop", exact: true })).toBeVisible();
    await expect(panel(page).getByRole("button", { name: "Start", exact: true })).toHaveCount(0);
  });
});
