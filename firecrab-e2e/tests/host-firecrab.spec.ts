import { expect, test, type Page } from "@playwright/test";

const INFO = {
  version: "0.3.1",
  prefix: "/usr/local",
  datadir: "/var/lib/firecrab",
  confdir: "/etc/firecrab",
  unitdir: "/etc/systemd/system",
  apiBase: "http://127.0.0.1:5523",
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

async function openHost(page: Page, locale: "en" | "ko", info: "ok" | "unavailable") {
  await page.route(/^https?:\/\/[^/]+\/api\//, async (route) => {
    const pathname = new URL(route.request().url()).pathname;
    if (pathname === "/api/info") {
      return info === "ok"
        ? route.fulfill({ json: INFO })
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
  test("shows the version and what firecrab info prints, beside the host panel", async ({ page }, testInfo) => {
    await openHost(page, "en", "ok");

    const panel = page.getByRole("region", { name: "Firecrab" });
    await expect(panel.getByRole("heading", { name: "Firecrab" })).toBeVisible();
    await expect(panel.locator("dt")).toHaveText(["version", "prefix", "datadir", "confdir", "unitdir", "api"]);
    await expect(panel.locator("dd")).toHaveText([
      INFO.version,
      INFO.prefix,
      INFO.datadir,
      INFO.confdir,
      INFO.unitdir,
      INFO.apiBase,
    ]);

    // It sits under the host panel, which keeps working.
    await expect(page.getByRole("heading", { name: "Host" })).toBeVisible();
    await expect(page.locator(".host-info").nth(0)).toContainText("Debian GNU/Linux 13 (trixie)");
    expect(await page.locator(".host-info").count()).toBe(2);
    const host = await page.locator(".host-info").nth(0).boundingBox();
    const firecrab = await panel.boundingBox();
    expect(firecrab!.y).toBeGreaterThan(host!.y + host!.height);
    await page.screenshot({ path: testInfo.outputPath("host-en.png"), fullPage: true });
  });

  test("says so when the info cannot be read, and the host panel is unaffected", async ({ page }) => {
    await openHost(page, "ko", "unavailable");

    const panel = page.getByRole("region", { name: "Firecrab" });
    await expect(panel).toContainText("Firecrab 정보를 불러오지 못했습니다.");
    await expect(panel.locator("dt")).toHaveCount(0);
    await expect(page.locator(".host-info").nth(0)).toContainText("Debian GNU/Linux 13 (trixie)");
  });
});
