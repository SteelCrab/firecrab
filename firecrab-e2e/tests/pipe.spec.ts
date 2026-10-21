import { expect, test, type Page } from "@playwright/test";
import type { PipeConfig, PipeSnapshot } from "../../firecrab-frontend/src/api/pipe";
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
} satisfies PipeSnapshot;

type Service = "ingress" | "tunnel";
type ServiceAction = "start" | "stop" | "restart";

async function serviceMenu(page: Page, name = "Nginx") {
  await page.getByRole("region", { name: "Pipe", exact: true }).getByRole("button", { name: `${name} actions`, exact: true }).click();
  return page.getByRole("menu");
}

async function editNginx(page: Page) {
  await (await serviceMenu(page)).getByRole("menuitem", { name: "Edit", exact: true }).click();
}

const QUICK_ID = "11111111-1111-4111-8111-111111111111";
const SECOND_QUICK_ID = "22222222-2222-4222-8222-222222222222";
const LEGACY_QUICK_ID = "00000000-0000-0000-0000-000000000001";
const QUICK_URL = "https://first-tunnel.trycloudflare.com";
const SECOND_QUICK_URL = "https://second-tunnel.trycloudflare.com";
type QuickEntry = NonNullable<PipeConfig["quickTunnels"]>[number];
type QuickSave = Omit<QuickEntry, "id"> & { id?: string | null };
const webTunnel: QuickEntry = { id: QUICK_ID, name: "Public web", target: { type: "apps", upstreams: ["172.30.36.2:80"] } };
const apiTunnel: QuickEntry = { id: SECOND_QUICK_ID, name: "Public API", target: { type: "apps", upstreams: ["172.30.36.4:8080"] } };
const groupedRoutes = [
  ...initial.config.routes,
  { host: "app.example.com", pathPrefix: "/api", upstreams: ["172.30.36.4:8080"] },
  { host: "other.example.com", pathPrefix: "/", upstreams: ["172.30.36.4:9090"] },
  { host: "api-only.example.com", pathPrefix: "/api", upstreams: ["172.30.36.4:8080"] },
];

const CONNECTION_TOKEN = "a".repeat(64);
async function expectExistingQuickTunnel(page: Page, name: string) {
  const panel = page.getByRole("region", { name: "Quick Tunnel", exact: true });
  await expect(panel.getByRole("cell", { name, exact: true })).toBeVisible();
  await expect(panel.getByRole("button", { name: name + " actions", exact: true })).toBeVisible();
}

