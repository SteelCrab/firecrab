import {
  newServiceButton,
  openDeployForm,
  openMoreMenu,
  serviceAction,
  servicePanel,
  serviceStep,
  selectServiceApp,
  saveService,
} from "./support/serviceSetup";
import { expect, test, type Page } from "@playwright/test";

const ID = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const SECRET = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const STORAGE = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
const INITIAL = {
  id: ID,
  name: "orders",
  image: "ghcr.io/team/orders:1.0",
  runtime: "vm",
  port: 8080,
  healthPath: "/actuator/health",
  domain: "orders.example.com",
  ram: 1024,
  cpu: 2,
  replicas: 3,
  startupTimeout: 120,
  drainTimeout: 60,
  env: { PROFILE: "prod" },
  secrets: { DB_PASSWORD: SECRET },
};

async function openServices(
  page: Page,
  rows: object[] = [],
  failSave = false,
  auths: object[] = [
    {
      provider: "github",
      account: "team",
      registry: "ghcr.io",
      region: null,
      services: ["registry"],
    },
  ],
) {
  const writes: { method: string; body: Record<string, unknown> }[] = [];
  let saved = rows;
  await page.route(/^https?:\/\/[^/]+\/api\//, async (route) => {
    const path = new URL(route.request().url()).pathname;
    const method = route.request().method();
    if (path === "/api/pipe/auth") return route.fulfill({ json: auths });
    if (path.endsWith("/repositories"))
      return route.fulfill({
        json: {
          repositories: ["team/orders", "team/nginx"],
          listingSupported: true,
          nextPage: null,
        },
      });
    if (path === "/api/pipe/registry-tags")
      return route.fulfill({
        json: { tags: ["1.0", "2.0", "stable-alpine"], nextLast: null },
      });
    if (path.endsWith("/deployment/check"))
      return route.fulfill({ json: null });

    if (path === "/api/pipe")
      return route.fulfill({
        json: {
          config: null,
          units: [],
          nginxAvailable: true,
          cloudflaredAvailable: false,
        },
      });
    if (path === "/api/queues") return route.fulfill({ json: [{ id: STORAGE, name: "notifications", phase: "ready" }] });
    if (path === "/api/vms") return route.fulfill({ json: [] });
    if (path === "/api/micro-networks") return route.fulfill({ json: [] });
    if (path === "/api/pipe/storage")
      return route.fulfill({
        json: [
          {
            id: STORAGE,
            name: "Reports",
            bucket: "reports-bucket",
            region: "ap-northeast-2",
            prefix: "team/",
            credentialRef: "connection",
            services: [],
          },
        ],
      });
    if (path === "/api/security-groups")
      return route.fulfill({
        json: [
          { id: SECRET, name: "orders-policy", microNetworkId: ID, rules: [] },
        ],
      });
    if (path === `/api/security-groups/${SECRET}`)
      return route.fulfill({
        json: {
          id: SECRET,
          name: "orders-policy",
          microNetworkId: ID,
          rules: [],
        },
      });
    if (path === "/api/host")
      return route.fulfill({
        json: {
          loadAverage1m: 0.1,
          memoryTotalMib: 8192,
          memoryAvailableMib: 4096,
          diskTotalGib: 100,
          diskAvailableGib: 50,
          uptimeSeconds: 3600,
        },
      });
    if (path === "/api/update")
      return route.fulfill({
        json: { current: "0.3.1", latest: "0.3.1", updateAvailable: false },
      });
    if (path === "/api/update/progress")
      return route.fulfill({
        json: { phase: "idle", percent: 0, updatedAtMs: 0 },
      });
    if (path.endsWith("/deployment") && method === "GET") return route.fulfill({json:null});
    if (path.startsWith("/api/micro-services")) {
      if (method === "POST" || method === "PUT") {
        const body = route.request().postDataJSON();
        writes.push({ method, body });
        if (failSave)
          return route.fulfill({
            status: 400,
            json: {
              error: {
                code: "validation_failed",
                message: "request validation failed",
                fields: { name: "is already registered" },
              },
            },
          });
        saved = [{ id: ID, ...body }];
        return route.fulfill({
          status: method === "POST" ? 201 : 200,
          json: saved[0],
        });
      }
      if (method === "DELETE") {
        saved = [];
        return route.fulfill({ status: 204 });
      }
      const detail = /^\/api\/micro-services\/([^/]+)$/.exec(path);
      if (method === "GET" && detail) {
        const service = saved.find(row => "id" in row && row.id === decodeURIComponent(detail[1]));
        return service
          ? route.fulfill({ json: service })
          : route.fulfill({ status: 404, json: { error: { code: "not_found", message: "resource not found" } } });
      }
      return route.fulfill({
        json: path === "/api/micro-services" ? saved : saved[0],
      });
    }
    return route.fulfill({
      status: 404,
      json: { error: { message: "unused endpoint" } },
    });
  });
  await page.addInitScript(() => localStorage.setItem("firecrab.locale", "en"));
  await page.goto("/#/services");
  return writes;
}


const ACTIVE = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
/** A running deployment of `spec` behind its domain. */
function running(spec: Record<string, unknown>, extra: Record<string, unknown> = {}) {
  return {
    id: ACTIVE, serviceId: ID, spec, phase: "running", domainRouteOwned: true, request: { hostPort: 30190 },
    digest: `sha256:${"a".repeat(64)}`,
    instances: [{ vmId: SECRET, ipv4: "172.30.0.2", hostPort: 30190, healthy: true }],
    log: ["All replicas answered the app health check"], error: null, ...extra,
  };
}
/** Answers the deployment and the release list of the service; counts how often each is asked. */
async function routeRunning(page: Page, deployment: object, releases: object[] = [deployment]) {
  const reads = { deployment: 0, releases: 0 };
  await page.route(`**/api/micro-services/${ID}/deployment`, route => {
    reads.deployment++;
    return route.fulfill({ json: deployment });
  });
  await page.route(`**/api/micro-services/${ID}/releases`, route => {
    reads.releases++;
    return route.fulfill({ json: releases });
  });
  return reads;
}
/** Like `routeRunning`, with answers that the test can change while the page polls (and a failing release list). */
async function routeLive(page: Page, deployment: object, releases: object[] = [deployment]) {
  const live = { deployment, releases, releaseStatus: 200, reads: { deployment: 0, releases: 0 } };
  await page.route(`**/api/micro-services/${ID}/deployment`, route => {
    live.reads.deployment++;
    return route.fulfill({ json: live.deployment });
  });
  await page.route(`**/api/micro-services/${ID}/releases`, route => {
    live.reads.releases++;
    return live.releaseStatus === 200
      ? route.fulfill({ json: live.releases })
      : route.fulfill({ status: live.releaseStatus, json: { error: { code: "unavailable", message: "Release storage unavailable" } } });
  });
  return live;
}
function retained(id: string, image: string, digest = `sha256:${"b".repeat(64)}`) {
  return { id, serviceId: ID, spec: { ...INITIAL, image }, digest };
}

