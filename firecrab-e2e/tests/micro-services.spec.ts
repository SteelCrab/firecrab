import { expect, test, type Page } from "@playwright/test";

const ID = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const SECRET = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const NETWORK = { id: "cccccccc-cccc-4ccc-8ccc-cccccccccccc", name: "private-apps", subnetCidr: "172.31.0.0/24" };
const AUTH = { provider: "github", account: "team", registry: "ghcr.io", region: null, services: ["registry"] };
const AWS = { provider: "aws", account: "123456789012", registry: "123456789012.dkr.ecr.ap-northeast-2.amazonaws.com", region: "ap-northeast-2", services: ["registry"] };
const STORAGE = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
const INITIAL = {
  id: ID, name: "orders", image: "ghcr.io/team/orders:1.0", runtime: "vm",
  port: 8080, healthPath: "/actuator/health", domain: "orders.example.com",
  ram: 1024, cpu: 2, replicas: 3, startupTimeout: 120, drainTimeout: 60,
  env: { PROFILE: "prod" }, secrets: { DB_PASSWORD: SECRET },
};

async function openServices(page: Page, rows: object[] = [], failSave = false, auths: object[] = [AUTH]) {
  const writes: { method: string; body: Record<string, unknown> }[] = [];
  let saved = rows;
  await page.route(/^https?:\/\/[^/]+\/api\//, async (route) => {
    const path = new URL(route.request().url()).pathname;
    const method = route.request().method();
    if (path === "/api/pipe/auth") return route.fulfill({ json: auths });
    if (path.endsWith("/repositories")) {
      const registry = decodeURIComponent(path.split("/")[4] ?? "");
      return route.fulfill({ json: { repositories: registry === "ghcr.io" ? ["team/orders", "team/billing"] : ["web", "api"], listingSupported: true, nextPage: null, nextCursor: null } });
    }
    if (path === "/api/pipe/registry-tags") return route.fulfill({ json: { tags: ["1.0", "2.0"], nextLast: null } });
    if (path === "/api/pipe") return route.fulfill({ json: { config: null, units: [], nginxAvailable: true, cloudflaredAvailable: false } });
    if (path === "/api/vms") return route.fulfill({ json: [] });
    if (path === "/api/micro-networks") return route.fulfill({ json: [] });
    if (path === "/api/pipe/storage") return route.fulfill({ json: [{ id: STORAGE, name: "Reports", bucket: "reports-bucket", region: "ap-northeast-2", prefix: "team/", credentialRef: "connection", services: [] }] });
    if (path === "/api/security-groups") return route.fulfill({ json: [{ id: SECRET, name: "orders-policy", microNetworkId: ID, rules: [] }] });
    if (path === `/api/security-groups/${SECRET}`) return route.fulfill({ json: { id: SECRET, name: "orders-policy", microNetworkId: ID, rules: [] } });
    if (path === "/api/host") return route.fulfill({ json: { loadAverage1m: 0.1, memoryTotalMib: 8192, memoryAvailableMib: 4096, diskTotalGib: 100, diskAvailableGib: 50, uptimeSeconds: 3600 } });
    if (path === "/api/update") return route.fulfill({ json: { current: "0.3.1", latest: "0.3.1", updateAvailable: false } });
    if (path === "/api/update/progress") return route.fulfill({ json: { phase: "idle", percent: 0, updatedAtMs: 0 } });
    if (path.endsWith("/deployment/check")) return route.fulfill({ json: null });
    if (path.endsWith("/deployment")) return route.fulfill({ json: null });
    if (path.startsWith("/api/micro-services")) {
      if (method === "POST" || method === "PUT") {
        const body = route.request().postDataJSON();
        writes.push({ method, body });
        if (failSave) return route.fulfill({ status: 400, json: { error: { code: "validation_failed", message: "request validation failed", fields: { name: "is already registered" } } } });
        saved = [{ id: ID, ...body }];
        return route.fulfill({ status: method === "POST" ? 201 : 200, json: saved[0] });
      }
      if (method === "DELETE") { saved = []; return route.fulfill({ status: 204 }); }
      return route.fulfill({ json: path === "/api/micro-services" ? saved : saved[0] });
    }
    return route.fulfill({ status: 404, json: { error: { message: "unused endpoint" } } });
  });
  await page.addInitScript(() => localStorage.setItem("firecrab.locale", "en"));
  await page.goto("/#/services");
  return writes;
}

async function next(page: Page) {
  await page.getByRole("button", { name: "Continue", exact: true }).click();
}

async function selectApp(page: Page, repository = "team/orders", tag = "1.0") {
  await page.getByLabel("App repository", { exact: true }).selectOption(repository);
  await page.getByLabel("App version", { exact: true }).selectOption(tag);
}

async function advanced(page: Page) {
  await page.locator("summary").filter({ hasText: "Advanced settings" }).click();
}

