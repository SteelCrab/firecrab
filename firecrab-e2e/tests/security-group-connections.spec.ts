import { expect, test, type Page } from "@playwright/test";
import type {
  MicroServiceDeployment,
  MicroServiceResponse,
  SecurityGroupResponse,
} from "../../firecrab-e2e/types/bindings";
import { openDashboard, vm } from "./support/mockApi";

const NETWORK = "aaaaaaaa-0000-4000-8000-000000000001";
const GROUP = "bbbbbbbb-0000-4000-8000-000000000001";
const OTHER_GROUP = "bbbbbbbb-0000-4000-8000-000000000002";
const SERVICE = "cccccccc-0000-4000-8000-000000000001";
const VM1 = "dddddddd-0000-4000-8000-000000000001";
const VM2 = "dddddddd-0000-4000-8000-000000000002";
const POLICY: SecurityGroupResponse = {
  id: GROUP,
  name: "orders-policy",
  microNetworkId: NETWORK,
  rules: [
    {
      direction: "ingress",
      protocol: "tcp",
      portFrom: 8080,
      portTo: 8080,
      peerCidr: "172.30.0.0/24",
    },
  ],
};
const SPEC: MicroServiceResponse = {
  id: SERVICE,
  name: "orders-api",
  image: "nginx:stable-alpine",
  runtime: "vm",
  port: 8080,
  healthPath: "/health",
  domain: null,
  ram: 512,
  cpu: 1,
  replicas: 3,
  startupTimeout: 300,
  drainTimeout: 30,
  env: {},
  secrets: {},
  securityGroupId: GROUP,
};
const DEPLOYMENT: MicroServiceDeployment = {
  id: "deployment-1",
  serviceId: SERVICE,
  phase: "running",
  spec: SPEC,
  request: { hostPort: 30100, microNetworkId: NETWORK },
  digest: null,
  error: null,
  log: [],
  instances: [
    {
      vmId: VM1,
      ipv4: "172.30.0.2",
      hostPort: 30100,
      state: "running",
      healthy: true,
    },
    {
      vmId: VM2,
      ipv4: "172.30.0.3",
      hostPort: 30101,
      state: "starting",
      healthy: false,
    },
  ],
};

async function setup(page: Page) {
  const state = {
    groups: [POLICY],
    services: [
      SPEC,
      { ...SPEC, id: "service-pending", name: "billing-api", replicas: 2 },
    ],
    deployment: structuredClone(DEPLOYMENT),
    failServices: false,
    failDeployment: false,
    failVms: false,
    vms: [
      vm("orders-api-1", VM1, "running", {
        securityGroupId: GROUP,
        ipv4: "172.30.0.2",
      }),
      vm("orders-api-2", VM2, "starting", {
        securityGroupId: GROUP,
        ipv4: "172.30.0.3",
      }),
    ],
  };
  await openDashboard(page, {
    hash: "#/security-groups",
    routes: {
      "/api/host": {
        loadAverage1m: 0,
        memoryTotalMib: 8192,
        memoryAvailableMib: 4096,
        diskTotalGib: 100,
        diskAvailableGib: 50,
        uptimeSeconds: 3600,
      },
      "/api/update/progress": { phase: "idle", percent: 0, updatedAtMs: 0 },
      "/api/security-groups": (route) => route.fulfill({ json: state.groups }),
      "/api/micro-networks": [
        {
          id: NETWORK,
          name: "orders-net",
          subnetCidr: "172.30.0.0/24",
          ipv6Cidr: null,
        },
      ],
      "/api/vms": (route) =>
        state.failVms
          ? route.fulfill({
              status: 503,
              json: { error: { message: "VM list unavailable" } },
            })
          : route.fulfill({ json: state.vms }),
      "/api/micro-services": (route) =>
        state.failServices
          ? route.fulfill({
              status: 503,
              json: { error: { message: "Service list unavailable" } },
            })
          : route.fulfill({ json: state.services }),
      [`/api/micro-services/${SERVICE}/deployment`]: (route) =>
        state.failDeployment
          ? route.fulfill({
              status: 503,
              json: { error: { message: "Deployment unavailable" } },
            })
          : route.fulfill({ json: state.deployment }),
      "/api/micro-services/service-pending/deployment": (route) =>
        route.fulfill({ json: null }),
    },
  });
  return state;
}

const graph = (page: Page, name = "orders-policy") =>
  page.getByRole("figure", { name: `Connections for ${name}`, exact: true });

