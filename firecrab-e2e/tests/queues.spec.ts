import { test, expect, type Page } from "@playwright/test";
type QueueEngine = { vmId: string; microNetworkId: string; phase: string; image: string; error: string | null; log: string };
type ApplicationQueue = { id: string; name: string; phase: string; maxBytes: number; deliveryLimit: number; error: string | null; services: { id: string; name: string; bindings: Record<string, string> }[] };
const QUEUE_ID = "00000000-0000-0000-0000-000000000501";
const NETWORK_ID = "00000000-0000-0000-0000-000000000502";
const VM_ID = "00000000-0000-0000-0000-000000000503";
const initialEngine: QueueEngine = { vmId: VM_ID, microNetworkId: NETWORK_ID, phase: "running", image: "rabbitmq:4.3.7-management", error: null, log: "" };
const initialQueue: ApplicationQueue = { id: QUEUE_ID, name: "notifications", phase: "ready", maxBytes: 64 * 1024 * 1024, deliveryLimit: 5, error: null, services: [] };
async function open(page: Page, installed = true, bound = false) {
  const state = { engine: installed ? { ...initialEngine } : null as QueueEngine | null, rows: installed ? [{ ...initialQueue, services: bound ? [{ id: "app", name: "mail-app", bindings: { NOTIFICATIONS: "consume" as const } }] : [] }] : [] as ApplicationQueue[], fail: false };
  const writes: { path: string; body: unknown }[] = [];
  await page.addInitScript(() => localStorage.setItem("firecrab.locale", "en"));
  for (const url of ["https://fonts.googleapis.com/**", "https://cdn.jsdelivr.net/gh/orioncactus/pretendard@*/**"]) {
    await page.route(url, route => route.fulfill({ contentType: "text/css", body: "" }));
  }
  await page.route(/^https?:\/\/[^/]+\/api\//, async route => {
    const request = route.request(); const path = new URL(request.url()).pathname; const method = request.method();
    if (path === "/api/vms") return route.fulfill({ json: [] });
    if (path === "/api/micro-networks") return route.fulfill({ json: [{ id: NETWORK_ID, name: "apps", subnetCidr: "10.42.0.0/24" }] });
    if (state.fail && ["/api/queue/engine", "/api/queues"].includes(path)) return route.fulfill({ status: 503, json: { error: { message: "Queue API unavailable" } } });
    if (path === "/api/queue/engine") {
      if (method === "POST") { writes.push({ path, body: request.postDataJSON() }); state.engine = { ...initialEngine, phase: "installing" }; }
      return route.fulfill({ status: method === "POST" ? 202 : 200, json: state.engine });
    }
    if (path === "/api/queues") {
      if (method === "POST") { const body = request.postDataJSON(); writes.push({ path, body }); state.rows = [{ ...initialQueue, ...body, phase: "provisioning" }]; return route.fulfill({ status: 202, json: state.rows[0] }); }
      return route.fulfill({ json: state.rows });
    }
    if (path.endsWith("/metrics")) return route.fulfill({ json: { ready: 3, unacknowledged: 1, consumers: 1, failed: 2 } });
    if (path.includes("/connection/")) { const role = path.endsWith("publish") ? "sender" : "receiver"; return route.fulfill({ json: { url: `amqp://${role}:test-only@10.42.0.2:5672/test`, queue: "notifications" } }); }
    if (path === `/api/queues/${QUEUE_ID}` && method === "DELETE") { writes.push({ path, body: null }); state.rows = []; return route.fulfill({ status: 204 }); }
    return route.fulfill({ status: 404, json: { error: { message: "Fixture unavailable" } } });
  });
  await page.goto("/#/queues"); return { state, writes };
}

test.describe("Queue @dashboard", () => {
  test("installs an independent engine and creates a named durable queue", async ({ page }) => {
    const { state, writes } = await open(page, false);
    await expect(page.getByRole("heading", { name: "Queue", exact: true }).first()).toBeVisible();
    await page.getByRole("combobox", { name: "MicroNetwork", exact: true }).selectOption(NETWORK_ID);
    await page.getByRole("button", { name: "Install RabbitMQ", exact: true }).click();
    await expect.poll(() => writes.length).toBe(1);
    expect(writes[0].body).toEqual({ microNetworkId: NETWORK_ID, cpu: 4, ram: 4096, diskGb: 8 });
    await expect(page.getByText("Installing", { exact: true })).toBeVisible();
    expect(state.engine).not.toBeNull(); state.engine!.phase = "running";
    await expect(page.getByText("Running", { exact: true })).toBeVisible();
    await page.getByLabel("Queue name", { exact: true }).fill("notifications");
    await page.getByRole("button", { name: "Create Queue", exact: true }).click();
    await expect.poll(() => writes.length).toBe(2);
    expect(writes[1].body).toEqual({ name: "notifications", deliveryLimit: 5, maxBytes: 64 * 1024 * 1024 });
    state.rows[0].phase = "ready";
    await expect(page.getByText("Ready", { exact: true })).toBeVisible();
    await page.getByRole("button", { name: "notifications", exact: true }).click();
    await expect(page.getByText("Unacknowledged", { exact: true })).toBeVisible();
    await expect(page.getByText("Consumers", { exact: true })).toBeVisible();
  });
  test("reveals only explicitly requested role credentials and hides them on blur", async ({ page }) => {
    await open(page);
    await page.getByRole("button", { name: "notifications", exact: true }).click();
    await expect(page.getByText(/amqp:\/\//)).toHaveCount(0);
    await page.getByRole("button", { name: "Show receiver connection", exact: true }).click();
    await expect(page.getByText("amqp://receiver:test-only@10.42.0.2:5672/test", { exact: true })).toBeVisible();
    await page.evaluate(() => window.dispatchEvent(new Event("blur")));
    await expect(page.getByText(/amqp:\/\//)).toHaveCount(0);
    await page.getByRole("button", { name: "Show sender connection", exact: true }).click();
    await expect(page.getByText("amqp://sender:test-only@10.42.0.2:5672/test", { exact: true })).toBeVisible();
    await page.getByRole("button", { name: "Hide", exact: true }).click();
    await expect(page.getByText(/amqp:\/\//)).toHaveCount(0);
  });
  test("blocks deletion of service-bound queues and clears metrics when the API is unavailable", async ({ page }) => {
    const { state } = await open(page, true, true);
    await expect(page.getByRole("button", { name: "Delete", exact: true })).toBeDisabled();
    await page.getByRole("button", { name: "notifications", exact: true }).click();
    await expect(page.getByText("Unacknowledged", { exact: true })).toBeVisible();
    state.fail = true;
    await expect(page.getByText(/Queue API unavailable/)).toBeVisible();
    await expect(page.getByRole("button", { name: "Create Queue", exact: true })).toBeDisabled();
    await expect(page.getByText("Unacknowledged", { exact: true })).toHaveCount(0);
  });
  test("keeps the Queue page usable on a narrow screen", async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 }); await open(page);
    await expect(page.getByRole("button", { name: "notifications", exact: true })).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  });
});
