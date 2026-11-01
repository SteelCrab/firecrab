import { test } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";
import { expect } from "@playwright/test";
import { createFixtureServer } from "./fixture-server.mjs";

test("MicroService configuration includes its security group, S3 and Queue binding", async () => {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  const browser = await chromium.launch({ headless: true }),
    page = await browser.newPage();
  fixture.state.services = [];
  fixture.state.securityGroups = [
    { id: "group-1", name: "web", microNetworkId: "network-1", rules: [] },
  ];
  fixture.state.storageConnections = [
    {
      id: "s3-1",
      name: "files",
      bucket: "bucket",
      prefix: "",
      region: "ap-northeast-2",
      credentialRef: "pipeAuthAws",
      services: [],
    },
  ];
  fixture.handlers.set("GET /api/queues", ({ send }) =>
    send(200, [{ id: "q-1", name: "jobs", phase: "ready" }]),
  );
  fixture.handlers.set("POST /api/micro-services", ({ body, send }) => {
    fixture.state.services.push({ id: "service-1", ...body });
    send(201, { id: "service-1", ...body });
  });
  try {
    fixture.state.auth = [
      {
        provider: "github",
        account: "team",
        registry: "ghcr.io",
        region: null,
        services: ["registry"],
      },
    ];
    await page.goto(`${fixture.url}/#/services`);
    await page
      .locator(".screen-header")
      .getByRole("link", { name: "New MicroService" })
      .click();
    await page.locator("#service-name").fill("worker");
    await page.locator("#service-image").fill("ghcr.io/team/nginx:1.27");
    await page.getByRole("button", { name: "Next", exact: true }).click();
    await page.locator("#service-security-group").selectOption("group-1");
    await page.getByRole("button", { name: "Next", exact: true }).click();
    await page.getByRole("button", { name: "Next", exact: true }).click();
    await page.locator("#service-storage").selectOption("s3-1");
    await page.getByRole("button", { name: "Add queue" }).click();
    const queue = page
      .locator("fieldset")
      .filter({ has: page.getByRole("button", { name: "Add queue" }) });
    await queue.getByLabel("Variable").fill("JOBS");
    await queue.locator("select").first().selectOption("q-1");
    await queue.getByLabel("Access").selectOption("consume");
    await page.getByRole("button", { name: "Next", exact: true }).click();
    assert.equal(
      await page.locator("#service-steps").getAttribute("data-step"),
      "4",
      JSON.stringify(
        await page.locator("#editor :invalid").evaluateAll((nodes) =>
          nodes.map((node) => ({
            id: node.id,
            value: node.value,
            message: node.validationMessage,
          })),
        ),
      ),
    );
    assert.equal(
      fixture.requests.filter(
        (request) =>
          request.method === "POST" && request.path === "/api/micro-services",
      ).length,
      0,
    );
    await page.getByRole("button", { name: "Save configuration" }).click();
    await expect
      .poll(() =>
        fixture.requests.some(
          (r) => r.method === "POST" && r.path === "/api/micro-services",
        ),
      )
      .toBe(true);
    const body = fixture.requests.find(
      (r) => r.method === "POST" && r.path === "/api/micro-services",
    ).body;
    assert.equal(body.securityGroupId, "group-1");
    assert.equal(body.storageConnectionId, "s3-1");
    assert.deepEqual(body.queues, {
      JOBS: { queueId: "q-1", access: "consume" },
    });
  } finally {
    await page.close();
    await browser.close();
    await fixture.close();
  }
});

test("MicroService rollback submits the selected release and expected current release", async () => {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  const browser = await chromium.launch({ headless: true }),
    page = await browser.newPage();
  const spec = {
    name: "web",
    image: "ghcr.io/team/nginx:1.27",
    runtime: "vm",
    port: 80,
    healthPath: "/",
    domain: "web.example.com",
    ram: 512,
    cpu: 1,
    replicas: 1,
    startupTimeout: 300,
    drainTimeout: 30,
    env: {},
    secrets: {},
    queues: {},
  };
  fixture.state.services = [{ id: "service-1", ...spec }];
  fixture.handlers.set(
    "GET /api/micro-services/service-1/deployment",
    ({ send }) =>
      send(200, {
        id: "release-current",
        serviceId: "service-1",
        phase: "running",
        spec,
        request: { hostPort: 30100, microNetworkId: "network-1" },
        instances: [],
        log: [],
        error: null,
        domainRouteOwned: true,
        update: null,
      }),
  );
  fixture.handlers.set(
    "GET /api/micro-services/service-1/releases",
    ({ send }) =>
      send(200, [
        { id: "release-current", spec },
        {
          id: "release-old",
          digest: `sha256:${"a".repeat(64)}`,
          spec: { ...spec, image: "nginx:1.26" },
        },
      ]),
  );
  fixture.handlers.set(
    "POST /api/micro-services/service-1/rollback",
    ({ send }) => send(200, { id: "new-release" }),
  );
  try {
    page.on("dialog", (dialog) => dialog.accept());
    await page.goto(`${fixture.url}/#/services`);
    await page.getByRole("button", { name: "Open web" }).click();
    await page.getByRole("tab", { name: "Releases", exact: true }).click();
    await page.getByLabel("Rollback release").selectOption("release-old");
    await page
      .getByRole("button", {
        name: "Roll back to selected release",
        exact: true,
      })
      .click();
    await expect
      .poll(() =>
        fixture.requests.some(
          (r) => r.path === "/api/micro-services/service-1/rollback",
        ),
      )
      .toBe(true);
    const body = fixture.requests.find(
      (r) => r.path === "/api/micro-services/service-1/rollback",
    ).body;
    assert.equal(body.expectedReleaseId, "release-current");
    assert.equal(body.releaseId, "release-old");
    assert.equal(body.strategy, "rolling");
    assert.ok(body.operationId);
  } finally {
    await page.close();
    await browser.close();
    await fixture.close();
  }
});