async function open(page: Page, options: { unavailable?: boolean; firecrabFailure?: boolean; firecrabPending?: boolean; locale?: string; reject?: boolean; quickFailure?: string; quickDeleteFailure?: string; namedFailure?: string; zonesFailure?: boolean; zoneName?: string; missingConnector?: boolean; missingNginx?: boolean; holdConnection?: boolean; quickUrlPending?: boolean; vms?: VmResponse[]; savedAddresses?: string[]; savedRoutes?: PipeConfig["routes"]; vmsUnavailable?: boolean; configuredTunnel?: boolean; configuredLegacyQuick?: boolean; quickTunnels?: QuickEntry[]; ingressState?: string; tunnelState?: string; serviceFailure?: string; unconfiguredIngress?: boolean } = {}) {
  let snapshot: PipeSnapshot = structuredClone(initial);
  if (options.savedRoutes) snapshot.config!.routes = structuredClone(options.savedRoutes);
  if (options.unconfiguredIngress) snapshot.config = null;
  if (options.configuredTunnel) snapshot.config!.tunnel = { id: QUICK_ID, credentialsFile: "/etc/firecrab/pipe/secrets/tunnel-credentials.json" };
  snapshot.quickTunnels = (options.quickTunnels ?? []).map((entry, index) => ({ ...structuredClone(entry), unit: { unit: `firecrab-quick-tunnel-${entry.id.replaceAll("-", "")}.service`, loadState: "loaded", activeState: "active", subState: "running", mainPid: 301 + index }, publicUrl: index === 0 ? QUICK_URL : SECOND_QUICK_URL }));
  if (options.quickTunnels) snapshot.config!.quickTunnels = structuredClone(options.quickTunnels);
  if (options.configuredLegacyQuick) {
    snapshot.config!.quickTunnel = { upstreams: ["172.30.36.2:80"] };
    snapshot.quickTunnel = { publicUrl: QUICK_URL };
    snapshot.quickTunnels.push({ id: LEGACY_QUICK_ID, name: "Quick Tunnel", target: { type: "apps", upstreams: ["172.30.36.2:80"] }, unit: { ...snapshot.units[1], loadState: "loaded", activeState: "active", subState: "running", mainPid: 202 }, publicUrl: QUICK_URL });
  }
  if (options.configuredTunnel || options.configuredLegacyQuick) snapshot.units[1] = { ...snapshot.units[1], loadState: "loaded", activeState: "active", subState: "running", mainPid: 202 };
  for (const [index, state] of [options.ingressState, options.tunnelState].entries()) {
    if (state) snapshot.units[index] = { ...snapshot.units[index], activeState: state, subState: state === "active" ? "running" : "dead", mainPid: state === "active" ? index + 101 : 0 };
  }
  snapshot.nginxAvailable = !options.missingNginx;
  snapshot.cloudflaredAvailable = !options.missingConnector;
  if (options.savedAddresses) snapshot.config!.routes[0].upstreams = options.savedAddresses;
  let vms = structuredClone(options.vms ?? appVms);
  let vmsUnavailable = options.vmsUnavailable ?? false;
  let unavailable = options.unavailable ?? false;
  const writes: { config: PipeConfig; credentials: unknown }[] = [];
  const actions: { service: Service; action: ServiceAction }[] = [];
  const namedConnections: { apiToken: string; zoneId: string; hostname: string; config: PipeConfig }[] = [];
  const zoneRequests: string[] = [];
  let namedDisconnects = 0;
  const firecrabSaves: { subdomain?: string; token?: string; target: import("../../firecrab-frontend/src/api/pipe").FirecrabTarget }[] = [];
  const firecrabDeletes: string[] = [];
  const quickSaves: QuickSave[] = [];
  const quickDeletes: string[] = [];
  const quickActions: { id: string; action: ServiceAction }[] = [];
  let finishConnection: () => void = () => {};
  let quickGeneration = 0;
  const connectionGate = new Promise<void>(resolve => { finishConnection = resolve; });
  await page.route(/^https?:\/\/[^/]+\/api\//, async route => {
    const path = new URL(route.request().url()).pathname;
    if (path === "/api/pipe/cloudflare/zones") {
      zoneRequests.push(route.request().postDataJSON().apiToken);
      if (options.zonesFailure) return route.fulfill({ status: 400, json: { error: { message: "Cloudflare rejected the API token" } } });
      return route.fulfill({ json: [{ id: "01234567890123456789012345678901", name: options.zoneName ?? "example.com", status: "active", account: { id: "abcdefabcdefabcdefabcdefabcdefab", name: "Test account" } }] });
    }
    if (path === "/api/pipe/cloudflare/connect") {
      const body = route.request().postDataJSON(); namedConnections.push(body);
      if (options.namedFailure) return route.fulfill({ status: 400, json: { error: { message: options.namedFailure } } });
      snapshot.config = { ...snapshot.config!, tunnel: { id: SECOND_QUICK_ID, credentialsFile: "/etc/firecrab/pipe/secrets/tunnel-credentials.json", hostname: body.hostname } };
      snapshot.units[1] = { ...snapshot.units[1], loadState: "loaded", activeState: "active", subState: "running", mainPid: 202 };
      return route.fulfill({ json: snapshot });
    }
    if (path === "/api/pipe/cloudflare/disconnect") {
      namedDisconnects++;
      snapshot.config = { ...snapshot.config!, tunnel: null };
      snapshot.units[1] = { ...snapshot.units[1], loadState: "not-found", activeState: "inactive", subState: "dead", mainPid: 0 };
      return route.fulfill({ json: snapshot });
    }
    if (path === "/api/pipe/firecrab-tunnels") {
      const body = route.request().postDataJSON(); firecrabSaves.push(body);
      if (options.firecrabFailure) return route.fulfill({ status: 400, json: { error: { message: "Connection token is invalid or revoked" } } });
      const subdomain = body.subdomain ?? "my-app";
      snapshot.firecrabTunnels = [...(snapshot.firecrabTunnels ?? []).filter(a => a.subdomain !== subdomain), { subdomain, target: body.target, publicUrl: `https://${subdomain}.firecrab.dev`, connectorId: body.target.type === "quick_tunnel" ? body.target.id : SECOND_QUICK_ID, connected: !options.firecrabPending, error: options.firecrabPending ? "Cannot reach the Firecrab connection service" : null }];
      return route.fulfill({ json: snapshot });
    }
    if (path.startsWith("/api/pipe/firecrab-tunnels/")) {
      const name = decodeURIComponent(path.split("/").at(-1)!); firecrabDeletes.push(name);
      snapshot.firecrabTunnels = snapshot.firecrabTunnels?.filter(a => a.subdomain !== name);
      return route.fulfill({ json: snapshot });
    }
    if (path === "/api/pipe/quick-tunnels") {
      const body = route.request().postDataJSON() as QuickSave; quickSaves.push(body);
      if (options.quickFailure) return route.fulfill({ status: 400, json: { error: { message: "request validation failed", fields: { quickTunnel: options.quickFailure } } } });
      const id = body.id ?? (quickSaves.length === 1 ? QUICK_ID : SECOND_QUICK_ID);
      const entry: QuickEntry = { id, name: body.name, target: body.target };
      const saved: PipeConfig = snapshot.config ?? structuredClone(initial.config);
      snapshot.config = { ...saved, quickTunnel: id === LEGACY_QUICK_ID ? null : saved.quickTunnel, quickTunnels: [...(saved.quickTunnels ?? []).filter(value => value.id !== id), entry] };
      const existing = snapshot.quickTunnels?.find(value => value.id === id);
      snapshot.quickTunnels = [...(snapshot.quickTunnels ?? []).filter(value => value.id !== id), { ...entry, unit: { unit: `firecrab-quick-tunnel-${id.replaceAll("-", "")}.service`, loadState: "loaded", activeState: "active", subState: "running", mainPid: existing?.unit.mainPid || 303 }, publicUrl: options.quickUrlPending ? null : id === LEGACY_QUICK_ID && saved.quickTunnel ? "https://migrated-tunnel.trycloudflare.com" : existing?.publicUrl ?? (id === QUICK_ID ? QUICK_URL : SECOND_QUICK_URL) }];
      snapshot.cloudflaredAvailable = true;
      if (options.holdConnection) await connectionGate;
      return route.fulfill({ json: snapshot });
    }
    const quickOperation = path.match(/^\/api\/pipe\/quick-tunnels\/([^/]+)(\/service)?$/);
    if (quickOperation) {
      const id = quickOperation[1];
      const entry = snapshot.quickTunnels?.find(value => value.id === id);
      if (!entry) return route.fulfill({ status: 404, json: { error: { message: "Tunnel not found" } } });
      if (route.request().method() === "DELETE") {
        quickDeletes.push(id);
        if (options.quickDeleteFailure) return route.fulfill({ status: 500, json: { error: { message: options.quickDeleteFailure } } });
        snapshot.quickTunnels = snapshot.quickTunnels!.filter(value => value.id !== id);
        snapshot.config = { ...snapshot.config!, quickTunnel: id === LEGACY_QUICK_ID ? null : snapshot.config?.quickTunnel, quickTunnels: (snapshot.config?.quickTunnels ?? []).filter(value => value.id !== id) };
      } else if (quickOperation[2]) {
        const { action } = route.request().postDataJSON() as { action: ServiceAction };
        quickActions.push({ id, action });
        if (options.serviceFailure) return route.fulfill({ status: 500, json: { error: { message: options.serviceFailure } } });
        entry.unit.activeState = action === "stop" ? "inactive" : "active";
        entry.unit.subState = action === "stop" ? "dead" : "running";
        entry.unit.mainPid = action === "stop" ? 0 : 400 + ++quickGeneration;
        if (action !== "stop") entry.publicUrl = `https://restarted-${quickGeneration}.trycloudflare.com`;
      }
      return route.fulfill({ json: snapshot });
    }
    if (path === "/api/vms") return vmsUnavailable ? route.fulfill({ status: 503, json: { error: { message: "VM list unavailable" } } }) : route.fulfill({ json: vms });
    if (path === "/api/update") return route.fulfill({ json: { current: "0.3.1", latest: "0.3.1", updateAvailable: false } });
    if (path === "/api/pipe/service") {
      const action = route.request().postDataJSON() as { service: Service; action: ServiceAction };
      actions.push(action);
      if (options.serviceFailure) return route.fulfill({ status: 500, json: { error: { message: options.serviceFailure } } });
      const unit = snapshot.units[action.service === "ingress" ? 0 : 1];
      unit.activeState = action.action === "stop" ? "inactive" : "active";
      unit.subState = action.action === "stop" ? "dead" : "running";
      unit.mainPid = action.action === "stop" ? 0 : unit.mainPid || 303;
      if (action.service === "tunnel" && action.action !== "stop" && snapshot.units[0].activeState !== "active") snapshot.units[0] = { ...snapshot.units[0], activeState: "active", subState: "running", mainPid: 404 };
      return route.fulfill({ json: snapshot });
    }
    if (path === "/api/pipe") {
      if (unavailable) return route.fulfill({ status: 503, json: { error: { message: "Pipe helper unavailable" } } });
      if (route.request().method() === "PUT") {
        const body = route.request().postDataJSON(); writes.push(body);
        const missingHost = snapshot.quickTunnels?.find(value => value.target.type === "routes" && !body.config.routes.some((existing: { host: string }) => existing.host === (value.target as { host: string }).host));
        if (missingHost) return route.fulfill({ status: 400, json: { error: { message: "request validation failed", fields: { config: "This domain is used by a Quick Tunnel. Update or delete the tunnel first." } } } });
        if (options.reject) return route.fulfill({ status: 400, json: { error: { message: "request validation failed", fields: { config: "invalid app address" } } } });
        snapshot = { ...snapshot, config: body.config };
      }
      return route.fulfill({ json: snapshot });
    }
    return route.fulfill({ status: 404 });
  });
  await page.addInitScript(locale => localStorage.setItem("firecrab.locale", locale), options.locale ?? "en");
  await page.goto("/#/pipe");
  return { get snapshot() { return snapshot; }, firecrabSaves, firecrabDeletes, namedConnections, zoneRequests, get namedDisconnects() { return namedDisconnects; }, writes, actions, quickSaves, quickDeletes, quickActions, finishConnection, unavailable: () => { unavailable = true; }, setVms: (next: VmResponse[]) => { vms = next; }, failVms: () => { vmsUnavailable = true; }, publishQuickUrl: (id = QUICK_ID) => { snapshot.quickTunnels!.find(value => value.id === id)!.publicUrl = QUICK_URL; }, failTunnel: (id = QUICK_ID) => { const unit = snapshot.quickTunnels!.find(value => value.id === id)!.unit; unit.activeState = "failed"; unit.subState = "failed"; unit.mainPid = 0; } };
}

