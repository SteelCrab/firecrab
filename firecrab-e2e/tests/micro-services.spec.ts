import {
  serviceAction,
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
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    await page.getByLabel("First host port (optional)", { exact: true }).fill("");
    await page.getByRole("button", { name: "Deploy and run", exact: true }).click();
    await expect(page.getByText("Available to apps in the same MicroNetwork.", { exact: true })).toBeVisible();
    await expect(page.locator("code").filter({ hasText: "http://orders.internal" })).toBeVisible();
    await expect(page.getByRole("link", { name: "Open app", exact: true })).toHaveCount(0);
    await expect(page.getByRole("button", { name: "Stop service", exact: true })).toBeEnabled();
  });

  test("retains Stop after uncertain internal routing cleanup without replicas", async ({ page }) => {
    await openServices(page, [{ ...INITIAL, domain: null }]);
    await page.route(`**/api/micro-services/${ID}/deployment`, route => route.fulfill({ json: {
      id: SECRET, serviceId: ID, spec: { ...INITIAL, domain: null }, phase: "failed", request: { hostPort: 0 }, digest: null,
      internalRouteOwned: true, instances: [], log: [], error: "Could not withdraw internal DNS and routing; retry Stop."
    } }));
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
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

  test("keeps expanded runtime sections and keyboard focus through status polling", async ({ page }) => {
    const spec = { ...INITIAL, env: { PRIVATE_VALUE: "configuration-value-not-for-overview" } };
    await openServices(page, [spec]);
    let reads = 0;
    await page.route(`**/api/micro-services/${ID}/deployment`, route => {
      reads++;
      return route.fulfill({ json: {
        id: SECRET, serviceId: ID, spec, phase: "running", domainRouteOwned: true, request: { hostPort: 30190 },
        instances: [{ vmId: SECRET, ipv4: "172.30.0.2", hostPort: 30190, healthy: true }],
        log: ["All replicas answered the app health check"], error: null,
      } });
    });
    await page.route(`**/api/micro-services/${ID}/releases`, route => route.fulfill({ json: [
      { id: SECRET, spec, digest: `sha256:${"a".repeat(64)}` },
    ] }));
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
    await expect(page.locator("#service-status").getByRole("status")).toHaveText("Running");
    await expect(page.locator("#service-deploy-form")).toBeHidden();
    for (const id of ["service-instances", "service-log", "service-configuration"]) {
      await expect(page.locator(`#${id}`)).not.toHaveAttribute("open");
      await page.locator(`#${id}-toggle`).click();
    }
    await page.locator("#service-log-toggle").focus();
    const before = reads;
    await expect.poll(() => reads).toBeGreaterThan(before);
    for (const id of ["service-instances", "service-log", "service-configuration"]) {
      await expect(page.locator(`#${id}`)).toHaveAttribute("open");
    }
    await expect(page.locator("#service-log-toggle")).toBeFocused();
    await expect(page.getByText("All replicas answered the app health check", { exact: true })).toBeVisible();
    await expect(page.locator(".service-panel")).not.toContainText("configuration-value-not-for-overview");
    await expect(page.getByRole("button", { name: "Stop service", exact: true })).toBeEnabled();
    const history = page.getByRole("button", { name: "Release history", exact: true });
    await history.click();
    await expect(page.locator("#deploy-release")).toBeVisible();
    for (const control of [page.getByRole("button", { name: "Stop service", exact: true }), history]) {
      await control.focus();
      const original = await control.elementHandle();
      const prior = reads;
      await expect.poll(() => reads).toBeGreaterThan(prior);
      await expect(control).toBeFocused();
      expect(await original?.evaluate(node => node.isConnected)).toBe(true);
      await original?.dispose();
    }
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
    await expect(page.locator("#service-deploy-form")).toBeHidden();
    await expect(page.getByRole("button", { name: "Stop service", exact: true })).toBeEnabled();
    page.once("dialog", dialog => dialog.accept());
    await page.getByRole("button", { name: "Stop service", exact: true }).click();
    await expect(page.locator("#service-status").getByRole("status")).toHaveText("Stopped");
    await expect(page.getByRole("button", { name: "Stop service", exact: true })).toHaveCount(0);
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
    await page.getByRole("button", { name: "New MicroService", exact: true }).click();
    await page.getByLabel("Name", { exact: true }).fill("orders");
    await selectServiceApp(page);
    await serviceStep(page, 2);
    await page.getByRole("button", { name: "Add Queue connection", exact: true }).click();
    await page.locator(".queue-binding-row").getByLabel("Alias", { exact: true }).fill("NOTIFICATIONS");
    await page.locator(".queue-binding-row").getByRole("combobox", { name: "Access", exact: true }).selectOption("consume");
    await saveService(page);
    await expect.poll(() => writes.length).toBe(1);
    expect(writes[0].body.queues).toEqual({ NOTIFICATIONS: { queueId: STORAGE, access: "consume" } });
    expect(JSON.stringify(writes[0].body)).not.toContain("amqp://");
    await serviceAction(page, "orders", "Edit");
    await serviceStep(page, 2);
    await page.locator(".queue-binding-row").getByRole("button", { name: "Remove", exact: true }).click();
    await saveService(page);
    await expect.poll(() => writes.length).toBe(2);
    expect(writes[1].body.queues).toBeUndefined();
  });

  test("binds and clears an S3 connection while preserving its security group", async ({
    page,
  }) => {
    const writes = await openServices(page);
    await page
      .getByRole("button", { name: "New MicroService", exact: true })
      .click();
    await page.getByLabel("Name", { exact: true }).fill("orders");
    await selectServiceApp(page, "ghcr.io/team/orders:1.0");
    await serviceStep(page, 1);
    await page
      .getByLabel("Security group", { exact: true })
      .selectOption(SECRET);
    await serviceStep(page, 2);
    await page
      .getByLabel("S3 connection (optional)", { exact: true })
      .selectOption(STORAGE);
    await saveService(page);
    await expect(
      page.getByRole("cell", { name: "orders", exact: true }),
    ).toBeVisible();
    expect(writes[0].body.storageConnectionId).toBe(STORAGE);
    expect(writes[0].body.securityGroupId).toBe(SECRET);
    await serviceAction(page, "orders", "Edit");
    await serviceStep(page, 2);
    await page
      .getByLabel("S3 connection (optional)", { exact: true })
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
    await expect(page.locator("#service-deploy-form")).toBeHidden();
    await expect(page.locator("#service-instances")).not.toHaveAttribute("open");
    await expect(page.locator("#service-log")).not.toHaveAttribute("open");
    await page.locator("#service-instances-toggle").click();
    await expect(
      page.getByRole("cell", { name: "Healthy", exact: true }),
    ).toBeVisible();
    const stop = page.getByRole("button", { name: "Stop service", exact: true });
    await stop.focus();
    const before = statusReads;
    await expect.poll(() => statusReads).toBeGreaterThan(before);
    await expect(stop).toBeFocused();
    await page.locator("#service-instances-toggle").click();
    await page.screenshot({
      path: testInfo.outputPath("service-running-desktop.png"),
      fullPage: true,
    });
    await page.setViewportSize({ width: 390, height: 844 });
    await expect(page.getByRole("button", { name: "New MicroService", exact: true })).toBeHidden();
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
    await expect(
      page.locator("#service-status").getByRole("status"),
    ).toHaveText("Stopped");
    await expect(
      page.getByRole("button", { name: "Deploy", exact: true }),
    ).toBeEnabled();
    await page.getByRole("button", { name: "Close", exact: true }).click();
    await expect(page.getByRole("button", { name: "New MicroService", exact: true })).toBeVisible();
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
    await page.getByRole("button", { name: "Open orders", exact: true }).click();
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
    await expect(page.getByLabel("First host port (optional)")).toHaveAttribute(
      "max",
      "65533",
    );
    await page.setViewportSize({ width: 390, height: 844 });
    await expect(
      page.getByRole("columnheader", { name: "Port", exact: true }),
    ).toHaveCount(1);
    await expect(page.getByRole("cell", { name: /9090/ })).toHaveAttribute(
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
    await expect(
      page.getByText("No service configurations yet."),
    ).toBeVisible();
    await page
      .getByRole("button", { name: "New MicroService", exact: true })
      .click();
    await page.getByLabel("Name", { exact: true }).fill("orders");
    await selectServiceApp(page, "ghcr.io/team/orders:1.0");
    await serviceStep(page, 2);
    await page
      .getByLabel("Environment (NAME=value)", { exact: true })
      .fill("PROFILE=prod");
    await serviceStep(page, 2);
    await page
      .getByLabel("Secret references (NAME=CrabSecrets ID)", { exact: true })
      .fill(`DB_PASSWORD=${SECRET}`);
    await page.screenshot({
      path: testInfo.outputPath("microservice-desktop.png"),
      fullPage: true,
    });
    await saveService(page);
    await expect(
      page.getByRole("cell", { name: "orders", exact: true }),
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
    await expect(
      page.getByText("No service configurations yet."),
    ).toBeVisible();
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
      page.getByRole("cell", { name: "orders", exact: true }),
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
  await expect(page.getByLabel("First host port (optional)")).toHaveCount(0);
  await expect(page.getByLabel("Network", { exact: true })).not.toBeVisible();
  await page.getByText("Advanced settings", { exact: true }).click();
  await expect(page.getByLabel("Network", { exact: true })).toHaveValue(ID);
  await expect(page.getByLabel("Network", { exact: true })).toBeDisabled();
  await page.locator("#service-configuration-toggle").click();
  await expect(
    page.getByText(`${SECRET}.cfargotunnel.com`, { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("link", { name: "Open domain", exact: true }),
  ).toHaveCount(0);
  await page
    .getByRole("button", { name: "Deploy and run", exact: true })
    .click();
  await expect(
    page.getByRole("link", { name: "Open domain", exact: true }),
  ).toHaveAttribute("href", "https://orders.example.com");
  await expect(page.getByRole("link", { name: ":0", exact: true })).toHaveCount(
    0,
  );
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
  await page
    .getByRole("button", { name: "New MicroService", exact: true })
    .click();
  await page.getByLabel("Name", { exact: true }).fill("web");
  await selectServiceApp(page, "ghcr.io/team/nginx:stable-alpine");
  await serviceStep(page, 1);
  await page
    .getByLabel("Domain (optional)", { exact: true })
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
  await page
    .getByRole("button", { name: "New MicroService", exact: true })
    .click();
  await page.getByLabel("Name", { exact: true }).fill("orders");
  await selectServiceApp(page);
  await serviceStep(page, 3);
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
  await page.getByRole("button", { name: "New MicroService", exact: true }).click();
  await expect(page.getByText("Connect an account first", { exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "Continue", exact: true })).toBeDisabled();
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
  await page
    .getByRole("button", { name: "New MicroService", exact: true })
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
  await serviceStep(page, 3);
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
  await page.getByRole("button", { name: "Open orders", exact: true }).click();
  await page.getByLabel("Network", { exact: true }).selectOption(STORAGE);
  await page.getByLabel("First host port (optional)", { exact: true }).fill("31234");
  await expect(
    page.getByRole("button", { name: "Deploy and run", exact: true }),
  ).toBeDisabled();
  await page.getByRole("link", { name: "Reconnect AUTH", exact: true }).click();
  await page
    .getByRole("button", { name: "Connect GitHub", exact: true })
    .click();
  await page.getByLabel("Token", { exact: true }).fill("fixture-only-token");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page).toHaveURL(/#\/services$/);
  await expect(page.getByLabel("First host port (optional)", { exact: true })).toHaveValue(
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
  await page.getByRole("button", { name: "Open orders", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "Deploy and run", exact: true }),
  ).toBeEnabled();
  await page.getByLabel("First host port (optional)", { exact: true }).fill("31234");
  await expect.poll(() => delayed).toBe(true);
  await page.getByLabel("First host port (optional)", { exact: true }).fill("32000");
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
    await page.getByRole("button", { name: "Deploy update", exact: true }).click();
    await expect(page.getByLabel("Name", { exact: true })).toHaveAttribute("readonly", "");
    await selectServiceApp(page, "ghcr.io/team/orders:2.0");
    await page.getByRole("button", { name: "Continue", exact: true }).click();
    await expect(page.getByLabel("Domain (optional)", { exact: true })).toHaveAttribute("readonly", "");
    await page.getByRole("button", { name: "Continue", exact: true }).click();
    await page.getByLabel("Memory (MiB)", { exact: true }).fill("256");
    await page.getByRole("button", { name: "Continue", exact: true }).click();
    await page.getByLabel("Deployment strategy", { exact: true }).selectOption("blue_green");
    await page.getByRole("button", { name: "Deploy update", exact: true }).click();
    await expect(page.getByText("Switching Nginx traffic", { exact: true })).toBeVisible();
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
    await page.getByRole("button", { name: "Release history", exact: true }).click();
    await page.getByLabel("Rollback release", { exact: true }).selectOption(RELEASE_A);
    await page.getByRole("button", { name: "Roll back", exact: true }).click();
    await expect(page.getByText(/Unable to connect to the API/)).toBeVisible();
    await page.getByRole("button", { name: "Roll back", exact: true }).click();
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
    await page.getByRole("button", { name: "Release history", exact: true }).click();
    await expect(page.getByText("HTTP 503: Release storage unavailable", { exact: true })).toBeVisible();
    await expect(page.getByText("No successful releases are retained yet.", { exact: true })).toHaveCount(0);
    await page.getByRole("button", { name: "Retry Deploy", exact: true }).click();
    expect(retries).toEqual([{ operationId: RELEASE_B }]);
    await expect(page.getByText("Draining previous requests", { exact: true })).toBeVisible();
    await expect(page.getByRole("button", { name: "Stop service", exact: true })).toBeDisabled();
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
    await page.getByRole("button", { name: "Automation", exact: true }).click();
    await expect(page.getByRole("alert")).toContainText("Use Git sync");
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
    await page.locator(".service-panel-body").evaluate(node => { node.scrollTop = 0; });
    await page.screenshot({ path: testInfo.outputPath("deploy-ci-desktop.png"), fullPage: true });
    await page.setViewportSize({ width: 390, height: 1100 });
    await page.locator(".service-panel-body").evaluate(node => { node.scrollTop = 0; });
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
    const panel = page.locator("#service-observability");
    for (const name of ["Logs", "Events", "Alerts", "OTLP"]) {
      const button = panel.getByRole("button", { name, exact: true });
      await expect(button).toHaveText("");
      await expect(button.locator("svg")).toHaveCount(1);
    }
    await panel.getByRole("button", { name: "Logs", exact: true }).click();
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
    const panel = page.locator("#service-observability");
    await panel.getByRole("button", { name: "OTLP", exact: true }).click();
    const endpoint = panel.getByLabel("Collector base URL", { exact: true });
    await expect(endpoint).toBeVisible();
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
    const panel = page.locator("#service-observability");
    const logs = panel.getByRole("button", { name: "Logs", exact: true });
    await logs.click();
    await expect(panel).toContainText("<img src=x onerror=alert(1)>");
    await page.locator("#service-configuration-toggle").click();
    await expect(page.locator("#service-configuration")).toHaveAttribute("open");
    await logs.focus();
    const previous = reads;
    data.logs[0].message = "Updated log";
    await expect.poll(() => reads).toBeGreaterThan(previous);
    await expect(panel).toContainText("Updated log");
    await expect(logs).toBeFocused();
    await expect(page.locator("#service-configuration")).toHaveAttribute("open");
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
    await page.locator("#service-observability").getByRole("button", { name: "Logs", exact: true }).click();
    await expect.poll(() => firstStarted).toBe(true);
    await page.getByRole("button", { name: "Close", exact: true }).click();
    await page.getByRole("button", { name: "Open reports", exact: true }).click();
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