test("deployment token is shown only in its open panel and can be dismissed", async () => {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  const browser = await chromium.launch({ headless: true }),
    page = await browser.newPage();
  const spec = {
    name: "web",
    image: "ghcr.io/team/nginx:1.27",
    runtime: "vm",
    port: 80,
    healthPath: "/",
    domain: "web.example.com",
    ram: 512,
    cpu: 1,
    replicas: 1,
    startupTimeout: 300,
    drainTimeout: 30,
    env: {},
    secrets: {},
    queues: {},
  };
  fixture.state.services = [{ id: "service-1", ...spec }];
  fixture.handlers.set(
    "GET /api/micro-services/service-1/deployment",
    ({ send }) =>
      send(200, {
        id: "release-current",
        serviceId: "service-1",
        phase: "running",
        spec,
        request: { hostPort: 30100, microNetworkId: "network-1" },
        instances: [],
        log: [],
        error: null,
        domainRouteOwned: true,
        update: null,
      }),
  );
  fixture.handlers.set(
    "GET /api/micro-services/service-1/releases",
    ({ send }) => send(200, []),
  );
  fixture.handlers.set(
    "GET /api/micro-services/service-1/deploy-tokens",
    ({ send }) => send(200, []),
  );
  fixture.handlers.set(
    "POST /api/micro-services/service-1/deploy-tokens",
    ({ send }) =>
      send(200, {
        token: "fixture-token",
        credential: {
          id: "token-1",
          name: "CI",
          repository: "team/app",
          expiresAt: 1924992000,
        },
      }),
  );
  try {
    await page.goto(`${fixture.url}/#/services`);
    await page.getByRole("button", { name: "Open web" }).click();
    await page.getByRole("tab", { name: "Settings", exact: true }).click();
    await page.getByRole("button", { name: "Deployment tokens" }).click();
    await page.getByLabel("Name", { exact: true }).fill("CI");
    await page.getByRole("button", { name: "Create token" }).click();
    const token = page.getByLabel("Token", { exact: true });
    await expect(token).toHaveValue("fixture-token");
    await expect(token).toHaveAttribute("type", "password");
    await page.getByRole("button", { name: "Reveal", exact: true }).click();
    await expect(token).toHaveAttribute("type", "text");
    await page.getByRole("button", { name: "Deployment tokens" }).click();
    await expect(token).toHaveCount(0);
    assert.deepEqual(
      fixture.requests.find(
        (r) =>
          r.path === "/api/micro-services/service-1/deploy-tokens" &&
          r.method === "POST",
      ).body,
      { name: "CI", expiresInSeconds: 2592000 },
    );
  } finally {
    await page.close();
    await browser.close();
    await fixture.close();
  }
});

