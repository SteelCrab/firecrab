import { expect, test, type Page } from "@playwright/test";
import type { VmReconciliationOutcome } from "../../firecrab-frontend/src/bindings/VmReconciliationOutcome.js";

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
  await expect(page.locator(".vm-table tbody tr")).toHaveCount(vms.length);
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
        const row = page.locator(".vm-table tbody tr").filter({ hasText: `vm-${outcome}` });
        const vm = vms.find((item) => item.name === `vm-${outcome}`)!;
        await expect(row.locator(".state-label")).toHaveText(["VM"]);
        await expect(row.locator(".vm-state-labels svg")).toHaveCount(0);
        await expect(row.locator(".vm-status-label")).toHaveCSS("font-weight", "700");
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
        const detail = page.locator(".reconciliation-detail");
        const vm = vms.find((item) => item.name === `vm-${outcome}`)!;
        await expect(page.locator(".vm-detail-heading dt")).toHaveText(["NAME", "ID", "VM-STATUS"]);
        await expect(page.locator(".vm-detail-name dd")).toHaveText(vm.name);
        await expect(page.locator(".vm-detail-id dd")).toHaveText(vm.id);
        await expect(page.locator(".vm-detail-state dd")).toHaveText(vm.state);
        await expect(page.getByRole("dialog").getByRole("heading", { name: "API-STATUS", exact: true })).toBeVisible();
        await expect(detail.locator(".api-status-service")).toHaveText("Firecrab API firecrab-api");
        await expect(detail.locator(".reconciliation-badge")).toHaveText(locale === "ko" ? korean : english);
        await expect(detail.locator("time")).toHaveAttribute("datetime", new Date(CHECKED_AT).toISOString());
        await expect(detail.locator("time")).toHaveText("2026-10-03 13:00:00 UTC+09:00");
        await expect(detail.locator(".reconciliation-note")).toContainText(locale === "ko" ? "VM 상태 확인 결과" : "VM lifecycle check");
        if (outcome === "networkFailed") {
          await expect(detail.locator(".reconciliation-diagnostic")).toHaveText(DIAGNOSTIC);
          await page.screenshot({ path: testInfo.outputPath(`detail-${locale}.png`), fullPage: true });
        } else {
          await expect(detail.locator(".reconciliation-diagnostic")).toHaveCount(0);
        }
        await page.locator(".console-close").click();
      }

      await page.getByRole("button", { name: "vm-unchecked-7", exact: true }).click();
      await expect(page.locator(".detail-body")).toBeVisible();
      await expect(page.locator(".reconciliation-detail")).toHaveCount(0);
    });
  }

  test("polling clears a result after a new start", async ({ page }) => {
    const vm = fixture(1, "reconnected");
    const vms = [vm];
    await openDashboard(page, "en", vms);
    await page.getByRole("button", { name: vm.name, exact: true }).click();
    await expect(page.locator(".reconciliation-detail")).toBeVisible();
    vm.state = "starting";
    vm.reconciliation = null;
    await expect(page.locator(".vm-table .vm-status-label")).toHaveAttribute("data-state", "starting");
    await expect(page.locator(".vm-table .vm-status-label")).toHaveCSS("color", "rgb(154, 106, 0)");
    await expect(page.locator(".reconciliation-detail")).toHaveCount(0);
  });

  test("Korean diagnostics wrap within a narrow detail panel", async ({ page }, testInfo) => {
    await page.setViewportSize({ width: 390, height: 844 });
    const vm = fixture(1, "networkFailed");
    vm.reconciliation!.detail = `${DIAGNOSTIC}; ${"long-helper-diagnostic/".repeat(12)}`;
    await openDashboard(page, "ko", [vm]);
    await expect(page.locator(".vm-state-labels svg")).toHaveCount(0);
    await expect(page.locator(".vm-state-labels .state-label")).toHaveText(["VM"]);
    await page.screenshot({ path: testInfo.outputPath("list-mobile-ko.png"), fullPage: true });
    await page.getByRole("button", { name: vm.name, exact: true }).click();
    const detail = page.locator(".reconciliation-detail");
    await expect(detail).toBeVisible();
    const bounds = await detail.boundingBox();
    expect(bounds!.x).toBeGreaterThanOrEqual(0);
    expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(390);
    expect(await page.locator(".detail-body").evaluate((element) => element.scrollWidth <= element.clientWidth)).toBe(true);
    await page.screenshot({ path: testInfo.outputPath("detail-mobile-ko.png"), fullPage: true });
  });
});