test.describe("MicroService configuration @dashboard", () => {
  test("deploys privately with an empty host port and shows its internal address", async ({ page }) => {
    const service = { ...INITIAL, domain: null, replicas: 2 };
    await openServices(page, [service]);
    let deployment: object | null = null;
    await page.route(`**/api/micro-services/${ID}/deployment`, async route => {
      if (route.request().method() === "POST") {
        expect(route.request().postDataJSON()).toEqual({ hostPort: 0, microNetworkId: null });
        deployment = { id: SECRET, serviceId: ID, spec: service, phase: "running", request: { hostPort: 0 },
          digest: null, internalRouteOwned: true, domainRouteOwned: false, error: null, log: [],
          instances: [{ vmId: SECRET, ipv4: "172.30.0.2", hostPort: 0, state: "running", healthy: true }] };
        return route.fulfill({ status: 202, json: deployment });
      }
      return route.fulfill({ json: deployment });
    });
    await openDeployForm(page);
    const deploy = page.getByRole("button", { name: "Deploy and run", exact: true });
    const port = page.getByLabel("First host port", { exact: true });
    await expect(deploy).toBeEnabled();
    await port.fill("");
    await expect(port).toHaveValue("");
    await expect(deploy).toBeEnabled();
    await deploy.click();
    await expect(page.getByText("Available to apps in the same MicroNetwork.", { exact: true })).toBeVisible();
    await expect(page.locator("code").filter({ hasText: "http://orders.internal" })).toBeVisible();
    await expect(page.getByRole("link", { name: "Open app", exact: true })).toHaveCount(0);
    await openMoreMenu(page);
    await expect(page.getByRole("button", { name: "Stop service", exact: true })).toBeEnabled();
  });

  test("retains Stop after uncertain internal routing cleanup without replicas", async ({ page }) => {
    await openServices(page, [{ ...INITIAL, domain: null }]);
    await page.route(`**/api/micro-services/${ID}/deployment`, route => route.fulfill({ json: {
      id: SECRET, serviceId: ID, spec: { ...INITIAL, domain: null }, phase: "failed", request: { hostPort: 0 }, digest: null,
      internalRouteOwned: true, instances: [], log: [], error: "Could not withdraw internal DNS and routing; retry Stop."
    } }));
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    await openMoreMenu(page);
    await expect(page.getByRole("button", { name: "Stop service", exact: true })).toBeEnabled();
    await expect(page.getByRole("button", { name: "Deploy and run", exact: true })).toBeHidden();
    await expect(page.getByText("Available to apps in the same MicroNetwork.", { exact: true })).toHaveCount(0);
  });


  test("keeps long image references compact and exposes row actions by keyboard", async ({ page }) => {
    const image = `118167167440.dkr.ecr.ap-northeast-2.amazonaws.com/ecr/nginx@sha256:${"a".repeat(64)}`;
    await openServices(page, [{ ...INITIAL, image }]);
    await expect(page.getByRole("complementary", { name: "MicroService detail" })).toBeHidden();
    await expect(page.getByTitle(image, { exact: true })).toBeVisible();
    await expect(page.getByRole("button", { name: "Edit orders", exact: true })).toBeHidden();
    const menu = page.getByLabel("Actions for orders", { exact: true });
    await menu.focus();
    await page.keyboard.press("Space");
    await expect(page.getByRole("button", { name: "Edit orders", exact: true })).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(menu).toBeFocused();
    await expect(page.getByRole("button", { name: "Delete orders", exact: true })).toBeHidden();
    await page.setViewportSize({ width: 390, height: 844 });
    expect(await page.locator(".service-list").evaluate(el => el.scrollWidth <= el.clientWidth + 1)).toBe(true);
  });

  test("splits the service panel into four tabs", async ({ page }) => {
    await openServices(page, [INITIAL]);
    await routeRunning(page, running(INITIAL));
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    const panel = page.getByRole("complementary", { name: "MicroService detail" });
    // The Instances tab says how many VMs run.
    await expect(panel.getByRole("tab")).toHaveText(["Overview", "Instances 1", "Releases", "Settings"]);
    await expect(panel.locator("#service-status")).toBeVisible();
    await expect(panel.locator("details.service-disclosure")).toHaveCount(0);
    await panel.getByRole("tab", { name: "Instances" }).click();
    await expect(panel.locator("table.service-instances")).toBeVisible();
    await panel.getByRole("tab", { name: "Releases" }).click();
    await expect(panel.locator("#deploy-panel")).toBeVisible();
    await panel.getByRole("tab", { name: "Settings" }).click();
    await expect(panel.getByRole("button", { name: "Edit configuration" })).toBeVisible();
  });

  test("the panel head says the phase, the release and the strategy, with Deploy update, Roll back and More", async ({ page }) => {
    const update = { strategy: "blue_green", previous: null, completed: true };
    await openServices(page, [INITIAL]);
    await routeRunning(page, running(INITIAL, { update }));
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    const panel = servicePanel(page);
    await expect(panel.getByRole("heading", { name: "orders", exact: true })).toBeVisible();
    await expect(panel.locator(".detail-kind")).toHaveText("MicroService");
    await expect(panel.locator(".detail-meta")).toHaveText("Running·dddddddd·Blue-green");
    await expect(panel.locator(".detail-meta .mono")).toHaveAttribute("title", ACTIVE);
    // The first icon is the rocket; it updates a running service. Stop is not in the head.
    const head = panel.locator(".detail-head");
    const icons = head.locator(".detail-top-actions > fieldset > button, .detail-top-actions > fieldset > details > summary, .detail-top-actions > button");
    expect(await icons.evaluateAll(buttons => buttons.map(button => button.getAttribute("aria-label")))).toEqual(["Deploy update", "Roll back", "More", "Close detail"]);
    await expect(head.getByRole("button", { name: "Stop service", exact: true })).toHaveCount(0);
    // The head's Roll back opens the Releases tab.
    await panel.getByRole("button", { name: "Roll back", exact: true }).click();
    await expect(panel.getByRole("tab", { name: "Releases" })).toHaveAttribute("aria-selected", "true");
    await expect(panel.locator("#deploy-panel")).toBeVisible();
    // More holds Stop service first, then the Edit and Delete that Settings has, and closes with Escape.
    await panel.getByLabel("More", { exact: true }).click();
    const menu = panel.locator(".vm-action-menu");
    expect(await menu.getByRole("button").evaluateAll(buttons => buttons.map(button => (button.textContent ?? "").trim()))).toEqual(["Stop service", "Edit configuration", "Delete"]);
    await expect(menu.getByRole("button", { name: "Stop service", exact: true })).toBeEnabled();
    await expect(menu.locator("#service-stop")).toHaveCount(1);
    await page.keyboard.press("Escape");
    await expect(menu).toBeHidden();
    await expect(panel).toBeVisible();
  });

  test("the rocket in the head of a running service opens the update form of the Releases tab", async ({ page }) => {
    await openServices(page, [INITIAL]);
    await routeRunning(page, running(INITIAL));
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    const panel = servicePanel(page);
    await panel.getByRole("button", { name: "Deploy update", exact: true }).click();
    await expect(panel.getByRole("heading", { name: "Deploy update", exact: true })).toBeVisible();
    // Cancel goes back to the Releases tab, where the update belongs.
    await panel.getByRole("button", { name: "Cancel", exact: true }).click();
    await expect(panel.getByRole("tab", { name: "Releases" })).toHaveAttribute("aria-selected", "true");
    // The tab has the same action as a button, and the rocket is still there.
    await expect(panel.locator("#deploy-update")).toBeVisible();
    await expect(panel.locator(".detail-head").getByRole("button", { name: "Deploy update", exact: true })).toBeVisible();
  });

  test("the rocket and Stop wait while a deployment is going", async ({ page }) => {
    await openServices(page, [INITIAL]);
    await routeRunning(page, running(INITIAL, { phase: "draining", update: { strategy: "rolling", completed: false, previous: { id: SECRET } } }));
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    const panel = servicePanel(page);
    await expect(panel.locator(".detail-head").getByRole("button", { name: "Deploy update", exact: true })).toBeDisabled();
    await openMoreMenu(page);
    await expect(panel.getByRole("button", { name: "Stop service", exact: true })).toBeDisabled();
  });

  test("a service that was never deployed has Deploy and no Roll back", async ({ page }) => {
    await openServices(page, [INITIAL]);
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    const panel = servicePanel(page);
    await expect(panel.locator(".detail-meta")).toHaveText("Not deployed");
    await expect(panel.getByRole("button", { name: "Deploy", exact: true })).toBeVisible();
    await expect(panel.getByRole("button", { name: "Roll back", exact: true })).toHaveCount(0);
    await openMoreMenu(page);
    await expect(panel.getByRole("button", { name: "Edit configuration", exact: true })).toBeVisible();
    await expect(panel.getByRole("button", { name: "Stop service", exact: true })).toHaveCount(0);
    await page.keyboard.press("Escape");
    // No count while nothing ran.
    await expect(panel.getByRole("tab", { name: "Instances", exact: true })).toBeVisible();
    await panel.getByRole("tab", { name: "Instances" }).click();
    await expect(panel.getByText("No instances are running.", { exact: true })).toBeVisible();
    await panel.getByRole("tab", { name: "Releases" }).click();
    await expect(panel.locator("#deploy-panel")).toHaveText("No releases yet. Deploy the service first.");
  });

  test("the Overview shows the deployment, the properties, the connections and the latest three releases", async ({ page }) => {
    const older = ["11111111-1111-4111-8111-111111111111", "22222222-2222-4222-8222-222222222222", "33333333-3333-4333-8333-333333333333"];
    const linked = { ...INITIAL, queues: { NOTIFICATIONS: { queueId: STORAGE, access: "consume" } }, storageConnectionId: STORAGE };
    await openServices(page, [linked]);
    await routeRunning(page, running(linked), [
      retained(ACTIVE, "ghcr.io/team/orders:1.0"), retained(older[0], "ghcr.io/team/orders:0.9"),
      retained(older[1], "ghcr.io/team/orders:0.8"), retained(older[2], "ghcr.io/team/orders:0.7"),
    ]);
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    const panel = servicePanel(page);
    // The deployment is the first section, with the one status of the tab.
    const sections = panel.locator("#service-tab-panel > section");
    await expect(sections.first()).toHaveAttribute("aria-label", "Deployment");
    await expect(sections.first().locator("#service-status").getByRole("status")).toHaveText("Running");
    await expect(panel.getByRole("status")).toHaveCount(1);
    const properties = panel.getByRole("region", { name: "Properties" });
    await expect(properties.locator("dt")).toHaveText(["Image", "Port", "Replicas", "Health path", "Size", "Connections"]);
    await expect(properties.locator("dd").nth(1)).toHaveText("8080");
    await expect(properties.locator("dd").nth(4)).toHaveText("2 vCPU · 1 GiB");
    await expect(properties.getByRole("button", { name: "Copy Image", exact: true })).toBeVisible();
    // The names of the Queue and the S3 connection come from what the screen loaded, and Secrets are a count.
    await expect(properties.locator("dd").nth(5).locator(".chip")).toHaveText(["Queue notifications", "S3 Reports", "Secret 1"]);
    const recent = panel.getByRole("region", { name: "Recent releases" });
    await expect(recent.locator("li")).toHaveCount(3);
    await expect(recent.locator("li").first()).toContainText("Active");
    await expect(recent.locator("li").nth(1)).toContainText("Archived");
    // Only the archived releases can be rolled back to, and the icon is there without a hover.
    const roll = recent.getByRole("button", { name: "Roll back to this release", exact: true });
    await expect(roll).toHaveCount(2);
    await page.mouse.move(0, 0);
    await expect(roll.first()).toHaveCSS("opacity", "1");
    // The icon picks the release and opens the Releases tab, where the Roll back button is.
    await roll.first().click();
    await expect(panel.getByRole("tab", { name: "Releases" })).toHaveAttribute("aria-selected", "true");
    await expect(page.getByLabel("Rollback release", { exact: true })).toHaveValue(older[0]);
    await expect(panel.locator("#deploy-rollback")).toBeEnabled();
    await expect(panel.getByRole("button", { name: "Roll back to selected release", exact: true })).toBeEnabled();
    // The Releases tab lists all four.
    await expect(panel.locator("#deploy-panel table.release-table tbody tr")).toHaveCount(4);
  });

  test("the deployment log is behind an icon, shut at first, and stays open through the polls", async ({ page }) => {
    await openServices(page, [INITIAL]);
    const live = await routeLive(page, running(INITIAL));
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    const panel = servicePanel(page);
    const section = panel.getByRole("region", { name: "Deployment", exact: true });
    const toggle = section.getByRole("button", { name: "Deployment log", exact: true });
    await expect(toggle).toHaveAttribute("aria-expanded", "false");
    // Nothing of the log is inline in the Overview.
    await expect(panel.getByText("All replicas answered the app health check", { exact: true })).toHaveCount(0);
    await expect(panel.locator("#service-log")).toHaveCount(0);
    await toggle.click();
    await expect(toggle).toHaveAttribute("aria-expanded", "true");
    await expect(panel.locator("#service-log")).toHaveText("All replicas answered the app health check");
    expect(await panel.locator("#service-log").evaluate(el => el.closest("section")?.getAttribute("aria-label"))).toBe("Deployment");
    // Polls, even with a new line in the log, keep it open.
    const before = live.reads.deployment;
    live.deployment = running(INITIAL, { log: ["All replicas answered the app health check", "Route confirmed"] });
    await expect.poll(() => live.reads.deployment).toBeGreaterThan(before + 1);
    await expect(panel.locator("#service-log")).toContainText("Route confirmed");
    await expect(toggle).toHaveAttribute("aria-expanded", "true");
    await toggle.click();
    await expect(panel.locator("#service-log")).toHaveCount(0);
    // No lines, no icon.
    live.deployment = running(INITIAL, { log: [] });
    await expect(section.getByRole("button", { name: "Deployment log", exact: true })).toHaveCount(0);
  });

  test("an update reads from release to release, with a bar of its replicas while it goes", async ({ page }) => {
    const previous = "11111111-1111-4111-8111-111111111111";
    const spec = { ...INITIAL, replicas: 3 };
    await openServices(page, [spec]);
    const going = running(spec, {
      phase: "switching_traffic",
      instances: [1, 2, 3].map(n => ({ vmId: `${n}`.repeat(8) + "-0000-4000-8000-000000000000", ipv4: `172.30.0.${n}`, hostPort: 30190 + n, healthy: n < 3 })),
      update: { strategy: "rolling", previous: running(spec, { id: previous }), completed: false },
    });
    const live = await routeLive(page, going, [retained(previous, "ghcr.io/team/orders:0.9")]);
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    const panel = servicePanel(page);
    const status = panel.locator("#service-status");
    await expect(status.getByRole("status")).toHaveText("Switching Nginx traffic");
    await expect(status.locator(".service-health")).toHaveText("2/3 healthy");
    await expect(status.locator(".service-update")).toHaveText("11111111 → dddddddd · Rolling");
    const bar = status.getByRole("progressbar");
    await expect(bar).toHaveAttribute("aria-valuenow", "2");
    await expect(bar).toHaveAttribute("aria-valuemax", "3");
    await expect(bar.locator("i")).toHaveCount(3);
    expect(await bar.locator("i").evaluateAll(cells => cells.map(cell => cell.getAttribute("data-state")))).toEqual(["ok", "ok", "busy"]);
    // The recent releases: the one being made, in progress, then the one that serves.
    const recent = panel.getByRole("region", { name: "Recent releases" });
    await expect(recent.locator("li")).toHaveCount(2);
    await expect(recent.locator("li").first()).toContainText("In progress");
    await expect(recent.locator("li").nth(1)).toContainText("Active");
    // Settled: no bar, no update line.
    live.deployment = running(spec, { instances: (going as { instances: object[] }).instances.map(instance => ({ ...instance, healthy: true })), update: { strategy: "rolling", previous: running(spec, { id: previous }), completed: true } });
    await expect(status.getByRole("status")).toHaveText("Running");
    await expect(status.getByRole("progressbar")).toHaveCount(0);
    await expect(status.locator(".service-update")).toHaveCount(0);
  });

  test("asks for the releases once for the active release, not on every poll", async ({ page }) => {
    await openServices(page, [INITIAL]);
    const reads = await routeRunning(page, running(INITIAL));
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    await expect.poll(() => reads.releases).toBe(1);
    const polls = reads.deployment;
    await expect.poll(() => reads.deployment).toBeGreaterThan(polls + 1);
    expect(reads.releases).toBe(1);
    // Another service asks again; so does the same one when a new release becomes the active one.
    await servicePanel(page).getByRole("tab", { name: "Releases" }).click();
    await expect(servicePanel(page).locator("#deploy-panel table.release-table")).toBeVisible();
    expect(reads.releases).toBe(1);
  });

  test("the new release appears once the update settles, not before", async ({ page }) => {
    const previous = "11111111-1111-4111-8111-111111111111";
    await openServices(page, [INITIAL]);
    const update = (completed: boolean) => ({ strategy: "rolling", previous: running(INITIAL, { id: previous }), completed });
    const live = await routeLive(page, running(INITIAL, { phase: "switching_traffic", update: update(false) }), [retained(previous, "ghcr.io/team/orders:0.9")]);
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    const recent = servicePanel(page).getByRole("region", { name: "Recent releases" });
    // The host has not stored the new release: it is "in progress", and the release that serves is active, not archived.
    await expect(recent.locator("li")).toHaveCount(2);
    await expect(recent.locator("li").first()).toContainText("In progress");
    await expect(recent.locator("li").nth(1)).toContainText("Active");
    await expect(recent.locator("li").nth(1)).toHaveAttribute("data-release", previous);
    // Going on does not ask again: the list is asked for once per state of the deployment.
    const polls = live.reads.deployment;
    await expect.poll(() => live.reads.deployment).toBeGreaterThan(polls + 1);
    expect(live.reads.releases).toBe(1);
    // The update settles and the host stores its release: the list is asked again, once.
    live.releases = [retained(ACTIVE, "ghcr.io/team/orders:1.0"), retained(previous, "ghcr.io/team/orders:0.9")];
    live.deployment = running(INITIAL, { update: update(true) });
    await expect(recent.locator("li")).toHaveCount(2);
    await expect(recent.locator("li").first()).toContainText("Active");
    await expect(recent.locator("li").first()).toHaveAttribute("data-release", ACTIVE);
    await expect(recent.locator("li").nth(1)).toContainText("Archived");
    await expect(recent.locator("li").nth(1)).toHaveAttribute("data-release", previous);
    expect(live.reads.releases).toBe(2);
    const settled = live.reads.deployment;
    await expect.poll(() => live.reads.deployment).toBeGreaterThan(settled + 1);
    expect(live.reads.releases).toBe(2);
    // The Releases tab agrees: the release that serves is marked, and has no roll back.
    await servicePanel(page).getByRole("tab", { name: "Releases" }).click();
    const rows = servicePanel(page).locator("table.release-table tbody tr");
    await expect(rows).toHaveCount(2);
    await expect(rows.first()).toContainText("Active");
    await expect(rows.first().getByRole("button", { name: "Roll back to this release" })).toHaveCount(0);
  });

  test("the first release shows as in progress, then as the active one", async ({ page }) => {
    await openServices(page, [INITIAL]);
    const live = await routeLive(page, running(INITIAL, { phase: "starting", instances: [] }), []);
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    const recent = servicePanel(page).getByRole("region", { name: "Recent releases" });
    await expect(recent.locator("li")).toHaveCount(1);
    await expect(recent.locator("li").first()).toContainText("In progress");
    live.releases = [retained(ACTIVE, "ghcr.io/team/orders:1.0")];
    live.deployment = running(INITIAL);
    await expect(recent.locator("li").first()).toContainText("Active");
    await expect(recent.locator("li")).toHaveCount(1);
    expect(live.reads.releases).toBe(2);
  });

  test("a late list of one service is dropped after another service was opened", async ({ page }) => {
    const OTHER = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee";
    const OTHER_RELEASE = "99999999-9999-4999-8999-999999999999";
    const worker = { ...INITIAL, id: OTHER, name: "worker", image: "ghcr.io/team/nginx:stable-alpine", domain: "worker.example.com" };
    await openServices(page, [INITIAL, worker]);
    let open!: () => void;
    const gate = new Promise<void>(resolve => (open = resolve));
    let asked = 0;
    await page.route(`**/api/micro-services/${ID}/deployment`, route => route.fulfill({ json: running(INITIAL) }));
    await page.route(`**/api/micro-services/${ID}/releases`, async route => {
      asked++;
      await gate;
      return route.fulfill({ json: [retained(ACTIVE, "ghcr.io/team/orders:1.0")] });
    });
    await page.route(`**/api/micro-services/${OTHER}`, route => route.fulfill({ json: worker }));
    await page.route(`**/api/micro-services/${OTHER}/deployment`, route => route.fulfill({ json: running(worker, { id: OTHER_RELEASE, serviceId: OTHER }) }));
    await page.route(`**/api/micro-services/${OTHER}/releases`, route => route.fulfill({ json: [{ id: OTHER_RELEASE, serviceId: OTHER, spec: worker, digest: `sha256:${"c".repeat(64)}` }] }));
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    await expect.poll(() => asked).toBe(1);
    await page.getByRole("button", { name: "Open worker", exact: true }).click();
    const recent = servicePanel(page).getByRole("region", { name: "Recent releases" });
    await expect(recent.locator("li")).toHaveCount(1);
    await expect(recent.locator("li")).toHaveAttribute("data-release", OTHER_RELEASE);
    // The first list arrives late. It is not the worker's, and it does not replace the worker's.
    open();
    await page.waitForTimeout(500);
    await expect(recent.locator("li")).toHaveCount(1);
    await expect(recent.locator("li")).toHaveAttribute("data-release", OTHER_RELEASE);
    await servicePanel(page).getByRole("tab", { name: "Releases" }).click();
    await expect(servicePanel(page).locator("table.release-table tbody tr")).toHaveCount(1);
    await expect(servicePanel(page).locator("table.release-table tbody tr")).toContainText("ghcr.io/team/nginx:stable-alpine");
  });

  test("a service without a domain has no releases to ask for", async ({ page }) => {
    const plain = { ...INITIAL, domain: null };
    await openServices(page, [plain]);
    const reads = await routeRunning(page, running(plain, { domainRouteOwned: false }));
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    const panel = servicePanel(page);
    await expect(panel.locator("#service-status").getByRole("status")).toHaveText("Running");
    await expect(panel.getByRole("button", { name: "Roll back", exact: true })).toHaveCount(0);
    await panel.getByRole("tab", { name: "Releases" }).click();
    await expect(panel.locator("#deploy-panel")).toHaveText("Releases and updates are available for a service that has a domain.");
    await expect.poll(() => reads.deployment).toBeGreaterThan(1);
    expect(reads.releases).toBe(0);
  });

  test("arrow keys, Home and End move between the tabs", async ({ page }) => {
    await openServices(page, [INITIAL]);
    await routeRunning(page, running(INITIAL));
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    const panel = servicePanel(page);
    await panel.getByRole("tab", { name: "Overview" }).focus();
    await page.keyboard.press("ArrowRight");
    await expect(panel.getByRole("tab", { name: "Instances" })).toBeFocused();
    await expect(panel.getByRole("tab", { name: "Instances" })).toHaveAttribute("aria-selected", "true");
    await page.keyboard.press("End");
    await expect(panel.getByRole("tab", { name: "Settings" })).toBeFocused();
    await page.keyboard.press("ArrowRight");
    await expect(panel.getByRole("tab", { name: "Overview" })).toBeFocused();
    await page.keyboard.press("ArrowLeft");
    await expect(panel.getByRole("tab", { name: "Settings" })).toHaveAttribute("aria-selected", "true");
    await page.keyboard.press("Home");
    await expect(panel.getByRole("tab", { name: "Overview" })).toHaveAttribute("aria-selected", "true");
  });

  test("lists name, port, replicas and domain, with Deploy and a menu on each row", async ({ page }) => {
    const worker = { ...INITIAL, id: SECRET, name: "worker", image: "worker:2.0", port: 9000, replicas: 2, domain: null };
    await openServices(page, [INITIAL, worker]);
    await expect(page.getByRole("columnheader")).toHaveText(["Service", "Port", "Replicas", "Domain", "Actions"]);
    await expect(page.getByRole("columnheader", { name: "State" })).toHaveCount(0);
    const rows = page.locator("table.service-table tbody tr");
    await expect(rows.first().locator("td").nth(0)).toContainText("orders");
    await expect(rows.first().locator(".cell-sub")).toHaveText("orders:1.0");
    await expect(rows.first().locator("td").nth(1)).toHaveText("8080");
    await expect(rows.first().locator("td").nth(2)).toHaveText("3");
    await expect(rows.first().locator("td").nth(3)).toHaveText("orders.example.com");
    await expect(rows.nth(1).locator("td").nth(3)).toHaveText("—");
    await expect(rows.nth(1).locator("td").nth(3).locator(".none")).toBeVisible();
    // The row's icons show on hover, and the row is 75px like every table.
    const deploy = page.getByRole("button", { name: "Deploy orders", exact: true });
    await expect(deploy).toHaveCSS("opacity", "0");
    await rows.first().hover();
    await expect(deploy).toHaveCSS("opacity", "1");
    expect((await rows.first().boundingBox())?.height).toBe(75);
    await deploy.click();
    const panel = servicePanel(page);
    await expect(panel.getByRole("heading", { name: "Deploy MicroService", exact: true })).toBeVisible();
    await expect(panel.locator(".detail-meta")).toHaveText("orders");
    // A row click opens the tabs; the name is a button too, for the keyboard.
    await panel.getByRole("button", { name: "Cancel", exact: true }).click();
    const workerDetail = page.waitForResponse(response =>
      response.request().method() === "GET" && new URL(response.url()).pathname === `/api/micro-services/${SECRET}`);
    await rows.nth(1).locator("td").nth(1).click();
    expect(await (await workerDetail).json()).toMatchObject({ id: SECRET, name: "worker" });
    await expect(panel.getByRole("heading", { name: "worker", exact: true })).toBeVisible();
    await expect(panel.getByRole("tab")).toHaveCount(4);
    await page.getByRole("button", { name: "Open orders", exact: true }).focus();
    await page.keyboard.press("Enter");
    await expect(panel.getByRole("heading", { name: "orders", exact: true })).toBeVisible();
  });

  test("an empty list explains the screen and starts the first service", async ({ page }) => {
    await openServices(page);
    const empty = page.locator("#content .empty-state");
    await expect(empty.locator(".empty-state-title")).toHaveText("No MicroServices yet");
    await expect(empty.locator("p")).toHaveText("Run a container image on MicroVMs and deploy without downtime.");
    await expect(empty.locator(".empty-state-icon svg")).toBeVisible();
    await empty.getByRole("link", { name: "New MicroService", exact: true }).click();
    await expect(page).toHaveURL(/#\/services\/new$/);
    await expect(page.getByRole("heading", { name: "New MicroService", level: 2 })).toBeVisible();
    await page.getByRole("button", { name: "Cancel", exact: true }).click();
    await expect(page).toHaveURL(/#\/services$/);
    await page.getByRole("combobox", { name: "Language", exact: true }).selectOption("ko");
    await expect(empty.locator(".empty-state-title")).toHaveText("아직 MicroService가 없습니다");
    await expect(empty.locator("p")).toHaveText("컨테이너 이미지를 MicroVM에서 실행하고, 무중단으로 배포합니다.");
    await expect(empty.getByRole("link", { name: "새 MicroService", exact: true })).toBeVisible();
  });

  test("the tabs, the head and the list read in Korean", async ({ page }) => {
    await openServices(page, [INITIAL]);
    await routeRunning(page, running(INITIAL));
    await page.getByRole("combobox", { name: "Language", exact: true }).selectOption("ko");
    await expect(page.getByRole("columnheader")).toHaveText(["서비스", "포트", "복제 수", "도메인", "작업"]);
    await page.getByRole("button", { name: "orders 열기", exact: true }).click();
    const panel = servicePanel(page);
    await expect(panel.getByRole("tab")).toHaveText(["개요", "인스턴스 1", "릴리스", "설정"]);
    await expect(panel.getByRole("button", { name: "롤백", exact: true })).toBeVisible();
    await expect(panel.getByRole("region", { name: "속성" })).toBeVisible();
    await panel.getByRole("tab", { name: "설정" }).click();
    await expect(panel.getByRole("button", { name: "구성 수정", exact: true })).toBeVisible();
  });

  test("long values stay on one line in the list, the instances and the releases", async ({ page }) => {
    const domain = "orders-with-a-very-long-service-name.production.eu-west-1.team.internal.example.com";
    const image = `registry.example.com/team/orders@sha256:${"c".repeat(64)}`;
    const spec = { ...INITIAL, domain, image };
    const vm = "ffffffff-ffff-4fff-8fff-ffffffffffff-with-a-long-suffix";
    await openServices(page, [spec]);
    await routeRunning(page, running(spec, { instances: [{ vmId: vm, ipv4: "172.30.0.2", hostPort: 30190, healthy: false }] }),
      [retained(ACTIVE, image, `sha256:${"d".repeat(64)}`), retained("99999999-9999-4999-8999-999999999999", `${image}-previous`)]);
    // The list: 75px rows, the domain cut with its whole text as the tooltip.
    const row = page.locator("table.service-table tbody tr").first();
    expect((await row.boundingBox())?.height).toBe(75);
    await expect(row.getByTitle(domain, { exact: true })).toBeVisible();
    await expect(row.getByTitle(image, { exact: true })).toBeVisible();
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    const panel = servicePanel(page);
    // The instances table: the VM id is cut, not wrapped.
    await panel.getByRole("tab", { name: "Instances" }).click();
    const instance = panel.locator("table.service-instances tbody tr").first();
    await expect(instance.getByTitle(vm, { exact: true })).toBeVisible();
    expect((await instance.boundingBox())?.height).toBe(75);
    // The releases: one line each, and nothing wider than the panel.
    await panel.getByRole("tab", { name: "Releases" }).click();
    const releases = panel.locator("table.release-table tbody tr");
    await expect(releases).toHaveCount(2);
    for (const index of [0, 1]) expect((await releases.nth(index).boundingBox())?.height).toBe(75);
    await expect(releases.first().getByTitle(`sha256:${"d".repeat(64)}`, { exact: true })).toBeVisible();
    expect(await panel.evaluate(el => el.scrollWidth <= el.clientWidth)).toBe(true);
    expect(await panel.locator(".detail-content").evaluate(el => el.scrollWidth <= el.clientWidth)).toBe(true);
  });

  test("Edit MicroService keeps Cancel, Back and Next in one footer row at the bottom of the panel", async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 900 });
    await openServices(page, [INITIAL]);
    await serviceAction(page, "orders", "Edit");
    const panel = servicePanel(page);
    const footer = panel.locator(".form-footer");
    await expect(footer).toHaveCount(1);
    await expect(footer.getByRole("button", { name: "Cancel", exact: true })).toBeVisible();
    await expect(footer.getByRole("button", { name: "Next", exact: true })).toBeVisible();
    // The first step has no Back; Next is the primary button.
    await expect(footer.getByRole("button", { name: "Back", exact: true })).toBeHidden();
    await expect(footer.getByRole("button", { name: "Next", exact: true })).toHaveClass(/primary/);
    await expect(footer.getByRole("button", { name: "Cancel", exact: true })).toHaveClass(/ghost/);
    const layout = () => panel.evaluate(() => {
      const bar = document.querySelector("#detail-panel .form-footer")!.getBoundingClientRect();
      const body = document.querySelector("#detail-panel")!.getBoundingClientRect();
      const buttons = [...document.querySelectorAll<HTMLElement>("#detail-panel button:not([hidden])")]
        .filter(button => button.offsetParent !== null);
      return {
        gap: Math.abs(bar.bottom - body.bottom),
        rowHeight: bar.height,
        // Visible buttons below the top of the footer that are not in it.
        below: buttons.filter(b => {
          const box = b.getBoundingClientRect();
          return box.top >= bar.top - 1 && !b.closest(".form-footer") &&
            b.contains(document.elementFromPoint(box.left + box.width / 2, box.top + box.height / 2));
        }).length,
        outside: [...document.querySelectorAll("#detail-panel [data-action=service-next], #detail-panel [data-action=service-back]")]
          .filter(b => !b.closest(".form-footer")).length,
        centers: [...document.querySelectorAll<HTMLElement>("#detail-panel .form-footer button:not([hidden])")]
          .map(b => Math.round(b.getBoundingClientRect().top + b.getBoundingClientRect().height / 2)),
      };
    });
    for (const step of [0, 1, 2, 3, 4]) {
      await serviceStep(page, step);
      const box = await layout();
      expect(box.gap).toBeLessThanOrEqual(1);
      expect(box.rowHeight).toBeLessThan(96);
      expect(box.below).toBe(0);
      expect(box.outside).toBe(0);
      expect(new Set(box.centers).size).toBe(1);
      // Back from the second step; on the last the submit button takes the place of Next.
      await expect(footer.getByRole("button", { name: "Back", exact: true })).toBeVisible({ visible: step > 0 });
      await expect(footer.getByRole("button", { name: "Next", exact: true })).toBeVisible({ visible: step < 4 });
      await expect(footer.locator("button[type=submit]")).toBeVisible({ visible: step === 4 });
    }
    // The order along the row: Cancel, Back, then the way on.
    const order = await footer.locator("button:visible").evaluateAll(buttons => buttons.map(b => (b.textContent ?? "").trim()));
    expect(order).toEqual(["Cancel", "Back", "Save configuration"]);
    // A short panel keeps the bar at the bottom as well.
    await page.setViewportSize({ width: 1440, height: 520 });
    expect((await layout()).gap).toBeLessThanOrEqual(1);
  });

  test("the Deploy form keeps its footer at the bottom of the panel, with the status above the form", async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 900 });
    await openServices(page, [INITIAL]);
    await openDeployForm(page);
    const panel = servicePanel(page);
    await expect(panel.locator("#service-status").getByRole("status")).toHaveText("Not deployed");
    await expect(panel.locator(".form-footer")).toHaveCount(1);
    for (const height of [900, 520]) {
      await page.setViewportSize({ width: 1440, height });
      const boxes = await panel.evaluate(() => ({
        footer: document.querySelector("#detail-panel .form-footer")!.getBoundingClientRect().bottom,
        panel: document.querySelector("#detail-panel")!.getBoundingClientRect().bottom,
        status: document.querySelector("#service-status")!.getBoundingClientRect().bottom,
        form: document.querySelector("#editor")!.getBoundingClientRect().top,
      }));
      expect(Math.abs(boxes.footer - boxes.panel)).toBeLessThanOrEqual(1);
      expect(boxes.status).toBeLessThanOrEqual(boxes.form + 1);
    }
  });

  test("the setup steps of a new MicroService are inside the form above its footer", async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 900 });
    await openServices(page);
    await newServiceButton(page).click();
    // The page loads its lists before it shows the form.
    await expect(page.locator("#editor")).toBeVisible();
    const panel = page.locator("#content");
    const order = await panel.evaluate(() => {
      const index = (selector: string) => {
        const element = document.querySelector(selector);
        return element ? [...document.querySelectorAll("#content *")].indexOf(element) : -1;
      };
      return {
        picker: index("#editor #service-app-picker"), error: index("#editor #service-wizard-error"),
        review: index("#editor #service-review"), footer: index("#editor .form-footer"),
      };
    });
    expect(order.picker).toBeGreaterThan(-1);
    expect(order.error).toBeGreaterThan(order.picker);
    expect(order.review).toBeGreaterThan(order.error);
    expect(order.footer).toBeGreaterThan(order.review);
    // The picker is the content of the first step; it is above the footer on screen too.
    const boxes = await panel.evaluate(() => ({
      picker: document.querySelector("#service-app-picker")!.getBoundingClientRect().bottom,
      footer: document.querySelector("#editor .form-footer")!.getBoundingClientRect().top,
    }));
    expect(boxes.picker).toBeLessThanOrEqual(boxes.footer + 1);
  });

  test("keeps the open tab, the open menu and keyboard focus through status polling", async ({ page }) => {
    const spec = { ...INITIAL, env: { PRIVATE_VALUE: "configuration-value-not-for-overview" } };
    await openServices(page, [spec]);
    const live = await routeLive(page, running(spec), [{ id: ACTIVE, spec, digest: `sha256:${"a".repeat(64)}` }]);
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    const panel = servicePanel(page);
    await expect(panel.locator("#service-status").getByRole("status")).toHaveText("Running");
    await expect(panel.locator("details.service-disclosure")).toHaveCount(0);
    await expect(panel).not.toContainText("configuration-value-not-for-overview");
    // A tab that was opened stays open, and focus stays on it, through the polls.
    const instances = panel.getByRole("tab", { name: /^Instances/ });
    await instances.click();
    await openMoreMenu(page);
    const stop = panel.getByRole("button", { name: "Stop service", exact: true });
    await expect(stop).toBeEnabled();
    // The open menu is the person's: a poll that changes nothing replaces nothing, so the button that has focus is the same node.
    for (const control of [instances, stop]) {
      await control.focus();
      const original = await control.elementHandle();
      const prior = live.reads.deployment;
      await expect.poll(() => live.reads.deployment).toBeGreaterThan(prior);
      await expect(control).toBeFocused();
      expect(await original?.evaluate(node => node.isConnected)).toBe(true);
      await original?.dispose();
    }
    await expect(instances).toHaveAttribute("aria-selected", "true");
    await expect(panel.locator("#vm-more-actions")).toHaveAttribute("open", "");
    await expect(panel.locator("table.service-instances")).toBeVisible();
  });

  test("keeps Stop available until an owned domain route is withdrawn", async ({ page }) => {
    await openServices(page, [INITIAL]);
    let deployment = { id: SECRET, serviceId: ID, spec: INITIAL, phase: "failed", domainRouteOwned: true, instances: [], log: [], error: "Could not withdraw the owned domain route" as string | null };
    await page.route(`**/api/micro-services/${ID}/deployment`, route => route.fulfill({ json: deployment }));
    await page.route(`**/api/micro-services/${ID}/stop`, route => {
      expect(route.request().method()).toBe("POST");
      deployment = { ...deployment, phase: "stopped", domainRouteOwned: false, error: null };
      return route.fulfill({ status: 202, json: deployment });
    });
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    await expect(page.locator("#service-status").getByRole("status")).toHaveText("Deployment failed");
    // The service still owns its route: the head offers no Deploy form, only the update, and Stop is in More.
    const head = servicePanel(page).locator(".detail-head");
    await expect(head.getByRole("button", { name: "Deploy", exact: true })).toHaveCount(0);
    await expect(head.getByRole("button", { name: "Deploy update", exact: true })).toBeVisible();
    await openMoreMenu(page);
    await expect(page.getByRole("button", { name: "Stop service", exact: true })).toBeEnabled();
    page.once("dialog", dialog => dialog.accept());
    await page.getByRole("button", { name: "Stop service", exact: true }).click();
    await expect(page.locator("#service-status").getByRole("status")).toHaveText("Stopped");
    await openMoreMenu(page);
    await expect(page.getByRole("button", { name: "Stop service", exact: true })).toHaveCount(0);
    await page.keyboard.press("Escape");
    await servicePanel(page).getByRole("button", { name: "Deploy", exact: true }).click();
    await expect(page.getByRole("button", { name: "Deploy and run", exact: true })).toBeEnabled();
  });

  test("uses Korean in the Delete menu and confirmation", async ({ page }) => {
    await openServices(page, [INITIAL]);
    await page.getByRole("combobox", { name: "Language", exact: true }).selectOption("ko");
    await page.getByLabel("orders 작업", { exact: true }).click();
    let confirmation = "";
    page.once("dialog", async dialog => { confirmation = dialog.message(); await dialog.dismiss(); });
    await page.getByRole("button", { name: "orders 삭제", exact: true }).click();
    expect(confirmation).toBe("orders 삭제?");
    await expect(page.getByRole("button", { name: "orders 열기", exact: true })).toBeVisible();
  });

  test("saves and removes a Queue binding without exposing credentials", async ({ page }) => {
    const writes = await openServices(page);
    await newServiceButton(page).click();
    await page.getByLabel("Name", { exact: true }).fill("orders");
    await selectServiceApp(page);
    await serviceStep(page, 3);
    await page.getByRole("button", { name: "Add Queue connection", exact: true }).click();
    await page.locator(".queue-binding-row").getByLabel("Alias", { exact: true }).fill("NOTIFICATIONS");
    await page.locator(".queue-binding-row").getByRole("combobox", { name: "Access", exact: true }).selectOption("consume");
    await saveService(page);
    await expect.poll(() => writes.length).toBe(1);
    expect(writes[0].body.queues).toEqual({ NOTIFICATIONS: { queueId: STORAGE, access: "consume" } });
    expect(JSON.stringify(writes[0].body)).not.toContain("amqp://");
    await serviceAction(page, "orders", "Edit");
    await serviceStep(page, 3);
    await page.locator(".queue-binding-row").getByRole("button", { name: "Remove", exact: true }).click();
    await saveService(page);
    await expect.poll(() => writes.length).toBe(2);
    expect(writes[1].body.queues).toBeUndefined();
  });

  test("binds and clears an S3 connection while preserving its security group", async ({
    page,
  }) => {
    const writes = await openServices(page);
    await newServiceButton(page)
      .click();
    await page.getByLabel("Name", { exact: true }).fill("orders");
    await selectServiceApp(page, "ghcr.io/team/orders:1.0");
    await serviceStep(page, 1);
    await page
      .getByLabel("Security group", { exact: true })
      .selectOption(SECRET);
    await serviceStep(page, 3);
    await page
      .getByLabel("S3 connection", { exact: true })
      .selectOption(STORAGE);
    await saveService(page);
    await expect(
      page.getByRole("cell", { name: /^orders(\s|$)/ }),
    ).toBeVisible();
    expect(writes[0].body.storageConnectionId).toBe(STORAGE);
    expect(writes[0].body.securityGroupId).toBe(SECRET);
    await serviceAction(page, "orders", "Edit");
    await serviceStep(page, 3);
    await page
      .getByLabel("S3 connection", { exact: true })
      .selectOption("");
    await saveService(page);
    await expect.poll(() => writes.length).toBe(2);
    expect(writes[1].body).not.toHaveProperty("storageConnectionId");
    expect(writes[1].body.securityGroupId).toBe(SECRET);
    expect(JSON.stringify(writes)).not.toContain("secretAccessKey");
  });
  test("deploys from the runtime panel, reports health, and stops the service", async ({
    page,
  }, testInfo) => {
    const service = { ...INITIAL, domain: null, replicas: 1 };
    await openServices(page, [service]);
    let deployment: Record<string, unknown> | null = null;
    let read = 0;
    let statusReads = 0;
    const instances = [
      {
        vmId: SECRET,
        ipv4: "172.30.0.2",
        hostPort: 30100,
        state: "running",
        healthy: true,
      },
    ];
    await page.route(
      `**/api/micro-services/${ID}/deployment`,
      async (route) => {
        if (route.request().method() === "POST") {
          expect(route.request().postDataJSON()).toEqual({
            hostPort: 30100,
            microNetworkId: null,
          });
          deployment = {
            id: SECRET,
            serviceId: ID,
            spec: service,
            phase: "queued",
            request: { hostPort: 30100 },
            digest: `sha256:${"a".repeat(64)}`,
            instances: [],
            log: ["Deployment queued"],
            error: null,
          };
          read = 0;
          return route.fulfill({ status: 202, json: deployment });
        }
        statusReads++;
        if (deployment?.phase === "queued" && ++read >= 1)
          deployment = {
            ...deployment,
            phase: "running",
            instances,
            log: ["All replicas answered the app health check"],
          };
        return route.fulfill({ json: deployment });
      },
    );
    await page.route(`**/api/micro-services/${ID}/stop`, async (route) => {
      expect(route.request().method()).toBe("POST");
      deployment = {
        ...deployment,
        phase: "stopped",
        instances: [],
        log: ["Service instances removed"],
      };
      return route.fulfill({ status: 202, json: deployment });
    });
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    await expect(
      page.locator("#service-status").getByRole("status"),
    ).toHaveText("Not deployed");
    // The panel opens on its Overview. The Deploy form starts from the head.
    await expect(page.locator("#service-deploy-form")).toHaveCount(0);
    await servicePanel(page).getByRole("button", { name: "Deploy", exact: true }).click();
    await expect(
      page.getByText(
        "Secret values are injected into every replica. Changes apply on the next deployment.",
        { exact: true },
      ),
    ).toBeVisible();
    await expect(
      page.getByText("Secret injection is not available yet.", {
        exact: false,
      }),
    ).toHaveCount(0);

    await page
      .getByRole("button", { name: "Deploy and run", exact: true })
      .click();
    await expect(
      page.locator("#service-status").getByRole("status"),
    ).toHaveText("Running");
    // The form closes once the deployment is accepted.
    await expect(page.locator("#editor")).toHaveCount(0);
    await expect(page.locator("details.service-disclosure")).toHaveCount(0);
    await servicePanel(page).getByRole("tab", { name: /^Instances/ }).click();
    await expect(
      page.getByRole("cell", { name: /^Healthy(\s|$)/ }),
    ).toBeVisible();
    await openMoreMenu(page);
    const stop = page.getByRole("button", { name: "Stop service", exact: true });
    await stop.focus();
    const before = statusReads;
    await expect.poll(() => statusReads).toBeGreaterThan(before);
    await expect(stop).toBeFocused();
    await page.screenshot({
      path: testInfo.outputPath("service-running-desktop.png"),
      fullPage: true,
    });
    await page.setViewportSize({ width: 390, height: 844 });
    await expect(newServiceButton(page)).toBeHidden();
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= window.innerWidth,
      ),
    ).toBe(true);
    await page.screenshot({
      path: testInfo.outputPath("service-running-mobile.png"),
      fullPage: true,
    });
    page.once("dialog", (dialog) => dialog.accept());
    await page
      .getByRole("button", { name: "Stop service", exact: true })
      .click();
    await servicePanel(page).getByRole("tab", { name: "Overview" }).click();
    await expect(
      page.locator("#service-status").getByRole("status"),
    ).toHaveText("Stopped");
    await expect(
      page.getByRole("button", { name: "Deploy", exact: true }),
    ).toBeEnabled();
    await page.getByRole("button", { name: "Close detail", exact: true }).click();
    await expect(newServiceButton(page)).toBeVisible();
  });

  test("accepts a status response slower than the polling interval", async ({
    page,
  }) => {
    await openServices(page, [INITIAL]);
    let activeReads = 0;
    let maxActiveReads = 0;
    await page.route(
      `**/api/micro-services/${ID}/deployment`,
      async (route) => {
        activeReads += 1;
        maxActiveReads = Math.max(maxActiveReads, activeReads);
        await new Promise((resolve) => setTimeout(resolve, 4000));
        await route.fulfill({ json: null });
        activeReads -= 1;
      },
    );
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    await expect(
      page.locator("#service-status").getByRole("status"),
    ).toHaveText("Not deployed");
    expect(maxActiveReads).toBe(1);
  });

  test("shows a missing Secret error before a deployment is accepted", async ({
    page,
  }) => {
    const service = { ...INITIAL, domain: null, replicas: 1 };
    await openServices(page, [service]);
    let submitted = false;
    await page.route(
      `**/api/micro-services/${ID}/deployment`,
      async (route) => {
        if (route.request().method() === "POST") {
          submitted = true;
          return route.fulfill({
            status: 400,
            json: {
              error: {
                code: "validation_failed",
                message: "request validation failed",
                fields: { secrets: "a referenced PipeSecret does not exist" },
              },
            },
          });
        }
        return route.fulfill({ json: null });
      },
    );
    await openDeployForm(page);
    await expect(page.getByRole("status")).toHaveText("Not deployed");
    await page
      .getByRole("button", { name: "Deploy and run", exact: true })
      .click();
    await expect(page.getByRole("alert")).toContainText(
      "a referenced PipeSecret does not exist",
    );
    await expect(page.getByRole("status")).toHaveText("Not deployed");
    await expect(
      page.getByRole("button", { name: "Deploy and run", exact: true }),
    ).toBeEnabled();
    expect(submitted).toBe(true);
  });

  test("closes stale runtime settings after saving and preserves mobile headers", async ({
    page,
  }) => {
    const writes = await openServices(page, [
      { ...INITIAL, domain: null, securityGroupId: SECRET },
    ]);
    await page.route(`**/api/micro-services/${ID}/deployment`, (route) =>
      route.fulfill({ json: null }),
    );
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    await expect(
      page.locator("#service-status").getByRole("status"),
    ).toHaveText("Not deployed");
    await serviceAction(page, "orders", "Edit");
    await serviceStep(page, 1);
    await page.getByLabel("Port", { exact: true }).fill("9090");
    await serviceStep(page, 2);
    await page.getByLabel("Replicas", { exact: true }).fill("3");
    await saveService(page);
    await expect(
      page.getByRole("heading", { name: "Deploy MicroService", exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("heading", { name: "Edit MicroService", exact: true }),
    ).toHaveCount(0);
    expect(writes[0].body.securityGroupId).toBe(SECRET);

    await expect(
      page.getByText(
        "Leave the host port empty for internal access. App port: 9090.",
        { exact: true },
      ),
    ).toBeVisible();
    await expect(page.getByLabel("First host port")).toHaveAttribute(
      "max",
      "65533",
    );
    await page.setViewportSize({ width: 390, height: 844 });
    await expect(
      page.getByRole("columnheader", { name: "Port", exact: true, includeHidden: true }),
    ).toHaveCount(1);
    await expect(page.getByRole("cell", { name: /9090/, includeHidden: true })).toHaveAttribute(
      "headers",
      "service-column-2",
    );
  });
  const browserErrors = new WeakMap<Page, string[]>();
  test.beforeEach(({ page }) => {
    const errors: string[] = [];
    browserErrors.set(page, errors);
    page.on("pageerror", (error) => errors.push(error.message));
    page.on("console", (message) => {
      // The validation test intentionally receives this HTTP error.
      const expectedValidation =
        "Failed to load resource: the server responded with a status of 400 (Bad Request)";
      if (message.type() === "error" && message.text() !== expectedValidation)
        errors.push(message.text());
    });
  });
  test.afterEach(({ page }) => expect(browserErrors.get(page)).toEqual([]));
  test("shows a failed deployment and allows a retry without claiming success", async ({
    page,
  }) => {
    await openServices(page, [{ ...INITIAL, domain: null, secrets: {} }]);
    await page.route(`**/api/micro-services/${ID}/deployment`, (route) =>
      route.fulfill({
        json: {
          id: SECRET,
          serviceId: ID,
          spec: INITIAL,
          request: { hostPort: 30200 },
          phase: "failed",
          digest: null,
          instances: [],
          log: ["App health check failed"],
          error: "App health check failed",
        },
      }),
    );
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    await expect(
      page.locator("#service-status").getByRole("status"),
    ).toHaveText("Deployment failed");
    await expect(page.getByRole("alert")).toHaveText("App health check failed");
    await servicePanel(page).getByRole("button", { name: "Deploy", exact: true }).click();
    await expect(
      page.getByRole("button", { name: "Deploy and run", exact: true }),
    ).toBeEnabled();
    await expect(
      page.getByRole("button", { name: "Stop service", exact: true }),
    ).toHaveCount(0);
  });
  test("creates, reads, edits and deletes configurations", async ({
    page,
  }, testInfo) => {
    const writes = await openServices(page);
    await expect(page.getByText("No MicroServices yet", { exact: true })).toBeVisible();
    await newServiceButton(page)
      .click();
    await page.getByLabel("Name", { exact: true }).fill("orders");
    await selectServiceApp(page, "ghcr.io/team/orders:1.0");
    await serviceStep(page, 3);
    await page
      .getByLabel("Environment (NAME=value)", { exact: true })
      .fill("PROFILE=prod");
    await serviceStep(page, 3);
    await page
      .getByLabel("Secret references (NAME=CrabSecrets ID)", { exact: true })
      .fill(`DB_PASSWORD=${SECRET}`);
    await page.screenshot({
      path: testInfo.outputPath("microservice-desktop.png"),
      fullPage: true,
    });
    await saveService(page);
    await expect(
      page.getByRole("cell", { name: /^orders(\s|$)/ }),
    ).toBeVisible();
    expect(writes[0]).toEqual({
      method: "POST",
      body: {
        name: "orders",
        image: "ghcr.io/team/orders:1.0",
        runtime: "vm",
        securityGroupId: null,
        port: 8080,
        healthPath: "/health",
        domain: null,
        ram: 512,
        cpu: 1,
        replicas: 1,
        startupTimeout: 300,
        drainTimeout: 30,
        env: { PROFILE: "prod" },
        secrets: { DB_PASSWORD: SECRET },
      },
    });

    await serviceAction(page, "orders", "Edit");
    await expect(
      page.getByLabel("Secret references (NAME=CrabSecrets ID)", {
        exact: true,
      }),
    ).toHaveValue(`DB_PASSWORD=${SECRET}`);
    await selectServiceApp(page, "ghcr.io/team/orders:2.0");
    await serviceStep(page, 2);
    await page.getByLabel("Replicas", { exact: true }).fill("2");
    await saveService(page);
    await expect(
      page.getByTitle("ghcr.io/team/orders:2.0", { exact: true }),
    ).toBeVisible();
    expect(writes[1].method).toBe("PUT");
    expect(writes[1].body).not.toHaveProperty("id");
    expect(writes[1].body.replicas).toBe(2);
    page.once("dialog", (dialog) => dialog.accept());
    await serviceAction(page, "orders", "Delete");
    await expect(page.getByText("No MicroServices yet", { exact: true })).toBeVisible();
  });

  test("keeps edits after validation failure and cancels without saving", async ({
    page,
  }) => {
    const writes = await openServices(page, [INITIAL], true);
    await serviceAction(page, "orders", "Edit");
    await expect(page.getByLabel("Health path", { exact: true })).toHaveValue(
      "/actuator/health",
    );
    await expect(page.getByLabel("Startup timeout (seconds)")).toHaveValue(
      "120",
    );
    await expect(page.getByLabel("Drain timeout (seconds)")).toHaveValue("60");
    await page.getByLabel("Name", { exact: true }).fill("billing");
    await saveService(page);
    await expect(
      page.getByText("is already registered", { exact: true }),
    ).toBeVisible();
    await expect(page.getByLabel("Name", { exact: true })).toHaveValue(
      "billing",
    );
    await page.getByRole("button", { name: "Cancel", exact: true }).click();
    await expect(
      page.getByRole("cell", { name: /^orders(\s|$)/ }),
    ).toBeVisible();
    expect(writes).toHaveLength(1);
  });

  test("supports Korean and mobile editing without page overflow", async ({
    page,
  }, testInfo) => {
    await openServices(page, [INITIAL]);
    await page
      .getByRole("combobox", { name: "Language", exact: true })
      .selectOption("ko");
    await page.setViewportSize({ width: 390, height: 844 });
    await page.getByLabel("orders 작업", { exact: true }).click();
    await page
      .getByRole("button", { name: "orders 수정", exact: true })
      .click();
    await expect(
      page.getByRole("heading", { name: "MicroService 편집", exact: true }),
    ).toBeVisible();
    await serviceStep(page, 1);
    await expect(page.getByLabel("상태 확인 경로")).toHaveValue(
      "/actuator/health",
    );
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= window.innerWidth,
      ),
    ).toBe(true);
    expect(
      await page
        .locator(".service-list")
        .evaluate((element) => element.scrollWidth <= element.clientWidth),
    ).toBe(true);
    await page.screenshot({
      path: testInfo.outputPath("microservice-mobile.png"),
      fullPage: true,
    });
  });
});

