import { expect, test, type Page } from "@playwright/test";
import type { MicroServiceResponse, SecurityGroupResponse, SecurityGroupRule } from "../../firecrab-frontend/src/bindings";
import { openDashboard } from "./support/mockApi";

const NETWORK = "aaaaaaaa-0000-4000-8000-000000000001";
const IPV6_NETWORK = "aaaaaaaa-0000-4000-8000-000000000002";
const GROUP = "bbbbbbbb-0000-4000-8000-000000000001";
const SERVICE = "cccccccc-0000-4000-8000-000000000001";
const RULE: SecurityGroupRule = { direction: "ingress", protocol: "tcp", portFrom: 8080, portTo: 8080, peerCidr: "172.30.0.0/24" };
const POLICY: SecurityGroupResponse = { id: GROUP, name: "orders-policy", microNetworkId: NETWORK, rules: [RULE] };
const SPEC: MicroServiceResponse = {
  id: SERVICE, name: "orders", image: "nginx:stable-alpine", runtime: "vm", port: 8080,
  healthPath: "/health", domain: null, ram: 512, cpu: 1, replicas: 2,
  startupTimeout: 300, drainTimeout: 30, env: { PROFILE: "prod" }, secrets: {}, securityGroupId: GROUP,
};

async function setup(page: Page, groups: SecurityGroupResponse[] = [], hash = "#/security-groups") {
  const state = {
    groups: structuredClone(groups), service: { ...SPEC }, failRules: 0, failDelete: false,
    failGroups: false, ruleWrites: [] as { rules: SecurityGroupRule[] }[], serviceWrites: [] as Record<string, unknown>[],
    deployments: [] as Record<string, unknown>[],
  };
  await openDashboard(page, { hash, routes: {
    "/api/host": { loadAverage1m: 0, memoryTotalMib: 8192, memoryAvailableMib: 4096, diskTotalGib: 100, diskAvailableGib: 50, uptimeSeconds: 3600 },
    "/api/update/progress": { phase: "idle", percent: 0, updatedAtMs: 0 },
    "/api/micro-networks": [
      { id: NETWORK, name: "orders-net", subnetCidr: "172.30.0.0/24", gateway: "172.30.0.1", internetEnabled: true, ipv6Cidr: null },
      { id: IPV6_NETWORK, name: "dual-stack", subnetCidr: "172.31.0.0/24", gateway: "172.31.0.1", internetEnabled: true, ipv6Cidr: "fd42:31::/64" },
    ],
    "/api/security-groups": (route) => {
      if (route.request().method() === "POST") {
        const group = { id: GROUP, ...route.request().postDataJSON(), rules: [] };
        state.groups.push(group);
        return route.fulfill({ status: 201, json: group });
      }
      if (state.failGroups) return route.fulfill({ status: 503, json: { error: { message: "Policy list unavailable" } } });
      return route.fulfill({ json: state.groups });
    },
    [`/api/security-groups/${GROUP}`]: (route) => {
      if (route.request().method() === "DELETE") {
        if (state.failDelete) return route.fulfill({ status: 409, json: { error: { code: "security_group_in_use", message: "security group is referenced by a service or VM" } } });
        state.groups = [];
        return route.fulfill({ status: 204 });
      }
      const group = state.groups.find((row) => row.id === GROUP);
      return group ? route.fulfill({ json: group }) : route.fulfill({ status: 404, json: { error: { message: "Policy not found" } } });
    },
    [`/api/security-groups/${GROUP}/rules`]: (route) => {
      const body = route.request().postDataJSON();
      state.ruleWrites.push(body);
      if (state.failRules) return route.fulfill({ status: state.failRules, json: { error: {
        code: state.failRules === 400 ? "validation_failed" : "security_group_in_use",
        message: state.failRules === 400 ? "request validation failed" : "stop affected VMs before changing rules",
        fields: state.failRules === 400 ? { rules: "peer must be an IPv4 CIDR" } : undefined,
      } } });
      state.groups = state.groups.map((row) => ({ ...row, rules: body.rules }));
      return route.fulfill({ json: state.groups[0] });
    },
    "/api/micro-services": (route) => {
      if (route.request().method() === "POST") {
        const body = route.request().postDataJSON();
        state.serviceWrites.push(body); state.service = { id: SERVICE, ...body };
        return route.fulfill({ status: 201, json: state.service });
      }
      return route.fulfill({ json: [state.service] });
    },
    [`/api/micro-services/${SERVICE}`]: (route) => {
      if (route.request().method() === "PUT") {
        const body = route.request().postDataJSON();
        state.serviceWrites.push(body); state.service = { id: SERVICE, ...body };
      }
      return route.fulfill({ json: state.service });
    },
    [`/api/micro-services/${SERVICE}/deployment`]: (route) => {
      if (route.request().method() === "POST") {
        const request = route.request().postDataJSON(); state.deployments.push(request);
        return route.fulfill({ status: 202, json: { id: GROUP, serviceId: SERVICE, spec: state.service, phase: "queued", request, digest: null, instances: [], log: [], error: null } });
      }
      return route.fulfill({ json: null });
    },
  } });
  return state;
}

