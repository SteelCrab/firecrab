import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";
import type { ScheduleResponse, ScheduleRun, ScheduleSpec } from "../../firecrab-e2e/types/bindings";
import { openDashboard, vm } from "./support/mockApi";
import type { MockRoute } from "./support/mockApi";

const WEB = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const DB = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
const IDS = ["aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", "cccccccc-cccc-4ccc-8ccc-cccccccccccc", "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee"];
const NEW = "ffffffff-ffff-4fff-8fff-ffffffffffff";
const NEXT = Date.parse("2026-10-26T09:00:00+09:00");
const VMS = [
  vm("dev-web", WEB, "running", { cpu: 2, ram: 1024, diskGb: 8, cpuUsagePercent: 2.4, memoryUsedMib: 182, memoryTotalMib: 1024, memoryUsedPercent: 17.8 }),
  vm("postgres-main", DB, "stopped", { cpu: 2, ram: 2048, diskGb: 20 }),
];

function seeds(korean = false): ScheduleResponse[] {
  return [
    { id: IDS[0], name: korean ? "업무 시작" : "Workday start", vmId: WEB, action: "start", enabled: true, timing: { kind: "cron", expression: "0 9 * * Mon-Fri", timeZone: "Asia/Seoul" }, nextRunAtMs: NEXT, createdAtMs: 1, updatedAtMs: 1, lastRun: null },
    { id: IDS[1], name: korean ? "업무 종료" : "Workday stop", vmId: WEB, action: "stop", enabled: true, timing: { kind: "cron", expression: "0 18 * * Mon-Fri", timeZone: "Asia/Seoul" }, nextRunAtMs: NEXT + 9 * 3600000, createdAtMs: 1, updatedAtMs: 1, lastRun: null },
    { id: IDS[2], name: korean ? "주말 점검" : "Weekend maintenance", vmId: DB, action: "start", enabled: false, timing: { kind: "once", at: "2030-01-05T14:00:37+09:00" }, nextRunAtMs: null, createdAtMs: 1, updatedAtMs: 1, lastRun: null },
  ];
}

interface Write { method: string; path: string; body: Record<string, unknown> }
async function open(page: Page, options: { seed?: ScheduleResponse[]; hash?: string; ko?: boolean; failSave?: boolean; runs?: ScheduleRun[] } = {}) {
  let saved = [...(options.seed ?? [])];
  const writes: Write[] = [];
  const routes: Record<string, MockRoute> = {
    "/api/host": { loadAverage1m: 0.12, memoryTotalMib: 8192, memoryAvailableMib: 6310, diskTotalGib: 120, diskAvailableGib: 94, uptimeSeconds: 76800 },
    "/api/update/progress": { phase: "idle", percent: 0, updatedAtMs: 0 },
    "/api/micro-networks": [],
    "/api/storage": [{ id: "default", name: "Local storage", path: "/var/lib/firecrab", availableGib: 94 }],
    "/api/images": [], "/api/shells": [],
    "/api/schedules": async route => {
      if (route.request().method() === "GET") return route.fulfill({ json: saved });
      const body = route.request().postDataJSON() as ScheduleSpec;
      writes.push({ method: "POST", path: "/api/schedules", body: body as unknown as Record<string, unknown> });
      if (options.failSave) return route.fulfill({ status: 400, json: { error: { code: "validation_failed", message: "request validation failed", fields: { timing: "invalid cron expression" } } } });
      const result = { ...body, id: NEW, createdAtMs: 1, updatedAtMs: 1, nextRunAtMs: body.enabled ? NEXT : null, lastRun: null };
      saved = [...saved, result];
      return route.fulfill({ status: 201, json: result });
    },
  };
  for (const item of VMS) routes["/api/vms/" + item.id] = item;
  for (const id of [...IDS, NEW]) {
    const path = "/api/schedules/" + id;
    routes[path + "/runs"] = options.runs ?? [];
    routes[path + "/enabled"] = async route => {
      const body = route.request().postDataJSON();
      writes.push({ method: "PUT", path: path + "/enabled", body });
      saved = saved.map(item => item.id === id ? { ...item, enabled: body.enabled, nextRunAtMs: body.enabled ? NEXT : null } : item);
      return route.fulfill({ json: saved.find(item => item.id === id) });
    };
    routes[path] = async route => {
      const method = route.request().method();
      if (method === "DELETE") { saved = saved.filter(item => item.id !== id); return route.fulfill({ status: 204 }); }
      if (method === "PUT") {
        const body = route.request().postDataJSON();
        writes.push({ method, path, body });
        saved = saved.map(item => item.id === id ? { ...item, ...body } : item);
      }
      return route.fulfill({ json: saved.find(item => item.id === id) });
    };
  }
  await openDashboard(page, { vms: VMS, routes, locale: options.ko ? "ko" : "en", hash: options.hash ?? "#/schedules" });
  return writes;
}