test("domain deployment inherits the security-group network and shows DNS guidance on mobile", async ({
  page,
}, testInfo) => {
  const service = {
    ...INITIAL,
    replicas: 1,
    secrets: {},
    securityGroupId: SECRET,
  };
  await openServices(page, [service]);
  let deployment: Record<string, unknown> | null = null;
  await page.route(`**/api/micro-services/${ID}/deployment`, async (route) => {
    if (route.request().method() === "POST") {
      expect(route.request().postDataJSON()).toEqual({ microNetworkId: ID });
      deployment = {
        id: SECRET,
        serviceId: ID,
        spec: service,
        phase: "running",
        request: { hostPort: 0 },
        domainRouteOwned: true,
        instances: [
          {
            vmId: SECRET,
            ipv4: "172.30.0.2",
            hostPort: 0,
            state: "running",
            healthy: true,
          },
        ],
        digest: null,
        error: null,
        log: ["Domain connected"],
      };
      return route.fulfill({ status: 202, json: deployment });
    }
    return route.fulfill({ json: deployment });
  });
  await page.route("**/api/pipe", (route) =>
    route.fulfill({
      json: {
        config: {
          listen: "127.0.0.1:8080",
          routes: [],
          tunnel: { id: SECRET, credentialsFile: "hidden" },
        },
        units: [],
        nginxAvailable: true,
        cloudflaredAvailable: true,
      },
    }),
  );
  await page.getByRole("button", { name: "Open orders", exact: true }).click();
  await expect(page.getByRole("status")).toHaveText("Not deployed");
  await expect(
    page.getByRole("link", { name: "Open domain", exact: true }),
  ).toHaveCount(0);
  await servicePanel(page).getByRole("button", { name: "Deploy", exact: true }).click();
  await expect(page.getByLabel("First host port")).toHaveCount(0);
  await expect(page.getByLabel("Network", { exact: true })).not.toBeVisible();
  await page.getByText("Advanced settings", { exact: true }).click();
  await expect(page.getByLabel("Network", { exact: true })).toHaveValue(ID);
  await expect(page.getByLabel("Network", { exact: true })).toBeDisabled();
  await page
    .getByRole("button", { name: "Deploy and run", exact: true })
    .click();
  await expect(
    page.getByRole("link", { name: "Open domain", exact: true }),
  ).toHaveAttribute("href", "https://orders.example.com");
  await expect(page.getByRole("link", { name: ":0", exact: true })).toHaveCount(
    0,
  );
  // The DNS guidance is in the Settings tab.
  await servicePanel(page).getByRole("tab", { name: "Settings" }).click();
  await expect(
    page.getByText(`${SECRET}.cfargotunnel.com`, { exact: true }),
  ).toBeVisible();
  await servicePanel(page).getByRole("tab", { name: "Overview" }).click();
  await page.setViewportSize({ width: 390, height: 844 });
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= window.innerWidth,
    ),
  ).toBe(true);
  await page.screenshot({
    path: testInfo.outputPath("service-domain-mobile.png"),
    fullPage: true,
  });
  await page.setViewportSize({ width: 1280, height: 900 });
  await page
    .getByRole("combobox", { name: "Language", exact: true })
    .selectOption("ko");
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(
    page.getByRole("link", { name: "도메인 열기", exact: true }),
  ).toBeVisible();
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= window.innerWidth,
    ),
  ).toBe(true);
});