test.describe("Security groups @dashboard", () => {
  test.beforeEach(async ({ page }) => { await page.setViewportSize({ width: 1600, height: 1080 }); });

  test("creates a group, edits inbound and outbound rules, and reloads saved rules", async ({ page }, testInfo) => {
    const state = await setup(page);
    await expect(page.getByRole("heading", { name: "Security groups", exact: true }).first()).toBeVisible();
    await page.getByRole("button", { name: "New security group", exact: true }).click();
    await page.getByLabel("Name", { exact: true }).fill("orders-policy");
    await expect(page.getByLabel("MicroNetwork", { exact: true }).locator(`option[value="${IPV6_NETWORK}"]`)).toBeDisabled();
    await page.getByLabel("MicroNetwork", { exact: true }).selectOption(NETWORK);
    await page.getByRole("button", { name: "Create security group", exact: true }).click();
    await expect(page.getByText("No allow rules. All new peer connections are denied.", { exact: true })).toBeVisible();
    expect(state.groups[0].microNetworkId).toBe(NETWORK);
    for (const direction of ["ingress", "egress"]) {
      await page.getByRole("button", { name: "Add rule", exact: true }).click();
      const row = page.getByRole("group", { name: direction === "ingress" ? "Rule 1" : "Rule 2", exact: true });
      await row.getByLabel("Direction", { exact: true }).selectOption(direction);
      await row.getByLabel("Protocol", { exact: true }).selectOption(direction === "ingress" ? "tcp" : "udp");
      await row.getByLabel("Port from", { exact: true }).fill(direction === "ingress" ? "8080" : "53");
      await row.getByLabel("Port to", { exact: true }).fill(direction === "ingress" ? "8082" : "53");
      await row.getByLabel(/IPv4 CIDR/).fill(direction === "ingress" ? "172.30.0.0/24" : "0.0.0.0/0");
    }
    await page.getByRole("button", { name: "Save rules", exact: true }).click();
    await expect(page.getByRole("status")).toHaveText("Rules saved.");
    expect(state.ruleWrites[0]).toEqual({ rules: [{ ...RULE, portTo: 8082 }, { direction: "egress", protocol: "udp", portFrom: 53, portTo: 53, peerCidr: "0.0.0.0/0" }] });
    await page.screenshot({ path: testInfo.outputPath("security-groups-desktop.png"), fullPage: true });
    await page.reload();
    await page.getByRole("button", { name: "Edit rules for orders-policy", exact: true }).click();
    await expect(page.getByRole("group", { name: "Rule 2", exact: true }).getByLabel("Protocol")).toHaveValue("udp");
  });

  test("keeps draft rules after validation and active-policy conflicts, then saves an empty policy", async ({ page }) => {
    const state = await setup(page, [POLICY]);
    await page.getByRole("button", { name: "Edit rules for orders-policy", exact: true }).click();
    await page.getByLabel("Source IPv4 CIDR", { exact: true }).fill("bad-cidr");
    state.failRules = 400;
    await page.getByRole("button", { name: "Save rules", exact: true }).click();
    await expect(page.getByRole("alert")).toContainText("peer must be an IPv4 CIDR");
    await expect(page.getByLabel("Source IPv4 CIDR", { exact: true })).toHaveValue("bad-cidr");
    await page.getByLabel("Source IPv4 CIDR", { exact: true }).fill("10.0.0.0/8");
    state.failRules = 409;
    await page.getByRole("button", { name: "Save rules", exact: true }).click();
    await expect(page.getByRole("alert")).toHaveText("Stop affected VMs and service deployments before changing rules.");
    await expect(page.getByLabel("Source IPv4 CIDR", { exact: true })).toHaveValue("10.0.0.0/8");
    expect(state.groups[0].rules).toEqual([RULE]);
    await page.getByRole("button", { name: "Remove rule 1", exact: true }).click();
    state.failRules = 0;
    await page.getByRole("button", { name: "Save rules", exact: true }).click();
    await expect(page.getByRole("status")).toHaveText("Rules saved.");
    expect(state.groups[0].rules).toEqual([]);
    await expect(page.getByText("No allow rules. All new peer connections are denied.", { exact: true })).toBeVisible();
  });

  test("rejects invalid port ranges before sending and protects referenced groups on deletion", async ({ page }) => {
    const state = await setup(page, [POLICY]);
    await page.getByRole("button", { name: "Edit rules for orders-policy", exact: true }).click();
    for (const port of ["0", "9000"]) {
      await page.getByLabel("Port from", { exact: true }).fill(port);
      await page.getByRole("button", { name: "Save rules", exact: true }).click();
      expect(state.ruleWrites).toHaveLength(0);
    }
    await page.getByRole("button", { name: "Close", exact: true }).click();
    state.failDelete = true;
    page.once("dialog", (dialog) => dialog.accept());
    await page.getByRole("button", { name: "Delete orders-policy", exact: true }).click();
    await expect(page.getByRole("alert")).toHaveText("security group is referenced by a service or VM");
    expect(state.groups).toHaveLength(1);
    state.failDelete = false;
    page.once("dialog", (dialog) => dialog.accept());
    await page.getByRole("button", { name: "Delete orders-policy", exact: true }).click();
    await expect(page.getByText("No security groups yet.")).toBeVisible();
  });

  test("supports Korean and mobile rule editing without horizontal overflow", async ({ page }, testInfo) => {
    await setup(page, [POLICY]);
    await page.getByRole("combobox", { name: "Language", exact: true }).selectOption("ko");
    await page.setViewportSize({ width: 390, height: 844 });
    await page.getByRole("button", { name: "orders-policy 규칙 편집", exact: true }).click();
    await expect(page.getByLabel("출발지 IPv4 CIDR", { exact: true })).toHaveValue(RULE.peerCidr);
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    expect(await page.locator(".detail-body").evaluate((element) => element.scrollWidth <= element.clientWidth)).toBe(true);
    await page.screenshot({ path: testInfo.outputPath("security-groups-mobile.png"), fullPage: true });
    await page.getByLabel("시작 포트", { exact: true }).fill("8081");
    await page.getByLabel("끝 포트", { exact: true }).fill("8081");
    await page.getByRole("button", { name: "규칙 저장", exact: true }).click();
    await expect(page.getByRole("status")).toHaveText("규칙을 저장했다.");
  });

  test("connects a service on create and edit, deploys on the policy network, and explicitly detaches", async ({ page }, testInfo) => {
    const state = await setup(page, [POLICY], "#/services");
    await page.getByRole("button", { name: "New MicroService", exact: true }).click();
    await page.getByLabel("Name", { exact: true }).fill("orders");
    await page.getByLabel("OCI image", { exact: true }).fill("nginx:stable-alpine");
    await page.getByLabel("Security group", { exact: true }).selectOption(GROUP);
    await page.getByRole("button", { name: "Save configuration", exact: true }).click();
    await expect(page.getByRole("heading", { name: "New MicroService", exact: true })).toHaveCount(0);
    expect(state.serviceWrites[0].securityGroupId).toBe(GROUP);
    await page.getByRole("button", { name: "Edit orders", exact: true }).click();
    await expect(page.getByLabel("Security group", { exact: true })).toHaveValue(GROUP);
    await page.getByLabel("Replicas", { exact: true }).fill("2");
    await page.screenshot({ path: testInfo.outputPath("microservice-security-group-desktop.png"), fullPage: true });
    await page.getByRole("button", { name: "Save configuration", exact: true }).click();
    await page.getByRole("button", { name: "Edit orders", exact: true }).click();
    await page.getByLabel("Security group", { exact: true }).selectOption("");
    await page.getByRole("button", { name: "Save configuration", exact: true }).click();
    await expect(page.getByRole("heading", { name: "Edit: orders", exact: true })).toHaveCount(0);
    expect(state.serviceWrites.at(-1)?.securityGroupId).toBeNull();
    await page.getByRole("button", { name: "Edit orders", exact: true }).click();
    await page.getByLabel("Security group", { exact: true }).selectOption(GROUP);
    await page.getByRole("button", { name: "Save configuration", exact: true }).click();
    await page.getByRole("button", { name: "Run orders", exact: true }).click();
    await expect(page.getByLabel("Network", { exact: true })).toHaveValue(NETWORK);
    await expect(page.getByLabel("Network", { exact: true })).toBeDisabled();
    await page.getByRole("button", { name: "Deploy and run", exact: true }).click();
    expect(state.deployments).toEqual([{ hostPort: 30100, microNetworkId: NETWORK }]);
  });

  test("preserves service edits when policies cannot load and retries without clearing the selection", async ({ page }) => {
    const state = await setup(page, [POLICY], "#/services");
    state.failGroups = true;
    await page.getByRole("button", { name: "Edit orders", exact: true }).click();
    await expect(page.getByRole("alert")).toContainText("Policy list unavailable");
    await expect(page.getByRole("button", { name: "Save configuration", exact: true })).toBeDisabled();
    await page.getByLabel("Name", { exact: true }).fill("orders-v2");
    state.failGroups = false;
    await page.getByRole("button", { name: "Retry loading security groups", exact: true }).click();
    await expect(page.getByLabel("Security group", { exact: true })).toHaveValue(GROUP);
    await page.getByRole("button", { name: "Save configuration", exact: true }).click();
    await expect(page.getByRole("heading", { name: "Edit: orders", exact: true })).toHaveCount(0);
    expect(state.serviceWrites[0]).toMatchObject({ name: "orders-v2", securityGroupId: GROUP, env: { PROFILE: "prod" } });
  });

  test("prevents deployment if the selected policy cannot be read", async ({ page }) => {
    const state = await setup(page, [], "#/services");
    await page.getByRole("button", { name: "Run orders", exact: true }).click();
    await expect(page.getByRole("alert")).toContainText("Policy not found");
    await expect(page.getByRole("button", { name: "Deploy and run", exact: true })).toBeDisabled();
    expect(state.deployments).toEqual([]);
  });
});