const detail = (page: Page) => page.getByRole("complementary", { name: "Schedule detail", exact: true });
test.use({ timezoneId: "Asia/Seoul", locale: "ko-KR" });

test.describe("Schedules @dashboard", () => {
  test("creates, edits, pauses and deletes a weekday schedule without entering cron", async ({ page }) => {
    const writes = await open(page);
    await page.locator(".screen-header").getByRole("button", { name: "New schedule", exact: true }).click();
    await detail(page).getByRole("combobox", { name: "MicroVM", exact: true }).selectOption(WEB);
    await detail(page).getByLabel("Time", { exact: true }).fill("09:00");
    await expect(detail(page).getByLabel("Cron expression", { exact: true })).toBeHidden();
    await detail(page).getByRole("button", { name: "Create schedule", exact: true }).click();
    await expect(page.getByRole("row", { name: /^dev\-web\ ·\ Start(\s|$)/ })).toBeVisible();
    expect(writes[0].body).toMatchObject({ vmId: WEB, action: "start", enabled: true, timing: { kind: "cron", expression: "0 9 * * Mon-Fri", timeZone: "Asia/Seoul" } });
    await page.getByRole("row", { name: /^dev\-web\ ·\ Start(\s|$)/ }).click();
    await detail(page).getByRole("button", { name: "Edit schedule", exact: true }).click();
    await detail(page).getByRole("radiogroup", { name: "Action", exact: true }).getByRole("radio", { name: "Stop", exact: true }).check();
    await detail(page).getByLabel("Time", { exact: true }).fill("18:00");
    await detail(page).getByRole("button", { name: "Save changes", exact: true }).click();
    expect(writes.at(-1)?.body).toMatchObject({ action: "stop", timing: { expression: "0 18 * * Mon-Fri" } });
    const row = page.locator("table.schedule-table tbody tr").first();
    await row.getByRole("switch").click();
    await expect(row.getByRole("switch")).toHaveAttribute("aria-checked", "false");
    expect(writes.at(-1)?.body).toEqual({ enabled: false });
    await row.getByRole("cell").first().click();
    page.once("dialog", dialog => dialog.accept());
    await detail(page).getByRole("button", { name: /Delete dev-web/ }).click();
    await expect(page.getByText("No schedules yet", { exact: true })).toBeVisible();
  });

  test("keeps VM context through creation, details, reload and back navigation", async ({ page }) => {
    const writes = await open(page, { seed: seeds(), hash: "#/vms?vm=" + DB });
    const vmPanel = page.getByRole("complementary", { name: "MicroVM detail", exact: true });
    await expect(vmPanel.getByRole("heading", { name: "postgres-main", exact: true })).toBeVisible();
    await vmPanel.getByLabel("More VM actions", { exact: true }).click();
    await vmPanel.getByRole("link", { name: "Add schedule postgres-main", exact: true }).click();
    await expect(detail(page).getByRole("combobox", { name: "MicroVM", exact: true })).toHaveValue(DB);
    await detail(page).getByRole("radiogroup", { name: "Repeat", exact: true }).getByRole("radio", { name: "Weekly", exact: true }).check();
    await detail(page).getByRole("button", { name: "Wed", exact: true }).click();
    await detail(page).getByLabel("Time", { exact: true }).fill("23:00");
    await detail(page).getByRole("button", { name: "Create schedule", exact: true }).click();
    expect(writes[0].body).toMatchObject({ vmId: DB, timing: { expression: "0 23 * * Mon,Wed", timeZone: "Asia/Seoul" } });
    await expect(page.getByRole("combobox", { name: "Filter by VM", exact: true })).toHaveValue(DB);
    await expect(page.getByRole("row", { name: /^Workday\ start(\s|$)/ })).toHaveCount(0);
    await page.getByRole("link", { name: "Open VM", exact: true }).click();
    await expect(vmPanel.getByRole("heading", { name: "postgres-main", exact: true })).toBeVisible();
    await vmPanel.getByRole("region", { name: "Schedules", exact: true }).getByRole("link").filter({ hasText: "Mon·Wed 23:00" }).click();
    await expect(page).toHaveURL(new RegExp("schedule=" + NEW));
    await expect(detail(page).getByRole("heading", { name: /postgres-main/ })).toBeVisible();
    await page.reload();
    await expect(detail(page).getByRole("heading", { name: /postgres-main/ })).toBeVisible();
    await page.goBack();
    await expect(vmPanel.getByRole("heading", { name: "postgres-main", exact: true })).toBeVisible();
  });

  test("preserves imported cron expressions and the exact instant of a one-time schedule", async ({ page }) => {
    const initial = seeds();
    initial[0].timing = { kind: "cron", expression: "*/15 9-18 * * Mon-Fri", timeZone: "America/New_York" };
    const writes = await open(page, { seed: initial });
    await page.getByRole("row", { name: /^Workday\ start(\s|$)/ }).click();
    await detail(page).getByRole("button", { name: "Edit schedule", exact: true }).click();
    await expect(detail(page).getByRole("radiogroup", { name: "Repeat", exact: true }).getByRole("radio", { name: "Custom cron", exact: true })).toBeChecked();
    await expect(detail(page).getByLabel("Cron expression", { exact: true })).toHaveValue("*/15 9-18 * * Mon-Fri");
    await detail(page).getByRole("button", { name: "Save changes", exact: true }).click();
    expect(writes[0].body.timing).toEqual(initial[0].timing);
    await page.getByRole("row", { name: /^Weekend\ maintenance(\s|$)/ }).click();
    await detail(page).getByRole("button", { name: "Edit schedule", exact: true }).click();
    await detail(page).getByRole("button", { name: "Save changes", exact: true }).click();
    expect(writes[1].body.timing).toEqual(initial[2].timing);
  });

  test("retains the entered values after server validation fails", async ({ page }) => {
    const writes = await open(page, { failSave: true });
    await page.locator(".screen-header").getByRole("button", { name: "New schedule", exact: true }).click();
    await detail(page).getByRole("radiogroup", { name: "Repeat", exact: true }).getByRole("radio", { name: "Once", exact: true }).check();
    await detail(page).getByLabel("Date", { exact: true }).fill("2030-01-01");
    await detail(page).getByLabel("Time", { exact: true }).fill("18:00");
    await detail(page).getByRole("button", { name: "Create schedule", exact: true }).click();
    await expect(detail(page).getByText("invalid cron expression", { exact: true })).toBeVisible();
    expect(writes[0].body.timing).toEqual({ kind: "once", at: "2030-01-01T09:00:00.000Z" });
    await expect(detail(page).getByLabel("Time", { exact: true })).toHaveValue("18:00");
  });

  test("shows failures and disables re-enabling a consumed one-time schedule", async ({ page }) => {
    const initial = seeds();
    const run: ScheduleRun = { id: NEW, scheduleId: IDS[2], scheduledAtMs: Date.parse(initial[2].timing.kind === "once" ? initial[2].timing.at : ""), startedAtMs: NEXT, finishedAtMs: NEXT + 1000, status: "failed", error: "target VM no longer exists; schedule disabled" };
    initial[2].lastRun = run;
    await open(page, { seed: initial, runs: [run] });
    const row = page.getByRole("row", { name: /^Weekend maintenance(\s|$)/ });
    await expect(row.getByRole("switch")).toBeDisabled();
    await row.getByRole("cell").first().click();
    await expect(detail(page).getByText(run.error!, { exact: true })).toBeVisible();
  });

  test("captures the implemented Korean desktop, VM integration, editor and mobile screens", async ({ page }, testInfo) => {
    await page.setViewportSize({ width: 1440, height: 960 });
    await open(page, { seed: seeds(true), ko: true });
    await expect(page.getByRole("row", { name: /^업무\ 시작(\s|$)/ })).toBeVisible();
    await expect(page.getByRole("button", { name: "예약 추가", exact: true })).toBeEnabled();
    await page.screenshot({ path: testInfo.outputPath("scheduler-list-ko.png"), fullPage: true });
    await page.getByRole("button", { name: "예약 추가", exact: true }).click();
    await expect(page.getByRole("complementary", { name: "예약 상세", exact: true }).getByRole("button", { name: "예약 만들기", exact: true })).toBeVisible();
    await page.screenshot({ path: testInfo.outputPath("scheduler-create-ko.png"), fullPage: true });
    await page.getByRole("button", { name: "취소", exact: true }).click();
    await page.getByRole("row", { name: /^업무 시작(\s|$)/ }).getByRole("link", { name: "dev-web", exact: true }).click();
    const panel = page.getByRole("complementary", { name: "MicroVM 상세", exact: true });
    await expect(panel.getByRole("region", { name: "예약", exact: true }).getByText("평일 09:00", { exact: true })).toBeVisible();
    await page.screenshot({ path: testInfo.outputPath("scheduler-vm-ko.png"), fullPage: true });
    await page.setViewportSize({ width: 390, height: 844 });
    await page.goto("/#/schedules");
    await expect(page.getByRole("row", { name: /^업무\ 시작(\s|$)/ })).toBeVisible();
    await expect(page.getByRole("row", { name: /^주말 점검(\s|$)/ }).getByRole("switch")).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBeTruthy();
    await page.screenshot({ path: testInfo.outputPath("scheduler-mobile-ko.png"), fullPage: true });
  });
});

test.describe("Schedule daylight saving time @dashboard", () => {
  test.use({ timezoneId: "America/New_York" });
  test("rejects a missing local hour and chooses the first repeated hour", async ({ page }) => {
    const writes = await open(page);
    await page.locator(".screen-header").getByRole("button", { name: "New schedule", exact: true }).click();
    await detail(page).getByRole("radiogroup", { name: "Repeat", exact: true }).getByRole("radio", { name: "Once", exact: true }).check();
    await detail(page).getByLabel("Date", { exact: true }).fill("2027-03-14");
    await detail(page).getByLabel("Time", { exact: true }).fill("02:30");
    await detail(page).getByRole("button", { name: "Create schedule", exact: true }).click();
    await expect(page.getByRole("alert")).toContainText("daylight saving");
    expect(writes).toHaveLength(0);
    await detail(page).getByLabel("Date", { exact: true }).fill("2027-11-07");
    await detail(page).getByLabel("Time", { exact: true }).fill("01:30");
    await detail(page).getByRole("button", { name: "Create schedule", exact: true }).click();
    await expect.poll(() => writes.length).toBe(1);
    expect(writes[0].body.timing).toEqual({ kind: "once", at: "2027-11-07T05:30:00.000Z" });
  });
});
