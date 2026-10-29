import { expect, test, type Page } from "@playwright/test";
import type { VmReconciliationOutcome } from "../types/bindings/VmReconciliationOutcome.js";

interface MockVm {
  id: string;
  name: string;
  state: string;
  reconciliation?: {
    outcome: VmReconciliationOutcome;
    checkedAtMs: number;
    detail: string | null;
  } | null;
  [field: string]: unknown;
}

const CHECKED_AT = 1791000000000;
const DIAGNOSTIC = "failed to re-attach TAP: helper unavailable";
const CASES: Array<[VmReconciliationOutcome, string, string]> = [
  ["reconnected", "Reconnected", "재연결 성공"],
  ["gone", "VM not found", "VM 없음"],
  ["mismatched", "Connection mismatch", "연결 불일치"],
  ["networkFailed", "Network recovery failed", "네트워크 복구 실패"],
  ["interrupted", "Startup interrupted", "시작 중단"],
  ["exited", "Exited while API offline", "API 중단 중 종료"],
];
function fixture(index: number, outcome?: VmReconciliationOutcome): MockVm {
  return {
    id: `12312312-3123-4123-8123-${String(index).padStart(12, "0")}`,
    name: outcome ? `vm-${outcome}` : `vm-unchecked-${index}`,
    state: outcome === "interrupted"
      ? "error"
      : outcome === "gone" || outcome === "exited" || outcome === "mismatched"
        ? "stopped"
        : "running",
    template: "alpine-3.24.1",
    templateVersion: "alpine-3.24.1-v5",
    cpu: 1,
    ram: 512,
    diskGb: 2,
    startupStep: null,
    startupTimeline: [],
    egressPolicy: "isolated",
    ipv4: "172.30.0.2",
    ipv6: null,
    mac: "02:fc:00:00:00:02",
    hostname: "fc-reconciled",
    microNetworkId: "00000000-0000-0000-0000-000000000001",
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
    ...(outcome ? {
      reconciliation: {
        outcome,
        checkedAtMs: CHECKED_AT,
        detail: outcome === "networkFailed" ? DIAGNOSTIC : null,
      },
    } : {}),
  };
}

async function openDashboard(page: Page, locale: "en" | "ko", vms: MockVm[]) {
  // Every API read is intercepted; these tests never mutate a live host.
  await page.route(/^https?:\/\/[^/]+\/api\//, async (route) => {
    const pathname = new URL(route.request().url()).pathname;
    if (pathname === "/api/vms") return route.fulfill({ json: vms });
    const vm = vms.find((item) => pathname === `/api/vms/${item.id}`);
    if (vm) return route.fulfill({ json: vm });
    if (pathname.endsWith("/log")) return route.fulfill({ json: { consoleLog: "", truncated: false } });
    if (pathname === "/api/update") return route.fulfill({ json: { current: "0.3.0", latest: "0.3.0", updateAvailable: false } });
    if (["/api/images", "/api/micro-networks", "/api/storage", "/api/shells"].includes(pathname)) {
      return route.fulfill({ json: [] });
    }
    return route.fulfill({ status: 503, json: { error: { message: "unused mock endpoint" } } });
  });
  await page.addInitScript((language) => localStorage.setItem("firecrab.locale", language), locale);
  await page.goto("/#/vms");
  await expect(page.locator("table.vm-list tbody tr")).toHaveCount(vms.length);
}