test.describe("Pipe dashboard @dashboard", () => {
  test.describe.configure({ timeout: 45_000 });
  test("shows provisioning errors on both tunnels and allows stopping automatic retries", async ({ page }) => {
    await page.clock.install();
    const mock = await open(page, { quickTunnels: [webTunnel] });
    const failure = "Cloudflare rate limited URL creation (HTTP 429). Automatic retries use increasing delays; wait before restarting.";
    const tunnel = mock.snapshot.quickTunnels![0];
    tunnel.publicUrl = null;
    tunnel.error = failure;
    tunnel.unit.activeState = "activating";
    tunnel.unit.subState = "auto-restart";
    tunnel.unit.mainPid = 0;
    mock.snapshot.firecrabTunnels = [{
      subdomain: "my-app", publicUrl: "https://my-app.firecrab.dev", target: { type: "apps", upstreams: ["172.30.36.2:80"] },
      connectorId: tunnel.id, connected: false, error: failure,
    }];
    await page.clock.runFor(8_100);
    const quick = page.getByRole("region", { name: "Quick Tunnel", exact: true });
    const fixed = page.getByRole("region", { name: "Firecrab Tunnel", exact: true });
    await expect(quick.getByText(failure, { exact: true })).toBeVisible();
    await expect(quick.getByText("Preparing URL…", { exact: true })).toHaveCount(0);
    await expect(fixed.getByText(failure, { exact: true })).toBeVisible();
    await expect(fixed.getByText("Pending", { exact: true })).toBeVisible();
    await quick.getByRole("button", { name: webTunnel.name + " actions" }).click();
    await expect(page.getByRole("menuitem", { name: "Stop", exact: true })).toBeEnabled();
    await page.getByRole("menuitem", { name: "Stop", exact: true }).click();
    expect(mock.quickActions).toEqual([{ id: webTunnel.id, action: "stop" }]);

    tunnel.error = null;
    tunnel.unit.activeState = "active";
    tunnel.unit.subState = "running";
    tunnel.unit.mainPid = 402;
    tunnel.publicUrl = QUICK_URL;
    mock.snapshot.firecrabTunnels[0].connected = true;
    mock.snapshot.firecrabTunnels[0].error = null;
    await page.clock.runFor(8_100);
    await expect(quick.getByRole("link", { name: QUICK_URL, exact: true })).toBeVisible();
    await expect(fixed.getByText("Connected", { exact: true })).toBeVisible();
    await expect(page.getByText(failure, { exact: true })).toHaveCount(0);
  });

  test("connects a dashboard token to VM apps and retargets it to Nginx", async ({ page }, info) => {
    const mock = await open(page, { quickTunnels: [webTunnel], savedRoutes: groupedRoutes });
    const panel = page.getByRole("region", { name: "Firecrab Tunnel", exact: true });
    await panel.getByRole("button", { name: "Add fixed address" }).click();
    await expect(page.getByRole("region", { name: "Quick Tunnel", exact: true })).toBeVisible();
    await expect(panel.getByLabel("Quick Tunnel", { exact: true })).toHaveCount(0);
    await expect(panel.getByLabel("Subdomain", { exact: true })).toHaveCount(0);
    await expect(panel.getByRole("link", { name: "Get a token from the dashboard", exact: true })).toHaveAttribute("href", "https://firecrab.dev/dashboard/");
    await expect(panel.getByLabel("Connection token", { exact: true })).toHaveAttribute("type", "password");
    await panel.getByLabel("Connection token", { exact: true }).fill(CONNECTION_TOKEN);
    await expect(panel.getByLabel("Connection target").locator("option")).toHaveText(["VM apps", "Nginx routes"]);
    const apps = panel.getByRole("group", { name: "Public apps", exact: true });
    await apps.getByRole("checkbox", { name: "web-a · TCP 80", exact: true }).check();
    await apps.getByRole("checkbox", { name: "web-b · TCP 80", exact: true }).check();
    await panel.getByRole("button", { name: "Connect address", exact: true }).click();
    const url = "https://my-app.firecrab.dev";
    await expect(panel.getByRole("link", { name: url, exact: true })).toBeVisible();
    await expect(panel.getByText("Connected", { exact: true })).toBeVisible();
    await panel.getByRole("button", { name: "my-app actions", exact: true }).click();
    await page.getByRole("menuitem", { name: "Edit", exact: true }).click();
    await expect(panel.getByLabel("Connection token", { exact: true })).toHaveCount(0);
    await panel.getByLabel("Connection target", { exact: true }).selectOption("nginx");
    await panel.getByLabel("Nginx route domain", { exact: true }).selectOption("app.example.com");
    await panel.getByRole("button", { name: "Connect address", exact: true }).click();
    expect(mock.firecrabSaves).toEqual([{ token: CONNECTION_TOKEN, target: { type: "apps", upstreams: ["172.30.36.2:80", "172.30.36.3:80"] } }, { subdomain: "my-app", target: { type: "nginx", host: "app.example.com" } }]);
    expect(mock.writes).toHaveLength(0);
    expect(mock.quickSaves).toHaveLength(0);
    await page.reload();
    await expect(panel.getByRole("link", { name: url, exact: true })).toBeVisible();
    await expect(panel.getByText("app.example.com", { exact: true })).toBeVisible();
    await expect(panel.getByLabel(/token|server URL/i)).toHaveCount(0);
    await page.screenshot({ path: info.outputPath("firecrab-address-desktop.png"), fullPage: true });
    await page.setViewportSize({ width: 390, height: 844 });
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    await page.screenshot({ path: info.outputPath("firecrab-address-mobile.png"), fullPage: true });
    await panel.getByRole("button", { name: "my-app actions", exact: true }).click();
    await page.getByRole("menuitem", { name: "Delete", exact: true }).click();
    await expect(panel.getByRole("link", { name: url, exact: true })).toHaveCount(0);
    expect(mock.firecrabDeletes).toEqual(["my-app"]);
    await expectExistingQuickTunnel(page, "Public web");
  });

  test("connects saved Nginx routes without requiring a pre-existing Quick Tunnel", async ({ page }) => {
    const mock = await open(page, { savedRoutes: groupedRoutes, firecrabPending: true });
    const panel = page.getByRole("region", { name: "Firecrab Tunnel", exact: true });
    await panel.getByRole("button", { name: "Add fixed address" }).click();
    await panel.getByLabel("Connection token", { exact: true }).fill(CONNECTION_TOKEN);
    await panel.getByLabel("Connection target", { exact: true }).selectOption("nginx");
    await panel.getByLabel("Nginx route domain", { exact: true }).selectOption("app.example.com");
    await panel.getByRole("button", { name: "Connect address", exact: true }).click();
    await expect(panel.getByText("Pending", { exact: true })).toBeVisible();
    await expect(panel.getByText("Cannot reach the Firecrab connection service", { exact: true })).toBeVisible();
    await expect(panel.getByText("Connected", { exact: true })).toHaveCount(0);
    expect(mock.firecrabSaves).toEqual([{ token: CONNECTION_TOKEN, target: { type: "nginx", host: "app.example.com" } }]);
  });

  test("keeps the address form and unfinished routes when registration fails", async ({ page }) => {
    await open(page, { quickTunnels: [webTunnel], firecrabFailure: true });
    await page.getByRole("button", { name: "Add route", exact: true }).click();
    await page.getByLabel("Domain", { exact: true }).fill("draft.test");
    const panel = page.getByRole("region", { name: "Firecrab Tunnel", exact: true });
    await panel.getByRole("button", { name: "Add fixed address" }).click();
    await panel.getByLabel("Connection token", { exact: true }).fill(CONNECTION_TOKEN);
    await panel.getByRole("group", { name: "Public apps", exact: true }).getByRole("checkbox", { name: "web-a · TCP 80", exact: true }).check();
    await panel.getByRole("button", { name: "Connect address", exact: true }).click();
    await expect(panel.getByRole("alert")).toContainText("Connection token is invalid or revoked");
    await expect(panel.getByLabel("Connection token", { exact: true })).toHaveValue(CONNECTION_TOKEN);
    await expect(page.getByLabel("Domain", { exact: true })).toHaveValue("draft.test");
  });

  for (const kind of ["apps", "nginx", "legacy"] as const) test(`only warns about modified Firecrab ${kind} targets`, async ({ page }) => {
    await page.clock.install();
    const mock = await open(page, { savedRoutes: groupedRoutes, quickTunnels: [webTunnel] });
    mock.snapshot.firecrabTunnels = [{
      subdomain: "my-app", publicUrl: "https://my-app.firecrab.dev", connectorId: QUICK_ID, connected: true, error: null,
      target: kind === "nginx" ? { type: "nginx", host: "app.example.com" } : kind === "legacy" ? { type: "quick_tunnel", id: QUICK_ID } : { type: "apps", upstreams: ["172.30.36.2:80"] },
    }];
    await page.clock.runFor(8_100);
    const panel = page.getByRole("region", { name: "Firecrab Tunnel", exact: true });
    const edit = async () => {
      await panel.getByRole("button", { name: "my-app actions", exact: true }).click();
      await page.getByRole("menuitem", { name: "Edit", exact: true }).click();
    };
    const dialogs: string[] = [];
    page.on("dialog", async dialog => { dialogs.push(dialog.message()); await dialog.dismiss(); });
    await edit();
    await page.getByRole("button", { name: "MicroVM", exact: true }).click();
    await expect(page).toHaveURL(/#\/vms$/);
    expect(dialogs).toEqual([]);
    await page.getByRole("button", { name: "Pipe", exact: true }).click();
    await edit();
    if (kind === "nginx") await panel.getByLabel("Nginx route domain", { exact: true }).selectOption("other.example.com");
    else await panel.getByRole("checkbox", { name: "web-b · TCP 80", exact: true }).check();
    await page.getByRole("button", { name: "MicroVM", exact: true }).click();
    await expect(page).toHaveURL(/#\/pipe$/);
    expect(dialogs).toEqual(["Pipe changes have not been saved. Leave without saving?"]);
    if (kind === "nginx") await panel.getByLabel("Nginx route domain", { exact: true }).selectOption("app.example.com");
    else await panel.getByRole("checkbox", { name: "web-b · TCP 80", exact: true }).uncheck();
    await page.getByRole("button", { name: "MicroVM", exact: true }).click();
    await expect(page).toHaveURL(/#\/vms$/);
    expect(dialogs).toHaveLength(1);
    expect(mock.firecrabSaves).toEqual([]);
  });

  test("shows Firecrab field validation details through the common API error message", async ({ page }) => {
    await open(page);
    const reason = "The Firecrab subdomain is reserved";
    await page.route("**/api/pipe/firecrab-tunnels", route => route.fulfill({ status: 400, json: { error: { message: "request validation failed", fields: { firecrabTunnel: reason } } } }));
    const panel = page.getByRole("region", { name: "Firecrab Tunnel", exact: true });
    await panel.getByRole("button", { name: "Add fixed address" }).click();
    await panel.getByLabel("Connection token", { exact: true }).fill(CONNECTION_TOKEN);
    await panel.getByRole("checkbox", { name: "web-a · TCP 80", exact: true }).check();
    await panel.getByRole("button", { name: "Connect address", exact: true }).click();
    await expect(panel.getByRole("alert")).toHaveText(`request validation failed (firecrabTunnel: ${reason})`);
  });

  test("controls Nginx from its menu without applying configuration", async ({ page }) => {
    const mock = await open(page, { ingressState: "inactive" });
    const panel = page.getByRole("region", { name: "Pipe", exact: true });
    await expect(panel.getByRole("button", { name: "Save", exact: true })).toHaveCount(0);
    await expect(panel.getByRole("button", { name: "Stop", exact: true })).toHaveCount(0);
    let menu = await serviceMenu(page);
    await expect(menu.getByRole("menuitem", { name: "Stop", exact: true })).toBeDisabled();
    await menu.getByRole("menuitem", { name: "Start", exact: true }).click();
    await expect(page.getByText("Nginx start requested.", { exact: true })).toBeVisible();
    await expect(page.getByRole("menu")).toHaveCount(0);
    menu = await serviceMenu(page);
    await expect(menu.getByRole("menuitem", { name: "Start", exact: true })).toBeDisabled();
    await menu.getByRole("menuitem", { name: "Restart", exact: true }).click();
    await expect(page.getByText("Nginx restart requested.", { exact: true })).toBeVisible();
    await (await serviceMenu(page)).getByRole("menuitem", { name: "Stop", exact: true }).click();
    await expect(page.getByText("Nginx stop requested.", { exact: true })).toBeVisible();
    expect(mock.actions).toEqual([{ service: "ingress", action: "start" }, { service: "ingress", action: "restart" }, { service: "ingress", action: "stop" }]);
    await expect(panel.getByRole("button", { name: "Cloudflare Tunnel actions", exact: true })).toHaveCount(0);
    expect(mock.writes).toHaveLength(0);
  });

  test("controls a configured tunnel separately and retains unfinished routes", async ({ page }) => {
    const mock = await open(page, { configuredTunnel: true });
    await page.getByRole("button", { name: "Edit", exact: true }).click();
    await page.getByLabel("Domain", { exact: true }).fill("unfinished.example.com");
    await editNginx(page);
    await page.getByLabel("Listen address", { exact: true }).fill("127.0.0.1:8081");
    const panel = page.getByRole("region", { name: "Pipe", exact: true });
    await (await serviceMenu(page, "Cloudflare Tunnel")).getByRole("menuitem", { name: "Stop", exact: true }).click();
    await expect(page.getByText("Cloudflare Tunnel stop requested.", { exact: true })).toBeVisible();
    await expect(panel.getByRole("row").filter({ hasText: "Nginx" }).first().getByRole("cell", { name: "active", exact: true })).toBeVisible();
    await expect(panel.getByRole("row").filter({ hasText: "Cloudflare Tunnel" }).getByRole("cell", { name: "inactive", exact: true })).toBeVisible();
    await expect(page.getByLabel("Domain", { exact: true })).toHaveValue("unfinished.example.com");
    await expect(page.getByLabel("Listen address", { exact: true })).toHaveValue("127.0.0.1:8081");
    await expect(page.getByText("Unsaved changes", { exact: true })).toHaveCount(0);
    await (await serviceMenu(page, "Cloudflare Tunnel")).getByRole("menuitem", { name: "Start", exact: true }).click();
    await expect(page.getByText("Cloudflare Tunnel start requested.", { exact: true })).toBeVisible();
    expect(mock.actions).toEqual([{ service: "tunnel", action: "stop" }, { service: "tunnel", action: "start" }]);
    expect(mock.writes).toHaveLength(0);
  });

  test("keeps the running tunnel when stopping Nginx and starts ingress with the tunnel", async ({ page }) => {
    const mock = await open(page, { configuredTunnel: true });
    const panel = page.getByRole("region", { name: "Pipe", exact: true });
    await (await serviceMenu(page)).getByRole("menuitem", { name: "Stop", exact: true }).click();
    await expect(page.getByText("Nginx stop requested.", { exact: true })).toBeVisible();
    await expect(panel.getByRole("row").filter({ hasText: "Cloudflare Tunnel" }).getByRole("cell", { name: "active", exact: true })).toBeVisible();
    await (await serviceMenu(page, "Cloudflare Tunnel")).getByRole("menuitem", { name: "Stop", exact: true }).click();
    await expect(page.getByText("Cloudflare Tunnel stop requested.", { exact: true })).toBeVisible();
    await (await serviceMenu(page, "Cloudflare Tunnel")).getByRole("menuitem", { name: "Start", exact: true }).click();
    await expect(page.getByText("Cloudflare Tunnel start requested.", { exact: true })).toBeVisible();
    await expect(panel.getByRole("row").filter({ hasText: "Nginx" }).first().getByRole("cell", { name: "active", exact: true })).toBeVisible();
    expect(mock.actions).toHaveLength(3);
    expect(mock.writes).toHaveLength(0);
  });

  test("disables start and restart for an unconfigured service", async ({ page }) => {
    const mock = await open(page, { unconfiguredIngress: true, ingressState: "inactive" });
    for (const name of ["Nginx"]) {
      const menu = await serviceMenu(page, name);
      for (const action of ["Start", "Stop", "Restart"]) await expect(menu.getByRole("menuitem", { name: action, exact: true })).toBeDisabled();
      await page.keyboard.press("Escape");
    }
    expect(mock.actions).toHaveLength(0);
  });

  test("disables start and restart when an executable is unavailable", async ({ page }) => {
    const mock = await open(page, { configuredTunnel: true, missingNginx: true, missingConnector: true, ingressState: "inactive", tunnelState: "inactive" });
    for (const name of ["Nginx", "Cloudflare Tunnel"]) {
      const menu = await serviceMenu(page, name);
      await expect(menu.getByRole("menuitem", { name: "Start", exact: true })).toBeDisabled();
      await expect(menu.getByRole("menuitem", { name: "Restart", exact: true })).toBeDisabled();
      await page.keyboard.press("Escape");
    }
    expect(mock.actions).toHaveLength(0);
  });

  for (const state of ["activating", "deactivating", "reloading"]) test(`disables lifecycle actions while Nginx is ${state}`, async ({ page }) => {
    const mock = await open(page, { ingressState: state });
    await expect(page.getByRole("button", { name: "Nginx actions", exact: true })).toBeDisabled();
    await expect(page.getByRole("menu")).toHaveCount(0);
    expect(mock.actions).toHaveLength(0);
  });

  test("keeps draft and service status when a lifecycle request fails", async ({ page }) => {
    const mock = await open(page, { serviceFailure: "service control unavailable" });
    await page.getByRole("button", { name: "Edit", exact: true }).click();
    await page.getByLabel("Domain", { exact: true }).fill("unfinished.example.com");
    await (await serviceMenu(page)).getByRole("menuitem", { name: "Restart", exact: true }).click();
    await expect(page.getByRole("alert")).toContainText("service control unavailable");
    await expect(page.getByLabel("Domain", { exact: true })).toHaveValue("unfinished.example.com");
    await expect(page.getByRole("region", { name: "Pipe", exact: true }).getByRole("row").filter({ hasText: "Nginx" }).first().getByRole("cell", { name: "active", exact: true })).toBeVisible();
    expect(mock.actions).toEqual([{ service: "ingress", action: "restart" }]);
    expect(mock.writes).toHaveLength(0);
  });

  test("closes service menus on Escape and outside click without a request", async ({ page }) => {
    const mock = await open(page);
    await serviceMenu(page);
    await page.keyboard.press("Escape");
    await expect(page.getByRole("menu")).toHaveCount(0);
    await expect(page.getByRole("button", { name: "Nginx actions", exact: true })).toBeFocused();
    await serviceMenu(page);
    await page.getByRole("region", { name: "Routes", exact: true }).getByText("Routes", { exact: true }).click();
    await expect(page.getByRole("menu")).toHaveCount(0);
    expect(mock.actions).toHaveLength(0);
  });

  test("edits the Nginx listener with one Save and closes the editor", async ({ page }) => {
    const mock = await open(page);
    const panel = page.getByRole("region", { name: "Pipe", exact: true });
    await expect(panel.getByLabel("Listen address", { exact: true })).toHaveCount(0);
    await expect(panel.getByText("Listen address", { exact: true })).toHaveCount(0);
    await expect(panel.getByText("127.0.0.1:8080", { exact: true })).toHaveCount(0);
    await editNginx(page);
    const settings = panel.getByRole("form", { name: "Nginx settings", exact: true });
    const listen = settings.getByLabel("Listen address", { exact: true });
    await expect(settings).toBeVisible();
    await expect(listen).toBeFocused();
    await expect(listen).toHaveValue("127.0.0.1:8080");
    await listen.fill("127.0.0.1:8081");
    await expect(page.getByText("Unsaved changes", { exact: true })).toHaveCount(0);
    await settings.getByRole("button", { name: "Save", exact: true }).click();
    await expect.poll(() => mock.writes.length).toBe(1);
    expect(mock.writes[0].config.listen).toBe("127.0.0.1:8081");
    expect(mock.writes[0].config.routes).toEqual(initial.config.routes);
    await expect(settings).toHaveCount(0);
    await expect(listen).toHaveCount(0);
    await expect(panel.getByText("Listen address", { exact: true })).toHaveCount(0);
    await expect(panel.getByText("127.0.0.1:8081", { exact: true })).toHaveCount(0);
    await page.reload();
    await expect(panel.getByRole("button", { name: "Nginx actions", exact: true })).toBeEnabled();
    await expect(panel.getByText("Listen address", { exact: true })).toHaveCount(0);
    await expect(panel.getByText("127.0.0.1:8081", { exact: true })).toHaveCount(0);
    await editNginx(page);
    await expect(listen).toHaveValue("127.0.0.1:8081");
  });

  test("cancelling the listener keeps pending route removal", async ({ page }) => {
    const mock = await open(page);
    const panel = page.getByRole("region", { name: "Pipe", exact: true });
    const routes = page.getByRole("region", { name: "Routes", exact: true });
    await routes.getByRole("button", { name: "Remove", exact: true }).click();
    await editNginx(page);
    await panel.getByLabel("Listen address", { exact: true }).fill("127.0.0.1:8081");
    await panel.getByRole("button", { name: "Cancel", exact: true }).click();
    await expect(panel.getByText("Listen address", { exact: true })).toHaveCount(0);
    await expect(panel.getByText("127.0.0.1:8080", { exact: true })).toHaveCount(0);
    await expect(routes.getByRole("button", { name: "Remove", exact: true })).toHaveCount(0);
    await expect(page.getByText("Unsaved changes", { exact: true })).toHaveCount(0);
    expect(mock.writes).toHaveLength(0);
    await editNginx(page);
    await expect(panel.getByLabel("Listen address", { exact: true })).toHaveValue("127.0.0.1:8080");
    await panel.getByRole("button", { name: "Cancel", exact: true }).click();
    await expect(panel.getByLabel("Listen address", { exact: true })).toHaveCount(0);
    await routes.getByRole("button", { name: "Save", exact: true }).click();
    await expect.poll(() => mock.writes.length).toBe(1);
    expect(mock.writes[0].config).toMatchObject({ listen: "127.0.0.1:8080", routes: [] });
  });

  test("cancelling the listener keeps the open route editor", async ({ page }) => {
    const mock = await open(page);
    const panel = page.getByRole("region", { name: "Pipe", exact: true });
    await page.getByRole("button", { name: "Edit", exact: true }).click();
    await page.getByLabel("Domain", { exact: true }).fill("edited.example.com");
    await editNginx(page);
    await panel.getByLabel("Listen address", { exact: true }).fill("127.0.0.1:8081");
    await panel.getByRole("button", { name: "Cancel", exact: true }).click();
    await expect(page.getByLabel("Domain", { exact: true })).toHaveValue("edited.example.com");
    await expect(page.getByRole("group", { name: "Apps", exact: true }).getByRole("checkbox", { name: "web-a · TCP 80", exact: true })).toBeChecked();
    await page.getByRole("region", { name: "Routes", exact: true }).getByRole("button", { name: "Save", exact: true }).click();
    await expect.poll(() => mock.writes.length).toBe(1);
    expect(mock.writes[0].config.listen).toBe("127.0.0.1:8080");
    expect(mock.writes[0].config.routes[0].host).toBe("edited.example.com");
  });

  test("keeps listener and route inputs when saving fails", async ({ page }) => {
    const mock = await open(page, { reject: true });
    const panel = page.getByRole("region", { name: "Pipe", exact: true });
    await page.getByRole("button", { name: "Edit", exact: true }).click();
    await page.getByLabel("Domain", { exact: true }).fill("edited.example.com");
    await editNginx(page);
    await panel.getByLabel("Listen address", { exact: true }).fill("127.0.0.1:8081");
    await panel.getByRole("form", { name: "Nginx settings", exact: true }).getByRole("button", { name: "Save", exact: true }).click();
    await expect(page.getByRole("alert")).toContainText("invalid app address");
    await expect(panel.getByLabel("Listen address", { exact: true })).toHaveValue("127.0.0.1:8081");
    await expect(page.getByLabel("Domain", { exact: true })).toHaveValue("edited.example.com");
    await expect.poll(() => mock.writes.length).toBe(1);
    expect(mock.writes[0].config.listen).toBe("127.0.0.1:8081");
    expect(mock.writes[0].config.routes[0].host).toBe("edited.example.com");
  });

  test("opens route fields on demand and cancels without changing saved routes", async ({ page }) => {
    const mock = await open(page);
    const routes = page.getByRole("region", { name: "Routes", exact: true });
    const add = routes.getByRole("button", { name: "Add route", exact: true });
    await expect(page.getByLabel("Domain", { exact: true })).toHaveCount(0);
    await expect(page.getByLabel("Path prefix", { exact: true })).toHaveCount(0);
    await expect(page.getByRole("group", { name: "Apps", exact: true })).toHaveCount(0);
    await add.click();
    await expect(add).toHaveAttribute("aria-expanded", "true");
    await expect(page.getByLabel("Domain", { exact: true })).toBeFocused();
    await expect(page.getByLabel("Path prefix", { exact: true })).toHaveValue("/");
    await expect(page.getByRole("group", { name: "Apps", exact: true })).toBeVisible();
    await page.getByLabel("Domain", { exact: true }).fill("draft.example.com");
    await routes.getByRole("button", { name: "Cancel", exact: true }).click();
    await expect(page.getByLabel("Domain", { exact: true })).toHaveCount(0);
    await expect(add).toHaveAttribute("aria-expanded", "false");
    await expect(routes.getByRole("row").filter({ hasText: "app.example.com" })).toHaveCount(1);
    expect(mock.writes).toHaveLength(0);
    await add.click();
    await expect(page.getByLabel("Domain", { exact: true })).toHaveValue("");
    await expect(routes.getByRole("status")).toHaveCount(0);
    await expect(page.getByText("Unsaved changes", { exact: true })).toHaveCount(0);
    await expect(routes.getByRole("row").filter({ hasText: "app.example.com" })).toHaveCount(1);
    await expect(routes.getByRole("button", { name: "Save", exact: true })).toBeDisabled();
  });

  test("edits domain/path routes and applies multiple app addresses", async ({ page }, info) => {
    const mock = await open(page);
    await expect(page.getByRole("button", { name: "Pipe", exact: true })).toHaveAttribute("aria-current", "page");
    await expect(page.getByRole("region", { name: "Pipe", exact: true })).toContainText("active");
    await page.getByRole("button", { name: "Add route", exact: true }).click();
    await page.getByLabel("Domain", { exact: true }).fill("api.example.com");
    await page.getByLabel("Path prefix", { exact: true }).fill("/api");
    const apps = page.getByRole("group", { name: "Apps", exact: true });
    await apps.getByRole("checkbox", { name: "api · TCP 8080", exact: true }).check();
    await apps.getByRole("checkbox", { name: "api · TCP 9090", exact: true }).check();
    await apps.screenshot({ path: info.outputPath("pipe-app-picker-desktop.png") });
    await expect(page.getByText("Unsaved changes", { exact: true })).toHaveCount(0);
    await page.getByRole("region", { name: "Routes", exact: true }).getByRole("button", { name: "Save", exact: true }).click();
    await expect(page.getByText("Routes saved.", { exact: true })).toBeVisible();
    await expect(page.getByLabel("Domain", { exact: true })).toHaveCount(0);
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

  test("saves edited routes with one local Save action", async ({ page }) => {
    const mock = await open(page);
    await page.getByRole("button", { name: "Edit", exact: true }).click();
    await page.getByLabel("Domain", { exact: true }).fill("edited.example.com");
    await page.getByLabel("Path prefix", { exact: true }).fill("/api");
    await page.getByRole("group", { name: "Apps", exact: true }).getByRole("checkbox", { name: "web-b · TCP 80", exact: true }).uncheck();
    await page.getByRole("region", { name: "Routes", exact: true }).getByRole("button", { name: "Save", exact: true }).click();
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
    await page.getByRole("button", { name: "Add route", exact: true }).click();
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
    await page.getByRole("button", { name: "Add route", exact: true }).click();
    const apps = page.getByRole("group", { name: "Apps", exact: true });
    await expect(apps).toContainText(unavailable ? "VMs are unavailable." : "No VMs.");
    await expect(page.getByRole("region", { name: "Routes", exact: true }).getByRole("button", { name: "Save", exact: true })).toBeDisabled();
  });

  for (const change of ["stopped", "address", "unavailable"] as const) test(`rechecks app selection after the VM ${change} poll`, async ({ page }) => {
    await page.clock.install();
    const mock = await open(page);
    await page.getByRole("button", { name: "Add route", exact: true }).click();
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
    await expect(page.getByLabel("Domain", { exact: true })).toHaveCount(0);
    await expect(page.getByRole("region", { name: "Routes", exact: true }).getByRole("row").filter({ hasText: "renamed.example.com" })).toHaveCount(1);
    await expect.poll(() => mock.writes.length).toBe(1);
    await expect(page.getByText("Unsaved changes", { exact: true })).toHaveCount(0);
    expect(mock.writes[0].config.routes[0].upstreams).toEqual(saved);

    await page.getByRole("button", { name: "Edit", exact: true }).click();
    await legacy.click();
    await expect(legacy).toHaveCount(0);
    await page.getByRole("region", { name: "Routes", exact: true }).getByRole("button", { name: "Save", exact: true }).click();
    await expect(page.getByLabel("Domain", { exact: true })).toHaveCount(0);
    await expect(page.getByRole("region", { name: "Routes", exact: true }).getByText("192.0.2.10:8888", { exact: true })).toHaveCount(0);
    await expect.poll(() => mock.writes.length).toBe(2);
    await expect(page.getByText("Unsaved changes", { exact: true })).toHaveCount(0);
    expect(mock.writes[1].config.routes[0].upstreams).toEqual(["172.30.36.2:80"]);
  });

  test("keeps a removed route out of the draft across polling and cancelled refresh", async ({ page }, info) => {
    await page.clock.install();
    const mock = await open(page);
    const routes = page.getByRole("region", { name: "Routes", exact: true });
    await routes.getByRole("button", { name: "Remove", exact: true }).click();
    await expect(routes.getByRole("button", { name: "Remove", exact: true })).toHaveCount(0);
    await expect(routes.getByRole("status")).toHaveCount(0);
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
    await expect(routes.getByRole("status")).toHaveCount(0);
    await expect(routes.getByText("라우트가 없다. 도메인, 경로, 앱 주소를 추가한다.", { exact: true })).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    await routes.screenshot({ path: info.outputPath("pipe-unsaved-mobile-ko.png") });
    await routes.getByRole("button", { name: "저장", exact: true }).click();
    await expect(routes.getByRole("status")).toHaveCount(0);
    await expect(page.getByText("저장하지 않은 변경", { exact: true })).toHaveCount(0);
    await expect.poll(() => mock.writes.length).toBe(1);
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
    await page.getByRole("button", { name: "Add route", exact: true }).click();
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
    await expect(page.getByLabel("Domain", { exact: true })).toHaveCount(0);
    await page.getByRole("button", { name: "Add route", exact: true }).click();
    await expect(page.getByLabel("Domain", { exact: true })).toHaveValue("");
    const routes = page.getByRole("region", { name: "Routes", exact: true });
    await expect(routes.getByRole("status")).toHaveCount(0);
    await expect(page.getByText("Unsaved changes", { exact: true })).toHaveCount(0);
    await expect(routes.getByRole("row").filter({ hasText: "app.example.com" })).toHaveCount(1);
    await expect(routes.getByRole("button", { name: "Save", exact: true })).toBeDisabled();
  });

  test("disables route and Nginx saves when saved Quick Tunnels have no connector", async ({ page }) => {
    const mock = await open(page, { quickTunnels: [webTunnel], missingConnector: true });
    const routes = page.getByRole("region", { name: "Routes", exact: true });
    await routes.getByRole("button", { name: "Edit", exact: true }).click();
    await routes.getByLabel("Domain", { exact: true }).fill("edited.example.com");
    await expect(routes.getByRole("button", { name: "Save", exact: true })).toBeDisabled();
    await editNginx(page);
    const settings = page.getByRole("form", { name: "Nginx settings", exact: true });
    await settings.getByLabel("Listen address", { exact: true }).fill("127.0.0.1:8081");
    await expect(settings.getByRole("button", { name: "Save", exact: true })).toBeDisabled();
    expect(mock.writes).toHaveLength(0);
  });

  test("keeps Cloudflare Quick Tunnel controls independently of Firecrab app targets", async ({ page }) => {
    const mock = await open(page, { configuredLegacyQuick: true });
    await expect(page.getByRole("region", { name: "Quick Tunnel", exact: true }).getByRole("button", { name: "Add tunnel", exact: true })).toBeVisible();
    await expectExistingQuickTunnel(page, "Quick Tunnel");
    expect(mock.quickSaves).toHaveLength(0);
    expect(mock.quickDeletes).toHaveLength(0);
  });

  test("keeps referenced route protection with independent Quick Tunnel controls", async ({ page }) => {
    const routed: QuickEntry = { id: QUICK_ID, name: "Routed web", target: { type: "routes", host: "app.example.com" } };
    const mock = await open(page, { quickTunnels: [routed] });
    const routes = page.getByRole("region", { name: "Routes", exact: true });
    await routes.getByRole("button", { name: "Remove", exact: true }).click();
    await routes.getByRole("button", { name: "Save", exact: true }).click();
    await expect(page.getByRole("alert")).toContainText("This domain is used by a Quick Tunnel");
    expect(mock.quickDeletes).toHaveLength(0);
    expect(mock.writes).toHaveLength(1);
  });

  test("requires a token and offers apps and Nginx without saved Quick Tunnels", async ({ page }) => {
    await open(page, { savedRoutes: groupedRoutes });
    const panel = page.getByRole("region", { name: "Firecrab Tunnel", exact: true });
    await panel.getByRole("button", { name: "Add fixed address" }).click();
    await expect(panel.getByLabel("Connection target").locator("option")).toHaveText(["VM apps", "Nginx routes"]);
    await expect(panel.getByRole("option", { name: "Quick Tunnel", exact: true })).toHaveCount(0);
    await expect(panel.getByLabel("Quick Tunnel", { exact: true })).toHaveCount(0);
    await panel.getByLabel("Connection target").selectOption("nginx");
    await panel.getByLabel("Nginx route domain").selectOption("app.example.com");
    await expect(panel.getByRole("button", { name: "Connect address" })).toBeDisabled();
    await panel.getByLabel("Connection token").fill("invalid");
    await expect(panel.getByRole("button", { name: "Connect address" })).toBeDisabled();
  });

});

test.describe("Named Cloudflare Tunnel @dashboard", () => {
  const token = "test-api-token-01234567890123456789";
  async function configure(page: Page) {
    const panel = page.getByRole("region", { name: "Cloudflare Tunnel", exact: true });
    await expect(panel.getByLabel("Cloudflare API token", { exact: true })).toBeVisible();
    await panel.getByLabel("Cloudflare API token", { exact: true }).fill(token);
    await panel.getByRole("button", { name: "Load domains", exact: true }).click();
    await panel.getByLabel("Route domain", { exact: true }).selectOption("app.example.com");
    return panel;
  }
  test("connects a stable domain alongside Quick Tunnels and copies it with an icon", async ({ page }, info) => {
    const mock = await open(page, { quickTunnels: [webTunnel], savedRoutes: groupedRoutes });
    const panel = await configure(page);
    await expect(panel).toContainText("/, /api");
    await panel.screenshot({ path: info.outputPath("named-tunnel-form-desktop.png") });
    await panel.getByRole("button", { name: "Connect and publish", exact: true }).click();
    await expect(panel.getByRole("link", { name: "https://app.example.com", exact: true })).toBeVisible();
    await expect(panel.getByLabel("Cloudflare API token", { exact: true })).toHaveCount(0);
    expect(mock.zoneRequests).toEqual([token]);
    expect(mock.namedConnections[0]).toMatchObject({ apiToken: token, hostname: "app.example.com", config: { quickTunnels: [webTunnel], routes: groupedRoutes } });
    await expectExistingQuickTunnel(page, "Public web");
    await page.context().grantPermissions(["clipboard-read", "clipboard-write"]);
    const copy = panel.getByRole("button", { name: "Copy URL", exact: true });
    await expect(copy).toHaveText("");
    await copy.click();
    await expect(copy).toHaveAttribute("title", "Copied");
    await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe("https://app.example.com");
    await page.reload();
    await expect(panel.getByRole("link", { name: "https://app.example.com", exact: true })).toBeVisible();
    await (await serviceMenu(page, "Cloudflare Tunnel")).getByRole("menuitem", { name: "Restart", exact: true }).click();
    await expect(panel.getByRole("link", { name: "https://app.example.com", exact: true })).toBeVisible();
    await panel.getByRole("button", { name: "Disconnect", exact: true }).click();
    await expect(panel.getByLabel("Cloudflare API token", { exact: true })).toHaveValue("");
    expect(mock.namedDisconnects).toBe(1);
    await expectExistingQuickTunnel(page, "Public web");
    expect(mock.writes).toHaveLength(0);
  });
  test("keeps route and listener drafts during named setup and disconnect without an unsaved notice", async ({ page }) => {
    const mock = await open(page, { savedRoutes: groupedRoutes });
    const routes = page.getByRole("region", { name: "Routes", exact: true });
    await routes.getByRole("row").filter({ hasText: "other.example.com" }).getByRole("button", { name: "Remove", exact: true }).click();
    await editNginx(page);
    await page.getByLabel("Listen address", { exact: true }).fill("127.0.0.1:8081");
    const panel = await configure(page);
    await panel.getByRole("button", { name: "Connect and publish", exact: true }).click();
    await expect(panel.getByRole("button", { name: "Disconnect", exact: true })).toBeVisible();
    expect(mock.namedConnections[0].config.listen).toBe("127.0.0.1:8080");
    expect(mock.namedConnections[0].config.routes).toEqual(groupedRoutes);
    await panel.getByRole("button", { name: "Disconnect", exact: true }).click();
    await expect(page.getByLabel("Listen address", { exact: true })).toHaveValue("127.0.0.1:8081");
    await expect(routes.getByRole("row").filter({ hasText: "other.example.com" })).toHaveCount(0);
    await expect(routes.getByRole("button", { name: "Save", exact: true })).toBeEnabled();
    await expect(page.getByText(/unsaved changes/i)).toHaveCount(0);
    expect(mock.writes).toHaveLength(0);
  });
  test("clears a rejected API token without hiding the setup form", async ({ page }) => {
    const mock = await open(page, { configuredLegacyQuick: true, zonesFailure: true });
    const panel = page.getByRole("region", { name: "Cloudflare Tunnel", exact: true });
    const input = panel.getByLabel("Cloudflare API token", { exact: true });
    await input.fill(token);
    await panel.getByRole("button", { name: "Load domains", exact: true }).click();
    await expect(panel.getByRole("alert")).toContainText("Cloudflare rejected the API token");
    await expect(input).toHaveValue(token);
    await panel.getByRole("button", { name: "Clear input", exact: true }).click();
    await expect(input).toBeVisible();
    await expect(input).toHaveValue("");
    await expect(panel.getByRole("alert")).toHaveCount(0);
    await expect(panel.getByRole("button", { name: "Load domains", exact: true })).toBeDisabled();
    expect(mock.zoneRequests).toEqual([token]);
    expect(mock.namedConnections).toHaveLength(0);
    expect(mock.quickSaves).toHaveLength(0);
  });
  test("retains token and route choices when named setup fails", async ({ page }) => {
    const mock = await open(page, { namedFailure: "Cloudflare rejected the API token" });
    const panel = await configure(page);
    await panel.getByRole("button", { name: "Connect and publish", exact: true }).click();
    await expect(panel.getByRole("alert")).toContainText("Cloudflare rejected");
    await expect(panel.getByLabel("Cloudflare API token", { exact: true })).toHaveValue(token);
    await expect(panel.getByLabel("Route domain", { exact: true })).toHaveValue("app.example.com");
    await expect(panel.getByRole("button", { name: "Connect and publish", exact: true })).toBeEnabled();
    expect(mock.namedConnections).toHaveLength(1);
  });
  test("provides token instructions and route selection without mobile overflow", async ({ page }, info) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await open(page, { locale: "ko", quickTunnels: [webTunnel], savedRoutes: groupedRoutes });
    const panel = page.getByRole("region", { name: "Cloudflare Tunnel", exact: true });
    await expect(panel.getByLabel("Cloudflare API token", { exact: true })).toBeVisible();
    await panel.getByText("API 토큰 발급 방법", { exact: true }).click();
    await expect(panel).toContainText("Cloudflare Tunnel / Edit");
    await panel.getByLabel("Cloudflare API token", { exact: true }).fill(token);
    await panel.getByRole("button", { name: "도메인 불러오기", exact: true }).click();
    await panel.getByLabel("라우트 도메인", { exact: true }).selectOption("app.example.com");
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    await panel.screenshot({ path: info.outputPath("named-tunnel-form-mobile-ko.png") });
    await expect(page.getByText("저장하지 않은 변경", { exact: true })).toHaveCount(0);
    await expect(page.getByRole("region", { name: "Quick Tunnel", exact: true })).toBeVisible();
  });
});


test("preserves an existing legacy connector in both tunnel panels @dashboard", async ({ page }) => {
  const mock = await open(page, { configuredLegacyQuick: true, savedRoutes: groupedRoutes });
  await expectExistingQuickTunnel(page, "Quick Tunnel");
  await expect(page.getByRole("region", { name: "Quick Tunnel", exact: true })).toBeVisible();
  expect(mock.quickSaves).toHaveLength(0);
  expect(mock.quickDeletes).toHaveLength(0);
});

test.describe("Public address → Nginx → VM apps @dashboard", () => {
  const hostname = "my-app.firecrab.dev";
  const publicRoutes = [
    { host: hostname, pathPrefix: "/", upstreams: ["172.30.36.2:80", "172.30.36.3:80"] },
    { host: hostname, pathPrefix: "/api", upstreams: ["172.30.36.4:8080"] },
  ];
  function recordMutations(page: Page) {
    const requests: string[] = [];
    page.on("request", request => {
      if (request.method() !== "GET" && new URL(request.url()).pathname.startsWith("/api/")) {
        requests.push(request.method() + " " + new URL(request.url()).pathname);
      }
    });
    return requests;
  }
  test("saves a real domain as an Nginx route without a registration server or Firecrab token", async ({ page }, info) => {
    const mutations = recordMutations(page);
    const mock = await open(page, { savedRoutes: [], quickTunnels: [webTunnel] });
    await expect(page.getByRole("region", { name: "Firecrab Tunnel", exact: true })).toBeVisible();
    await expect(page.getByLabel("Registration server URL", { exact: true })).toHaveCount(0);
    await expect(page.getByLabel("Firecrab connection token", { exact: true })).toHaveCount(0);
    const routes = page.getByRole("region", { name: "Routes", exact: true });
    await routes.getByRole("button", { name: "Add route", exact: true }).click();
    await routes.getByLabel("Domain", { exact: true }).fill(hostname);
    await routes.getByRole("checkbox", { name: "web-a · TCP 80", exact: true }).check();
    await routes.getByRole("checkbox", { name: "web-b · TCP 80", exact: true }).check();
    await routes.screenshot({ path: info.outputPath("public-domain-route-desktop.png") });
    await routes.getByRole("button", { name: "Save", exact: true }).click();
    await expect.poll(() => mock.writes.length).toBe(1);
    expect(mock.writes[0].config.routes).toEqual([publicRoutes[0]]);
    expect(mock.writes[0].config.quickTunnels).toEqual([webTunnel]);
    expect(mock.zoneRequests).toHaveLength(0);
    expect(mock.namedConnections).toHaveLength(0);
    expect(mutations).toEqual(["PUT /api/pipe"]);
    await page.reload();
    await expect(routes.getByRole("cell", { name: hostname, exact: true })).toBeVisible();
  });
  test("connects saved public-domain routes using a dashboard token", async ({ page }) => {
    const mutations = recordMutations(page);
    const mock = await open(page, { savedRoutes: publicRoutes });
    const panel = page.getByRole("region", { name: "Firecrab Tunnel", exact: true });
    await panel.getByRole("button", { name: "Add fixed address" }).click();
    await panel.getByLabel("Connection token").fill(CONNECTION_TOKEN);
    await panel.getByLabel("Connection target").selectOption("nginx");
    await panel.getByLabel("Nginx route domain").selectOption(hostname);
    await panel.getByRole("button", { name: "Connect address" }).click();
    expect(mock.firecrabSaves).toEqual([{ token: CONNECTION_TOKEN, target: { type: "nginx", host: hostname } }]);
    expect(mock.quickSaves).toHaveLength(0);
    expect(mock.writes).toHaveLength(0);
    expect(mutations).toEqual(["POST /api/pipe/firecrab-tunnels"]);
  });
  test("connects the real public hostname using Cloudflare credentials only", async ({ page }) => {
    const mutations = recordMutations(page);
    const mock = await open(page, { savedRoutes: publicRoutes, zoneName: "firecrab.dev" });
    const named = page.getByRole("region", { name: "Cloudflare Tunnel", exact: true });
    await named.getByLabel("Cloudflare API token", { exact: true }).fill("test-cloudflare-api-token-0123456789");
    await named.getByRole("button", { name: "Load domains", exact: true }).click();
    await named.getByLabel("Route domain", { exact: true }).selectOption(hostname);
    await expect(named).toContainText("/, /api");
    await named.getByRole("button", { name: "Connect and publish", exact: true }).click();
    await expect(named.getByRole("link", { name: "https://" + hostname, exact: true })).toBeVisible();
    expect(mock.namedConnections[0]).toMatchObject({ hostname, config: { routes: publicRoutes } });
    expect(mutations).toEqual(["POST /api/pipe/cloudflare/zones", "POST /api/pipe/cloudflare/connect"]);
  });
  test("keeps public-domain setup usable on Korean mobile screens", async ({ page }, info) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await open(page, { locale: "ko", savedRoutes: publicRoutes });
    const panel = page.getByRole("region", { name: "Firecrab Tunnel", exact: true });
    await panel.getByRole("button", { name: "고정 주소 추가", exact: true }).click();
    await panel.getByLabel("연결 토큰", { exact: true }).fill(CONNECTION_TOKEN);
    await panel.getByLabel("연결 대상", { exact: true }).selectOption("nginx");
    await panel.getByLabel("Nginx 라우트 도메인", { exact: true }).selectOption(hostname);
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    await panel.screenshot({ path: info.outputPath("public-nginx-token-mobile-ko.png") });
    await expect(page.getByLabel("서브도메인", { exact: true })).toHaveCount(0);
  });
});