test("private service deployment keeps host ports closed and preserves cleanup controls", async () => {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  const spec = {
    name: "private-app",
    image: "ghcr.io/team/nginx:1.27",
    runtime: "vm",
    port: 80,
    healthPath: "/",
    domain: null,
    ram: 512,
    cpu: 1,
    replicas: 1,
    startupTimeout: 300,
    drainTimeout: 30,
    env: {},
    secrets: {},
    queues: {},
  };
  fixture.state.services = [{ ...spec, id: "service-private" }];
  let deployment = null;
  fixture.handlers.set(
    "GET /api/micro-services/service-private/deployment",
    ({ send }) => send(200, deployment),
  );
  fixture.handlers.set(
    "POST /api/micro-services/service-private/deployment",
    ({ body, send }) => {
      deployment = {
        id: "release-private",
        serviceId: "service-private",
        phase: "running",
        spec,
        request: body,
        instances: [],
        log: [],
        error: null,
        internalRouteOwned: true,
        domainRouteOwned: false,
      };
      send(202, deployment);
    },
  );
  fixture.handlers.set(
    "POST /api/micro-services/service-private/stop",
    ({ send }) => {
      deployment = {
        ...deployment,
        phase: "stopped",
        internalRouteOwned: false,
        error: null,
      };
      send(202, deployment);
    },
  );
  const browser = await chromium.launch({ headless: true }),
    page = await browser.newPage();
  try {
    await page.goto(`${fixture.url}/#/services`);
    await page
      .getByRole("button", { name: "Open private-app", exact: true })
      .click();
    await page.getByRole("button", { name: "Deploy", exact: true }).click();
    await expect(page.getByLabel("First host port")).toHaveValue("");
    await page
      .getByRole("button", { name: "Deploy and run", exact: true })
      .click();
    await expect(
      page.getByText("http://private-app.internal", { exact: true }),
    ).toBeVisible();
    assert.equal(
      fixture.requests.find(
        (r) => r.method === "POST" && r.path.endsWith("/deployment"),
      ).body.hostPort,
      0,
    );
    deployment = {
      ...deployment,
      phase: "failed",
      error: "Internal routing cleanup must be retried.",
    };
    await expect(
      page.getByRole("alert").filter({ hasText: "Internal routing cleanup" }),
    ).toBeVisible();
    await expect(
      page.getByRole("button", { name: "Deploy and run", exact: true }),
    ).toHaveCount(0);
    await page.getByRole("button", { name: "More", exact: true }).click();
    await expect(
      page.getByRole("menuitem", { name: "Stop service", exact: true }),
    ).toBeEnabled();
    page.once("dialog", (dialog) => dialog.accept());
    await page
      .getByRole("menuitem", { name: "Stop service", exact: true })
      .click();
    await expect(
      page.getByRole("button", { name: "Deploy", exact: true }),
    ).toBeVisible();
    assert.equal(
      fixture.requests.filter(
        (r) => r.method === "POST" && r.path.endsWith("/stop"),
      ).length,
      1,
    );
  } finally {
    await page.close();
    await browser.close();
    await fixture.close();
  }
});

test("reconnecting image authentication restores deployment choices without deploying", async () => {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  const browser = await chromium.launch({ headless: true }),
    page = await browser.newPage();
  const spec = {
    id: "service-auth",
    name: "auth-app",
    image: "ghcr.io/team/app:1.0",
    runtime: "vm",
    port: 8080,
    healthPath: "/",
    domain: null,
    ram: 512,
    cpu: 1,
    replicas: 1,
    startupTimeout: 300,
    drainTimeout: 30,
    env: {},
    secrets: {},
    queues: {},
    securityGroupId: null,
  };
  fixture.state.services = [spec];
  fixture.state.auth = [];
  fixture.state.networks = [
    {
      id: "network-recovery",
      name: "chosen-network",
      subnetCidr: "172.31.0.0/24",
    },
  ];
  fixture.handlers.set(
    "GET /api/micro-services/service-auth/deployment",
    ({ send }) => send(200, null),
  );
  fixture.handlers.set("PUT /api/pipe/auth/github", ({ send }) => {
    const account = {
      provider: "github",
      account: "team",
      registry: "ghcr.io",
      region: null,
      services: ["registry"],
    };
    fixture.state.auth.push(account);
    send(200, account);
  });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  try {
    await page.goto(`${fixture.url}/#/services`);
    await page
      .getByRole("button", { name: "Open auth-app", exact: true })
      .click();
    await page.getByRole("button", { name: "Deploy", exact: true }).click();
    await page
      .getByLabel("Network", { exact: true })
      .selectOption("network-recovery");
    await page.getByLabel("First host port", { exact: true }).fill("31234");
    await expect(
      page.getByRole("button", { name: "Deploy and run", exact: true }),
    ).toBeDisabled();
    await page
      .getByRole("link", { name: "Reconnect AUTH", exact: true })
      .click();
    await page
      .getByRole("button", { name: "Connect GitHub", exact: true })
      .click();
    await page
      .getByLabel("Personal access token (classic)", { exact: true })
      .fill("fixture-only-token");
    await page
      .getByRole("button", { name: "Verify and connect", exact: true })
      .click();
    await expect(page).toHaveURL(/#\/services$/);
    await expect(
      page.getByLabel("First host port", { exact: true }),
    ).toHaveValue("31234");
    await expect(page.getByLabel("Network", { exact: true })).toHaveValue(
      "network-recovery",
    );
    await expect(
      page.getByRole("button", { name: "Deploy and run", exact: true }),
    ).toBeEnabled();
    assert.equal(
      fixture.requests.filter(
        (request) =>
          request.method === "POST" && request.path.endsWith("/deployment"),
      ).length,
      0,
    );
    assert.equal(
      (
        await page.evaluate(() =>
          JSON.stringify({
            ...localStorage,
            ...sessionStorage,
            history: history.state,
          }),
        )
      ).includes("fixture-only-token"),
      false,
    );
    assert.deepEqual(errors, []);
  } finally {
    await page.close();
    await browser.close();
    await fixture.close();
  }
});
