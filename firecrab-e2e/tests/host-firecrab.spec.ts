import { expect, test, type Page } from "@playwright/test";

const INFO = {
  version: "0.3.1",
  prefix: "/usr/local",
  datadir: "/var/lib/firecrab",
  confdir: "/etc/firecrab",
  unitdir: "/etc/systemd/system",
  apiBase: "http://127.0.0.1:5523",
  firecrackerVersion: "v1.17.0",
  apiService: "active",
  netHelperService: "active",
  helperUnit: "firecrab-helper.service",
};

const HOST = {
  loadAverage1m: 0.42,
  memoryTotalMib: 3918,
  memoryAvailableMib: 3519,
  diskTotalGib: 15.6,
  diskAvailableGib: 14.9,
  uptimeSeconds: 93_784,
  platform: {
    os: "linux",
    name: "Debian GNU/Linux 13 (trixie)",
    version: null,
    architecture: "arm64",
    virtualization: null,
    system: "Debian GNU/Linux 13 (trixie)",
    kernel: "6.12.74+deb13+1-arm64",
  },
};

const NETWORK = { bridgeName: "mnb0", subnetCidr: "172.30.0.0/24", gateway: "172.30.0.1", uplink: "eth0", interfaces: ["eth0"] };

async function openHost(page: Page, locale: "en" | "ko", info: object | null | (() => object | null)) {
  await page.route(/^https?:\/\/[^/]+\/api\//, async (route) => {
    const pathname = new URL(route.request().url()).pathname;
    if (pathname === "/api/info") {
      const response = typeof info === "function" ? info() : info;
      return response
        ? route.fulfill({ json: response })
        : route.fulfill({ status: 503, json: { error: { message: "unavailable" } } });
    }
    if (pathname === "/api/host") return route.fulfill({ json: HOST });
    if (pathname === "/api/network") return route.fulfill({ json: NETWORK });
    if (pathname === "/api/update") return route.fulfill({ json: { current: "0.3.1", latest: "0.3.1", updateAvailable: false } });
    if (pathname === "/api/vms") return route.fulfill({ json: [] });
    return route.fulfill({ status: 503, json: { error: { message: "unused mock endpoint" } } });
  });
  await page.addInitScript((language) => localStorage.setItem("firecrab.locale", language), locale);
  await page.goto("/#/host");
}

test.describe("Host Firecrab panel @dashboard", () => {
  test("shows installation info, the Firecracker version, and service states", async ({ page }, testInfo) => {
    await openHost(page, "en", INFO);

    const panel = page.getByRole("region", { name: "Firecrab" });
    await expect(panel.getByRole("heading", { name: "Firecrab" })).toBeVisible();
    await expect(panel.locator("dt")).toHaveText(["version", "firecracker", "API service", "helper service", "prefix", "datadir", "confdir", "unitdir", "api"]);
    await expect(panel.locator("dd")).toHaveText([
      INFO.version,
      INFO.firecrackerVersion,
      INFO.apiService,
      INFO.netHelperService,
      INFO.prefix,
      INFO.datadir,
      INFO.confdir,
      INFO.unitdir,
      INFO.apiBase,
    ]);

    await expect(page.getByRole("heading", { name: "Host" })).toBeVisible();
    await expect(page.locator(".host-info").nth(0)).toContainText("Debian GNU/Linux 13 (trixie)");
    expect(await page.locator(".host-info").count()).toBe(2);
    const host = await page.locator(".host-info").nth(0).boundingBox();
    const firecrab = await panel.boundingBox();
    expect(firecrab!.y).toBeGreaterThan(host!.y + host!.height);
    await page.screenshot({ path: testInfo.outputPath("host-en.png"), fullPage: true });
    await page.setViewportSize({ width: 390, height: 844 });
    await expect(panel.getByText(INFO.firecrackerVersion, { exact: true })).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    await page.screenshot({ path: testInfo.outputPath("host-mobile.png"), fullPage: true });
  });

  test("says so when the info cannot be read, and the host panel is unaffected", async ({ page }) => {
    await openHost(page, "ko", null);

    const panel = page.getByRole("region", { name: "Firecrab" });
    await expect(panel).toContainText("Firecrab 정보를 불러오지 못했습니다.");
    await expect(panel.locator("dt")).toHaveCount(0);
    await expect(page.locator(".host-info").nth(0)).toContainText("Debian GNU/Linux 13 (trixie)");
  });

  test("missing runtime tools do not hide installation info", async ({ page }) => {
    await openHost(page, "ko", { ...INFO, firecrackerVersion: null, apiService: "unknown", netHelperService: "unknown" });
    const panel = page.getByRole("region", { name: "Firecrab" });
    await expect(panel.getByText("확인 불가", { exact: true })).toHaveCount(3);
    await expect(panel).toContainText(INFO.datadir);
    await expect(panel).toContainText(INFO.version);
  });

  test("older APIs remain readable without invented service states", async ({ page }) => {
    const { firecrackerVersion: _version, apiService: _api, netHelperService: _helper, helperUnit: _unit, ...legacy } = INFO;
    await openHost(page, "en", legacy);
    const panel = page.getByRole("region", { name: "Firecrab" });
    await expect(panel.getByText("Unavailable", { exact: true })).toBeVisible();
    await expect(panel.getByText("unknown", { exact: true })).toHaveCount(2);
    await expect(panel).toContainText(INFO.datadir);
  });

  test("service changes refresh, outage clears stale green states, and recovery retries", async ({ page }) => {
    await page.clock.install();
    let response: object | null = INFO;
    await openHost(page, "en", () => response);
    const panel = page.getByRole("region", { name: "Firecrab" });
    await expect(panel.getByText("active", { exact: true })).toHaveCount(2);

    response = { ...INFO, netHelperService: "failed", helperUnit: "firecrab-net-helper.service" };
    await page.clock.fastForward(10_001);
    await expect(panel.getByText("failed", { exact: true })).toHaveAttribute("title", "firecrab-net-helper.service");
    await expect(panel.getByText("failed", { exact: true })).toHaveClass(/error/);

    response = null;
    await page.clock.fastForward(10_001);
    await expect(panel).toContainText("Could not load the Firecrab info.");
    await expect(panel.getByText("active", { exact: true })).toHaveCount(0);

    response = INFO;
    await page.clock.fastForward(10_001);
    await expect(panel.getByText("active", { exact: true })).toHaveCount(2);
  });

  test("a stalled API request clears stale active states after five seconds", async ({ page }) => {
    await page.clock.install();
    await openHost(page, "en", INFO);
    const panel = page.getByRole("region", { name: "Firecrab" });
    await expect(panel.getByText("active", { exact: true })).toHaveCount(2);
    await page.route("**/api/info", () => {});
    const request = page.waitForRequest("**/api/info");
    await page.clock.fastForward(10_001);
    await request;
    await page.clock.fastForward(5_001);
    await expect(panel).toContainText("Could not load the Firecrab info.");
    await expect(panel.getByText("active", { exact: true })).toHaveCount(0);
  });
});