test("accepts a pasted domain URL", async ({ page }) => {
  const writes = await openServices(page);
  await newServiceButton(page)
    .click();
  await page.getByLabel("Name", { exact: true }).fill("web");
  await selectServiceApp(page, "ghcr.io/team/nginx:stable-alpine");
  await serviceStep(page, 1);
  await page
    .getByLabel("Domain", { exact: true })
    .fill("https://Orders.Example.COM/");
  await saveService(page);
  await expect.poll(() => writes.length).toBe(1);
  expect(writes[0].body.domain).toBe("orders.example.com");
});

test("AUTH setup refuses a save after the account is disconnected", async ({
  page,
}) => {
  const accounts: object[] = [
    {
      provider: "github",
      account: "team",
      registry: "ghcr.io",
      region: null,
      services: ["registry"],
    },
  ];
  const writes = await openServices(page, [], false, accounts);
  await newServiceButton(page)
    .click();
  await page.getByLabel("Name", { exact: true }).fill("orders");
  await selectServiceApp(page);
  await serviceStep(page, 4);
  accounts.length = 0;
  await page.locator("#editor button[type=submit]").click();
  await expect(page.getByRole("alert")).toContainText(
    "AUTH account is no longer connected",
  );
  expect(writes).toHaveLength(0);
  await expect(page.locator("#service-steps")).toHaveAttribute(
    "data-step",
    "0",
  );
});