test.describe("VM startup reconciliation @dashboard", () => {
  test.use({ timezoneId: "Asia/Seoul" });
  for (const locale of ["en", "ko"] as const) {
    test(`list shows only the VM state and detail shows all six results in ${locale}`, async ({ page }, testInfo) => {
      const vms = CASES.map(([outcome], index) => fixture(index + 1, outcome));
      vms.push(fixture(7), { ...fixture(8), reconciliation: null });
      await openDashboard(page, locale, vms);

      // The list says only what the VM itself is doing; what the API recorded
      // about it is in the VM's detail.
      await expect(page.locator(".api-status-label")).toHaveCount(0);
      for (const [outcome] of CASES) {
        const row = page.locator("table.vm-list tbody tr").filter({ hasText: `vm-${outcome}` });
        const vm = vms.find((item) => item.name === `vm-${outcome}`)!;
        // One state badge per row, no API-status icon next to it.
        await expect(row.locator(".state-label")).toHaveCount(1);
        await expect(row.locator(".vm-status-label")).toHaveClass(new RegExp(`\\bbadge\\b.*\\b${vm.state}\\b`));
        await expect(row.locator(".vm-status-label svg")).toHaveCount(0);
        await expect(row.locator(".vm-status-label")).toHaveAttribute("data-state", vm.state);

        await row.locator(".vm-status-label").hover();
        const tooltip = page.getByRole("tooltip");
        await expect(tooltip).toHaveCount(1);
        await expect(tooltip).toBeVisible();
        await expect(tooltip.getByRole("heading")).toHaveText("VM-STATUS");
        await expect(tooltip).toContainText(vm.name);
        await expect(tooltip).toContainText(vm.id);
        await page.keyboard.press("Escape");
        await expect(tooltip).toHaveCount(0);
      }
      await page.screenshot({ path: testInfo.outputPath(`list-${locale}.png`), fullPage: true });

      for (const [outcome, english, korean] of CASES) {
        await page.getByRole("button", { name: `vm-${outcome}`, exact: true }).click();
        const panel = page.locator(".detail-panel");
        const detail = panel.getByRole("region", { name: "API-STATUS", exact: true });
        const vm = vms.find((item) => item.name === `vm-${outcome}`)!;
        await expect(panel.locator(".detail-title")).toHaveText(vm.name);
        await expect(panel.getByRole("region", { name: locale === "ko" ? "속성" : "Properties", exact: true })).toContainText(vm.state);
        await expect(panel.locator(".props").first()).toContainText(vm.id);
        await expect(panel.getByRole("heading", { name: "API-STATUS", exact: true })).toBeVisible();
        await expect(detail).toContainText("Firecrab API · firecrab-api");
        await expect(detail).toContainText(locale === "ko" ? korean : english);
        await expect(detail).toContainText(new Date(CHECKED_AT).toISOString());
        if (outcome === "networkFailed") {
          await expect(detail.locator(".wasm-diagnostic")).toHaveText(DIAGNOSTIC);
          await page.screenshot({ path: testInfo.outputPath(`detail-${locale}.png`), fullPage: true });
        } else {
          await expect(detail.locator(".wasm-diagnostic")).toHaveCount(0);
        }
      }

      await page.getByRole("button", { name: "vm-unchecked-7", exact: true }).click();
      await expect(page.locator(".detail-panel .detail-title")).toHaveText("vm-unchecked-7");
      await expect(page.locator(".detail-body")).toBeVisible();
      await expect(page.getByRole("region", { name: "API-STATUS", exact: true })).toHaveCount(0);
    });
  }

  test("polling clears a result after a new start", async ({ page }) => {
    const vm = fixture(1, "reconnected");
    const vms = [vm];
    await openDashboard(page, "en", vms);
    await page.getByRole("button", { name: vm.name, exact: true }).click();
    await expect(page.getByRole("region", { name: "API-STATUS", exact: true })).toBeVisible();
    vm.state = "starting";
    vm.reconciliation = null;
    await expect(page.locator("table.vm-list .vm-status-label")).toHaveAttribute("data-state", "starting");
    // The state is a colored dot plus plain text; "starting" uses the amber dot.
    await expect
      .poll(() => page.locator("table.vm-list .vm-status-label").evaluate((el) => getComputedStyle(el, "::before").backgroundColor))
      .toBe("rgb(217, 119, 6)");
    await expect(page.getByRole("region", { name: "API-STATUS", exact: true })).toHaveCount(0);
  });

  test("Korean diagnostics wrap within a narrow detail panel", async ({ page }, testInfo) => {
    await page.setViewportSize({ width: 390, height: 844 });
    const vm = fixture(1, "networkFailed");
    vm.reconciliation!.detail = `${DIAGNOSTIC}; ${"long-helper-diagnostic/".repeat(12)}`;
    await openDashboard(page, "ko", [vm]);
    await expect(page.locator(".vm-status-label svg")).toHaveCount(0);
    await expect(page.locator(".vm-status-label")).toHaveText(["실행 중"]);
    await page.screenshot({ path: testInfo.outputPath("list-mobile-ko.png"), fullPage: true });
    await page.getByRole("button", { name: vm.name, exact: true }).click();
    const detail = page.getByRole("region", { name: "API-STATUS", exact: true });
    await expect(detail).toBeVisible();
    const bounds = await detail.boundingBox();
    expect(bounds!.x).toBeGreaterThanOrEqual(0);
    expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(390);
    expect(await page.locator(".detail-body").evaluate((element) => element.scrollWidth <= element.clientWidth)).toBe(true);
    await page.screenshot({ path: testInfo.outputPath("detail-mobile-ko.png"), fullPage: true });
  });
});
