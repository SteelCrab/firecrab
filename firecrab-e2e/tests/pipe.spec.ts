import { installWasmClock } from "./support/wasmClock.js";
import { expect, test, type Page } from "@playwright/test";
import type { PipeConfig, PipeSnapshot } from "../types/api/pipe";
import type { VmResponse } from "../types/bindings/VmResponse";
import type { PipeCardId } from "../types/navigation";

function appVm(
  name: string,
  ipv4: string | null,
  ports: number[],
  state: VmResponse["state"] = "running",
): VmResponse {
  return {
    id: name,
    name,
    state,
    template: "test-http-app",
    templateVersion: "latest",
    cpu: 1,
    ram: 256,
    diskGb: 1,
    startupStep: null,
    startupTimeline: [],
    egressPolicy: "internet",
    ipv4,
    ipv6: null,
    mac: null,
    hostname: name,
    microNetworkId: "test-network",
    storageRoot: "default",
    cpuUsagePercent: null,
    memoryUsedMib: null,
    memoryTotalMib: null,
    memoryUsedPercent: null,
    usageHistory: [],
    portForwards: ports.map((port, index) => ({
      hostPort: 18081 + index,
      guestPort: port,
      protocol: "tcp",
    })),
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
appVms[2].portForwards!.push({
  hostPort: 18053,
  guestPort: 53,
  protocol: "udp",
});

const initial = {
  config: {
    listen: "127.0.0.1:8080",
    routes: [
      {
        host: "app.example.com",
        pathPrefix: "/",
        upstreams: ["172.30.36.2:80", "172.30.36.3:80"],
      },
    ],
    tunnel: null,
  },
  units: [
    {
      unit: "firecrab-ingress.service",
      loadState: "loaded",
      activeState: "active",
      subState: "running",
      mainPid: 101,
    },
    {
      unit: "firecrab-tunnel.service",
      loadState: "not-found",
      activeState: "inactive",
      subState: "dead",
      mainPid: 0,
    },
  ],
  nginxAvailable: true,
  cloudflaredAvailable: true,
} satisfies PipeSnapshot;

type Service = "ingress" | "tunnel";
type ServiceAction = "start" | "stop" | "restart";

async function serviceMenu(page: Page, name = "Nginx") {
  const region = name === "Nginx" ? "Route" : "Cloudflare Named Tunnel";
  await page
    .getByRole("region", { name: region, exact: true })
    .getByRole("button", { name: `${name} actions`, exact: true })
    .click();
  return page.getByRole("menu");
}

/** Return to the card list and open another Pipe service, as a user would. */
async function switchCard(page: Page, title: string) {
  await page
    .getByRole("navigation", { name: "Pipe navigation", exact: true })
    .getByRole("link", { name: "Pipe", exact: true })
    .click();
  await page
    .getByRole("region", { name: "Pipe", exact: true })
    .getByRole("link", { name: title, exact: true })
    .click();
}

async function editNginx(page: Page) {
  await (
    await serviceMenu(page)
  )
    .getByRole("menuitem", { name: "Edit", exact: true })
    .click();
}

const QUICK_ID = "11111111-1111-4111-8111-111111111111";
const SECOND_QUICK_ID = "22222222-2222-4222-8222-222222222222";
const LEGACY_QUICK_ID = "00000000-0000-0000-0000-000000000001";
const QUICK_URL = "https://first-tunnel.trycloudflare.com";
const SECOND_QUICK_URL = "https://second-tunnel.trycloudflare.com";
type QuickEntry = NonNullable<PipeConfig["quickTunnels"]>[number];
type QuickSave = Omit<QuickEntry, "id"> & { id?: string | null };
const webTunnel: QuickEntry = {
  id: QUICK_ID,
  name: "Public web",
  target: { type: "apps", upstreams: ["172.30.36.2:80"] },
};
const apiTunnel: QuickEntry = {
  id: SECOND_QUICK_ID,
  name: "Public API",
  target: { type: "apps", upstreams: ["172.30.36.4:8080"] },
};
const groupedRoutes = [
  ...initial.config.routes,
  {
    host: "app.example.com",
    pathPrefix: "/api",
    upstreams: ["172.30.36.4:8080"],
  },
  {
    host: "other.example.com",
    pathPrefix: "/",
    upstreams: ["172.30.36.4:9090"],
  },
  {
    host: "api-only.example.com",
    pathPrefix: "/api",
    upstreams: ["172.30.36.4:8080"],
  },
];

const CONNECTION_TOKEN = "a".repeat(64);
async function expectExistingQuickTunnel(page: Page, name: string) {
  const panel = page.getByRole("region", { name: "Quick Tunnel", exact: true });
  await expect(panel.getByRole("cell", { name, exact: true })).toBeVisible();
  await expect(
    panel.getByRole("button", { name: name + " actions", exact: true }),
  ).toBeVisible();
}

async function open(
  page: Page,
  options: {
    card?: PipeCardId | null;
    unavailable?: boolean;
    firecrabFailure?: boolean;
    firecrabPending?: boolean;
    locale?: string;
    reject?: boolean;
    quickFailure?: string;
    quickDeleteFailure?: string;
    namedFailure?: string;
    zonesFailure?: boolean;
    zoneName?: string;
    missingConnector?: boolean;
    missingNginx?: boolean;
    holdConnection?: boolean;
    quickUrlPending?: boolean;
    vms?: VmResponse[];
    savedAddresses?: string[];
    savedRoutes?: PipeConfig["routes"];
    vmsUnavailable?: boolean;
    configuredTunnel?: boolean;
    configuredLegacyQuick?: boolean;
    quickTunnels?: QuickEntry[];
    ingressState?: string;
    tunnelState?: string;
    serviceFailure?: string;
    unconfiguredIngress?: boolean;
  } = {},
) {
  let snapshot: PipeSnapshot = structuredClone(initial);
  if (options.savedRoutes)
    snapshot.config!.routes = structuredClone(options.savedRoutes);
  if (options.unconfiguredIngress) snapshot.config = null;
  if (options.configuredTunnel)
    snapshot.config!.tunnel = {
      id: QUICK_ID,
      credentialsFile: "/etc/firecrab/pipe/secrets/tunnel-credentials.json",
    };
  snapshot.quickTunnels = (options.quickTunnels ?? []).map((entry, index) => ({
    ...structuredClone(entry),
    unit: {
      unit: `firecrab-quick-tunnel-${entry.id.replaceAll("-", "")}.service`,
      loadState: "loaded",
      activeState: "active",
      subState: "running",
      mainPid: 301 + index,
    },
    publicUrl: index === 0 ? QUICK_URL : SECOND_QUICK_URL,
  }));
  if (options.quickTunnels)
    snapshot.config!.quickTunnels = structuredClone(options.quickTunnels);
  if (options.configuredLegacyQuick) {
    snapshot.config!.quickTunnel = { upstreams: ["172.30.36.2:80"] };
    snapshot.quickTunnel = { publicUrl: QUICK_URL };
    snapshot.quickTunnels.push({
      id: LEGACY_QUICK_ID,
      name: "Quick Tunnel",
      target: { type: "apps", upstreams: ["172.30.36.2:80"] },
      unit: {
        ...snapshot.units[1],
        loadState: "loaded",
        activeState: "active",
        subState: "running",
        mainPid: 202,
      },
      publicUrl: QUICK_URL,
    });
  }
  if (options.configuredTunnel || options.configuredLegacyQuick)
    snapshot.units[1] = {
      ...snapshot.units[1],
      loadState: "loaded",
      activeState: "active",
      subState: "running",
      mainPid: 202,
    };
  for (const [index, state] of [
    options.ingressState,
    options.tunnelState,
  ].entries()) {
    if (state)
      snapshot.units[index] = {
        ...snapshot.units[index],
        activeState: state,
        subState: state === "active" ? "running" : "dead",
        mainPid: state === "active" ? index + 101 : 0,
      };
  }
  snapshot.nginxAvailable = !options.missingNginx;
  snapshot.cloudflaredAvailable = !options.missingConnector;
  if (options.savedAddresses)
    snapshot.config!.routes[0].upstreams = options.savedAddresses;
  let vms = structuredClone(options.vms ?? appVms);
  let vmsUnavailable = options.vmsUnavailable ?? false;
  let unavailable = options.unavailable ?? false;
  const writes: { config: PipeConfig; credentials: unknown }[] = [];
  const actions: { service: Service; action: ServiceAction }[] = [];
  const namedConnections: {
    apiToken: string;
    zoneId: string;
    hostname: string;
    config: PipeConfig;
  }[] = [];
  const zoneRequests: string[] = [];
  let namedDisconnects = 0;
  const firecrabSaves: {
    subdomain?: string;
    token?: string;
    target: import("../types/api/pipe").FirecrabTarget;
  }[] = [];
  const firecrabDeletes: string[] = [];
  const quickSaves: QuickSave[] = [];
  const quickDeletes: string[] = [];
  const quickActions: { id: string; action: ServiceAction }[] = [];
  let finishConnection: () => void = () => {};
  let quickGeneration = 0;
  const connectionGate = new Promise<void>((resolve) => {
    finishConnection = resolve;
  });
  await page.route(/^https?:\/\/[^/]+\/api\//, async (route) => {
    const path = new URL(route.request().url()).pathname;
    if (path === "/api/pipe/databases") return route.fulfill({ json: [] });
    if (path === "/api/pipe/cloudflare/zones") {
      zoneRequests.push(route.request().postDataJSON().apiToken);
      if (options.zonesFailure)
        return route.fulfill({
          status: 400,
          json: { error: { message: "Cloudflare rejected the API token" } },
        });
      return route.fulfill({
        json: [
          {
            id: "01234567890123456789012345678901",
            name: options.zoneName ?? "example.com",
            status: "active",
            account: {
              id: "abcdefabcdefabcdefabcdefabcdefab",
              name: "Test account",
            },
          },
        ],
      });
    }
    if (path === "/api/pipe/cloudflare/connect") {
      const body = route.request().postDataJSON();
      namedConnections.push(body);
      if (options.namedFailure)
        return route.fulfill({
          status: 400,
          json: { error: { message: options.namedFailure } },
        });
      snapshot.config = {
        ...snapshot.config!,
        tunnel: {
          id: SECOND_QUICK_ID,
          credentialsFile: "/etc/firecrab/pipe/secrets/tunnel-credentials.json",
          hostname: body.hostname,
        },
      };
      snapshot.units[1] = {
        ...snapshot.units[1],
        loadState: "loaded",
        activeState: "active",
        subState: "running",
        mainPid: 202,
      };
      return route.fulfill({ json: snapshot });
    }
    if (path === "/api/pipe/cloudflare/disconnect") {
      namedDisconnects++;
      snapshot.config = { ...snapshot.config!, tunnel: null };
      snapshot.units[1] = {
        ...snapshot.units[1],
        loadState: "not-found",
        activeState: "inactive",
        subState: "dead",
        mainPid: 0,
      };
      return route.fulfill({ json: snapshot });
    }
    if (path === "/api/pipe/firecrab-tunnels") {
      const body = route.request().postDataJSON();
      firecrabSaves.push(body);
      if (options.firecrabFailure)
        return route.fulfill({
          status: 400,
          json: {
            error: { message: "Connection token is invalid or revoked" },
          },
        });
      const subdomain = body.subdomain ?? "my-app";
      snapshot.firecrabTunnels = [
        ...(snapshot.firecrabTunnels ?? []).filter(
          (a) => a.subdomain !== subdomain,
        ),
        {
          subdomain,
          target: body.target,
          publicUrl: `https://${subdomain}.firecrab.dev`,
          connectorId:
            body.target.type === "quick_tunnel"
              ? body.target.id
              : SECOND_QUICK_ID,
          connected: !options.firecrabPending,
          error: options.firecrabPending
            ? "Cannot reach the Firecrab connection service"
            : null,
        },
      ];
      return route.fulfill({ json: snapshot });
    }
    if (path.startsWith("/api/pipe/firecrab-tunnels/")) {
      const name = decodeURIComponent(path.split("/").at(-1)!);
      firecrabDeletes.push(name);
      snapshot.firecrabTunnels = snapshot.firecrabTunnels?.filter(
        (a) => a.subdomain !== name,
      );
      return route.fulfill({ json: snapshot });
    }
    if (path === "/api/pipe/quick-tunnels") {
      const body = route.request().postDataJSON() as QuickSave;
      quickSaves.push(body);
      if (options.quickFailure)
        return route.fulfill({
          status: 400,
          json: {
            error: {
              message: "request validation failed",
              fields: { quickTunnel: options.quickFailure },
            },
          },
        });
      const id =
        body.id ?? (quickSaves.length === 1 ? QUICK_ID : SECOND_QUICK_ID);
      const entry: QuickEntry = { id, name: body.name, target: body.target };
      const saved: PipeConfig =
        snapshot.config ?? structuredClone(initial.config);
      snapshot.config = {
        ...saved,
        quickTunnel: id === LEGACY_QUICK_ID ? null : saved.quickTunnel,
        quickTunnels: [
          ...(saved.quickTunnels ?? []).filter((value) => value.id !== id),
          entry,
        ],
      };
      const existing = snapshot.quickTunnels?.find((value) => value.id === id);
      snapshot.quickTunnels = [
        ...(snapshot.quickTunnels ?? []).filter((value) => value.id !== id),
        {
          ...entry,
          unit: {
            unit: `firecrab-quick-tunnel-${id.replaceAll("-", "")}.service`,
            loadState: "loaded",
            activeState: "active",
            subState: "running",
            mainPid: existing?.unit.mainPid || 303,
          },
          publicUrl: options.quickUrlPending
            ? null
            : id === LEGACY_QUICK_ID && saved.quickTunnel
              ? "https://migrated-tunnel.trycloudflare.com"
              : (existing?.publicUrl ??
                (id === QUICK_ID ? QUICK_URL : SECOND_QUICK_URL)),
        },
      ];
      snapshot.cloudflaredAvailable = true;
      if (options.holdConnection) await connectionGate;
      return route.fulfill({ json: snapshot });
    }
    const quickOperation = path.match(
      /^\/api\/pipe\/quick-tunnels\/([^/]+)(\/service)?$/,
    );
    if (quickOperation) {
      const id = quickOperation[1];
      const entry = snapshot.quickTunnels?.find((value) => value.id === id);
      if (!entry)
        return route.fulfill({
          status: 404,
          json: { error: { message: "Tunnel not found" } },
        });
      if (route.request().method() === "DELETE") {
        quickDeletes.push(id);
        if (options.quickDeleteFailure)
          return route.fulfill({
            status: 500,
            json: { error: { message: options.quickDeleteFailure } },
          });
        snapshot.quickTunnels = snapshot.quickTunnels!.filter(
          (value) => value.id !== id,
        );
        snapshot.config = {
          ...snapshot.config!,
          quickTunnel:
            id === LEGACY_QUICK_ID ? null : snapshot.config?.quickTunnel,
          quickTunnels: (snapshot.config?.quickTunnels ?? []).filter(
            (value) => value.id !== id,
          ),
        };
      } else if (quickOperation[2]) {
        const { action } = route.request().postDataJSON() as {
          action: ServiceAction;
        };
        quickActions.push({ id, action });
        if (options.serviceFailure)
          return route.fulfill({
            status: 500,
            json: { error: { message: options.serviceFailure } },
          });
        entry.unit.activeState = action === "stop" ? "inactive" : "active";
        entry.unit.subState = action === "stop" ? "dead" : "running";
        entry.unit.mainPid = action === "stop" ? 0 : 400 + ++quickGeneration;
        if (action !== "stop")
          entry.publicUrl = `https://restarted-${quickGeneration}.trycloudflare.com`;
      }
      return route.fulfill({ json: snapshot });
    }
    if (path === "/api/vms")
      return vmsUnavailable
        ? route.fulfill({
            status: 503,
            json: { error: { message: "VM list unavailable" } },
          })
        : route.fulfill({ json: vms });
    if (path === "/api/update")
      return route.fulfill({
        json: { current: "0.3.1", latest: "0.3.1", updateAvailable: false },
      });
    if (path === "/api/pipe/service") {
      const action = route.request().postDataJSON() as {
        service: Service;
        action: ServiceAction;
      };
      actions.push(action);
      if (options.serviceFailure)
        return route.fulfill({
          status: 500,
          json: { error: { message: options.serviceFailure } },
        });
      const unit = snapshot.units[action.service === "ingress" ? 0 : 1];
      unit.activeState = action.action === "stop" ? "inactive" : "active";
      unit.subState = action.action === "stop" ? "dead" : "running";
      unit.mainPid = action.action === "stop" ? 0 : unit.mainPid || 303;
      if (
        action.service === "tunnel" &&
        action.action !== "stop" &&
        snapshot.units[0].activeState !== "active"
      )
        snapshot.units[0] = {
          ...snapshot.units[0],
          activeState: "active",
          subState: "running",
          mainPid: 404,
        };
      return route.fulfill({ json: snapshot });
    }
    if (
      [
        "/api/pipe/auth",
        "/api/pipe/ai/connections",
        "/api/pipe/registries",
        "/api/pipe/registry-images",
      ].includes(path)
    )
      return route.fulfill({ json: [] });
    if (path === "/api/pipe") {
      if (unavailable)
        return route.fulfill({
          status: 503,
          json: { error: { message: "Pipe helper unavailable" } },
        });
      if (route.request().method() === "PUT") {
        const body = route.request().postDataJSON();
        writes.push(body);
        const missingHost = snapshot.quickTunnels?.find(
          (value) =>
            value.target.type === "routes" &&
            !body.config.routes.some(
              (existing: { host: string }) =>
                existing.host === (value.target as { host: string }).host,
            ),
        );
        if (missingHost)
          return route.fulfill({
            status: 400,
            json: {
              error: {
                message: "request validation failed",
                fields: {
                  config:
                    "This domain is used by a Quick Tunnel. Update or delete the tunnel first.",
                },
              },
            },
          });
        if (options.reject)
          return route.fulfill({
            status: 400,
            json: {
              error: {
                message: "request validation failed",
                fields: { config: "invalid app address" },
              },
            },
          });
        snapshot = { ...snapshot, config: body.config };
      }
      return route.fulfill({ json: snapshot });
    }
    return route.fulfill({ status: 404 });
  });
  await page.addInitScript(
    (locale) => localStorage.setItem("firecrab.locale", locale),
    options.locale ?? "en",
  );
  const card = options.card === undefined ? "route" : options.card;
  await page.goto(card ? `/#/pipe/${card}` : "/#/pipe");
  return {
    get snapshot() {
      return snapshot;
    },
    firecrabSaves,
    firecrabDeletes,
    namedConnections,
    zoneRequests,
    get namedDisconnects() {
      return namedDisconnects;
    },
    writes,
    actions,
    quickSaves,
    quickDeletes,
    quickActions,
    finishConnection,
    unavailable: () => {
      unavailable = true;
    },
    setVms: (next: VmResponse[]) => {
      vms = next;
    },
    failVms: () => {
      vmsUnavailable = true;
    },
    recoverVms: () => {
      vmsUnavailable = false;
    },
    publishQuickUrl: (id = QUICK_ID) => {
      snapshot.quickTunnels!.find((value) => value.id === id)!.publicUrl =
        QUICK_URL;
    },
    failTunnel: (id = QUICK_ID) => {
      const unit = snapshot.quickTunnels!.find(
        (value) => value.id === id,
      )!.unit;
      unit.activeState = "failed";
      unit.subState = "failed";
      unit.mainPid = 0;
    },
  };
}

const editor = (page: Page) => page.locator("#editor");
async function editRoutes(page: Page) {
  await page.getByRole("button", { name: "Edit routes", exact: true }).click();
}
async function save(page: Page) {
  await page.getByRole("button", { name: "Save", exact: true }).click();
}
test.describe("Pipe cards @dashboard", () => {
  test("excludes private service routes from public connector targets", async ({ page }) => {
    const savedRoutes: PipeConfig["routes"] = [...initial.config.routes, {
      host: "orders.internal", pathPrefix: "/", upstreams: ["172.30.36.4:8080"],
      owner: { serviceId: QUICK_ID, deploymentId: SECOND_QUICK_ID },
      network: { micro_network_id: QUICK_ID, gateway: "172.30.36.1", prefix: 24 },
    }];
    await open(page, { savedRoutes, card: "firecrab-tunnel" });
    await page.getByRole("button", { name: "Connect", exact: true }).click();
    await page.getByLabel("Target", { exact: true }).selectOption("nginx");
    await expect(page.getByLabel("Route host", { exact: true }).locator('option[value="orders.internal"]')).toHaveCount(0);
    await expect(page.getByLabel("Route host", { exact: true }).locator('option[value="app.example.com"]')).toHaveCount(1);
    await page.getByRole("button", { name: "Cancel", exact: true }).click();
    await page.goto("/#/pipe/quick-tunnel");
    await page.getByRole("button", { name: "Connect", exact: true }).click();
    await page.getByLabel("Target", { exact: true }).selectOption("routes");
    await expect(page.getByLabel("Route host", { exact: true }).locator('option[value="orders.internal"]')).toHaveCount(0);
  });


  test("opens all nine destinations from their cards", async ({ page }) => {
    await open(page, { card: null });
    const cards = [
      "auth",
      "ai",
      "storage",
      "route",
      "registry",
      "database",
      "firecrab-tunnel",
      "quick-tunnel",
      "named-tunnel",
    ];
    for (const card of cards) {
      await page.goto("/#/pipe");
      await page.locator(`#content a[href="#/pipe/${card}"]`).click();
      await expect(page).toHaveURL(new RegExp(`#/pipe/${card}$`));
      await expect(page.locator("#content")).not.toBeEmpty();
    }
  });
  test("protects a dirty route draft on navigation and ignores cancelled refreshes", async ({
    page,
  }) => {
    const m = await open(page);
    await editRoutes(page);
    await page
      .getByLabel("Listen address", { exact: true })
      .fill("127.0.0.1:8090");
    page.once("dialog", (d) => d.dismiss());
    await page.getByRole("link", { name: "MicroVM", exact: true }).click();
    await expect(
      page.getByLabel("Listen address", { exact: true }),
    ).toHaveValue("127.0.0.1:8090");
    await page.waitForTimeout(3200);
    await expect(
      page.getByLabel("Listen address", { exact: true }),
    ).toHaveValue("127.0.0.1:8090");
    expect(m.writes).toEqual([]);
  });
  test("saves listener and routes once while preserving the named tunnel", async ({
    page,
  }) => {
    const m = await open(page, { configuredTunnel: true });
    await editRoutes(page);
    await page
      .getByLabel("Listen address", { exact: true })
      .fill("127.0.0.1:8090");
    await page
      .getByLabel("HOST | /PATH | ADDRESS:PORT (one per line)", { exact: true })
      .fill("app.example.com | /api | 172.30.36.2:80,172.30.36.3:80");
    await save(page);
    await expect(editor(page)).toHaveCount(0);
    expect(m.writes).toHaveLength(1);
    expect(m.writes[0].config).toMatchObject({
      listen: "127.0.0.1:8090",
      routes: [
        {
          host: "app.example.com",
          pathPrefix: "/api",
          upstreams: ["172.30.36.2:80", "172.30.36.3:80"],
        },
      ],
      tunnel: { id: QUICK_ID },
    });
  });
  test("preserves rejected route inputs and cancels without writing again", async ({
    page,
  }) => {
    const m = await open(page, { reject: true });
    await editRoutes(page);
    await page
      .getByLabel("Listen address", { exact: true })
      .fill("127.0.0.1:8090");
    await save(page);
    await expect(page.getByRole("alert")).toContainText("invalid app address");
    await expect(
      page.getByLabel("Listen address", { exact: true }),
    ).toHaveValue("127.0.0.1:8090");
    await page.getByRole("button", { name: "Cancel", exact: true }).click();
    expect(m.writes).toHaveLength(1);
  });
  test("saves explicit route removal without restoring it during polling", async ({
    page,
  }) => {
    const m = await open(page);
    await editRoutes(page);
    await page
      .getByLabel("HOST | /PATH | ADDRESS:PORT (one per line)", { exact: true })
      .fill("");
    await save(page);
    await expect(editor(page)).toHaveCount(0);
    await page.waitForTimeout(3200);
    expect(m.snapshot.config?.routes).toEqual([]);
  });
  test("controls ingress and the named connector without applying configuration", async ({
    page,
  }) => {
    const m = await open(page, { configuredTunnel: true });
    for (const name of ["ingress: stop", "tunnel: restart"]) {
      page.once("dialog", (d) => d.accept());
      await page.getByRole("button", { name, exact: true }).click();
      await expect(page.getByRole("status")).toContainText("Saved");
    }
    expect(m.actions).toEqual([
      { service: "ingress", action: "stop" },
      { service: "tunnel", action: "restart" },
    ]);
    expect(m.writes).toEqual([]);
  });
  test("reports lifecycle failures without discarding a draft", async ({
    page,
  }) => {
    const m = await open(page, { serviceFailure: "service operation failed" });
    await editRoutes(page);
    await page
      .getByLabel("Listen address", { exact: true })
      .fill("127.0.0.1:8090");
    await page
      .getByRole("button", { name: "ingress: restart", exact: true })
      .click();
    await expect(page.getByRole("alert")).toContainText(
      "service operation failed",
    );
    await expect(
      page.getByLabel("Listen address", { exact: true }),
    ).toHaveValue("127.0.0.1:8090");
    expect(m.writes).toEqual([]);
  });
  test("creates a Quick Tunnel from registered running TCP applications", async ({
    page,
  }) => {
    const m = await open(page, { card: "quick-tunnel" });
    await page.getByRole("button", { name: "Connect", exact: true }).click();
    await page.getByLabel("Name", { exact: true }).fill("Public web");
    const apps = page.getByLabel("App addresses (one per line)", {
      exact: true,
    });
    expect(await apps.locator("option").allTextContents()).not.toEqual(
      expect.arrayContaining([expect.stringContaining("stopped-app")]),
    );
    await apps.selectOption(["172.30.36.2:80", "172.30.36.4:8080"]);
    await save(page);
    await expect(editor(page)).toHaveCount(0);
    expect(m.quickSaves[0]).toMatchObject({
      name: "Public web",
      target: {
        type: "apps",
        upstreams: ["172.30.36.2:80", "172.30.36.4:8080"],
      },
    });
    await expect(
      page.getByRole("link", { name: QUICK_URL, exact: true }),
    ).toBeVisible();
  });
  test("retains unlisted saved application addresses until explicitly removed", async ({
    page,
  }) => {
    const saved = {
      ...webTunnel,
      target: {
        type: "apps" as const,
        upstreams: ["10.90.0.8:8080", "172.30.36.2:80"],
      },
    };
    const m = await open(page, { card: "quick-tunnel", quickTunnels: [saved] });
    await page.getByRole("button", { name: "Edit", exact: true }).click();
    await expect(
      page.getByLabel("App addresses (one per line)", { exact: true }),
    ).toHaveValues(["172.30.36.2:80", "10.90.0.8:8080"]);
    await save(page);
    await expect.poll(() => m.quickSaves.length).toBe(1);
    expect(m.quickSaves[0].target).toMatchObject({
      upstreams: expect.arrayContaining(saved.target.upstreams),
    });
  });
  test("retargets a Quick Tunnel to a saved domain and preserves its identity", async ({
    page,
  }) => {
    const m = await open(page, {
      card: "quick-tunnel",
      quickTunnels: [webTunnel],
    });
    await page.getByRole("button", { name: "Edit", exact: true }).click();
    await page.getByLabel("Target", { exact: true }).selectOption("routes");
    await page
      .getByLabel("Route host", { exact: true })
      .selectOption("app.example.com");
    await save(page);
    await expect.poll(() => m.quickSaves.length).toBe(1);
    expect(m.quickSaves[0]).toMatchObject({
      id: QUICK_ID,
      target: { type: "routes", host: "app.example.com" },
    });
  });
  test("shows Quick Tunnel validation errors without losing inputs", async ({
    page,
  }) => {
    await open(page, {
      card: "quick-tunnel",
      quickFailure: "The selected app is unavailable",
    });
    await page.getByRole("button", { name: "Connect", exact: true }).click();
    await page.getByLabel("Name", { exact: true }).fill("draft");
    await page
      .getByLabel("App addresses (one per line)", { exact: true })
      .selectOption("172.30.36.2:80");
    await save(page);
    await expect(page.getByRole("alert")).toContainText(
      "selected app is unavailable",
    );
    await expect(page.getByLabel("Name", { exact: true })).toHaveValue("draft");
  });
  test("stops and deletes only the selected Quick Tunnel", async ({ page }) => {
    const m = await open(page, {
      card: "quick-tunnel",
      quickTunnels: [webTunnel, apiTunnel],
    });
    await page
      .locator('[data-path="/api/pipe/quick-tunnels/' + QUICK_ID + '/service"]')
      .filter({ hasText: "stop" })
      .click();
    await expect.poll(() => m.quickActions.length).toBe(1);
    page.once("dialog", (d) => d.accept());
    await page
      .locator(
        '[data-method=DELETE][data-path="/api/pipe/quick-tunnels/' +
          QUICK_ID +
          '"]',
      )
      .click();
    await expect.poll(() => m.quickDeletes).toEqual([QUICK_ID]);
    expect(m.snapshot.quickTunnels?.map((t) => t.id)).toEqual([
      SECOND_QUICK_ID,
    ]);
  });
  test("registers a Firecrab token and edits its target without resending ownership", async ({
    page,
  }) => {
    const m = await open(page, { card: "firecrab-tunnel" });
    await page.getByRole("button", { name: "Connect", exact: true }).click();
    await page
      .getByLabel("Ownership token", { exact: true })
      .fill(CONNECTION_TOKEN);
    await page
      .getByLabel("App addresses (one per line)", { exact: true })
      .selectOption("172.30.36.2:80");
    await save(page);
    await expect(editor(page)).toHaveCount(0);
    expect(m.firecrabSaves[0]).toMatchObject({
      token: CONNECTION_TOKEN,
      target: { type: "apps" },
    });
    await page.getByRole("button", { name: "Edit", exact: true }).click();
    await page.getByLabel("Target", { exact: true }).selectOption("nginx");
    await save(page);
    await expect.poll(() => m.firecrabSaves.length).toBe(2);
    expect(m.firecrabSaves[1]).toEqual({
      subdomain: "my-app",
      target: { type: "nginx" },
    });
  });
  test("keeps a rejected Firecrab token editable", async ({ page }) => {
    await open(page, { card: "firecrab-tunnel", firecrabFailure: true });
    await page.getByRole("button", { name: "Connect", exact: true }).click();
    await page
      .getByLabel("Ownership token", { exact: true })
      .fill(CONNECTION_TOKEN);
    await page.getByLabel("Target", { exact: true }).selectOption("nginx");
    await save(page);
    await expect(page.getByRole("alert")).toContainText("invalid or revoked");
    await expect(
      page.getByLabel("Ownership token", { exact: true }),
    ).toHaveValue(CONNECTION_TOKEN);
  });
  test("loads Cloudflare zones from an explicit token and connects a saved hostname", async ({
    page,
  }) => {
    const m = await open(page, { card: "named-tunnel" });
    await page.getByRole("button", { name: "Connect", exact: true }).click();
    await page
      .getByLabel("API token", { exact: true })
      .fill("private-cloudflare-token");
    await page.getByRole("button", { name: "Load zones", exact: true }).click();
    await expect(page.getByLabel("Hostname", { exact: true })).toHaveValue(
      "app.example.com",
    );
    await save(page);
    await expect(editor(page)).toHaveCount(0);
    expect(m.namedConnections[0]).toMatchObject({
      apiToken: "private-cloudflare-token",
      hostname: "app.example.com",
      config: { routes: initial.config.routes },
    });
    expect(
      await page.evaluate(
        () => JSON.stringify(localStorage) + JSON.stringify(sessionStorage),
      ),
    ).not.toContain("private-cloudflare-token");
  });
  test("clears Cloudflare zone choices after changing the token", async ({
    page,
  }) => {
    const m = await open(page, { card: "named-tunnel" });
    await page.getByRole("button", { name: "Connect", exact: true }).click();
    await page.getByLabel("API token", { exact: true }).fill("token-one");
    await page.getByRole("button", { name: "Load zones", exact: true }).click();
    await expect(page.getByLabel("Hostname", { exact: true })).toHaveValue(
      "app.example.com",
    );
    await page.getByLabel("API token", { exact: true }).fill("token-two");
    await expect(
      page.getByLabel("Zone", { exact: true }).locator("option"),
    ).toHaveCount(0);
    expect(m.zoneRequests).toEqual(["token-one"]);
  });
  test("reports unavailable API state and preserves saved provisioning errors", async ({
    page,
  }) => {
    const m = await open(page, { quickTunnels: [webTunnel] });
    m.unavailable();
    await page.getByRole("button", { name: "Refresh", exact: true }).click();
    await expect(page.getByRole("alert")).toContainText(
      "Pipe helper unavailable",
    );
  });
  test("keeps Korean route editing within the mobile viewport", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await open(page, { locale: "ko" });
    await page.locator("[data-action=create][data-id=route]").click();
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    ).toBe(true);
  });
});