test("keeps a console account out of the app's registry account choices", async ({ page }) => {
  const account = { provider: "firecrab", account: "owner@example.com", registry: null, region: null, services: [] };
  const accounts: object[] = [account];
  await openServices(page, [], false, accounts);
  await newServiceButton(page).click();
  await expect(page.getByText("Connect an account first", { exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "Next", exact: true })).toBeDisabled();
  accounts.push({ provider: "github", account: "team", registry: "ghcr.io", region: null, services: ["registry"] });
  await page.getByRole("button", { name: "Check connections", exact: true }).click();
  await expect(page.getByLabel("AUTH account", { exact: true }).getByRole("option", { name: /owner@example.com/ })).toHaveCount(0);
  await selectServiceApp(page);
  await expect(page.getByLabel("App version", { exact: true })).toHaveValue("1.0");
});

test("AUTH setup retains repository and version pages without accepting unlisted tags", async ({
  page,
}) => {
  await openServices(page);
  const queries: string[] = [];
  await page.route("**/repositories?*", (route) => {
    const url = new URL(route.request().url());
    queries.push(url.search);
    return route.fulfill({
      json: url.searchParams.has("cursor")
        ? {
            repositories: ["team/billing"],
            listingSupported: true,
            nextPage: null,
          }
        : {
            repositories: ["team/orders"],
            listingSupported: true,
            nextPage: 2,
            nextCursor: "opaque+/=",
          },
    });
  });
  await page.route("**/registry-tags?*", (route) =>
    route.fulfill({
      json: new URL(route.request().url()).searchParams.has("last")
        ? { tags: ["2.0"], nextLast: null }
        : { tags: ["1.0"], nextLast: "1.0" },
    }),
  );
  await newServiceButton(page)
    .click();
  await page.getByLabel("Name", { exact: true }).fill("billing");
  await page
    .getByRole("button", { name: "More repositories", exact: true })
    .click();
  await page
    .getByLabel("App repository", { exact: true })
    .selectOption("team/billing");
  await page
    .getByRole("button", { name: "More versions", exact: true })
    .click();
  await page.getByLabel("App version", { exact: true }).selectOption("2.0");
  await expect(
    page.getByLabel("App repository", { exact: true }).locator("option"),
  ).toHaveCount(3);
  await expect(
    page.getByLabel("App version", { exact: true }).locator("option"),
  ).toHaveCount(3);
  expect(new URLSearchParams(queries.at(-1)).get("cursor")).toBe("opaque+/=");
  await serviceStep(page, 4);
  await expect(page.locator(".service-review")).toContainText(
    "ghcr.io/team/billing:2.0",
  );
});

test("AUTH recovery restores deployment choices without automatically deploying", async ({
  page,
}) => {
  const accounts: object[] = [];
  const writes = await openServices(
    page,
    [{ ...INITIAL, domain: null, securityGroupId: null }],
    false,
    accounts,
  );
  await page.route("**/api/micro-networks", (route) =>
    route.fulfill({
      json: [
        { id: STORAGE, name: "chosen-network", subnetCidr: "172.31.0.0/24" },
      ],
    }),
  );
  await page.route("**/api/pipe/auth/github", (route) => {
    expect(route.request().postDataJSON().token).toBe("fixture-only-token");
    const account = {
      provider: "github",
      account: "team",
      registry: "ghcr.io",
      region: null,
      services: ["registry"],
    };
    accounts.push(account);
    return route.fulfill({ json: account });
  });
  await openDeployForm(page);
  await page.getByLabel("Network", { exact: true }).selectOption(STORAGE);
  await page.getByLabel("First host port", { exact: true }).fill("31234");
  await expect(
    page.getByRole("button", { name: "Deploy and run", exact: true }),
  ).toBeDisabled();
  // The AUTH check above the form is a row with room between its link and its button, and under it.
  const link = await page.getByRole("link", { name: "Reconnect AUTH", exact: true }).boundingBox();
  const check = await page.getByRole("button", { name: "Check again", exact: true }).boundingBox();
  expect(check!.x - (link!.x + link!.width)).toBeGreaterThanOrEqual(8);
  const form = await page.locator("#service-deploy-form").boundingBox();
  expect(form!.y - (check!.y + check!.height)).toBeGreaterThanOrEqual(12);
  await page.getByRole("link", { name: "Reconnect AUTH", exact: true }).click();
  await page
    .getByRole("button", { name: "Connect GitHub", exact: true })
    .click();
  await page.getByLabel("Token", { exact: true }).fill("fixture-only-token");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page).toHaveURL(/#\/services$/);
  await expect(page.getByLabel("First host port", { exact: true })).toHaveValue(
    "31234",
  );
  await expect(page.getByLabel("Network", { exact: true })).toHaveValue(
    STORAGE,
  );
  await expect(
    page.getByRole("button", { name: "Deploy and run", exact: true }),
  ).toBeEnabled();
  expect(writes).toHaveLength(0);
  expect(
    await page.evaluate(() =>
      JSON.stringify({
        history: history.state,
        local: { ...localStorage },
        session: { ...sessionStorage },
      }),
    ),
  ).not.toContain("fixture-only-token");
});