test.describe("Security group connections @dashboard", () => {
  test.beforeEach(async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 1000 });
  });

  test("shows real replica VMs, configured counts, and confirmed inheritance", async ({
    page,
  }, testInfo) => {
    const errors: string[] = [];
    page.on("pageerror", (error) => errors.push(error.message));
    await setup(page);
    const connections = graph(page);
    await expect(connections).toContainText("2 services · 2 replica VMs");
    await expect(
      connections.getByText("Configured: 3 replicas", { exact: true }),
    ).toBeVisible();
    await expect(
      connections.getByText("No deployed VMs", { exact: true }),
    ).toBeVisible();
    await expect(
      connections.getByText("Inherited", { exact: true }),
    ).toHaveCount(2);
    await expect(
      connections.getByText("orders-api-1", { exact: true }),
    ).toBeVisible();
    await expect(
      connections.getByText("172.30.0.3", { exact: true }),
    ).toBeVisible();
    await expect(connections.locator(".sg-connection-circle")).toHaveCount(2);
    await expect(
      page.getByRole("complementary", { name: "Security group detail" }),
    ).toHaveCount(0);
    await page.screenshot({
      path: testInfo.outputPath("security-group-connections-desktop.png"),
      fullPage: true,
    });
    await page
      .getByRole("combobox", { name: "Language", exact: true })
      .selectOption("ko");
    await expect(
      page
        .getByRole("figure", { name: "orders-policy 연결 관계", exact: true })
        .getByText("상속됨", { exact: true }),
    ).toHaveCount(2);
    await page.screenshot({
      path: testInfo.outputPath("security-group-connections-desktop-ko.png"),
      fullPage: true,
    });
    expect(errors).toEqual([]);
  });

  test("keeps running replicas under their deployment group when configuration changes", async ({
    page,
  }) => {
    const state = await setup(page);
    state.groups = [
      POLICY,
      { ...POLICY, id: OTHER_GROUP, name: "next-policy" },
    ];
    state.services = [{ ...SPEC, securityGroupId: OTHER_GROUP }];
    await page.getByRole("button", { name: "Refresh", exact: true }).click();
    await expect(graph(page)).toContainText("Previous deployment");
    await expect(graph(page).locator(".sg-connection-circle")).toHaveCount(2);
    await expect(graph(page, "next-policy")).toContainText(
      "Applies on the next deployment",
    );
    await expect(
      graph(page, "next-policy").locator(".sg-connection-circle"),
    ).toHaveCount(0);
  });

  test("does not claim inheritance for a mismatched or unreadable VM", async ({
    page,
  }) => {
    const state = await setup(page);
    state.vms[0].securityGroupId = OTHER_GROUP;
    state.vms[1].securityGroupId = undefined;
    await expect(
      graph(page).getByText("Group mismatch", { exact: true }),
    ).toBeVisible();
    await expect(
      graph(page).getByText("Group unverified", { exact: true }),
    ).toBeVisible();
    await expect(
      graph(page).getByText("Inherited", { exact: true }),
    ).toHaveCount(0);
    state.failVms = true;
    await expect(
      graph(page).getByText("Group unverified", { exact: true }),
    ).toHaveCount(2);
    await expect(
      graph(page).getByText("Group mismatch", { exact: true }),
    ).toHaveCount(0);
  });

  test("shows deployment failures as unknown and retries connection-list failures", async ({
    page,
  }) => {
    const state = await setup(page);
    state.failDeployment = true;
    await page.getByRole("button", { name: "Refresh", exact: true }).click();
    await expect(graph(page).getByRole("alert")).toHaveText(
      "VM status unavailable.",
    );
    await expect(graph(page)).not.toContainText("0 replica VMs");
    state.failServices = true;
    await page.getByRole("button", { name: "Refresh", exact: true }).click();
    await expect(graph(page).getByRole("alert")).toHaveText(
      "Connections are unavailable.",
    );
    await expect(
      page.getByRole("button", { name: "New security group", exact: true }),
    ).toBeEnabled();
    state.failServices = false;
    state.failDeployment = false;
    await graph(page)
      .getByRole("button", { name: "Retry connections", exact: true })
      .press("Enter");
    await expect(
      graph(page).getByText("Inherited", { exact: true }),
    ).toHaveCount(2);
    state.services = [{ ...SPEC, securityGroupId: null }];
    state.failDeployment = true;
    await page.getByRole("button", { name: "Refresh", exact: true }).click();
    await expect(graph(page).getByRole("alert")).toHaveText(
      "Some deployments could not be checked.",
    );
    await expect(graph(page)).not.toContainText("No MicroServices connected.");
  });

  test("polls replica changes without replacing unsaved rule edits", async ({
    page,
  }) => {
    const state = await setup(page);
    await expect(graph(page).locator(".sg-connection-circle")).toHaveCount(2);
    await page
      .getByRole("button", {
        name: "Edit rules for orders-policy",
        exact: true,
      })
      .click();
    await page.getByLabel("Port from", { exact: true }).fill("9090");
    state.deployment.instances = [state.deployment.instances[0]];
    await expect(graph(page).locator(".sg-connection-circle")).toHaveCount(1, {
      timeout: 15_000,
    });
    await expect(page.getByLabel("Port from", { exact: true })).toHaveValue(
      "9090",
    );
  });

  test("fits Korean mobile and narrow desktop layouts without clipped labels", async ({
    page,
  }, testInfo) => {
    await setup(page);
    await page
      .getByRole("combobox", { name: "Language", exact: true })
      .selectOption("ko");
    const connections = page.getByRole("figure", {
      name: "orders-policy 연결 관계",
      exact: true,
    });
    await expect(connections.getByText("상속됨", { exact: true })).toHaveCount(
      2,
    );
    for (const width of [1280, 390]) {
      await page.setViewportSize({ width, height: 1000 });
      expect(
        await page.evaluate(
          () => document.documentElement.scrollWidth <= window.innerWidth,
        ),
      ).toBe(true);
      expect(
        await connections.evaluate(
          (element) => element.scrollWidth <= element.clientWidth,
        ),
      ).toBe(true);
    }
    await page.screenshot({
      path: testInfo.outputPath("security-group-connections-mobile.png"),
      fullPage: true,
    });
  });
});
