import { expect, test } from "@playwright/test";
import { openDashboard } from "./support/mockApi";

const id = "11111111-1111-4111-8111-111111111111";
const second = "22222222-2222-4222-8222-222222222222";
const base = "/api/code/workspaces";
function workspace(key = id, name = "web-app") {
  return { id: key, revision: 4, createdAtMs: 1, updatedAtMs: 2,
    spec: { name, github: { account: "octocat", owner: "team", repository: name, branch: "feature/login" },
      runtime: "node_js", resources: { cpu: 2, ram: 4096, diskGb: 16 }, agent: { agent: "codex", connectionId: second } },
    connections: { github: "present_unverified", ai: "present_unverified" } };
}
function runtime(key = id, phase = "ready") {
  return { workspaceId: key, vmId: second, configurationRevision: 4, operationId: second, action: "start", phase,
    projectPath: "/home/fc-code/project", runtimeId: second, initialCommit: "a".repeat(40), vmState: "running", error: null as string | null,
    placement: { template: { name: "ubuntu-26.04", version: "2026.10" }, microNetworkId: second, storageRoot: "default" } };
}
const host = { uptimeSeconds: 3600, loadAverage1m: .2, memoryTotalMib: 16384, memoryAvailableMib: 8192,
  diskTotalGib: 200, diskAvailableGib: 100, platform: { name: "Debian", architecture: "aarch64" } };

for (const locale of ["en", "ko"] as const) for (const width of [1280, 390]) {
  test(`${locale} CODE navigation and real records at ${width}px @dashboard`, async ({ page }, info) => {
    await page.setViewportSize({ width, height: 1000 });
    const errors: string[] = [], writes: string[] = [];
    page.on("pageerror", error => errors.push(error.message));
    page.on("request", request => { if (request.url().includes("/api/") && request.method() !== "GET") writes.push(request.url()); });
    await openDashboard(page, { locale, hash: "#/code", routes: {
      [base]: [workspace(), workspace(second, "worker")], [`${base}/${id}/runtime`]: runtime(), "/api/host": host,
    } });
    await expect(page.locator("h1")).toHaveText(locale === "en" ? "Workspaces" : "워크스페이스");
    await expect(page.locator(".code-runtime-status")).toHaveText(locale === "en" ? "MicroVM ready" : "MicroVM 준비 완료");
    await expect(page.locator("#code-details")).toContainText("team/web-app");
    await expect(page.locator("#code-details")).toContainText("feature/login");
    await expect(page.locator("#code-details")).toContainText(locale === "en" ? "Agent authentication is not verified here." : "이 화면에서는 에이전트 인증을 검증하지 않습니다.");
    await expect(page.locator("#detail-panel")).toBeHidden();
    if (locale === "en") await page.screenshot({ path: info.outputPath(`code-workspaces-en-${width}.png`), fullPage: true });
    await page.locator("#code-overview-settings > summary").click();
    const auth = page.getByRole("button", { name: locale === "en" ? "Manage AI AUTH" : "AI AUTH 관리", exact: true });
    await auth.scrollIntoViewIfNeeded();
    await expect(auth).toBeInViewport();
    if (locale === "en") await page.screenshot({ path: info.outputPath(`code-workspaces-en-auth-${width}.png`), fullPage: true });
    const search = page.getByRole("searchbox");
    await search.fill("  WORKER  ");
    await expect(page.locator(".code-list li")).toHaveCount(1);
    await expect(search).toBeFocused();
    await expect(search).toHaveValue("  WORKER  ");
    await search.fill("no-such-workspace");
    await expect(page.locator(".code-list li")).toHaveCount(0);
    await expect(page.locator(".code-list")).toContainText(locale === "en" ? "No matching workspaces." : "검색 결과가 없습니다.");
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    expect(errors).toEqual([]); expect(writes).toEqual([]);
  });
}

test("selection survives reload, follows history and isolates late runtime responses @dashboard", async ({ page }) => {
  let release!: () => void;
  const blocked = new Promise<void>(resolve => { release = resolve; });
  let arrived = false, hold = true;
  await openDashboard(page, { hash: `#/code/${id}`, routes: {
    [base]: [workspace(), workspace(second, "worker")],
    [`${base}/${id}/runtime`]: async route => { arrived = true; if (hold) await blocked; await route.fulfill({ json: runtime() }); },
    [`${base}/${second}/runtime`]: runtime(second, "stopped"), "/api/host": host,
  } });
  // The first list and runtime are published together. Navigate while they are in flight.
  await expect.poll(() => arrived).toBe(true);
  await page.evaluate(key => { location.hash = `#/code/${key}`; }, second);
  await expect(page.locator("#code-details h2")).toHaveText("worker");
  hold = false; release();
  await expect(page.locator(".code-runtime-status")).toHaveText("Stopped · files retained");
  await page.reload();
  await expect(page.locator("#code-details h2")).toHaveText("worker");
  await page.locator(`#code-link-${id}`).click();
  await expect(page.locator("#code-details h2")).toHaveText("web-app");
  await expect(page.locator(".code-runtime-status")).toHaveText("MicroVM ready");
  await page.goBack();
  await expect(page.locator("#code-details h2")).toHaveText("worker");
});

