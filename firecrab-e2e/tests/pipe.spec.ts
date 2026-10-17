import { expect, test, type Page } from "@playwright/test";
import type { VmResponse } from "../../firecrab-frontend/src/bindings/VmResponse";

function appVm(name: string, ipv4: string | null, ports: number[], state: VmResponse["state"] = "running"): VmResponse {
  return {
    id: name, name, state, template: "test-http-app", templateVersion: "latest", cpu: 1, ram: 256, diskGb: 1,
    startupStep: null, startupTimeline: [], egressPolicy: "internet", ipv4, ipv6: null, mac: null,
    hostname: name, microNetworkId: "test-network", storageRoot: "default", cpuUsagePercent: null,
    memoryUsedMib: null, memoryTotalMib: null, memoryUsedPercent: null, usageHistory: [],
    portForwards: ports.map((port, index) => ({ hostPort: 18081 + index, guestPort: port, protocol: "tcp" })),
  };
}

const appVms: VmResponse[] = [
  appVm("web-a", "172.30.36.2", [80]),
  appVm("web-b", "172.30.36.3", [80]),
  appVm("api", "172.30.36.4", [8080, 9090, 8080]),
  appVm("stopped-app", "172.30.36.5", [80], "stopped"),
  appVm("no-address", null, [80]),
  appVm("no-ports", "172.30.36.7", []),
];
appVms[2].portForwards!.push({ hostPort: 18053, guestPort: 53, protocol: "udp" });

const initial = {
  config: { listen: "127.0.0.1:8080", routes: [{ host: "app.example.com", pathPrefix: "/", upstreams: ["172.30.36.2:80", "172.30.36.3:80"] }], tunnel: null },
  units: [
    { unit: "firecrab-ingress.service", loadState: "loaded", activeState: "active", subState: "running", mainPid: 101 },
    { unit: "firecrab-tunnel.service", loadState: "not-found", activeState: "inactive", subState: "dead", mainPid: 0 },
  ],
  nginxAvailable: true, cloudflaredAvailable: true,
};

async function open(page: Page, options: { unavailable?: boolean; locale?: string; reject?: boolean; cloudflareFailure?: string; missingConnector?: boolean; holdConnection?: boolean; vms?: VmResponse[]; savedAddresses?: string[]; vmsUnavailable?: boolean } = {}) {
  let snapshot = structuredClone(initial);
  if (options.savedAddresses) snapshot.config.routes[0].upstreams = options.savedAddresses;
  let vms = structuredClone(options.vms ?? appVms);
  let vmsUnavailable = options.vmsUnavailable ?? false;
  const writes: { config: typeof initial.config; credentials: unknown }[] = [];
  let unavailable = options.unavailable ?? false;
  snapshot.cloudflaredAvailable = !options.missingConnector;
  const connections: { apiToken: string; zoneId: string; hostname: string; config: typeof initial.config }[] = [];
  let finishConnection: () => void = () => {};
  const connectionGate = new Promise<void>(resolve => { finishConnection = resolve; });
  await page.route(/^https?:\/\/[^/]+\/api\//, async route => {
    const path = new URL(route.request().url()).pathname;
    if (path === "/api/pipe/cloudflare/zones") return route.fulfill({ json: [{ id: "a".repeat(32), name: "example.com", status: "active", account: { id: "b".repeat(32), name: "Example" } }] });
    if (path === "/api/pipe/cloudflare/connect") {
      const body = route.request().postDataJSON(); connections.push(body);
      if (options.cloudflareFailure) return route.fulfill({ status: 400, json: { error: { message: "request validation failed", fields: { cloudflare: options.cloudflareFailure } } } });
      snapshot = { ...snapshot, cloudflaredAvailable: true, config: { ...body.config, tunnel: { id: "11111111-1111-4111-8111-111111111111", credentialsFile: "/etc/firecrab/pipe/secrets/tunnel-credentials.json" } } };
      snapshot.units[1] = { ...snapshot.units[1], loadState: "loaded", activeState: "active", subState: "running", mainPid: 202 };
      if (options.holdConnection) await connectionGate;
      return route.fulfill({ json: snapshot });
    }
    if (path === "/api/vms") return vmsUnavailable ? route.fulfill({ status: 503, json: { error: { message: "VM list unavailable" } } }) : route.fulfill({ json: vms });
    if (path === "/api/update") return route.fulfill({ json: { current: "0.3.1", latest: "0.3.1", updateAvailable: false } });
    if (path === "/api/pipe/disable") { snapshot.units.forEach(unit => { unit.activeState = "inactive"; unit.mainPid = 0; }); return route.fulfill({ json: snapshot }); }
    if (path === "/api/pipe") {
      if (unavailable) return route.fulfill({ status: 503, json: { error: { message: "Pipe helper unavailable" } } });
      if (route.request().method() === "PUT") {
        const body = route.request().postDataJSON();
        writes.push(body);
        if (options.reject) return route.fulfill({ status: 400, json: { error: { message: "request validation failed", fields: { config: "invalid app address" } } } });
        snapshot = { ...snapshot, config: body.config };
      }
      return route.fulfill({ json: snapshot });
    }
    return route.fulfill({ status: 404 });
  });
  await page.addInitScript(locale => localStorage.setItem("firecrab.locale", locale), options.locale ?? "en");
  await page.goto("/#/pipe");
  return { writes, connections, finishConnection, unavailable: () => { unavailable = true; }, setVms: (next: VmResponse[]) => { vms = next; }, failVms: () => { vmsUnavailable = true; } };
}