test("placement ignores an older port result and checks the final request again", async ({
  page,
}) => {
  await openServices(page, [
    { ...INITIAL, domain: null, securityGroupId: null },
  ]);
  let release: () => void = () => {};
  const gate = new Promise<void>((resolve) => (release = resolve));
  let delayed = false;
  let posts = 0;
  let checks = 0;
  await page.route(
    `**/api/micro-services/${ID}/deployment/check`,
    async (route) => {
      const port = route.request().postDataJSON().hostPort;
      checks++;
      if (port === 31234) {
        delayed = true;
        await gate;
        return route.fulfill({
          status: 422,
          json: {
            error: {
              message: "occupied",
              fields: { hostPort: "31235 is occupied" },
            },
          },
        });
      }
      return route.fulfill({ json: null });
    },
  );
  await page.route(`**/api/micro-services/${ID}/deployment`, (route) => {
    if (route.request().method() === "POST") {
      posts++;
      return route.fulfill({
        json: { phase: "queued", instances: [], log: [] },
      });
    }
    return route.fulfill({ json: null });
  });
  await openDeployForm(page);
  await expect(
    page.getByRole("button", { name: "Deploy and run", exact: true }),
  ).toBeEnabled();
  await page.getByLabel("First host port", { exact: true }).fill("31234");
  await expect.poll(() => delayed).toBe(true);
  await page.getByLabel("First host port", { exact: true }).fill("32000");
  await expect(
    page.getByRole("button", { name: "Deploy and run", exact: true }),
  ).toBeEnabled();
  release();
  await expect(page.locator("#service-deploy-port-error")).toHaveCount(0);
  const before = checks;
  await page
    .getByRole("button", { name: "Deploy and run", exact: true })
    .click();
  await expect.poll(() => posts).toBe(1);
  expect(checks).toBeGreaterThan(before);
});

const RELEASE_A = "11111111-1111-4111-8111-111111111111";
const RELEASE_B = "22222222-2222-4222-8222-222222222222";
const DEPLOY_SPEC = { ...INITIAL, cpu: 1, ram: 128, replicas: 1, secrets: {} };
function release(id: string, image = "ghcr.io/team/orders:1.0") {
  return { id, serviceId: ID, phase: "running", spec: { ...DEPLOY_SPEC, image }, request: { microNetworkId: ID, hostPort: 0 },
    digest: `sha256:${(id === RELEASE_A ? "a" : "b").repeat(64)}`, instances: [{ vmId: SECRET, ipv4: "172.30.0.2", hostPort: 0, state: "running", healthy: true }],
    domainRouteOwned: true, log: ["All replicas answered the app health check"], error: null, startedAt: 1792886400, finishedAt: 1792886410 };
}