test("polling removes stale readiness, observes disconnection and recovers @dashboard", async ({ page }) => {
  let fail = false, record = workspace();
  await page.clock.install();
  await openDashboard(page, { hash: "#/code", routes: {
    [base]: route => route.fulfill({ json: [record] }),
    [`${base}/${id}/runtime`]: route => fail
      ? route.fulfill({ status: 503, json: { error: { code: "unavailable", message: "private diagnostic must stay hidden" } } })
      : route.fulfill({ json: runtime() }), "/api/host": host,
  } });
  await expect(page.locator(".code-runtime-status")).toHaveText("MicroVM ready");
  const search = page.getByRole("searchbox"); await search.fill("web");
  fail = true; record.connections.ai = "missing";
  await page.clock.fastForward(3100);
  await expect(page.locator("#code-details [role=alert]")).toContainText("Workspace status is unavailable");
  await expect(page.locator(".code-runtime-status")).toHaveCount(0);
  await expect(page.locator("#api-status")).toHaveText("Status unavailable");
  await expect(page.locator("#code-details")).toContainText("Account missing or disconnected");
  await expect(page.locator("body")).not.toContainText("private diagnostic");
  await expect(search).toBeFocused(); await expect(search).toHaveValue("web");
  fail = false; await page.clock.fastForward(3100);
  await expect(page.locator(".code-runtime-status")).toHaveText("MicroVM ready");
  await expect(page.locator("#api-status")).toHaveText("Live");
});

for (const endpoint of ["list", "runtime"]) test(`${endpoint} access failures never become empty or retain old private records @dashboard`, async ({ page }) => {
  let status = 200;
  await page.clock.install();
  await openDashboard(page, { hash: "#/code", routes: {
    [base]: route => route.fulfill({ status: endpoint === "list" ? status : 200, json: endpoint !== "list" || status === 200 ? [workspace()] : { error: { code: "forbidden" } } }),
    [`${base}/${id}/runtime`]: route => route.fulfill({ status: endpoint === "runtime" ? status : 200, json: endpoint !== "runtime" || status === 200 ? runtime() : { error: { code: "forbidden" } } }), "/api/host": host,
  } });
  await expect(page.locator("#code-details h2")).toHaveText("web-app");
  status = 403; await page.clock.fastForward(3100);
  await expect(page.locator("#content [role=alert]")).toContainText("Operator access is required");
  await expect(page.locator("#code-details")).toHaveCount(0);
  await expect(page.locator("#content")).not.toContainText("No workspaces yet.");
  status = 200; await page.clock.fastForward(3100);
  await expect(page.locator("#code-details h2")).toHaveText("web-app");
});

test("only a confirmed runtime absence is shown as unprovisioned @dashboard", async ({ page }) => {
  let code = "not_found", exists = true;
  await openDashboard(page, { hash: "#/code", routes: {
    [base]: [workspace()],
    [`${base}/${id}`]: route => route.fulfill({ status: exists ? 200 : 404, json: exists ? workspace() : { error: { code: "not_found" } } }),
    [`${base}/${id}/runtime`]: route => route.fulfill({ status: 404, json: { error: { code } } }), "/api/host": host,
  } });
  await expect(page.locator("#code-details")).toContainText("MicroVM not provisioned");
  code = "unknown_route"; await page.getByRole("button", { name: "Refresh", exact: true }).click();
  await expect(page.locator("#code-details [role=alert]")).toContainText("Workspace status is unavailable");
  code = "not_found"; exists = false; await page.getByRole("button", { name: "Refresh", exact: true }).click();
  await expect(page.locator("#code-details")).not.toContainText("MicroVM not provisioned");
});

test("empty, deleted and malformed records have distinct states @dashboard", async ({ page }) => {
  let records: unknown = [];
  await openDashboard(page, { hash: `#/code/${id}`, routes: { [base]: route => route.fulfill({ json: records }), "/api/host": host } });
  await expect(page.locator("#content")).toContainText("No workspaces yet.");
  records = [workspace(second, "worker")]; await page.getByRole("button", { name: "Refresh", exact: true }).click();
  await expect(page.locator("#content")).toContainText("This workspace no longer exists.");
  records = { unknown: "schema" }; await page.getByRole("button", { name: "Refresh", exact: true }).click();
  await expect(page.locator("#content [role=alert]")).toContainText("Workspace data could not be read");
});

test("configuration and runtime cannot inject markup or enable an agent @dashboard", async ({ page }) => {
  const record = workspace(); record.spec.name = "<img src=x onerror=alert(1)>"; record.connections.ai = "incompatible";
  const observation = runtime(id, "failed"); observation.error = "sync_failed";
  await openDashboard(page, { hash: "#/code", routes: { [base]: [record], [`${base}/${id}/runtime`]: observation, "/api/host": host } });
  await expect(page.locator("#code-details h2")).toHaveText(record.spec.name);
  await expect(page.locator("#content img")).toHaveCount(0);
  await expect(page.locator("#code-details")).toContainText("Account is incompatible with the selected agent");
  await expect(page.locator("#code-details [role=alert]")).toHaveText("File synchronization failed. The VM was kept running.");
  await expect(page.getByRole("button", { name: /Run agent|Open editor/ })).toHaveCount(0);
  await page.locator("#code-overview-settings > summary").click();
  await expect(page.getByRole("button", { name: "Manage AI AUTH" })).toHaveAttribute("data-action", "code-auth-open");
});
