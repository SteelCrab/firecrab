import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";
import { openDashboard } from "./support/mockApi";
import type { MockRoute } from "./support/mockApi";

const SERVICE = "33333333-3333-4333-8333-333333333333";
const HOSTS = ["11111111-1111-4111-8111-111111111111", "22222222-2222-4222-8222-222222222222"];
const NETWORKS = ["aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"];
const IMAGE = "ghcr.io/team/web@sha256:" + "a".repeat(64);
interface Write { path: string; key: string; body: any }

async function open(page: Page, loseFirstResponse = false) {
  const writes: Write[] = [];
  let service: any = null;
  const routes: Record<string, MockRoute> = {
    "/api/cluster/hosts": HOSTS.map((id, index) => ({
      report: { id, name: `Worker ${index === 0 ? "A" : "B"}`, capacity: { cpu: 4, ramMib: 4096 }, images: [],
        networks: [{ id: NETWORKS[index], name: "private" }], storages: [{ id: "default" }] },
      online: true, draining: false, reserved: { cpu: 0, ramMib: 0 },
    })),
    "/api/cluster/placements": [],
    "/api/cluster/services": async route => {
      if (route.request().method() === "GET") return route.fulfill({ json: service ? [service] : [] });
      const body = route.request().postDataJSON();
      writes.push({ path: "/api/cluster/services", key: route.request().headers()["idempotency-key"], body });
      service ??= { id: SERVICE, spec: { ...body, service: { ...body.service, env: { KEEP: "same" } } }, generation: 1, releaseGeneration: 1,
        operationId: "create-operation", desired: "running", observed: "pending", pending: true, degraded: false,
        hosts: body.targets.map((target: any, index: number) => ({ hostId: target.hostId, intent: { deploymentId: `deployment-${index}`, spec: { replicas: target.replicas } }, deployment: null, error: null })) };
      if (loseFirstResponse && writes.length === 1) return route.abort("failed");
      return route.fulfill({ status: 202, json: service });
    },
    [`/api/cluster/services/${SERVICE}/operations`]: [{ id: "create-operation", idempotencyKey: "history-key" }],
  };
  for (const action of ["update", "stop", "reconcile"]) {
    const path = `/api/cluster/services/${SERVICE}/${action}`;
    routes[path] = async route => {
      const body = route.request().postData() ? route.request().postDataJSON() : null;
      writes.push({ path, key: route.request().headers()["idempotency-key"], body });
      if (action === "update") service = { ...service, spec: body.spec, generation: service.generation + 1, releaseGeneration: service.releaseGeneration + 1 };
      if (action === "stop") service = { ...service, generation: service.generation + 1, desired: "stopped" };
      return route.fulfill({ status: 202, json: service });
    };
  }
  await openDashboard(page, { routes, hash: "#/cluster", locale: "en" });
  return writes;
}

async function fill(page: Page) {
  const form = page.getByRole("region", { name: "Controller service form" });
  await form.getByLabel("Service name", { exact: true }).fill("web");
  await form.getByLabel("Pinned OCI image", { exact: true }).fill(IMAGE);
  await form.getByRole("checkbox", { name: "Worker A", exact: true }).check();
  await form.getByRole("checkbox", { name: "Worker B", exact: true }).check();
  await form.getByRole("button", { name: "Create service", exact: true }).click();
}

test("creates a shared release, preserves environment on edit, and exposes fenced stop and history", async ({ page }) => {
  const writes = await open(page);
  await fill(page);
  const card = page.locator(`[data-cluster-service="${SERVICE}"]`);
  await expect(card).toBeVisible();
  expect(writes[0].key).toBeTruthy();
  expect(writes[0].body.service).toMatchObject({ name: "web", image: IMAGE, replicas: 2 });
  expect(writes[0].body.targets.map((target: any) => target.request.microNetworkId)).toEqual(NETWORKS);
  await card.getByRole("button", { name: "Edit release", exact: true }).click();
  await page.getByLabel("Pinned OCI image", { exact: true }).fill(IMAGE.replace(/a{64}$/, "b".repeat(64)));
  await page.getByRole("button", { name: "Deploy release", exact: true }).click();
  await expect.poll(() => writes.length).toBe(2);
  expect(writes[1].body).toMatchObject({ expectedGeneration: 1, spec: { service: { env: { KEEP: "same" } } } });
  await expect(card).toContainText("Generation: 2");
  await card.getByRole("button", { name: "Operations", exact: true }).click();
  await expect(card).toContainText("history-key");
  await page.screenshot({ path: test.info().outputPath("cluster-services.png"), fullPage: true });
  await card.getByRole("button", { name: "Stop", exact: true }).click();
  await expect.poll(() => writes.length).toBe(3);
  expect(writes[2].body).toEqual({ expectedGeneration: 2 });
  await expect(card).toContainText("Desired: stopped");
});

test("reload retries an unknown create result using the exact saved key and request", async ({ page }) => {
  const writes = await open(page, true);
  await fill(page);
  await expect(page.getByRole("button", { name: "Retry saved service request", exact: true })).toBeVisible();
  const first = structuredClone(writes[0]);
  await page.reload();
  await page.getByRole("button", { name: "Retry saved service request", exact: true }).click();
  await expect.poll(() => writes.length).toBe(2);
  expect(writes[1]).toEqual(first);
  await expect(page.locator("[data-cluster-service]")).toHaveCount(1);
  await expect(page.getByRole("button", { name: "Retry saved service request", exact: true })).toBeHidden();
});