test.describe("Deploy release management @dashboard", () => {
  test("cancels a queued private initial Deploy and allows a new Deploy", async ({ page }, testInfo) => {
    const spec = { ...DEPLOY_SPEC, domain: null };
    await openServices(page, [{ id: ID, ...spec }]);
    let current: object = { ...release(RELEASE_B), spec, phase: "queued", instances: [], domainRouteOwned: false,
      internalRouteOwned: false, finishedAt: null };
    let cancellations = 0;
    let deployments = 0;
    let releaseCancel!: () => void;
    const cancelGate = new Promise<void>(resolve => { releaseCancel = resolve; });
    await page.route(`**/api/micro-services/${ID}/deployment`, route => {
      if (route.request().method() === "POST") {
        deployments++;
        current = { ...release(RELEASE_A), spec, domainRouteOwned: false, internalRouteOwned: true };
        return route.fulfill({ status: 202, json: current });
      }
      return route.fulfill({ json: current });
    });
    await page.route(`**/api/micro-services/${ID}/deployments/${RELEASE_B}/cancel`, async route => {
      expect(route.request().method()).toBe("POST");
      expect(route.request().postData()).toBeNull();
      cancellations++;
      await cancelGate;
      current = { ...current, phase: "cancelled", finishedAt: 1792886420 };
      return route.fulfill({ json: current });
    });
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    const cancel = page.locator("#deploy-cancel");
    await expect(cancel).toHaveAccessibleName("Cancel");
    await expect(cancel).toHaveAttribute("title", "Cancel");
    await expect(cancel).toHaveText("");
    await expect(cancel.locator("svg")).toHaveCount(1);
    await openMoreMenu(page);
    await expect(page.getByRole("button", { name: "Stop service", exact: true })).toBeDisabled();
    const screenshot = testInfo.outputPath("deploy-queue-cancel-desktop.png");
    await page.screenshot({ path: screenshot, fullPage: true });
    await testInfo.attach("Queued initial Deploy cancellation", { path: screenshot, contentType: "image/png" });
    await cancel.click();
    await expect.poll(() => cancellations).toBe(1);
    await expect(cancel).toBeDisabled();
    releaseCancel();
    await expect(page.locator("#service-status").getByRole("status")).toHaveText("Cancelled");
    await expect(cancel).toHaveCount(0);
    await expect(page.getByRole("button", { name: "Stop service", exact: true })).toHaveCount(0);
    // Deploying again starts from the head of the panel.
    await servicePanel(page).getByRole("button", { name: "Deploy", exact: true }).click();
    await expect(page.getByRole("button", { name: "Deploy and run", exact: true })).toBeEnabled();
    await page.getByRole("button", { name: "Deploy and run", exact: true }).click();
    await expect.poll(() => deployments).toBe(1);
    expect(cancellations).toBe(1);
  });

  test("reloads the serving release after cancelling a queued update in Korean", async ({ page }, testInfo) => {
    const active = release(RELEASE_A);
    const queued = { ...release(RELEASE_B, "ghcr.io/team/orders:2.0"), phase: "queued", instances: [],
      domainRouteOwned: false, update: { previous: active, requestedSpec: DEPLOY_SPEC, strategy: "rolling",
        rollbackOf: null, cancelled: false, restoring: false, routedNew: false, transition: null, retiring: [], completed: false } };
    await openServices(page, [{ id: ID, ...active.spec }]);
    let current: object = queued;
    let cancelled = false;
    let refreshed = false;
    await page.route(`**/api/micro-services/${ID}/deployment`, route => {
      if (cancelled) refreshed = true;
      return route.fulfill({ json: current });
    });
    await page.route(`**/api/micro-services/${ID}/deployments/${RELEASE_B}/cancel`, route => {
      expect(route.request().method()).toBe("POST");
      expect(route.request().postData()).toBeNull();
      cancelled = true;
      current = active;
      return route.fulfill({ json: { ...queued, phase: "cancelled", update: { ...queued.update, cancelled: true, completed: true } } });
    });
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    await expect(page.locator("#deploy-cancel")).toBeEnabled();
    await page.getByRole("combobox", { name: "Language", exact: true }).selectOption("ko");
    const cancel = page.locator("#deploy-cancel");
    await expect(cancel).toHaveAccessibleName("취소");
    await expect(cancel).toHaveAttribute("title", "취소");
    await page.setViewportSize({ width: 390, height: 844 });
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    const screenshot = testInfo.outputPath("deploy-queue-cancel-mobile.png");
    await page.screenshot({ path: screenshot, fullPage: true });
    await testInfo.attach("Queued update cancellation", { path: screenshot, contentType: "image/png" });
    await cancel.click();
    await expect(page.locator("#service-status").getByRole("status")).toHaveText("실행 중");
    expect(refreshed).toBe(true);
    await expect(cancel).toHaveCount(0);
    await servicePanel(page).locator("#vm-more-summary").click();
    await expect(page.getByRole("button", { name: "서비스 중지", exact: true })).toBeEnabled();
    await expect(page.getByRole("button", { name: "업데이트 배포", exact: true })).toBeEnabled();
    await expect(page.getByText("배포 실패", { exact: true })).toHaveCount(0);
  });

  test("withdraws Cancel when a queued operation starts and reports the conflict", async ({ page }) => {
    await openServices(page, [DEPLOY_SPEC]);
    let current = { ...release(RELEASE_B), phase: "queued", instances: [], domainRouteOwned: false };
    let cancellations = 0;
    await page.route(`**/api/micro-services/${ID}/deployment`, route => route.fulfill({ json: current }));
    await page.route(`**/api/micro-services/${ID}/deployments/${RELEASE_B}/cancel`, route => {
      cancellations++;
      current = { ...current, phase: "importing" };
      return route.fulfill({ status: 409, json: { error: { code: "deployment_not_queued", message: "This Deploy has already started" } } });
    });
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    await page.locator("#deploy-cancel").click();
    await expect(page.getByText("HTTP 409: This Deploy has already started", { exact: true })).toBeVisible();
    await expect(page.locator("#service-status [data-phase=importing]")).toBeVisible();
    await expect(page.locator("#deploy-cancel")).toHaveCount(0);
    await openMoreMenu(page);
    await expect(page.getByRole("button", { name: "Stop service", exact: true })).toBeDisabled();
    expect(cancellations).toBe(1);
  });

  test("ignores a delayed cancellation refresh after navigation to a newer operation", async ({ page }) => {
    test.setTimeout(45_000);
    const reports = { ...DEPLOY_SPEC, id: STORAGE, name: "reports", domain: "reports.example.com" };
    await openServices(page, [{ id: ID, ...DEPLOY_SPEC }, { id: STORAGE, ...reports }]);
    const ordersOperation = { ...release(RELEASE_B), phase: "queued", instances: [], domainRouteOwned: false };
    let reportsOperation = { ...release(RELEASE_A), serviceId: STORAGE, spec: reports, phase: "queued", instances: [], domainRouteOwned: false };
    let ordersCancelled = false;
    let oldRefreshEntered = false;
    let reportsCancellationEntered = false;
    let releaseOldRefresh!: () => void;
    let releaseReports!: () => void;
    const oldRefreshGate = new Promise<void>(resolve => { releaseOldRefresh = resolve; });
    const reportsGate = new Promise<void>(resolve => { releaseReports = resolve; });
    await page.route(`**/api/micro-services/${STORAGE}`, route => route.fulfill({ json: { id: STORAGE, ...reports } }));
    await page.route(`**/api/micro-services/${ID}/deployment`, async route => {
      if (ordersCancelled) {
        oldRefreshEntered = true;
        await oldRefreshGate;
      }
      return route.fulfill({ json: { ...ordersOperation, phase: ordersCancelled ? "cancelled" : "queued" } });
    });
    await page.route(`**/api/micro-services/${ID}/deployments/${RELEASE_B}/cancel`, route => {
      ordersCancelled = true;
      return route.fulfill({ json: { ...ordersOperation, phase: "cancelled" } });
    });
    await page.route(`**/api/micro-services/${STORAGE}/deployment`, route => route.fulfill({ json: reportsOperation }));
    await page.route(`**/api/micro-services/${STORAGE}/deployments/${RELEASE_A}/cancel`, async route => {
      reportsCancellationEntered = true;
      await reportsGate;
      reportsOperation = { ...reportsOperation, phase: "cancelled" };
      return route.fulfill({ json: reportsOperation });
    });
    try {
      await page.getByRole("button", { name: "Open orders", exact: true }).click();
      await page.locator("#deploy-cancel").click();
      await expect.poll(() => oldRefreshEntered).toBe(true);
      await page.evaluate(() => { location.hash = "/vms"; });
      await expect(page.getByRole("heading", { name: "MicroVM", exact: true })).toBeVisible();
      await page.evaluate(() => { location.hash = "/services"; });
      await page.getByRole("button", { name: "Open reports", exact: true }).click();
      const cancel = page.locator("#deploy-cancel");
      await expect(cancel).toBeEnabled();
      await cancel.click();
      await expect.poll(() => reportsCancellationEntered).toBe(true);
      await expect(cancel).toBeDisabled();
      const oldResponse = page.waitForResponse(response => new URL(response.url()).pathname === `/api/micro-services/${ID}/deployment`);
      releaseOldRefresh();
      await oldResponse;
      await page.evaluate(() => new Promise<void>(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
      await expect(cancel).toBeDisabled();
      await expect(page.getByRole("heading", { name: "reports", exact: true })).toBeVisible();
      releaseReports();
      await expect(page.locator("#service-status").getByRole("status")).toHaveText("Cancelled");
      await expect(cancel).toHaveCount(0);
    } finally {
      releaseOldRefresh();
      releaseReports();
    }
  });

  test("uses Deploy for a running update and preserves the reviewed release identity", async ({ page }) => {
    const writes = await openServices(page, [DEPLOY_SPEC]);
    let current: object = release(RELEASE_A);
    const updates: Record<string, any>[] = [];
    await page.route(`**/api/micro-services/${ID}/deployment`, route => route.fulfill({ json: current }));
    await page.route(`**/api/micro-services/${ID}/updates`, async route => {
      const body = route.request().postDataJSON(); updates.push(body);
      current = { ...release(body.operationId, body.spec.image), spec: body.spec, phase: "switching_traffic", update: {
        previous: release(RELEASE_A), requestedSpec: body.spec, strategy: body.strategy, rollbackOf: null,
        cancelled: false, restoring: false, routedNew: false, transition: null, retiring: [], completed: false,
      } };
      return route.fulfill({ status: 202, json: current });
    });
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    // The rocket in the head starts the update from any tab.
    await servicePanel(page).locator(".detail-head").getByRole("button", { name: "Deploy update", exact: true }).click();
    await expect(page.getByLabel("Name", { exact: true })).toHaveAttribute("readonly", "");
    await selectServiceApp(page, "ghcr.io/team/orders:2.0");
    await page.getByRole("button", { name: "Next", exact: true }).click();
    await expect(page.getByLabel("Domain", { exact: true })).toHaveAttribute("readonly", "");
    await page.getByRole("button", { name: "Next", exact: true }).click();
    // The size cards come first; the memory box is the custom card's.
    await page.getByRole("radio", { name: /Custom/ }).check();
    await page.getByLabel("Memory (MiB)", { exact: true }).fill("256");
    await page.getByRole("button", { name: "Next", exact: true }).click();
    await page.getByRole("button", { name: "Next", exact: true }).click();
    await page.getByRole("radiogroup", { name: "Deployment strategy", exact: true }).getByRole("radio", { name: "Blue-green — a full extra generation", exact: true }).check();
    await page.getByRole("button", { name: "Deploy update", exact: true }).click();
    await expect(page.getByText("Switching Nginx traffic", { exact: true })).toBeVisible();
    // The panel is back on the tab of the update.
    await expect(servicePanel(page).getByRole("tab", { name: "Releases" })).toHaveAttribute("aria-selected", "true");
    expect(updates).toHaveLength(1);
    expect(updates[0].expectedReleaseId).toBe(RELEASE_A);
    expect(updates[0].operationId).toMatch(/^[0-9a-f-]{36}$/);
    expect(updates[0].strategy).toBe("blue_green");
    expect(updates[0].spec.image).toBe("ghcr.io/team/orders:2.0");
    expect(updates[0].spec.ram).toBe(256);
    expect(updates[0].spec.env).toEqual(DEPLOY_SPEC.env);
    expect(updates[0].spec).not.toHaveProperty("id");
    expect(writes).toHaveLength(0);
  });

  test("selects a previous release and reuses the rollback identity after a network failure", async ({ page }, testInfo) => {
    const active = release(RELEASE_B, "ghcr.io/team/orders:2.0");
    await openServices(page, [{ id: ID, ...active.spec }]);
    const rollbacks: Record<string, any>[] = [];
    await page.route(`**/api/micro-services/${ID}/deployment`, route => route.fulfill({ json: active }));
    await page.route(`**/api/micro-services/${ID}/releases`, route => route.fulfill({ json: [active, release(RELEASE_A)] }));
    await page.route(`**/api/micro-services/${ID}/rollback`, async route => {
      const body = route.request().postDataJSON(); rollbacks.push(body);
      if (rollbacks.length === 1) return route.abort("failed");
      return route.fulfill({ status: 202, json: { ...release(body.operationId), phase: "queued", instances: [], update: {
        previous: active, requestedSpec: DEPLOY_SPEC, strategy: body.strategy, rollbackOf: RELEASE_A,
        cancelled: false, restoring: false, routedNew: false, transition: null, retiring: [], completed: false,
      } } });
    });
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    // The Releases tab lists the releases; the head's Roll back only opens it.
    await servicePanel(page).getByRole("button", { name: "Roll back", exact: true }).click();
    await page.getByLabel("Rollback release", { exact: true }).selectOption(RELEASE_A);
    const rollBack = page.locator("#deploy-panel").getByRole("button", { name: "Roll back to selected release", exact: true });
    await rollBack.click();
    await expect(page.getByText(/Unable to connect to the API/)).toBeVisible();
    await rollBack.click();
    await expect.poll(() => rollbacks.length).toBe(2);
    expect(rollbacks[1]).toEqual(rollbacks[0]);
    expect(rollbacks[0].releaseId).toBe(RELEASE_A);
    expect(rollbacks[0].expectedReleaseId).toBe(RELEASE_B);
    await expect(page.getByText("Queued", { exact: true })).toBeVisible();
    await page.setViewportSize({ width: 390, height: 844 });
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    await page.screenshot({ path: testInfo.outputPath("deploy-rollback-mobile.png"), fullPage: true });
  });

  test("reports history errors and retries only the inspected interrupted operation", async ({ page }) => {
    await openServices(page, [DEPLOY_SPEC]);
    const current = { ...release(RELEASE_B), phase: "failed", error: "Drain timed out; VMs retained", update: {
      previous: release(RELEASE_A), requestedSpec: DEPLOY_SPEC, strategy: "rolling", rollbackOf: null,
      cancelled: false, restoring: false, routedNew: true, transition: null, retiring: [SECRET], completed: false,
    } };
    const retries: object[] = [];
    await page.route(`**/api/micro-services/${ID}/deployment`, route => route.fulfill({ json: current }));
    await page.route(`**/api/micro-services/${ID}/releases`, route => route.fulfill({ status: 503, json: { error: { code: "unavailable", message: "Release storage unavailable" } } }));
    await page.route(`**/api/micro-services/${ID}/deployment/retry`, route => {
      retries.push(route.request().postDataJSON());
      return route.fulfill({ status: 202, json: { ...current, phase: "draining", error: null } });
    });
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    await servicePanel(page).getByRole("tab", { name: "Releases" }).click();
    await expect(page.getByText("HTTP 503: Release storage unavailable", { exact: true })).toBeVisible();
    await expect(page.getByText("No successful releases are retained yet.", { exact: true })).toHaveCount(0);
    await page.getByRole("button", { name: "Retry Deploy", exact: true }).click();
    expect(retries).toEqual([{ operationId: RELEASE_B }]);
    await expect(page.getByText("Draining previous requests", { exact: true })).toBeVisible();
    await openMoreMenu(page);
    await expect(page.getByRole("button", { name: "Stop service", exact: true })).toBeDisabled();
  });

  test("a failed list is asked for once, shows its error and is asked for again only on request", async ({ page }) => {
    await openServices(page, [INITIAL]);
    const live = await routeLive(page, running(INITIAL));
    live.releaseStatus = 503;
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    const panel = servicePanel(page);
    await expect.poll(() => live.reads.releases).toBe(1);
    // The Overview has no alert for it; the Releases tab says what failed.
    await expect(panel.getByRole("alert")).toHaveCount(0);
    const polls = live.reads.deployment;
    await expect.poll(() => live.reads.deployment).toBeGreaterThan(polls + 2);
    expect(live.reads.releases).toBe(1);
    await panel.getByRole("tab", { name: "Releases" }).click();
    await expect(panel.getByRole("alert")).toHaveText("HTTP 503: Release storage unavailable");
    const more = live.reads.deployment;
    await expect.poll(() => live.reads.deployment).toBeGreaterThan(more + 1);
    expect(live.reads.releases).toBe(1);
    // The person asks again.
    live.releaseStatus = 200;
    live.releases = [retained(ACTIVE, "ghcr.io/team/orders:1.0")];
    await panel.getByRole("button", { name: "Reload releases", exact: true }).click();
    await expect(panel.locator("table.release-table tbody tr")).toHaveCount(1);
    await expect(panel.getByRole("alert")).toHaveCount(0);
    expect(live.reads.releases).toBe(2);
  });
});

test.describe("Registry automatic Deploy @dashboard", () => {
  test("saves a bounded policy, pauses/resumes and disables with icon controls", async ({ page }, testInfo) => {
    await openServices(page, [DEPLOY_SPEC]);
    const current = release(RELEASE_A);
    let status: any = { policy: null, pause: null, observedTag: null, observedDigest: null, nextCheckAt: 0, operationId: null, failures: 0, error: null };
    const writes: any[] = [];
    await page.route(`**/api/micro-services/${ID}/deployment`, route => route.fulfill({ json: current }));
    await page.route(`**/api/micro-services/${ID}/auto-deploy{,/**}`, async route => {
      const request = route.request();
      const suffix = new URL(request.url()).pathname.split("auto-deploy")[1];
      if (request.method() === "PUT") {
        const policy = request.postDataJSON();
        writes.push(policy);
        status = { ...status, policy, pause: null, observedTag: policy ? (policy.reference.endsWith("*") ? "v1.1.0" : policy.reference.split(":").at(-1)) : null, observedDigest: policy ? `sha256:${"b".repeat(64)}` : null };
      } else if (suffix === "/pause") status = { ...status, pause: "manual" };
      else if (suffix === "/resume") status = { ...status, pause: null };
      return route.fulfill({ json: status });
    });
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    await servicePanel(page).getByRole("tab", { name: "Releases" }).click();
    await page.getByRole("button", { name: "Automation", exact: true }).click();
    await page.getByLabel("Registry source", { exact: true }).fill("ghcr.io/team/orders:v1.*");
    await page.getByLabel("Interval (seconds)", { exact: true }).fill("59");
    await page.getByRole("button", { name: "Save", exact: true }).click();
    await expect(page.getByRole("alert")).toContainText("60–3600");
    expect(writes).toHaveLength(0);
    await page.getByLabel("Interval (seconds)", { exact: true }).fill("60");
    await page.getByRole("button", { name: "Save", exact: true }).click();
    await expect(page.getByRole("button", { name: "Pause", exact: true })).toBeEnabled();
    expect(writes).toEqual([{ reference: "ghcr.io/team/orders:v1.*", intervalSeconds: 60 }]);
    const pause = page.getByRole("button", { name: "Pause", exact: true });
    await expect(pause.locator("svg")).toHaveCount(1);
    await expect(pause).toHaveText("");
    await pause.click();
    await expect(page.getByRole("button", { name: "Resume", exact: true })).toBeEnabled();
    await page.screenshot({ path: testInfo.outputPath("deploy-automation-desktop.png"), fullPage: true });
    await page.getByRole("button", { name: "Resume", exact: true }).click();
    await expect(page.getByRole("button", { name: "Pause", exact: true })).toBeEnabled();
    await page.setViewportSize({ width: 390, height: 844 });
    await expect.poll(() => page.locator("#deploy-panel code").evaluate(node => node.getBoundingClientRect().right <= node.parentElement!.getBoundingClientRect().right + 1)).toBe(true);
    await page.screenshot({ path: testInfo.outputPath("deploy-automation-mobile.png"), fullPage: true });
    await page.getByLabel("Registry source", { exact: true }).fill(`ghcr.io/team/orders:${"r".repeat(128)}`);
    await page.getByRole("button", { name: "Save", exact: true }).click();
    await expect(page.getByRole("button", { name: "Pause", exact: true })).toBeEnabled();
    await expect.poll(() => page.locator("#deploy-panel [role=status]").evaluate(node => node.scrollWidth <= node.clientWidth + 1)).toBe(true);
    await page.getByRole("button", { name: "Disable", exact: true }).click();
    await expect(page.getByRole("button", { name: "Pause", exact: true })).toBeDisabled();
    expect(writes.at(-1)).toBeNull();
  });

  test("reports API policy errors without altering the running release", async ({ page }) => {
    await openServices(page, [DEPLOY_SPEC]);
    await page.route(`**/api/micro-services/${ID}/deployment`, route => route.fulfill({ json: release(RELEASE_A) }));
    await page.route(`**/api/micro-services/${ID}/auto-deploy`, route => route.fulfill({ status: 409, json: { error: { code: "stack_source_managed", message: "Use Git sync to change this policy" } } }));
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    await servicePanel(page).getByRole("tab", { name: "Releases" }).click();
    await page.getByRole("button", { name: "Automation", exact: true }).click();
    await expect(page.getByRole("alert")).toContainText("Use Git sync");
    await openMoreMenu(page);
    await expect(page.getByRole("button", { name: "Stop service", exact: true })).toBeEnabled();
  });
});

test.describe("Service CI Deploy @dashboard", () => {
  const tokenId = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
  const credential = { id: tokenId, serviceId: ID, name: "GitHub", repository: "ghcr.io/team/orders", createdAt: 1793040000, expiresAt: 1793644800 };
  const fixtureToken = `fcd_${tokenId.replaceAll("-", "")}_${"a".repeat(43)}`;

  test("issues one-time tokens, copies and hides them, and revokes with icon controls", async ({ page }, testInfo) => {
    await openServices(page, [DEPLOY_SPEC]);
    await page.context().grantPermissions(["clipboard-read", "clipboard-write"]);
    await page.route(`**/api/micro-services/${ID}/deployment`, route => route.fulfill({ json: release(RELEASE_A) }));
    let rows: object[] = [];
    const writes: any[] = [];
    await page.route(`**/api/micro-services/${ID}/deploy-tokens{,/**}`, async route => {
      const method = route.request().method();
      if (method === "POST") {
        writes.push(route.request().postDataJSON()); rows = [credential];
        return route.fulfill({ status: 201, json: { credential, token: fixtureToken } });
      }
      if (method === "DELETE") { rows = []; return route.fulfill({ status: 204 }); }
      return route.fulfill({ json: rows });
    });
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    await servicePanel(page).getByRole("tab", { name: "Releases" }).click();
    await page.getByRole("button", { name: "CI tokens", exact: true }).click();
    await page.getByLabel("Name", { exact: true }).fill("GitHub");
    await page.getByLabel("Lifetime", { exact: true }).selectOption("7");
    const issue = page.getByRole("button", { name: "Token", exact: true });
    await expect(issue).toHaveText(""); await expect(issue.locator("svg")).toHaveCount(1);
    await issue.click();
    await expect(page.locator("#deploy-ci-issued")).toHaveValue(fixtureToken);
    await expect(page.locator("#deploy-ci-issued")).toHaveAttribute("type", "password");
    expect(writes).toEqual([{ name: "GitHub", expiresInSeconds: 604800 }]);
    await page.getByRole("button", { name: "Reveal", exact: true }).click();
    await expect(page.locator("#deploy-ci-issued")).toHaveAttribute("type", "text");
    await page.getByRole("button", { name: "Copy", exact: true }).click();
    await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe(fixtureToken);
    await page.getByRole("button", { name: "Hide", exact: true }).click();
    await page.setViewportSize({ width: 1280, height: 1100 });
    await page.locator(".service-panel .detail-content").evaluate(node => { node.scrollTop = 0; });
    await page.screenshot({ path: testInfo.outputPath("deploy-ci-desktop.png"), fullPage: true });
    await page.setViewportSize({ width: 390, height: 1100 });
    await page.locator(".service-panel .detail-content").evaluate(node => { node.scrollTop = 0; });
    await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    await page.screenshot({ path: testInfo.outputPath("deploy-ci-mobile.png"), fullPage: true });
    await page.getByRole("button", { name: "CI tokens", exact: true }).click();
    await page.getByRole("button", { name: "CI tokens", exact: true }).click();
    await expect(page.locator("#deploy-ci-issued")).toHaveCount(0);
    await page.getByRole("button", { name: "Revocation: GitHub", exact: true }).click();
    await expect(page.getByRole("button", { name: "Revocation: GitHub", exact: true })).toHaveCount(0);
    await page.setViewportSize({ width: 1280, height: 900 });
    await page.getByRole("combobox", { name: "Language", exact: true }).selectOption("ko");
    await expect(page.getByRole("button", { name: "CI 토큰", exact: true })).toBeVisible();
  });

  test("closing during issuance cannot restore the one-time value from a late response", async ({ page }) => {
    await openServices(page, [DEPLOY_SPEC]);
    await page.route(`**/api/micro-services/${ID}/deployment`, route => route.fulfill({ json: release(RELEASE_A) }));
    let finish!: () => void;
    let started = false;
    const gate = new Promise<void>(resolve => { finish = resolve; });
    await page.route(`**/api/micro-services/${ID}/deploy-tokens`, async route => {
      if (route.request().method() === "POST") {
        started = true; await gate;
        return route.fulfill({ status: 201, json: { credential, token: fixtureToken } });
      }
      return route.fulfill({ json: [] });
    });
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    await servicePanel(page).getByRole("tab", { name: "Releases" }).click();
    await page.getByRole("button", { name: "CI tokens", exact: true }).click();
    await page.getByLabel("Name", { exact: true }).fill("GitHub");
    await page.getByRole("button", { name: "Token", exact: true }).click();
    await expect.poll(() => started).toBe(true);
    await page.getByRole("button", { name: "CI tokens", exact: true }).click();
    finish();
    await page.getByRole("button", { name: "CI tokens", exact: true }).click();
    await expect(page.locator("#deploy-ci-issued")).toHaveCount(0);
    expect(await page.content()).not.toContain(fixtureToken);
  });
});

test.describe("Service observations @dashboard", () => {
  const snapshot = () => ({
    logsStatus: "active",
    logs: [{ timestamp: "2026-10-27T01:02:03Z", stream: "stdout", instanceId: SECRET, message: "<img src=x onerror=alert(1)>" }],
    events: [{ timestamp: "2026-10-27T01:02:04Z", kind: "deploy", severity: "info", message: "Deployment completed" }],
    alerts: [{ timestamp: "2026-10-27T01:02:05Z", kind: "health", active: true, message: "Replica unavailable" }],
    otlp: { endpoint: null as string | null, status: "disabled", queuePersistent: false,
      queued: 0, inFlight: 0, delivered: 0, dropped: 0, rejected: 0, failedBatches: 0 },
  });

  test("shows escaped observations and each collection state with icon controls", async ({ page }, testInfo) => {
    await openServices(page, [DEPLOY_SPEC]);
    await page.route(`**/api/micro-services/${ID}/deployment`, route => route.fulfill({ json: release(RELEASE_A) }));
    const data = snapshot();
    await page.route(`**/api/micro-services/${ID}/observability`, route => route.fulfill({ json: data }));
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    // Logs, events, alerts and OTLP export are under the Instances tab.
    await servicePanel(page).getByRole("tab", { name: /^Instances/ }).click();
    const panel = page.locator("#service-observability");
    for (const name of ["Logs", "Events", "Alerts", "OTLP"]) {
      const button = panel.getByRole("button", { name, exact: true });
      await expect(button).toHaveText("");
      await expect(button.locator("svg")).toHaveCount(1);
      await expect(button).toHaveAttribute("aria-pressed", "false");
    }
    await panel.getByRole("button", { name: "Logs", exact: true }).click();
    await expect(panel.getByRole("button", { name: "Logs", exact: true })).toHaveAttribute("aria-pressed", "true");
    await expect(panel.getByRole("button", { name: "Refresh", exact: true })).not.toHaveAttribute("aria-pressed");
    await expect(panel.getByText("<img src=x onerror=alert(1)>", { exact: true })).toBeVisible();
    await expect(panel.locator("img")).toHaveCount(0);
    for (const [status, label] of [
      ["waiting", "Waiting for logs."],
      ["unavailable", "Log collection is unavailable. Start a new VM or redeploy."],
      ["truncated", "Some logs were lost."],
      ["error", "Log collection failed."],
      ["active", "Collecting logs."],
    ]) {
      data.logsStatus = status;
      await panel.getByRole("button", { name: "Refresh", exact: true }).click();
      await expect(panel.getByText(label, { exact: true })).toBeVisible();
    }
    await panel.getByRole("button", { name: "Events", exact: true }).click();
    await expect(panel.getByText("Deployment completed", { exact: true })).toBeVisible();
    await panel.getByRole("button", { name: "Alerts", exact: true }).click();
    await expect(panel.getByText("Replica unavailable", { exact: true })).toBeVisible();
    await page.setViewportSize({ width: 1280, height: 1100 });
    await page.screenshot({ path: testInfo.outputPath("service-observations-desktop.png"), fullPage: true });
    await page.setViewportSize({ width: 390, height: 1100 });
    await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    await page.screenshot({ path: testInfo.outputPath("service-observations-mobile.png"), fullPage: true });
  });

  test("saves and disables OTLP without allowing credentials or losing an edited endpoint", async ({ page }) => {
    await openServices(page, [DEPLOY_SPEC]);
    await page.route(`**/api/micro-services/${ID}/deployment`, route => route.fulfill({ json: release(RELEASE_A) }));
    const data = snapshot();
    data.otlp.dropped = 2;
    data.otlp.rejected = 1;
    data.otlp.failedBatches = 3;
    const writes: unknown[] = [];
    await page.route(`**/api/micro-services/${ID}/observability{,/otlp}`, route => {
      if (route.request().method() === "PUT") {
        const body = route.request().postDataJSON() as { endpoint: string | null };
        writes.push(body);
        data.otlp = { ...data.otlp, endpoint: body.endpoint, status: body.endpoint ? "configured" : "disabled" };
        return route.fulfill({ json: data.otlp });
      }
      return route.fulfill({ json: data });
    });
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    // Logs, events, alerts and OTLP export are under the Instances tab.
    await servicePanel(page).getByRole("tab", { name: /^Instances/ }).click();
    const panel = page.locator("#service-observability");
    await panel.getByRole("button", { name: "OTLP", exact: true }).click();
    const endpoint = panel.getByLabel("Collector base URL", { exact: true });
    await expect(endpoint).toBeVisible();
    await expect(panel.getByRole("button", { name: "Save", exact: true })).not.toHaveAttribute("aria-pressed");
    await expect(panel).toContainText("Saving sends platform logs and events to this collector. App logs can contain sensitive values.");
    await expect(panel).toContainText("Queued data is lost on API restart or when the endpoint is changed or disabled.");
    await expect(panel.locator(".service-otlp-metrics")).toContainText("Dropped2");
    await expect(panel.locator(".service-otlp-metrics")).toContainText("Rejected1");
    await expect(panel.locator(".service-otlp-metrics")).toContainText("Failed batches3");
    await endpoint.fill("https://user:secret@collector.example.test");
    await panel.getByRole("button", { name: "Save", exact: true }).click();
    await expect(panel.getByRole("alert")).toContainText("without credentials");
    expect(writes).toEqual([]);
    await endpoint.fill("https://collector.example.test");
    await endpoint.focus();
    await page.waitForTimeout(3500);
    await expect(endpoint).toBeFocused();
    await expect(endpoint).toHaveValue("https://collector.example.test");
    await panel.getByRole("button", { name: "Save", exact: true }).click();
    await expect.poll(() => writes.length).toBe(1);
    expect(writes[0]).toEqual({ endpoint: "https://collector.example.test" });
    await expect(panel.getByRole("status")).toContainText("configured");
    await endpoint.fill("");
    await panel.getByRole("button", { name: "Save", exact: true }).click();
    await expect.poll(() => writes.length).toBe(2);
    expect(writes[1]).toEqual({ endpoint: null });
    await expect(panel.getByRole("status")).toContainText("disabled");
  });

  test("keeps errors distinct from an empty result and retries", async ({ page }) => {
    await openServices(page, [DEPLOY_SPEC]);
    await page.route(`**/api/micro-services/${ID}/deployment`, route => route.fulfill({ json: release(RELEASE_A) }));
    let failing = true;
    await page.route(`**/api/micro-services/${ID}/observability`, route => failing
      ? route.fulfill({ status: 503, json: { error: { message: "collector secret" } } })
      : route.fulfill({ json: { ...snapshot(), logs: [], logsStatus: "waiting" } }));
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    // Logs, events, alerts and OTLP export are under the Instances tab.
    await servicePanel(page).getByRole("tab", { name: /^Instances/ }).click();
    const panel = page.locator("#service-observability");
    await panel.getByRole("button", { name: "Logs", exact: true }).click();
    await expect(panel.getByRole("alert")).toHaveText("Could not load observations.");
    await expect(panel).not.toContainText("collector secret");
    await expect(panel).not.toContainText("No logs.");
    failing = false;
    await panel.getByRole("button", { name: "Refresh", exact: true }).click();
    await expect(panel).toContainText("Waiting for logs.");
    await expect(panel).toContainText("No logs.");
  });

  test("preserves a save when a background observation read is still in flight", async ({ page }) => {
    await openServices(page, [DEPLOY_SPEC]);
    await page.route(`**/api/micro-services/${ID}/deployment`, route => route.fulfill({ json: release(RELEASE_A) }));
    const data = snapshot();
    let holdRead = false, readStarted = false;
    let finishRead!: () => void;
    const gate = new Promise<void>(resolve => { finishRead = resolve; });
    const writes: unknown[] = [];
    await page.route(`**/api/micro-services/${ID}/observability{,/otlp}`, async route => {
      if (route.request().method() === "PUT") {
        const body = route.request().postDataJSON() as { endpoint: string | null };
        writes.push(body);
        data.otlp = { ...data.otlp, endpoint: body.endpoint, status: "configured" };
        return route.fulfill({ json: data.otlp });
      }
      const response = structuredClone(data);
      if (holdRead && !readStarted) { readStarted = true; await gate; }
      return route.fulfill({ json: response });
    });
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    // Logs, events, alerts and OTLP export are under the Instances tab.
    await servicePanel(page).getByRole("tab", { name: /^Instances/ }).click();
    const panel = page.locator("#service-observability");
    await panel.getByRole("button", { name: "OTLP", exact: true }).click();
    const endpoint = panel.getByLabel("Collector base URL", { exact: true });
    await expect(endpoint).toBeVisible();
    holdRead = true;
    await expect.poll(() => readStarted).toBe(true);
    await endpoint.fill("https://collector.example.test");
    await panel.getByRole("button", { name: "Save", exact: true }).click();
    await expect.poll(() => writes.length).toBe(1);
    await expect(panel.getByRole("status")).toContainText("configured");
    const staleResponse = page.waitForResponse(response => new URL(response.url()).pathname === `/api/micro-services/${ID}/observability`);
    finishRead();
    await staleResponse;
    await page.evaluate(() => new Promise<void>(resolve => requestAnimationFrame(() => resolve())));
    await expect(endpoint).toHaveValue("https://collector.example.test");
    await expect(panel.getByRole("status")).toContainText("configured");
    expect(writes).toEqual([{ endpoint: "https://collector.example.test" }]);
  });

  test("preserves disclosure and focus when observation polling changes the log", async ({ page }) => {
    await openServices(page, [DEPLOY_SPEC]);
    await page.route(`**/api/micro-services/${ID}/deployment`, route => route.fulfill({ json: release(RELEASE_A) }));
    const data = snapshot();
    let reads = 0;
    await page.route(`**/api/micro-services/${ID}/observability`, route => { reads++; return route.fulfill({ json: data }); });
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    // Logs, events, alerts and OTLP export are under the Instances tab.
    await servicePanel(page).getByRole("tab", { name: /^Instances/ }).click();
    const panel = page.locator("#service-observability");
    const logs = panel.getByRole("button", { name: "Logs", exact: true });
    await logs.click();
    await expect(panel).toContainText("<img src=x onerror=alert(1)>");
    await expect(logs).toHaveAttribute("aria-pressed", "true");
    await logs.focus();
    const previous = reads;
    data.logs[0].message = "Updated log";
    await expect.poll(() => reads).toBeGreaterThan(previous);
    await expect(panel).toContainText("Updated log");
    await expect(logs).toBeFocused();
    await expect(logs).toHaveAttribute("aria-pressed", "true");
  });

  test("ignores a late observation response after selecting another service", async ({ page }) => {
    const reports = { ...DEPLOY_SPEC, id: STORAGE, name: "reports" };
    await openServices(page, [DEPLOY_SPEC, reports]);
    await page.route(`**/api/micro-services/${ID}/deployment`, route => route.fulfill({ json: release(RELEASE_A) }));
    await page.route(`**/api/micro-services/${STORAGE}`, route => route.fulfill({ json: reports }));
    await page.route(`**/api/micro-services/${STORAGE}/deployment`, route => route.fulfill({ json: null }));
    let releaseFirst!: () => void;
    let firstStarted = false;
    const gate = new Promise<void>(resolve => { releaseFirst = resolve; });
    await page.route(`**/api/micro-services/${ID}/observability`, async route => {
      firstStarted = true;
      await gate;
      return route.fulfill({ json: { ...snapshot(), logs: [{ timestamp: "", stream: "stdout", instanceId: "", message: "old service" }] } });
    });
    await page.route(`**/api/micro-services/${STORAGE}/observability`, route => route.fulfill({ json: { ...snapshot(), logs: [{ timestamp: "", stream: "stdout", instanceId: "", message: "new service" }] } }));
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    // Logs, events, alerts and OTLP export are under the Instances tab.
    await servicePanel(page).getByRole("tab", { name: /^Instances/ }).click();
    await page.locator("#service-observability").getByRole("button", { name: "Logs", exact: true }).click();
    await expect.poll(() => firstStarted).toBe(true);
    await page.getByRole("button", { name: "Close detail", exact: true }).click();
    await page.getByRole("button", { name: "Open reports", exact: true }).click();
    // Logs, events, alerts and OTLP export are under the Instances tab.
    await servicePanel(page).getByRole("tab", { name: /^Instances/ }).click();
    const panel = page.locator("#service-observability");
    await panel.getByRole("button", { name: "Logs", exact: true }).click();
    await expect(panel).toContainText("new service");
    const oldResponse = page.waitForResponse(response => new URL(response.url()).pathname === `/api/micro-services/${ID}/observability`);
    releaseFirst();
    await oldResponse;
    await page.evaluate(() => new Promise<void>(resolve => requestAnimationFrame(() => resolve())));
    await expect(panel).toContainText("new service");
    await expect(panel).not.toContainText("old service");
  });
});