test.describe("MicroService configuration @dashboard", () => {
  test("binds and clears an S3 connection while preserving its security group", async ({ page }) => {
    const writes = await openServices(page);
    await page.getByRole("button", { name: "New MicroService", exact: true }).click();
    await page.getByLabel("Name", { exact: true }).fill("orders");
    await selectApp(page);
    await next(page);
    await page.getByLabel("Security group", { exact: true }).selectOption(SECRET);
    await next(page);
    await advanced(page);
    await page.getByLabel("S3 connection (optional)", { exact: true }).selectOption(STORAGE);
    await next(page);
    await expect(page.locator(".service-review")).toContainText("Reports");
    await expect(page.locator(".service-review")).toContainText("orders-policy");
    await page.getByRole("button", { name: "Save configuration", exact: true }).click();
    await expect(page.getByRole("cell", { name: "orders", exact: true })).toBeVisible();
    expect(writes[0].body.storageConnectionId).toBe(STORAGE);
    expect(writes[0].body.securityGroupId).toBe(SECRET);
    await page.getByRole("button", { name: "Edit orders", exact: true }).click();
    await next(page);
    await next(page);
    await advanced(page);
    await page.getByLabel("S3 connection (optional)", { exact: true }).selectOption("");
    await next(page);
    await page.getByRole("button", { name: "Save configuration", exact: true }).click();
    await expect.poll(() => writes.length).toBe(2);
    expect(writes[1].body).not.toHaveProperty("storageConnectionId");
    expect(writes[1].body.securityGroupId).toBe(SECRET);
    expect(JSON.stringify(writes)).not.toContain("secretAccessKey");
  });
  test("deploys from the runtime panel, reports health, and stops the service", async ({ page }, testInfo) => {
    const service = { ...INITIAL, domain: null, replicas: 1 };
    await openServices(page, [service]);
    let deployment: Record<string, unknown> | null = null;
    let read = 0;
    const instances = [{ vmId: SECRET, ipv4: "172.30.0.2", hostPort: 30100, state: "running", healthy: true }];
    await page.route(`**/api/micro-services/${ID}/deployment`, async (route) => {
      if (route.request().method() === "POST") {
        expect(route.request().postDataJSON()).toEqual({ hostPort: 30100, microNetworkId: null });
        deployment = { id: SECRET, serviceId: ID, spec: service, phase: "queued", request: { hostPort: 30100 }, digest: `sha256:${"a".repeat(64)}`, instances: [], log: ["Deployment queued"], error: null };
        read = 0;
        return route.fulfill({ status: 202, json: deployment });
      }
      if (deployment?.phase === "queued" && ++read >= 1) deployment = { ...deployment, phase: "running", instances, log: ["All replicas answered the app health check"] };
      return route.fulfill({ json: deployment });
    });
    await page.route(`**/api/micro-services/${ID}/stop`, async (route) => {
      expect(route.request().method()).toBe("POST");
      deployment = { ...deployment, phase: "stopped", instances: [], log: ["Service instances removed"] };
      return route.fulfill({ status: 202, json: deployment });
    });
    await page.getByRole("button", { name: "Run orders", exact: true }).click();
    await expect(page.getByRole("status")).toHaveText("Not deployed");
    await expect(page.getByText("Secret values are injected into every replica. Changes apply on the next deployment.", { exact: true })).toBeVisible();
    await expect(page.getByText("Secret injection is not available yet.", { exact: false })).toHaveCount(0);
    await page.getByRole("button", { name: "Deploy and run", exact: true }).click();
    await expect(page.getByRole("status")).toHaveText("Running");
    await expect(page.getByRole("cell", { name: "Healthy", exact: true })).toBeVisible();
    await expect(page.getByRole("link", { name: "Open app", exact: true })).toHaveAttribute("href", /:30100\/$/);
    await expect(page.getByRole("list", { name: "Deployment progress" }).locator("li.done")).toHaveCount(4);
    await page.screenshot({ path: testInfo.outputPath("service-running-desktop.png"), fullPage: true });
    await page.setViewportSize({ width: 390, height: 844 });
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    await page.screenshot({ path: testInfo.outputPath("service-running-mobile.png"), fullPage: true });
    page.once("dialog", (dialog) => dialog.accept());
    await page.getByRole("button", { name: "Stop service", exact: true }).click();
    await expect(page.getByRole("status")).toHaveText("Stopped");
    await expect(page.getByRole("button", { name: "Deploy and run", exact: true })).toBeEnabled();
  });

  test("resumes deployment after AUTH reconnection with the chosen network and port", async ({ page }, testInfo) => {
    const service = { ...INITIAL, domain: null, secrets: {} };
    await openServices(page, [service]);
    let auths: object[] = [];
    const dialogs: string[] = [];
    page.on("dialog", async (dialog) => { dialogs.push(dialog.message()); await dialog.dismiss(); });
    const deploys: object[] = [];
    let deployment: object | null = null;
    await page.route("**/api/micro-networks", (route) => route.fulfill({ json: [NETWORK] }));
    await page.route(`**/api/micro-services/${ID}/deployment`, (route) => {
      if (route.request().method() === "POST") {
        const request = route.request().postDataJSON();
        deploys.push(request);
        deployment = { id: SECRET, serviceId: ID, spec: service, phase: "queued", request, digest: null, instances: [], log: ["Deployment queued"], error: null };
        return route.fulfill({ status: 202, json: deployment });
      }
      return route.fulfill({ json: deployment });
    });
    let releaseAuth!: () => void;
    const authReady = new Promise<void>((resolve) => { releaseAuth = resolve; });
    await page.route("**/api/pipe/auth", async (route) => {
      await authReady;
      return route.fulfill({ json: auths });
    });
    await page.route("**/api/pipe", (route) => route.fulfill({ json: { config: null, units: [], nginxAvailable: true, cloudflaredAvailable: true } }));
    await page.route("**/api/pipe/auth/github", async (route) => {
      expect(route.request().method()).toBe("PUT");
      auths = [AUTH];
      return route.fulfill({ json: AUTH });
    });
    await page.getByRole("button", { name: "Run orders", exact: true }).click();
    const deploy = page.getByRole("button", { name: "Deploy and run", exact: true });
    await expect(page.getByText("Checking AUTH connection…", { exact: true })).toBeVisible();
    await expect(deploy).toBeDisabled();
    releaseAuth();
    await expect(page.getByText("Reconnect AUTH to deploy", { exact: true })).toBeVisible();
    await expect(deploy).toBeDisabled();
    await expect(deploy).toHaveAttribute("aria-describedby", /service-deploy-auth/);
    await expect(page.getByRole("status")).toHaveText("Not deployed");
    await page.getByLabel("Network", { exact: true }).selectOption(NETWORK.id);
    await page.getByLabel("First host port", { exact: true }).fill("31234");
    const reconnect = page.getByRole("link", { name: "Reconnect AUTH", exact: true });
    await expect(reconnect).toHaveAttribute("href", "#/pipe/auth");
    await page.screenshot({ path: testInfo.outputPath("service-auth-disconnected-desktop.png"), fullPage: true });
    await page.setViewportSize({ width: 390, height: 844 });
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    await page.screenshot({ path: testInfo.outputPath("service-auth-disconnected-mobile.png"), fullPage: true });
    await reconnect.click();
    await expect(page.getByRole("heading", { name: "Auth", exact: true })).toBeVisible();
    await expect(page.getByText(/Reconnect AUTH to continue deploying orders/)).toBeVisible();
    await page.reload();
    await expect(page.getByText(/Reconnect AUTH to continue deploying orders/)).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    await page.screenshot({ path: testInfo.outputPath("service-auth-return-context-mobile.png"), fullPage: true });
    await page.getByRole("button", { name: "Connect GitHub", exact: true }).click();
    await page.getByLabel("Personal access token (classic)", { exact: true }).fill("fake-test-token");
    await page.getByRole("button", { name: "Verify and connect", exact: true }).click();
    await expect(page).toHaveURL(/#\/services$/);
    await expect(page.getByRole("heading", { name: "Run: orders", exact: true })).toBeVisible();
    await expect(page.getByLabel("Network", { exact: true })).toHaveValue(NETWORK.id);
    await expect(page.getByLabel("First host port", { exact: true })).toHaveValue("31234");
    await expect(deploy).toBeEnabled();
    await expect(reconnect).toHaveCount(0);
    expect(dialogs).toEqual([]);
    expect(deploys).toEqual([]);
    await page.screenshot({ path: testInfo.outputPath("service-auth-resumed-mobile.png"), fullPage: true });
    await deploy.click();
    await expect(page.getByRole("status")).toHaveText("Queued");
    expect(deploys).toEqual([{ hostPort: 31234, microNetworkId: NETWORK.id }]);
  });

  test("blocks deleted networks and occupied replica ports after AUTH return and lets the user fix both", async ({ page }, testInfo) => {
    const service = { ...INITIAL, domain: null, secrets: {} };
    await openServices(page, [service], false, []);
    let auths: object[] = [];
    let networks: object[] = [NETWORK];
    const deploys: object[] = [];
    await page.route("**/api/pipe/auth", (route) => route.fulfill({ json: auths }));
    await page.route("**/api/micro-networks", (route) => route.fulfill({ json: networks }));
    await page.route("**/api/pipe", (route) => route.fulfill({ json: { config: null, units: [], nginxAvailable: true, cloudflaredAvailable: true } }));
    await page.route("**/api/pipe/auth/github", (route) => { auths = [AUTH]; return route.fulfill({ json: AUTH }); });
    await page.route(`**/api/micro-services/${ID}/deployment/check`, (route) => {
      const request = route.request().postDataJSON();
      const fields: Record<string, string> = {};
      if (!networks.length && request.microNetworkId) fields.microNetworkId = "network does not exist";
      if (!networks.length && request.hostPort === 31234) fields.hostPort = "Host port 31235 is already assigned to a MicroVM";
      return Object.keys(fields).length ? route.fulfill({ status: 400, json: { error: { code: "validation_failed", message: "request validation failed", fields } } }) : route.fulfill({ json: null });
    });
    await page.route(`**/api/micro-services/${ID}/deployment`, (route) => {
      if (route.request().method() !== "POST") return route.fulfill({ json: null });
      const request = route.request().postDataJSON();
      deploys.push(request);
      return route.fulfill({ status: 202, json: { id: SECRET, serviceId: ID, spec: service, phase: "queued", request, instances: [], digest: null, error: null, log: [] } });
    });
    await page.getByRole("button", { name: "Run orders", exact: true }).click();
    await page.getByLabel("Network", { exact: true }).selectOption(NETWORK.id);
    await page.getByLabel("First host port", { exact: true }).fill("31234");
    await page.getByRole("link", { name: "Reconnect AUTH", exact: true }).click();
    networks = [];
    await page.getByRole("button", { name: "Connect GitHub", exact: true }).click();
    await page.getByLabel("Personal access token (classic)", { exact: true }).fill("fake-token");
    await page.getByRole("button", { name: "Verify and connect", exact: true }).click();
    await expect(page.getByRole("heading", { name: "Run: orders", exact: true })).toBeVisible();
    const network = page.getByLabel("Network", { exact: true });
    const port = page.getByLabel("First host port", { exact: true });
    const deploy = page.getByRole("button", { name: "Deploy and run", exact: true });
    await expect(network).toHaveValue(NETWORK.id);
    await expect(network).toHaveAttribute("aria-invalid", "true");
    await expect(network).toBeFocused();
    await expect(port).toHaveValue("31234");
    await expect(port).toHaveAttribute("aria-invalid", "true");
    await expect(page.locator("#service-deploy-port-error")).toContainText("31235");
    await expect(deploy).toBeDisabled();
    await page.locator(".service-deployment form").dispatchEvent("submit");
    expect(deploys).toEqual([]);
    await page.screenshot({ path: testInfo.outputPath("service-placement-invalid-desktop.png"), fullPage: true });
    await page.setViewportSize({ width: 390, height: 844 });
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    await page.screenshot({ path: testInfo.outputPath("service-placement-invalid-mobile.png"), fullPage: true });
    await page.getByRole("button", { name: "Use automatic services network", exact: true }).click();
    await expect(network).toHaveValue("");
    await expect(page.locator("#service-deploy-port-error")).toContainText("31235");
    await expect(deploy).toBeDisabled();
    await port.fill("31300");
    await expect(deploy).toBeEnabled();
    await expect(port).toHaveAttribute("aria-invalid", "false");
    expect(deploys).toEqual([]);
    await deploy.click();
    await expect(page.getByRole("status")).toHaveText("Queued");
    expect(deploys).toEqual([{ hostPort: 31300, microNetworkId: null }]);
  });

  test("blocks deployment while placement cannot be checked and retries without losing choices", async ({ page }) => {
    await openServices(page, [{ ...INITIAL, domain: null, secrets: {} }]);
    expectedConsoleErrors.get(page)!.add("Failed to load resource: the server responded with a status of 503 (Service Unavailable)");
    let fail = true;
    await page.route(`**/api/micro-services/${ID}/deployment/check`, (route) => fail ? route.fulfill({ status: 503, json: { error: { message: "Placement check unavailable" } } }) : route.fulfill({ json: null }));
    await page.getByRole("button", { name: "Run orders", exact: true }).click();
    await expect(page.getByText("Unable to verify deployment choices", { exact: true })).toBeVisible();
    const deploy = page.getByRole("button", { name: "Deploy and run", exact: true });
    await expect(deploy).toBeDisabled();
    await expect(page.getByLabel("First host port", { exact: true })).toBeEditable();
    fail = false;
    await page.getByRole("button", { name: "Retry deployment check", exact: true }).click();
    await expect(deploy).toBeEnabled();
    await expect(page.getByLabel("First host port", { exact: true })).toHaveValue("30100");
  });

  test("ignores a late placement response for an older port and validates the replica range locally", async ({ page }) => {
    await openServices(page, [{ ...INITIAL, domain: null, secrets: {} }]);
    let release!: () => void;
    let started!: () => void;
    const held = new Promise<void>((resolve) => { release = resolve; });
    const pending = new Promise<void>((resolve) => { started = resolve; });
    const checks: number[] = [];
    await page.route(`**/api/micro-services/${ID}/deployment/check`, async (route) => {
      const { hostPort } = route.request().postDataJSON();
      checks.push(hostPort);
      if (hostPort === 31234) { started(); await held; }
      if (hostPort === 31235) return route.fulfill({ status: 400, json: { error: { message: "request validation failed", fields: { hostPort: "Host port 31235 is already used by another process" } } } });
      return route.fulfill({ json: null });
    });
    await page.getByRole("button", { name: "Run orders", exact: true }).click();
    const deploy = page.getByRole("button", { name: "Deploy and run", exact: true });
    const port = page.getByLabel("First host port", { exact: true });
    await expect(deploy).toBeEnabled();
    await port.fill("31234");
    await pending;
    await expect(deploy).toBeDisabled();
    await port.fill("31235");
    await expect(page.locator("#service-deploy-port-error")).toContainText("31235");
    release();
    await expect(deploy).toBeDisabled();
    await port.fill("65534");
    await expect(page.locator("#service-deploy-port-error")).toContainText("65533");
    await expect(deploy).toBeDisabled();
    await port.fill("");
    await expect(deploy).toBeDisabled();
    await port.fill("31400");
    await expect(deploy).toBeEnabled();
    expect(checks).not.toContain(65534);
    expect(checks.every(Number.isInteger)).toBe(true);
  });

  for (const race of ["check", "deploy"] as const) {
    test(`reports a port conflict discovered during the final ${race} request in the editable field`, async ({ page }) => {
      await openServices(page, [{ ...INITIAL, domain: null, secrets: {} }]);
      let occupied = false;
      let deploys = 0;
      const conflict = { error: { message: "request validation failed", fields: { hostPort: "Host port 30101 is already used by another process" } } };
      await page.route(`**/api/micro-services/${ID}/deployment/check`, (route) => occupied && race === "check" ? route.fulfill({ status: 400, json: conflict }) : route.fulfill({ json: null }));
      await page.route(`**/api/micro-services/${ID}/deployment`, (route) => {
        if (route.request().method() !== "POST") return route.fulfill({ json: null });
        deploys += 1;
        return route.fulfill({ status: 400, json: conflict });
      });
      await page.getByRole("button", { name: "Run orders", exact: true }).click();
      const deploy = page.getByRole("button", { name: "Deploy and run", exact: true });
      const port = page.getByLabel("First host port", { exact: true });
      await expect(deploy).toBeEnabled();
      occupied = true;
      await deploy.click();
      await expect(page.locator("#service-deploy-port-error")).toContainText("30101");
      await expect(port).toBeFocused();
      await expect(deploy).toBeDisabled();
      await expect(port).toBeEditable();
      expect(deploys).toBe(race === "check" ? 0 : 1);
    });
  }

  for (const provider of ["github", "aws"] as const) {
    test(`returns only after reconnecting the matching ${provider} account`, async ({ page }) => {
      const expected = provider === "github" ? AUTH : AWS;
      const service = { ...INITIAL, image: provider === "github" ? INITIAL.image : `${AWS.registry}/web:1.0`, domain: null, secrets: {} };
      await openServices(page, [service], false, []);
      let connection: object | null = null;
      let response = provider === "github" ? { ...AUTH, account: "other" }
        : { ...AWS, region: "us-east-1", registry: AWS.registry.replace("ap-northeast-2", "us-east-1") };
      await page.route("**/api/pipe/auth", (route) => route.fulfill({ json: connection ? [connection] : [] }));
      await page.route("**/api/pipe", (route) => route.fulfill({ json: { config: null, units: [], nginxAvailable: true, cloudflaredAvailable: true } }));
      await page.route(`**/api/pipe/auth/${provider}`, (route) => {
        connection = response;
        return route.fulfill({ json: connection });
      });
      await page.getByRole("button", { name: "Run orders", exact: true }).click();
      await page.getByLabel("First host port", { exact: true }).fill("32100");
      await page.getByRole("link", { name: "Reconnect AUTH", exact: true }).click();
      await page.getByRole("button", { name: provider === "github" ? "Connect GitHub" : "Connect AWS", exact: true }).click();
      const fillCredentials = async () => {
        if (provider === "github") {
          await page.getByLabel("Personal access token (classic)", { exact: true }).fill("fake-token");
        } else {
          await page.getByLabel("AWS access key ID", { exact: true }).fill("fake-access-key");
          await page.getByLabel("AWS secret access key", { exact: true }).fill("fake-secret");
          await page.getByLabel("AWS region", { exact: true }).fill(response.region!);
        }
        await page.getByRole("button", { name: "Verify and connect", exact: true }).click();
      };
      await fillCredentials();
      await expect(page.getByRole("alert")).toHaveText("This account does not match the deployment image. Connect its GitHub account or the matching AWS account and region.");
      await expect(page).toHaveURL(/#\/pipe\/auth$/);
      response = expected;
      await page.getByRole("complementary", { name: "Authentication detail" }).getByRole("button", { name: "Update credentials", exact: true }).click();
      await fillCredentials();
      await expect(page).toHaveURL(/#\/services$/);
      await expect(page.getByRole("heading", { name: "Run: orders", exact: true })).toBeVisible();
      await expect(page.getByLabel("Network", { exact: true })).toHaveValue("");
      await expect(page.getByLabel("First host port", { exact: true })).toHaveValue("32100");
      await expect(page.getByRole("button", { name: "Deploy and run", exact: true })).toBeEnabled();
    });
  }

  test("keeps deployment choices when reconnection fails and the user goes back", async ({ page }) => {
    await openServices(page, [{ ...INITIAL, domain: null, secrets: {} }], false, []);
    await page.route("**/api/micro-networks", (route) => route.fulfill({ json: [NETWORK] }));
    await page.route("**/api/pipe", (route) => route.fulfill({ json: { config: null, units: [], nginxAvailable: true, cloudflaredAvailable: true } }));
    await page.route("**/api/pipe/auth/github", (route) => route.fulfill({ status: 400, json: { error: { message: "GitHub rejected the token" } } }));
    await page.getByRole("button", { name: "Run orders", exact: true }).click();
    await page.getByLabel("Network", { exact: true }).selectOption(NETWORK.id);
    await page.getByLabel("First host port", { exact: true }).fill("32200");
    await page.getByRole("link", { name: "Reconnect AUTH", exact: true }).click();
    await page.getByRole("button", { name: "Connect GitHub", exact: true }).click();
    await page.getByLabel("Personal access token (classic)", { exact: true }).fill("rejected-token");
    await page.getByRole("button", { name: "Verify and connect", exact: true }).click();
    await expect(page.getByRole("alert")).toHaveText("GitHub rejected the token");
    await expect(page).toHaveURL(/#\/pipe\/auth$/);
    await page.getByRole("button", { name: "Cancel", exact: true }).click();
    await page.goBack();
    await expect(page.getByRole("heading", { name: "Run: orders", exact: true })).toBeVisible();
    await expect(page.getByLabel("Network", { exact: true })).toHaveValue(NETWORK.id);
    await expect(page.getByLabel("First host port", { exact: true })).toHaveValue("32200");
    await expect(page.getByRole("button", { name: "Deploy and run", exact: true })).toBeDisabled();
    await page.getByRole("button", { name: "Close", exact: true }).click();
    await page.getByRole("button", { name: "Run orders", exact: true }).click();
    await expect(page.getByLabel("First host port", { exact: true })).toHaveValue("30100");
  });

  test("handles a service deleted during AUTH reconnection without opening another service", async ({ page }) => {
    await openServices(page, [{ ...INITIAL, domain: null, secrets: {} }], false, []);
    await page.route("**/api/pipe", (route) => route.fulfill({ json: { config: null, units: [], nginxAvailable: true, cloudflaredAvailable: true } }));
    await page.route("**/api/pipe/auth/github", (route) => route.fulfill({ json: AUTH }));
    await page.getByRole("button", { name: "Run orders", exact: true }).click();
    await page.getByRole("link", { name: "Reconnect AUTH", exact: true }).click();
    await page.route("**/api/micro-services", (route) => route.fulfill({ json: [{ ...INITIAL, id: SECRET, name: "billing" }] }));
    await page.getByRole("button", { name: "Connect GitHub", exact: true }).click();
    await page.getByLabel("Personal access token (classic)", { exact: true }).fill("fake-token");
    await page.getByRole("button", { name: "Verify and connect", exact: true }).click();
    await expect(page).toHaveURL(/#\/services$/);
    await expect(page.getByRole("alert")).toHaveText("The service to resume no longer exists. Select another service.");
    await expect(page.getByRole("heading", { name: /Run:/ })).toHaveCount(0);
    await expect(page.getByRole("cell", { name: "billing", exact: true })).toBeVisible();
  });

  test("disables deployment after AUTH disconnects and restores it only for the matching account", async ({ page }) => {
    await openServices(page, [{ ...INITIAL, domain: null, secrets: {} }]);
    let auths: object[] = [AUTH];
    await page.route("**/api/pipe/auth", (route) => route.fulfill({ json: auths }));
    await page.getByRole("button", { name: "Run orders", exact: true }).click();
    const deploy = page.getByRole("button", { name: "Deploy and run", exact: true });
    await expect(deploy).toBeEnabled();
    auths = [];
    await expect(deploy).toBeDisabled({ timeout: 7000 });
    await expect(page.getByRole("link", { name: "Reconnect AUTH", exact: true })).toBeVisible();
    auths = [{ ...AUTH, account: "team-other" }];
    await page.getByRole("button", { name: "Check again", exact: true }).click();
    await expect(page.getByText("Reconnect AUTH to deploy", { exact: true })).toBeVisible();
    await expect(deploy).toBeDisabled();
    auths = [{ ...AUTH, account: "TEAM" }];
    await expect(deploy).toBeEnabled({ timeout: 7000 });
    await expect(page.getByRole("link", { name: "Reconnect AUTH", exact: true })).toHaveCount(0);
  });

  test("rechecks AUTH before deployment and blocks direct form submissions after disconnection", async ({ page }) => {
    await openServices(page, [{ ...INITIAL, domain: null, secrets: {} }]);
    let auths: object[] = [AUTH];
    let deploys = 0;
    await page.route("**/api/pipe/auth", (route) => route.fulfill({ json: auths }));
    await page.route(`**/api/micro-services/${ID}/deployment`, (route) => {
      if (route.request().method() === "POST") deploys += 1;
      return route.fulfill({ json: null });
    });
    await page.getByRole("button", { name: "Run orders", exact: true }).click();
    const deploy = page.getByRole("button", { name: "Deploy and run", exact: true });
    await expect(deploy).toBeEnabled();
    auths = [];
    await deploy.click();
    await expect(page.getByRole("link", { name: "Reconnect AUTH", exact: true })).toBeVisible();
    await expect(deploy).toBeDisabled();
    await page.locator(".service-deployment form").dispatchEvent("submit");
    await page.getByLabel("First host port", { exact: true }).press("Enter");
    await expect(deploy).toBeDisabled();
    expect(deploys).toBe(0);
  });

  test("fails closed when AUTH cannot be verified and recovers with retry", async ({ page }) => {
    await openServices(page, [{ ...INITIAL, domain: null, secrets: {} }]);
    expectedConsoleErrors.get(page)!.add("Failed to load resource: net::ERR_FAILED");
    let failAuth = true;
    await page.route("**/api/pipe/auth", (route) => failAuth ? route.abort("failed") : route.fulfill({ json: [AUTH] }));
    await page.getByRole("button", { name: "Run orders", exact: true }).click();
    const deploy = page.getByRole("button", { name: "Deploy and run", exact: true });
    await expect(page.getByText("Unable to verify AUTH", { exact: true })).toBeVisible();
    await expect(deploy).toBeDisabled();
    await expect(page.getByRole("link", { name: "Reconnect AUTH", exact: true })).toHaveCount(0);
    failAuth = false;
    await page.getByRole("button", { name: "Retry AUTH check", exact: true }).click();
    await expect(deploy).toBeEnabled();
  });

  test("requires the image's exact ECR account and region with registry access", async ({ page }) => {
    await openServices(page, [{ ...INITIAL, image: `${AWS.registry}/web:1.0`, domain: null, secrets: {} }]);
    let auths: object[] = [AUTH];
    await page.route("**/api/pipe/auth", (route) => route.fulfill({ json: auths }));
    await page.getByRole("button", { name: "Run orders", exact: true }).click();
    const deploy = page.getByRole("button", { name: "Deploy and run", exact: true });
    await expect(page.getByRole("button", { name: "Check again", exact: true })).toBeVisible();
    for (const account of [
      { ...AWS, account: "999999999999", registry: AWS.registry.replace(AWS.account, "999999999999") },
      { ...AWS, region: "us-east-1", registry: AWS.registry.replace("ap-northeast-2", "us-east-1") },
      { ...AWS, services: [] },
    ]) {
      auths = [account];
      await page.getByRole("button", { name: "Check again", exact: true }).click();
      await expect(page.getByText("Reconnect AUTH to deploy", { exact: true })).toBeVisible();
      await expect(deploy).toBeDisabled();
    }
    auths = [AWS];
    await page.getByRole("button", { name: "Check again", exact: true }).click();
    await expect(deploy).toBeEnabled();
  });

  test("checks AUTH again on returning to the tab and ignores a replaced pending check", async ({ page }) => {
    await openServices(page, [{ ...INITIAL, domain: null, secrets: {} }]);
    let auths: object[] = [AUTH];
    let hold = false;
    let started!: () => void;
    let release!: () => void;
    const pendingStarted = new Promise<void>((resolve) => { started = resolve; });
    const pendingReady = new Promise<void>((resolve) => { release = resolve; });
    await page.route("**/api/pipe/auth", async (route) => {
      const snapshot = auths;
      if (hold) { hold = false; started(); await pendingReady; }
      return route.fulfill({ json: snapshot });
    });
    await page.getByRole("button", { name: "Run orders", exact: true }).click();
    const deploy = page.getByRole("button", { name: "Deploy and run", exact: true });
    await expect(deploy).toBeEnabled();
    hold = true;
    await page.evaluate(() => window.dispatchEvent(new Event("focus")));
    await pendingStarted;
    auths = [];
    await page.evaluate(() => window.dispatchEvent(new Event("focus")));
    await expect(page.getByRole("link", { name: "Reconnect AUTH", exact: true })).toBeVisible();
    release();
    await expect(deploy).toBeDisabled();
    await expect(page.getByRole("link", { name: "Reconnect AUTH", exact: true })).toBeVisible();
  });

  test("allows stopping a running service after AUTH is disconnected", async ({ page }) => {
    const service = { ...INITIAL, domain: null, secrets: {}, replicas: 1 };
    await openServices(page, [service], false, []);
    let stopped = false;
    await page.route(`**/api/micro-services/${ID}/deployment`, (route) => route.fulfill({ json: {
      id: SECRET, serviceId: ID, spec: service, phase: stopped ? "stopped" : "running", request: { hostPort: 30100 },
      digest: null, instances: stopped ? [] : [{ vmId: SECRET, ipv4: "172.30.0.2", hostPort: 30100, state: "running", healthy: true }], log: [], error: null,
    } }));
    await page.route(`**/api/micro-services/${ID}/stop`, (route) => {
      stopped = true;
      return route.fulfill({ status: 202, json: { id: SECRET, serviceId: ID, spec: service, phase: "stopped", request: { hostPort: 30100 }, digest: null, instances: [], log: [], error: null } });
    });
    await page.getByRole("button", { name: "Run orders", exact: true }).click();
    await expect(page.getByRole("status")).toHaveText("Running");
    await expect(page.getByText("Existing instances keep running. You can still stop them.", { exact: true })).toBeVisible();
    await expect(page.getByRole("button", { name: "Deploy and run", exact: true })).toBeDisabled();
    page.once("dialog", (dialog) => dialog.accept());
    await page.getByRole("button", { name: "Stop service", exact: true }).click();
    await expect(page.getByRole("status")).toHaveText("Stopped");
    await expect(page.getByRole("button", { name: "Deploy and run", exact: true })).toBeDisabled();
  });

  test("accepts a status response slower than the polling interval", async ({ page }) => {
    await openServices(page, [INITIAL]);
    let activeReads = 0;
    let maxActiveReads = 0;
    await page.route(`**/api/micro-services/${ID}/deployment`, async (route) => {
      activeReads += 1;
      maxActiveReads = Math.max(maxActiveReads, activeReads);
      await new Promise((resolve) => setTimeout(resolve, 4000));
      await route.fulfill({ json: null });
      activeReads -= 1;
    });
    await page.getByRole("button", { name: "Run orders", exact: true }).click();
    await expect(page.getByRole("status")).toHaveText("Not deployed");
    expect(maxActiveReads).toBe(1);
  });

  test("shows a missing Secret error before a deployment is accepted", async ({ page }) => {
    const service = { ...INITIAL, domain: null, replicas: 1 };
    await openServices(page, [service]);
    let submitted = false;
    await page.route(`**/api/micro-services/${ID}/deployment`, async route => {
      if (route.request().method() === "POST") {
        submitted = true;
        return route.fulfill({ status: 400, json: { error: { code: "validation_failed", message: "request validation failed", fields: { secrets: "a referenced PipeSecret does not exist" } } } });
      }
      return route.fulfill({ json: null });
    });
    await page.getByRole("button", { name: "Run orders", exact: true }).click();
    await expect(page.getByRole("status")).toHaveText("Not deployed");
    await page.getByRole("button", { name: "Deploy and run", exact: true }).click();
    await expect(page.getByRole("alert")).toContainText("a referenced PipeSecret does not exist");
    await expect(page.getByRole("status")).toHaveText("Not deployed");
    await expect(page.getByRole("button", { name: "Deploy and run", exact: true })).toBeEnabled();
    expect(submitted).toBe(true);
  });

  test("opens updated runtime settings after saving and preserves mobile headers", async ({ page }) => {
    const writes = await openServices(page, [{ ...INITIAL, domain: null, secrets: {}, securityGroupId: SECRET }]);
    await page.route(`**/api/micro-services/${ID}/deployment`, (route) => route.fulfill({ json: null }));
    await page.getByRole("button", { name: "Run orders", exact: true }).click();
    await expect(page.getByRole("status")).toHaveText("Not deployed");
    await page.getByRole("button", { name: "Edit orders", exact: true }).click();
    await next(page);
    await page.getByLabel("App port", { exact: true }).fill("9090");
    await next(page);
    await page.getByLabel("Replicas", { exact: true }).fill("3");
    await next(page);
    await page.getByRole("button", { name: "Save configuration" }).click();
    await expect(page.getByRole("heading", { name: "Run: orders", exact: true })).toBeVisible();
    await expect(page.getByRole("heading", { name: "Edit: orders", exact: true })).toHaveCount(0);
    expect(writes[0].body.securityGroupId).toBe(SECRET);
    await expect(page.getByText("Each replica uses one host port, starting at 30100. App port: 9090.", { exact: true })).toBeVisible();
    await expect(page.getByLabel("First host port")).toHaveAttribute("max", "65533");
    await page.setViewportSize({ width: 390, height: 844 });
    await expect(page.getByRole("columnheader", { name: "Port", exact: true })).toHaveCount(1);
    await expect(page.getByRole("cell", { name: /9090/ })).toHaveAttribute("headers", "service-column-2");
  });
  const browserErrors = new WeakMap<Page, string[]>();
  const expectedConsoleErrors = new WeakMap<Page, Set<string>>();
  test.beforeEach(({ page }) => {
    const errors: string[] = [];
    browserErrors.set(page, errors);
    expectedConsoleErrors.set(page, new Set(["Failed to load resource: the server responded with a status of 400 (Bad Request)"]));
    page.on("pageerror", (error) => errors.push(error.message));
    page.on("console", (message) => {
      if (message.type() === "error" && !expectedConsoleErrors.get(page)?.has(message.text())) errors.push(message.text());
    });
  });
  test.afterEach(({ page }) => expect(browserErrors.get(page)).toEqual([]));
  test("shows a failed deployment and allows a retry without claiming success", async ({ page }) => {
    await openServices(page, [{ ...INITIAL, domain: null, secrets: {} }]);
    await page.route(`**/api/micro-services/${ID}/deployment`, (route) => route.fulfill({ json: {
      id: SECRET, serviceId: ID, spec: INITIAL, request: { hostPort: 30200 },
      phase: "failed", digest: null, instances: [], log: ["App health check failed"], error: "App health check failed",
    } }));
    await page.getByRole("button", { name: "Run orders", exact: true }).click();
    await expect(page.getByRole("status")).toHaveText("Deployment failed");
    await expect(page.getByRole("alert")).toHaveText("App health check failed");
    await expect(page.getByRole("button", { name: "Deploy and run", exact: true })).toBeEnabled();
    await expect(page.getByRole("button", { name: "Stop service", exact: true })).toHaveCount(0);
  });
  test("creates, reads, edits and deletes configurations", async ({ page }, testInfo) => {
    const writes = await openServices(page);
    await expect(page.getByText("No service configurations yet.")).toBeVisible();
    await page.getByRole("button", { name: "New MicroService", exact: true }).click();
    await page.getByLabel("Name", { exact: true }).fill("orders");
    await selectApp(page);
    await next(page);
    await next(page);
    await advanced(page);
    await page.getByRole("button", { name: "Add variable", exact: true }).click();
    await page.locator("#service-env-key-0").fill("PROFILE");
    await page.locator("#service-env-value-0").fill("prod");
    await page.getByRole("button", { name: "Add secret reference", exact: true }).click();
    await page.locator("#service-secrets-key-0").fill("DB_PASSWORD");
    await page.locator("#service-secrets-value-0").fill(SECRET);
    await next(page);
    await page.screenshot({ path: testInfo.outputPath("microservice-desktop.png"), fullPage: true });
    await page.getByRole("button", { name: "Save configuration" }).click();
    await expect(page.getByRole("cell", { name: "orders", exact: true })).toBeVisible();
    expect(writes[0]).toEqual({ method: "POST", body: {
      name: "orders", image: "ghcr.io/team/orders:1.0", runtime: "vm", port: 8080,
      healthPath: "/health", domain: null, ram: 512, cpu: 1, replicas: 1,
      startupTimeout: 300, drainTimeout: 30, env: { PROFILE: "prod" }, secrets: { DB_PASSWORD: SECRET },
    } });

    await page.getByRole("button", { name: "Edit orders", exact: true }).click();
    await page.getByLabel("App version", { exact: true }).selectOption("2.0");
    await next(page);
    await next(page);
    await advanced(page);
    await expect(page.locator("#service-secrets-value-0")).toHaveValue(SECRET);
    await page.getByLabel("Replicas", { exact: true }).fill("2");
    await next(page);
    await page.getByRole("button", { name: "Save configuration" }).click();
    await expect(page.getByRole("cell", { name: "ghcr.io/team/orders:2.0", exact: true })).toBeVisible();
    expect(writes[1].method).toBe("PUT");
    expect(writes[1].body).not.toHaveProperty("id");
    expect(writes[1].body.replicas).toBe(2);
    page.once("dialog", (dialog) => dialog.accept());
    await page.getByRole("button", { name: "Delete orders", exact: true }).click();
    await expect(page.getByText("No service configurations yet.")).toBeVisible();
  });

  test("keeps edits after validation failure and cancels without saving", async ({ page }) => {
    const writes = await openServices(page, [INITIAL], true);
    await page.getByRole("button", { name: "Edit orders", exact: true }).click();
    await page.getByLabel("Name", { exact: true }).fill("billing");
    await next(page);
    await expect(page.getByLabel("Health path", { exact: true })).toHaveValue("/actuator/health");
    await next(page);
    await advanced(page);
    await expect(page.getByLabel("Startup timeout (s)")).toHaveValue("120");
    await expect(page.getByLabel("Drain timeout (s)")).toHaveValue("60");
    await next(page);
    await page.getByRole("button", { name: "Save configuration" }).click();
    await expect(page.getByText("is already registered", { exact: true })).toBeVisible();
    await expect(page.getByLabel("Name", { exact: true })).toHaveValue("billing");
    await page.getByRole("button", { name: "Cancel", exact: true }).click();
    await expect(page.getByRole("cell", { name: "orders", exact: true })).toBeVisible();
    expect(writes).toHaveLength(1);
  });

  test("supports Korean and mobile editing without page overflow", async ({ page }, testInfo) => {
    await openServices(page, [INITIAL]);
    await page.getByRole("combobox", { name: "Language", exact: true }).selectOption("ko");
    await page.setViewportSize({ width: 390, height: 844 });
    await page.getByRole("button", { name: "orders 수정", exact: true }).click();
    await expect(page.getByRole("heading", { name: "수정: orders", exact: true })).toBeVisible();
    await page.getByRole("button", { name: "다음", exact: true }).click();
    await expect(page.getByLabel("상태 검사 경로")).toHaveValue("/actuator/health");
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    await expect(page.locator(".service-steps [aria-current=step]")).toContainText("접속 설정");
    await page.screenshot({ path: testInfo.outputPath("microservice-mobile.png"), fullPage: true });
    for (let index = 0; index < 2; index++) {
      await page.getByRole("button", { name: "다음", exact: true }).click();
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    }
    await expect(page.getByText("마지막으로 설정을 확인한다", { exact: true })).toBeVisible();
    await page.screenshot({ path: testInfo.outputPath("microservice-review-mobile.png"), fullPage: true });
  });

  test("selects an AUTH app, preserves values when going back, and continues to deployment without starting it", async ({ page }, testInfo) => {
    const writes = await openServices(page);
    await page.getByRole("button", { name: "New MicroService", exact: true }).click();
    await page.getByRole("combobox", { name: "Language", exact: true }).selectOption("ko");
    await page.screenshot({ path: testInfo.outputPath("guided-setup-desktop.png"), fullPage: true });
    await page.setViewportSize({ width: 390, height: 844 });
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    await expect(page.getByRole("button", { name: "다음", exact: true })).toBeInViewport();
    await page.screenshot({ path: testInfo.outputPath("guided-setup-mobile.png"), fullPage: true });
    await page.setViewportSize({ width: 1280, height: 720 });
    await page.getByRole("combobox", { name: /Language|언어/, exact: true }).selectOption("en");
    await page.getByLabel("Name", { exact: true }).fill("hello-web");
    await selectApp(page);
    await expect(page.getByText("ghcr.io/team/orders:1.0", { exact: true }).first()).toBeVisible();
    await next(page);
    await expect(page.getByLabel("App port", { exact: true })).toHaveValue("8080");
    await expect(page.getByLabel("Health path", { exact: true })).toHaveValue("/health");
    await page.getByLabel("App port", { exact: true }).fill("8081");
    await page.getByRole("button", { name: "Previous", exact: true }).click();
    await next(page);
    await expect(page.getByLabel("App port", { exact: true })).toHaveValue("8081");
    await next(page);
    await expect(page.getByLabel("Memory per instance", { exact: true })).toHaveValue("512");
    await expect(page.locator("summary").filter({ hasText: "Advanced settings" }).locator("..")).not.toHaveAttribute("open");
    await next(page);
    await expect(page.getByRole("heading", { name: "Ready to save your app?", exact: true })).toBeFocused();
    expect(writes).toHaveLength(0);
    await page.getByRole("button", { name: "Save configuration", exact: true }).click();
    await expect(page.getByRole("heading", { name: "Run: hello-web", exact: true })).toBeVisible();
    await expect(page.getByRole("status")).toHaveText("Not deployed");
    await expect(page.getByRole("button", { name: "Deploy and run", exact: true })).toBeEnabled();
    expect(writes).toHaveLength(1);
    expect(writes[0].body).toMatchObject({ image: "ghcr.io/team/orders:1.0", port: 8081, healthPath: "/health", replicas: 1 });
  });

  test("validates each step and prevents Enter from saving before review", async ({ page }) => {
    const writes = await openServices(page);
    await page.getByRole("button", { name: "New MicroService", exact: true }).click();
    await expect(page.getByRole("button", { name: "Continue", exact: true })).toBeDisabled();
    await selectApp(page);
    await next(page);
    await expect(page.locator(".service-steps [aria-current=step]")).toHaveText("1App");
    await page.getByLabel("Name", { exact: true }).fill("Invalid Name");
    await next(page);
    await expect(page.getByLabel("Name", { exact: true })).toBeFocused();
    await page.getByLabel("Name", { exact: true }).fill("my-app");
    await page.getByLabel("Name", { exact: true }).press("Enter");
    await expect(page.getByRole("heading", { name: "How does your app respond?", exact: true })).toBeVisible();
    await page.getByLabel("App port", { exact: true }).fill("0");
    await next(page);
    await expect(page.getByLabel("App port", { exact: true })).toBeFocused();
    await page.getByLabel("App port", { exact: true }).fill("80");
    await page.getByLabel("Health path", { exact: true }).fill("//wrong");
    await next(page);
    await expect(page.getByLabel("Health path", { exact: true })).toHaveAttribute("aria-invalid", "true");
    await page.getByLabel("Health path", { exact: true }).fill("/");
    await next(page);
    await advanced(page);
    await page.getByLabel("Startup timeout (s)").fill("0");
    await page.locator("summary").filter({ hasText: "Advanced settings" }).click();
    await next(page);
    await expect(page.getByLabel("Startup timeout (s)")).toBeVisible();
    expect(writes).toHaveLength(0);
  });

  test("rejects duplicate and conflicting environment variable names without losing input", async ({ page }) => {
    const writes = await openServices(page);
    await page.getByRole("button", { name: "New MicroService", exact: true }).click();
    await page.getByLabel("Name", { exact: true }).fill("hello-web");
    await selectApp(page);
    await next(page);
    await next(page);
    await advanced(page);
    for (let index = 0; index < 2; index++) {
      await page.getByRole("button", { name: "Add variable", exact: true }).click();
      await page.locator(`#service-env-key-${index}`).fill("PROFILE");
      await page.locator(`#service-env-value-${index}`).fill("prod");
    }
    await next(page);
    await expect(page.getByRole("alert")).toContainText("unique variable name");
    await page.getByRole("button", { name: "Remove env row 2", exact: true }).click();
    await page.getByRole("button", { name: "Add secret reference", exact: true }).click();
    await page.locator("#service-secrets-key-0").fill("PROFILE");
    await page.locator("#service-secrets-value-0").fill(SECRET);
    await next(page);
    await expect(page.getByRole("alert")).toContainText("cannot appear in both");
    await expect(page.locator("#service-env-value-0")).toHaveValue("prod");
    expect(writes).toHaveLength(0);
  });

  test("keeps App usable when security groups fail and retries from Connection", async ({ page }) => {
    const writes = await openServices(page);
    let unavailable = true;
    expectedConsoleErrors.get(page)?.add("Failed to load resource: the server responded with a status of 503 (Service Unavailable)");
    await page.route("**/api/security-groups", route => unavailable
      ? route.fulfill({ status: 503, json: { error: { message: "Security groups unavailable" } } })
      : route.fulfill({ json: [{ id: SECRET, name: "orders-policy", microNetworkId: ID, rules: [] }] }));
    await page.getByRole("button", { name: "New MicroService", exact: true }).click();
    await page.getByLabel("Name", { exact: true }).fill("web");
    await selectApp(page);
    await next(page);
    await expect(page.getByLabel("Security group", { exact: true })).toBeDisabled();
    await expect(page.getByText("Security groups unavailable", { exact: true })).toBeVisible();
    unavailable = false;
    await page.getByRole("button", { name: "Retry loading security groups", exact: true }).click();
    await expect(page.getByLabel("Security group", { exact: true })).toBeEnabled();
    await page.getByLabel("Security group", { exact: true }).selectOption(SECRET);
    await next(page);
    await next(page);
    await page.getByRole("button", { name: "Save configuration", exact: true }).click();
    await expect.poll(() => writes.length).toBe(1);
    expect(writes[0].body.securityGroupId).toBe(SECRET);
  });

  test("edits Secret references through the wizard without blocking deployment", async ({ page }) => {
    const writes = await openServices(page, [{ ...INITIAL, domain: null }]);
    await page.getByRole("button", { name: "Run orders", exact: true }).click();
    await expect(page.getByRole("button", { name: "Deploy and run", exact: true })).toBeEnabled();
    await expect(page.getByText("Secret values are injected into every replica. Changes apply on the next deployment.", { exact: true })).toBeVisible();
    await page.getByRole("button", { name: "Edit orders", exact: true }).click();
    await next(page);
    await next(page);
    await advanced(page);
    await expect(page.locator("#service-secrets-key-0")).toHaveValue("DB_PASSWORD");
    await expect(page.locator("#service-secrets-value-0")).toHaveValue(SECRET);
    await page.locator("#service-secrets-value-0").fill(NETWORK.id);
    await next(page);
    await page.getByRole("button", { name: "Save configuration", exact: true }).click();
    await expect(page.getByRole("button", { name: "Deploy and run", exact: true })).toBeEnabled();
    expect(writes[0].body.secrets).toEqual({ DB_PASSWORD: NETWORK.id });
  });

  test("prevents deployment while status is unavailable and recovers on the next poll", async ({ page }) => {
    await openServices(page, [{ ...INITIAL, domain: null, secrets: {} }]);
    let unavailable = true;
    let deploys = 0;
    await page.route(`**/api/micro-services/${ID}/deployment`, (route) => {
      if (route.request().method() === "POST") deploys++;
      if (unavailable) return route.abort("failed");
      return route.fulfill({ json: null });
    });
    expectedConsoleErrors.get(page)!.add("Failed to load resource: net::ERR_FAILED");
    await page.getByRole("button", { name: "Run orders", exact: true }).click();
    await expect(page.getByRole("status")).toHaveText("Status unavailable");
    await expect(page.getByRole("button", { name: "Deploy and run", exact: true })).toBeDisabled();
    unavailable = false;
    await expect(page.getByRole("status")).toHaveText("Not deployed");
    await expect(page.getByRole("button", { name: "Deploy and run", exact: true })).toBeEnabled();
    expect(deploys).toBe(0);
  });

  test("requires AUTH before selecting an app and offers no manual image or public example", async ({ page }) => {
    const writes = await openServices(page, [], false, []);
    await page.getByRole("button", { name: "New MicroService", exact: true }).click();
    await expect(page.getByText("Connect an account first", { exact: true })).toBeVisible();
    await expect(page.getByRole("link", { name: "Connect in AUTH", exact: true })).toHaveAttribute("href", "#/pipe/auth");
    await expect(page.getByRole("button", { name: "Continue", exact: true })).toBeDisabled();
    await expect(page.getByLabel("OCI image", { exact: true })).toHaveCount(0);
    await expect(page.getByRole("button", { name: "Use example", exact: true })).toHaveCount(0);
    expect(writes).toHaveLength(0);
  });

  test("keeps a console account out of the app's registry account choices", async ({ page }) => {
    const account = { provider: "firecrab", account: "owner@example.com", registry: null, region: null, services: [] };
    await openServices(page, [], false, [account]);
    await page.getByRole("button", { name: "New MicroService", exact: true }).click();
    await expect(page.getByText("Connect an account first", { exact: true })).toBeVisible();
    await expect(page.getByRole("button", { name: "Continue", exact: true })).toBeDisabled();
    await page.route("**/api/pipe/auth", (route) => route.fulfill({ json: [account, AUTH] }));
    await page.getByRole("button", { name: "Check connections", exact: true }).click();
    await expect(page.getByLabel("AUTH account", { exact: true }).getByRole("option", { name: /owner@example.com/ })).toHaveCount(0);
    await selectApp(page);
    await expect(page.locator(".service-selected-image code")).toHaveText("ghcr.io/team/orders:1.0");
  });

  test("resets the app when switching AUTH accounts and discards late responses", async ({ page }) => {
    await openServices(page, [], false, [AUTH, AWS]);
    let releaseTags!: () => void;
    const pending = new Promise<void>((resolve) => { releaseTags = resolve; });
    await page.route("**/api/pipe/registry-tags?*", async (route) => {
      const reference = new URL(route.request().url()).searchParams.get("reference") ?? "";
      if (reference.startsWith("ghcr.io/")) await pending;
      await route.fulfill({ json: { tags: ["1.0", "2.0"], nextLast: null } });
    });
    await page.getByRole("button", { name: "New MicroService", exact: true }).click();
    await page.getByLabel("AUTH account", { exact: true }).selectOption("github");
    await page.getByLabel("App repository", { exact: true }).selectOption("team/orders");
    await page.getByLabel("AUTH account", { exact: true }).selectOption("aws");
    releaseTags();
    await expect(page.getByLabel("App repository", { exact: true })).toHaveValue("");
    await expect(page.getByRole("button", { name: "Continue", exact: true })).toBeDisabled();
    await selectApp(page, "web");
    await expect(page.locator(".service-selected-image code")).toHaveText(`${AWS.registry}/web:1.0`);
    await expect(page.getByLabel("App repository", { exact: true }).getByRole("option", { name: "team/orders", exact: true })).toHaveCount(0);
  });

  test("rechecks AUTH before saving and rejects an account disconnected during setup", async ({ page }) => {
    const writes = await openServices(page);
    await page.getByRole("button", { name: "New MicroService", exact: true }).click();
    await page.getByLabel("Name", { exact: true }).fill("orders");
    await selectApp(page);
    await next(page); await next(page); await next(page);
    await page.route("**/api/pipe/auth", (route) => route.fulfill({ json: [] }));
    await page.getByRole("button", { name: "Save configuration", exact: true }).click();
    await expect(page.getByRole("alert")).toContainText("no longer connected");
    await expect(page.getByText("Connect an account first", { exact: true })).toBeVisible();
    await expect(page.getByRole("button", { name: "Continue", exact: true })).toBeDisabled();
    expect(writes).toHaveLength(0);
  });

  test("loads additional repositories and versions only from the selected AUTH account", async ({ page }) => {
    await openServices(page);
    await page.route("**/api/pipe/registries/**/repositories?*", (route) => {
      const url = new URL(route.request().url());
      const more = url.searchParams.get("page") === "2";
      return route.fulfill({ json: { repositories: more ? ["team/second-page"] : ["team/orders"], listingSupported: true, nextPage: more ? null : 2, nextCursor: null } });
    });
    await page.route("**/api/pipe/registry-tags?*", (route) => {
      const more = new URL(route.request().url()).searchParams.get("last") === "1.0";
      return route.fulfill({ json: { tags: more ? ["2.0"] : ["1.0"], nextLast: more ? null : "1.0" } });
    });
    await page.getByRole("button", { name: "New MicroService", exact: true }).click();
    await page.getByRole("button", { name: "More repositories", exact: true }).click();
    await page.getByLabel("App repository", { exact: true }).selectOption("team/second-page");
    await page.getByRole("button", { name: "More versions", exact: true }).click();
    await page.getByLabel("App version", { exact: true }).selectOption("2.0");
    await expect(page.locator(".service-selected-image code")).toHaveText("ghcr.io/team/second-page:2.0");
  });
});

test("domain deployment inherits the security-group network and shows DNS guidance on mobile", async ({ page }, testInfo) => {
  const service = { ...INITIAL, replicas: 1, secrets: {}, securityGroupId: SECRET };
  await openServices(page, [service]);
  let deployment: Record<string, unknown> | null = null;
  await page.route(`**/api/micro-services/${ID}/deployment`, async route => {
    if (route.request().method() === "POST") {
      expect(route.request().postDataJSON()).toEqual({ microNetworkId: ID });
      deployment = { id: SECRET, serviceId: ID, spec: service, phase: "running", request: { hostPort: 0 }, domainRouteOwned: true,
        instances: [{ vmId: SECRET, ipv4: "172.30.0.2", hostPort: 0, state: "running", healthy: true }], digest: null, error: null, log: ["Domain connected"] };
      return route.fulfill({ status: 202, json: deployment });
    }
    return route.fulfill({ json: deployment });
  });
  await page.route("**/api/pipe", route => route.fulfill({ json: { config: { listen: "127.0.0.1:8080", routes: [], tunnel: { id: SECRET, credentialsFile: "hidden" } }, units: [], nginxAvailable: true, cloudflaredAvailable: true } }));
  await page.getByRole("button", { name: "Run orders", exact: true }).click();
  await expect(page.getByRole("status")).toHaveText("Not deployed");
  await expect(page.getByLabel("First host port")).toHaveCount(0);
  await expect(page.getByLabel("Network", { exact: true })).not.toBeVisible();
  await page.getByText("Advanced settings", { exact: true }).click();
  await expect(page.getByLabel("Network", { exact: true })).toHaveValue(ID);
  await expect(page.getByLabel("Network", { exact: true })).toBeDisabled();
  await expect(page.getByText(`${SECRET}.cfargotunnel.com`, { exact: true })).toBeVisible();
  await expect(page.getByRole("link", { name: "Open domain", exact: true })).toHaveCount(0);
  await page.getByRole("button", { name: "Deploy and run", exact: true }).click();
  await expect(page.getByRole("link", { name: "Open domain", exact: true })).toHaveAttribute("href", "https://orders.example.com");
  await expect(page.getByRole("link", { name: ":0", exact: true })).toHaveCount(0);
  await page.setViewportSize({ width: 390, height: 844 });
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  await page.screenshot({ path: testInfo.outputPath("service-domain-mobile.png"), fullPage: true });
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.getByRole("combobox", { name: "Language", exact: true }).selectOption("ko");
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(page.getByRole("link", { name: "도메인 열기", exact: true })).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
});

test("accepts a pasted domain URL", async ({ page }) => {
  const writes = await openServices(page);
  await page.getByRole("button", { name: "New MicroService", exact: true }).click();
  await page.getByLabel("Name", { exact: true }).fill("web");
  await selectApp(page);
  await next(page);
  await page.getByText("Domain configuration", { exact: true }).click();
  await page.getByLabel("Domain (optional)", { exact: true }).fill("https://Orders.Example.COM/");
  await next(page);
  await next(page);
  await page.getByRole("button", { name: "Save configuration", exact: true }).click();
  await expect.poll(() => writes.length).toBe(1);
  expect(writes[0].body.domain).toBe("orders.example.com");
});

const RELEASE_A = "11111111-1111-4111-8111-111111111111";
const RELEASE_B = "22222222-2222-4222-8222-222222222222";
const DEPLOY_SPEC = { ...INITIAL, cpu: 1, ram: 128, replicas: 1, secrets: {} };
function release(id: string, image = "ghcr.io/team/orders:1.0") {
  return { id, serviceId: ID, phase: "running", spec: { ...DEPLOY_SPEC, image }, request: { microNetworkId: NETWORK.id, hostPort: 0 },
    digest: `sha256:${(id === RELEASE_A ? "a" : "b").repeat(64)}`, instances: [{ vmId: SECRET, ipv4: "172.30.0.2", hostPort: 0, state: "running", healthy: true }],
    domainRouteOwned: true, log: ["All replicas answered the app health check"], error: null, startedAt: 1792886400, finishedAt: 1792886410 };
}

test.describe("Deploy release management @dashboard", () => {
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
    await page.getByRole("button", { name: "Run orders", exact: true }).click();
    await page.getByRole("button", { name: "Deploy update", exact: true }).click();
    await expect(page.getByLabel("Name", { exact: true })).toBeDisabled();
    await selectApp(page, "team/orders", "2.0");
    await next(page);
    await expect(page.getByLabel("Domain (optional)", { exact: true })).toBeDisabled();
    await next(page);
    await page.getByLabel("Memory per instance", { exact: true }).selectOption("256");
    await next(page);
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
    await page.getByRole("button", { name: "Run orders", exact: true }).click();
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
    await page.getByRole("button", { name: "Run orders", exact: true }).click();
    await page.getByRole("button", { name: "Release history", exact: true }).click();
    await expect(page.getByText("Release storage unavailable", { exact: true })).toBeVisible();
    await expect(page.getByText("No successful releases are retained yet.", { exact: true })).toHaveCount(0);
    await page.getByRole("button", { name: "Retry Deploy", exact: true }).click();
    expect(retries).toEqual([{ operationId: RELEASE_B }]);
    await expect(page.getByText("Draining previous requests", { exact: true })).toBeVisible();
    await expect(page.getByRole("button", { name: "Stop service", exact: true })).toBeDisabled();
  });
});