test.describe("Pipe dashboard @dashboard", () => {
  test.describe.configure({ timeout: 45_000 });
  test("edits domain/path routes and applies multiple app addresses", async ({ page }, info) => {
    const mock = await open(page);
    await expect(page.getByRole("button", { name: "Pipe", exact: true })).toHaveAttribute("aria-current", "page");
    await expect(page.getByRole("region", { name: "Pipe", exact: true })).toContainText("active");
    await page.getByLabel("Domain", { exact: true }).fill("api.example.com");
    await page.getByLabel("Path prefix", { exact: true }).fill("/api");
    const apps = page.getByRole("group", { name: "Apps", exact: true });
    await apps.getByRole("checkbox", { name: "api · TCP 8080", exact: true }).check();
    await apps.getByRole("checkbox", { name: "api · TCP 9090", exact: true }).check();
    await apps.screenshot({ path: info.outputPath("pipe-app-picker-desktop.png") });
    await expect(page.getByText("Unsaved changes", { exact: true })).toBeVisible();
    await page.getByRole("region", { name: "Routes", exact: true }).getByRole("button", { name: "Save", exact: true }).click();
    await expect(page.getByText("Routes saved.", { exact: true })).toBeVisible();
    expect(mock.writes).toHaveLength(1);
    expect(mock.writes[0].config.routes[1]).toEqual({ host: "api.example.com", pathPrefix: "/api", upstreams: ["172.30.36.4:8080", "172.30.36.4:9090"] });
    expect(mock.writes[0].credentials).toBeNull();
    await page.screenshot({ path: info.outputPath("pipe-desktop.png"), fullPage: true });
    await page.getByRole("row").filter({ hasText: "api.example.com" }).getByRole("button", { name: "Remove" }).click();
    await page.getByRole("region", { name: "Routes", exact: true }).getByRole("button", { name: "Save", exact: true }).click();
    await expect.poll(() => mock.writes.length).toBe(2);
    expect(mock.writes[1].config.routes).toHaveLength(1);
  });

  test("keeps edits when apply fails and clears stale service states", async ({ page }) => {
    await page.clock.install();
    const mock = await open(page, { reject: true });
    await page.getByRole("button", { name: "Edit", exact: true }).click();
    const apps = page.getByRole("group", { name: "Apps", exact: true });
    await expect(apps.getByRole("checkbox", { name: "web-a · TCP 80", exact: true })).toBeChecked();
    await expect(apps.getByRole("checkbox", { name: "web-b · TCP 80", exact: true })).toBeChecked();
    await apps.getByRole("checkbox", { name: "web-a · TCP 80", exact: true }).uncheck();
    await apps.getByRole("checkbox", { name: "web-b · TCP 80", exact: true }).uncheck();
    await apps.getByRole("checkbox", { name: "api · TCP 8080", exact: true }).check();
    await page.getByRole("region", { name: "Routes", exact: true }).getByRole("button", { name: "Save", exact: true }).click();
    await expect(page.getByRole("alert")).toContainText("invalid app address");
    await expect(page.getByRole("region", { name: "Routes", exact: true })).toContainText("172.30.36.4:8080");
    mock.unavailable(); await page.clock.runFor(8_100);
    const panel = page.getByRole("region", { name: "Pipe", exact: true });
    await expect(panel).toContainText("Unavailable");
    await expect(panel.locator(".state-badge.running")).toHaveCount(0);
  });

  for (const location of ["Routes", "Pipe"] as const) test(`saves edited routes with one ${location} Save action`, async ({ page }) => {
    const mock = await open(page);
    await page.getByRole("button", { name: "Edit", exact: true }).click();
    await page.getByLabel("Domain", { exact: true }).fill("edited.example.com");
    await page.getByLabel("Path prefix", { exact: true }).fill("/api");
    await page.getByRole("group", { name: "Apps", exact: true }).getByRole("checkbox", { name: "web-b · TCP 80", exact: true }).uncheck();
    await page.getByRole("region", { name: location, exact: true }).getByRole("button", { name: "Save", exact: true }).click();
    await expect(page.getByText("Routes saved.", { exact: true })).toBeVisible();
    expect(mock.writes).toHaveLength(1);
    expect(mock.writes[0].config.routes).toEqual([{ host: "edited.example.com", pathPrefix: "/api", upstreams: ["172.30.36.2:80"] }]);
    await expect(page.getByRole("button", { name: "Update route", exact: true })).toHaveCount(0);
    await expect(page.getByRole("button", { name: "Save and apply", exact: true })).toHaveCount(0);
    await page.reload();
    await expect(page.getByRole("row").filter({ hasText: "edited.example.com" })).toContainText("/api");
    expect(mock.writes).toHaveLength(1);
  });

  test("selects registered TCP ports and explains why other VMs are unavailable", async ({ page }) => {
    await open(page);
    const apps = page.getByRole("group", { name: "Apps", exact: true });
    await expect(apps.getByRole("checkbox", { name: "api · TCP 8080", exact: true })).toHaveCount(1);
    await expect(apps.getByRole("checkbox", { name: /TCP 53/ })).toHaveCount(0);
    await expect(apps.getByRole("checkbox", { name: "stopped-app · TCP 80", exact: true })).toBeDisabled();
    await expect(apps.getByRole("checkbox", { name: "no-address · TCP 80", exact: true })).toBeDisabled();
    await expect(apps.getByRole("checkbox", { name: "no-ports", exact: true })).toBeDisabled();
    await expect(apps).toContainText("No TCP ports registered");
    await expect(page.getByRole("textbox", { name: "App addresses", exact: true })).toHaveCount(0);
    await page.getByLabel("Domain", { exact: true }).fill("selected.example.com");
    const save = page.getByRole("region", { name: "Routes", exact: true }).getByRole("button", { name: "Save", exact: true });
    await expect(save).toBeDisabled();
    const web = apps.getByRole("checkbox", { name: "web-a · TCP 80", exact: true });
    await web.check();
    await expect(save).toBeEnabled();
    await web.uncheck();
    await expect(save).toBeDisabled();
  });

  for (const unavailable of [false, true]) test(`explains ${unavailable ? "failed" : "empty"} VM lists without allowing a route`, async ({ page }) => {
    await open(page, { vms: [], vmsUnavailable: unavailable });
    const apps = page.getByRole("group", { name: "Apps", exact: true });
    await expect(apps).toContainText(unavailable ? "VMs are unavailable." : "No VMs.");
    await expect(page.getByRole("region", { name: "Routes", exact: true }).getByRole("button", { name: "Save", exact: true })).toBeDisabled();
  });

  for (const change of ["stopped", "address", "unavailable"] as const) test(`rechecks app selection after the VM ${change} poll`, async ({ page }) => {
    await page.clock.install();
    const mock = await open(page);
    const apps = page.getByRole("group", { name: "Apps", exact: true });
    await apps.getByRole("checkbox", { name: "web-a · TCP 80", exact: true }).check();
    await page.getByLabel("Domain", { exact: true }).fill("selected.example.com");
    const save = page.getByRole("region", { name: "Routes", exact: true }).getByRole("button", { name: "Save", exact: true });
    await expect(save).toBeEnabled();
    if (change === "unavailable") mock.failVms();
    else mock.setVms(appVms.map(vm => vm.name === "web-a" ? { ...vm, ...(change === "stopped" ? { state: "stopped" as const } : { ipv4: "172.30.36.20" }) } : vm));
    await page.clock.runFor(3_100);
    await expect(save).toBeDisabled();
    await expect(apps).toContainText(change === "unavailable" ? "VMs are unavailable." : "Select a running app again.");
    expect(mock.writes).toHaveLength(0);
  });

  test("preserves existing addresses while editing and removes them only when unchecked", async ({ page }) => {
    const saved = ["192.0.2.10:8888", "172.30.36.2:80"];
    const mock = await open(page, { savedAddresses: saved });
    await page.getByRole("button", { name: "Edit", exact: true }).click();
    const legacy = page.getByRole("checkbox", { name: "192.0.2.10:8888", exact: true });
    await expect(legacy).toBeChecked();
    const apps = page.getByRole("group", { name: "Apps", exact: true });
    await expect(apps.getByText("Saved address", { exact: true })).toHaveCount(0);
    await expect(apps.getByText("192.0.2.10:8888", { exact: true })).toHaveCount(1);
    await expect(apps.getByRole("checkbox", { name: "web-a · TCP 80", exact: true })).toHaveCount(1);
    await page.getByLabel("Domain", { exact: true }).fill("renamed.example.com");
    await page.getByRole("region", { name: "Routes", exact: true }).getByRole("button", { name: "Save", exact: true }).click();
    await expect(page.getByText("Saved configuration", { exact: true })).toBeVisible();
    expect(mock.writes[0].config.routes[0].upstreams).toEqual(saved);

    await page.getByRole("button", { name: "Edit", exact: true }).click();
    await legacy.click();
    await expect(legacy).toHaveCount(0);
    await page.getByRole("region", { name: "Routes", exact: true }).getByRole("button", { name: "Save", exact: true }).click();
    await expect(page.getByText("Saved configuration", { exact: true })).toBeVisible();
    expect(mock.writes[1].config.routes[0].upstreams).toEqual(["172.30.36.2:80"]);
  });

  test("keeps a removed route out of the draft across polling and cancelled refresh", async ({ page }, info) => {
    await page.clock.install();
    const mock = await open(page);
    const routes = page.getByRole("region", { name: "Routes", exact: true });
    await routes.getByRole("button", { name: "Remove", exact: true }).click();
    await expect(routes.getByRole("button", { name: "Remove", exact: true })).toHaveCount(0);
    await expect(routes.getByRole("status")).toContainText("You have unsaved changes.");
    await page.clock.runFor(8_100);
    await expect(routes.getByRole("button", { name: "Remove", exact: true })).toHaveCount(0);
    expect(mock.writes).toHaveLength(0);
    await routes.screenshot({ path: info.outputPath("pipe-unsaved-desktop.png") });

    const dialogPromise = page.waitForEvent("dialog", { timeout: 10_000 });
    const reloading = page.reload({ timeout: 10_000 }).catch(() => undefined);
    const dialog = await dialogPromise;
    expect(dialog.type()).toBe("beforeunload");
    await dialog.dismiss();
    await reloading;
    await expect(routes.getByRole("button", { name: "Remove", exact: true })).toHaveCount(0);
    await expect(routes.getByRole("button", { name: "Save", exact: true })).toBeEnabled();
  });

  test("saves route removal from the nearby action and keeps it removed after refresh", async ({ page }, info) => {
    await page.setViewportSize({ width: 390, height: 844 });
    const mock = await open(page, { locale: "ko" });
    const routes = page.getByRole("region", { name: "라우트", exact: true });
    await expect(routes).toContainText("도메인과 URL 경로에 따라 요청을 앱으로 전달한다.");
    await routes.getByRole("button", { name: "제거", exact: true }).click();
    await expect(routes.getByRole("status")).toContainText("저장하지 않은 변경이 있다.");
    await expect(routes.getByText("라우트가 없다. 도메인, 경로, 앱 주소를 추가한다.", { exact: true })).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    await routes.screenshot({ path: info.outputPath("pipe-unsaved-mobile-ko.png") });
    await routes.getByRole("button", { name: "저장", exact: true }).click();
    await expect(page.getByText("저장된 설정", { exact: true })).toBeVisible();
    expect(mock.writes).toHaveLength(1);
    expect(mock.writes[0].config.routes).toEqual([]);
    await expect(routes.getByRole("status")).toHaveCount(0);
    const dialogs: string[] = [];
    page.on("dialog", async dialog => { dialogs.push(dialog.type()); await dialog.dismiss(); });
    await page.reload();
    await expect(routes).toContainText("라우트가 없다.");
    expect(dialogs).toEqual([]);
  });

  test("confirms leaving unsaved Pipe changes and keeps them when navigation is cancelled", async ({ page }) => {
    const mock = await open(page, { reject: true });
    const routes = page.getByRole("region", { name: "Routes", exact: true });
    await routes.getByRole("button", { name: "Remove", exact: true }).click();
    await routes.getByRole("button", { name: "Save", exact: true }).click();
    await expect(page.getByRole("alert")).toContainText("invalid app address");
    page.once("dialog", async dialog => {
      expect(dialog.type()).toBe("confirm");
      expect(dialog.message()).toContain("Pipe changes have not been saved.");
      await dialog.dismiss();
    });
    await page.getByRole("button", { name: "MicroVM", exact: true }).click();
    await expect(page).toHaveURL(/#\/pipe$/);
    await expect(routes.getByRole("button", { name: "Remove", exact: true })).toHaveCount(0);
    expect(mock.writes).toHaveLength(1);

    page.once("dialog", dialog => dialog.accept());
    await page.getByRole("button", { name: "MicroVM", exact: true }).click();
    await expect(page).toHaveURL(/#\/vms$/);
    await page.getByRole("button", { name: "Pipe", exact: true }).click();
    await expect(routes.getByRole("button", { name: "Remove", exact: true })).toHaveCount(1);
    await expect(routes.getByRole("status")).toHaveCount(0);
  });

  for (const direction of ["back", "forward"] as const) test(`keeps unfinished route inputs and browser history when ${direction} is cancelled`, async ({ page }) => {
    await open(page);
    await page.getByRole("button", { name: "MicroVM", exact: true }).click();
    await expect(page).toHaveURL(/#\/vms$/);
    await page.getByRole("button", { name: "Pipe", exact: true }).click();
    if (direction === "forward") {
      await page.getByRole("button", { name: "MicroVM", exact: true }).click();
      await expect(page).toHaveURL(/#\/vms$/);
      await page.goBack();
      await expect(page).toHaveURL(/#\/pipe$/);
    }
    await page.getByLabel("Domain", { exact: true }).fill("unfinished.example.com");
    page.once("dialog", dialog => dialog.dismiss());
    await (direction === "back" ? page.goBack() : page.goForward());
    await expect(page).toHaveURL(/#\/pipe$/);
    await expect(page.getByLabel("Domain", { exact: true })).toHaveValue("unfinished.example.com");

    page.once("dialog", dialog => dialog.accept());
    await (direction === "back" ? page.goBack() : page.goForward());
    await expect(page).toHaveURL(/#\/vms$/);
    await (direction === "back" ? page.goForward() : page.goBack());
    await expect(page).toHaveURL(/#\/pipe$/);
    await expect(page.getByLabel("Domain", { exact: true })).toHaveValue("");
    await expect(page.getByText("Saved configuration", { exact: true })).toBeVisible();
  });

  test("uses the existing responsive shell and clears credentials after saving", async ({ page }, info) => {
    await page.setViewportSize({ width: 390, height: 844 });
    const mock = await open(page, { locale: "ko" });
    await page.getByText("기존 터널 설정 열기", { exact: true }).click();
    await page.getByRole("checkbox", { name: "Cloudflare로 외부 접속 사용", exact: true }).check();
    await page.getByLabel("Tunnel UUID").fill("11111111-1111-4111-8111-111111111111");
    const credentials = { AccountTag: "fixture", TunnelID: "11111111-1111-4111-8111-111111111111", TunnelSecret: "private-fixture" };
    await page.getByLabel("인증 정보 JSON", { exact: true }).fill(JSON.stringify(credentials));
    await page.getByRole("region", { name: "Pipe", exact: true }).getByRole("button", { name: "저장", exact: true }).click();
    await expect(page.getByLabel("인증 정보 JSON", { exact: true })).toHaveValue("");
    expect(mock.writes[0].credentials).toEqual(credentials);
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    await page.screenshot({ path: info.outputPath("pipe-mobile-ko.png"), fullPage: true });
    page.on("dialog", dialog => dialog.accept());
    await page.getByRole("button", { name: "중지", exact: true }).click();
    await expect(page.getByText("Pipe를 중지했다.", { exact: false })).toBeVisible();
    await expect(page.locator(".state-badge.running")).toHaveCount(0);
  });

  test("guides token creation and connects without UUID, JSON or a preinstalled connector", async ({ page }, info) => {
    const mock = await open(page, { missingConnector: true });
    const panel = page.getByRole("region", { name: "Connect Cloudflare", exact: true });
    await panel.getByText("How to create an API token", { exact: true }).click();
    await expect(panel.getByRole("cell", { name: "Cloudflare Tunnel", exact: true })).toBeVisible();
    await expect(panel.getByRole("link", { name: "Open Cloudflare API Tokens" })).toHaveAttribute("href", "https://dash.cloudflare.com/profile/api-tokens");
    const token = "scoped-cloudflare-token-01234567890123456789";
    await panel.getByLabel("Cloudflare API token", { exact: true }).fill(token);
    await expect(panel.getByLabel("Cloudflare API token", { exact: true })).toHaveAttribute("type", "password");
    await panel.getByRole("button", { name: "Load domains", exact: true }).click();
    await expect(panel.getByRole("button", { name: "Connect and publish", exact: true })).toBeDisabled();
    await expect(panel.getByLabel("Cloudflare domain", { exact: true })).toHaveValue("a".repeat(32));
    await panel.getByLabel("Subdomain", { exact: true }).fill("demo");
    const apps = panel.getByRole("group", { name: "Public apps", exact: true });
    await apps.getByRole("checkbox", { name: "web-a · TCP 80", exact: true }).check();
    await apps.getByRole("checkbox", { name: "web-b · TCP 80", exact: true }).check();
    await page.screenshot({ path: info.outputPath("cloudflare-guided-desktop.png"), fullPage: true });
    await panel.getByRole("button", { name: "Connect and publish", exact: true }).click();
    await expect(panel.getByRole("link", { name: "https://demo.example.com" })).toBeVisible();
    expect(mock.connections).toHaveLength(1);
    expect(mock.connections[0]).toMatchObject({ apiToken: token, hostname: "demo.example.com", zoneId: "a".repeat(32) });
    expect(mock.connections[0].config.routes.at(-1)).toEqual({ host: "demo.example.com", pathPrefix: "/", upstreams: ["172.30.36.2:80", "172.30.36.3:80"] });
    await expect(panel.getByLabel("Cloudflare API token", { exact: true })).toHaveCount(0);
    expect(await page.evaluate(token => JSON.stringify(localStorage).includes(token), token)).toBe(false);
    expect(mock.writes).toHaveLength(0);
  });

  test("keeps connection inputs and existing routes when DNS setup is rejected", async ({ page }) => {
    const mock = await open(page, { cloudflareFailure: "A DNS record already uses this hostname; choose an unused subdomain" });
    const panel = page.getByRole("region", { name: "Connect Cloudflare", exact: true });
    await panel.getByLabel("Cloudflare API token", { exact: true }).fill("scoped-cloudflare-token-01234567890123456789");
    await panel.getByRole("button", { name: "Load domains", exact: true }).click();
    await panel.getByRole("checkbox", { name: "web-a · TCP 80", exact: true }).check();
    await panel.getByRole("button", { name: "Connect and publish", exact: true }).click();
    await expect(panel.getByRole("alert")).toContainText("choose an unused subdomain");
    await expect(panel.getByLabel("Subdomain", { exact: true })).toHaveValue("app");
    await expect(page.getByRole("region", { name: "Routes", exact: true })).toContainText("app.example.com");
    await expect(panel.getByRole("link", { name: "https://app.example.com" })).toHaveCount(0);
    expect(mock.writes).toHaveLength(0);
  });

  test("shows Korean token instructions and domain setup without mobile overflow", async ({ page }, info) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await open(page, { locale: "ko" });
    const panel = page.getByRole("region", { name: "Cloudflare 연결", exact: true });
    await panel.getByText("API 토큰 발급 방법", { exact: true }).click();
    await panel.getByLabel("Cloudflare API token", { exact: true }).fill("scoped-cloudflare-token-01234567890123456789");
    await panel.getByRole("button", { name: "도메인 불러오기", exact: true }).click();
    await expect(panel.getByLabel("Cloudflare 도메인", { exact: true })).toBeVisible();
    await panel.getByRole("checkbox", { name: "web-a · TCP 80", exact: true }).check();
    await panel.getByRole("checkbox", { name: "web-b · TCP 80", exact: true }).check();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    await panel.getByRole("group", { name: "공개할 앱 선택", exact: true }).screenshot({ path: info.outputPath("cloudflare-app-picker-mobile-ko.png") });
    await page.screenshot({ path: info.outputPath("cloudflare-guided-mobile-ko.png"), fullPage: true });
  });

  test("keeps setup visible while connection verification is pending across a status poll", async ({ page }) => {
    await page.clock.install();
    const mock = await open(page, { holdConnection: true });
    const panel = page.getByRole("region", { name: "Connect Cloudflare", exact: true });
    await panel.getByLabel("Cloudflare API token", { exact: true }).fill("scoped-cloudflare-token-01234567890123456789");
    await panel.getByRole("button", { name: "Load domains", exact: true }).click();
    await panel.getByRole("checkbox", { name: "web-a · TCP 80", exact: true }).check();
    await panel.getByRole("button", { name: "Connect and publish", exact: true }).click();
    await expect.poll(() => mock.connections.length).toBe(1);
    await page.clock.runFor(8_100);
    await expect(panel.getByRole("button", { name: "Connecting…", exact: true })).toBeDisabled();
    await expect(panel.getByLabel("Cloudflare API token", { exact: true })).toBeVisible();
    await expect(panel.getByRole("link", { name: "https://app.example.com" })).toHaveCount(0);
    mock.finishConnection();
    await expect(panel.getByRole("link", { name: "https://app.example.com" })).toBeVisible();
  });
});
