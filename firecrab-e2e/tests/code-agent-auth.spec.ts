import { expect, test, type Page } from "@playwright/test";
import { readFileSync } from "node:fs";

const profiles = JSON.parse(readFileSync(new URL("../../firecrab-api/src/code_agent_auth/adapters.json", import.meta.url), "utf8"));
const id = "11111111-1111-4111-8111-111111111111", account = "22222222-2222-4222-8222-222222222222", vm = "33333333-3333-4333-8333-333333333333", instance = "44444444-4444-4444-8444-444444444444";
async function open(page: Page, options: { locale?: "en" | "ko"; agent?: string; provider?: string; authentication?: string } = {}) {
  const state = { phase: "ready", instance, connected: true, unavailable: false, reason: "", badReceipt: false, writes: [] as any[], delay: null as null | (() => Promise<void>) };
  const agent = options.agent ?? "codex", provider = options.provider ?? "openai", authentication = options.authentication ?? "api_key";
  const profile = profiles.find((p: any) => p.agent === agent && p.provider === provider && p.authentication === authentication);
  const errors: string[] = []; page.on("pageerror", e => errors.push(e.message));
  await page.addInitScript(locale => localStorage.setItem("firecrab.locale", locale), options.locale ?? "en");
  const record = () => ({ id, revision: 4, spec: { name: "web-app", github: { account: "octocat", owner: "team", repository: "web-app", branch: "main" }, runtime: "node_js", resources: { cpu: 2, ram: 4096, diskGb: 16 }, agent: { agent, connectionId: account } }, connections: { github: "present_unverified", ai: state.connected ? "present_unverified" : "missing" } });
  await page.route(/^https?:\/\/[^/]+\/api\//, async r => {
    const request = r.request(), path = new URL(request.url()).pathname;
    if (request.method() !== "GET") state.writes.push({ path, body: request.postDataJSON() });
    const send = (json: unknown, status = 200) => r.fulfill({ json, status });
    if (path === "/api/code/agent-auth/adapters") return state.unavailable ? send({ error: { code: "unavailable" } }, 503) : send(profiles);
    if (path === "/api/pipe/ai/connections") return send(state.connected ? [{ id: account, name: "Shared account", provider, authentication, models: [], streaming: false, toolCalls: false }] : []);
    if (path === "/api/code/workspaces") return send([record()]);
    if (path === `/api/code/workspaces/${id}`) return send(record());
    if (path === `/api/code/workspaces/${id}/runtime`) return send({ workspaceId: id, vmId: vm, configurationRevision: 4, operationId: vm, action: "start", phase: state.phase, projectPath: "/home/fc-code/project", runtimeId: state.instance, initialCommit: "a".repeat(40), vmState: state.phase === "ready" ? "running" : "stopped", error: null, placement: { template: { name: "dev", version: "v1" }, microNetworkId: vm, storageRoot: "default" } });
    if (path === `/api/code/workspaces/${id}/editor`) return send(null);
    if (path === `/api/code/workspaces/${id}/agent-runs`) return send([]);
    if (path === `/api/code/workspaces/${id}/agent-auth/verify`) {
      const receipt = { id: state.badReceipt ? vm : id, revision: 4, checkedAtMs: Date.now(), result: state.reason ? { status: "blocked", reason: state.reason } : { status: "configuration_verified", adapterId: profile.id, cliVersion: profile.cliVersion, vmId: vm, runtimeId: state.instance } };
      if (state.delay) await state.delay(); return send(receipt);
    }
    if (path === "/api/update") return send({ current: "0.4.3", latest: "0.4.3", updateAvailable: false });
    if (path === "/api/vms" || path === "/api/pipe/auth") return send([]);
    return send({ error: { code: "not_found" } }, 404);
  });
  await page.goto(`/#/code/${id}/editor`);
  await expect(page.locator("#code-editor h2")).toHaveText("web-app");
  await page.locator("#code-workspace-settings > summary").click();
  return { state, errors, profile };
}
const result = (page: Page) => page.locator("#code-workspace-settings").getByRole("status");
const verify = (page: Page, ko = false) => page.getByRole("button", { name: ko ? "에이전트 설정 검증" : "Verify agent configuration", exact: true });

for (const locale of ["en", "ko"] as const) for (const width of [1280, 390]) test(`${locale} configuration verification at ${width}px never submits a task @dashboard`, async ({ page }, info) => {
  const ko = locale === "ko"; await page.setViewportSize({ width, height: 1000 }); const { state, errors, profile } = await open(page, { locale });
  await expect(verify(page, ko)).toBeEnabled(); expect(state.writes).toHaveLength(0);
  await verify(page, ko).click(); await expect(result(page)).toContainText("configuration_verified");
  await expect(result(page)).toContainText(profile.cliVersion);
  await expect(result(page)).toContainText(ko ? "CLI 설정 확인" : "CLI configuration check");
  await expect(page.getByRole("button", { name: ko ? "에이전트 실행" : "Run agent", exact: true })).toBeDisabled();
  expect(state.writes).toEqual([{ path: `/api/code/workspaces/${id}/agent-auth/verify`, body: { expectedRevision: 4 } }]);
  await result(page).scrollIntoViewIfNeeded(); expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  if (!ko) await page.screenshot({ path: info.outputPath(`code-agent-auth-en-${width}.png`), fullPage: true });
  expect(errors).toEqual([]);
});
for (const [agent, provider] of [["claude_code", "anthropic"], ["opencode", "openai"], ["opencode", "anthropic"]]) test(`${agent}/${provider} uses its pinned recipe @dashboard`, async ({ page }) => {
  const { state, profile, errors } = await open(page, { agent, provider });
  await expect(verify(page)).toBeEnabled(); await verify(page).click();
  await expect(result(page)).toContainText(profile.id); await expect(result(page)).toContainText(profile.cliVersion);
  expect(state.writes).toHaveLength(1); expect(errors).toEqual([]);
});
test("ChatGPT stays disabled without a verified native handoff @dashboard", async ({ page }) => {
  const { state, errors } = await open(page, { authentication: "chatgpt" });
  await expect(verify(page)).toBeDisabled();
  await page.getByLabel("Task", { exact: true }).fill("Update README");
  await expect(page.locator("#code-agent-execution")).toContainText("Select a supported agent");
  await expect(page.getByRole("button", { name: "Run agent", exact: true })).toBeDisabled(); expect(state.writes).toHaveLength(0); expect(errors).toEqual([]);
});
test("CLI failures and mismatched receipts never become an accepted configuration check @dashboard", async ({ page }) => {
  const { state, errors } = await open(page); await expect(verify(page)).toBeEnabled(); state.reason = "version_mismatch"; await verify(page).click();
  await expect(result(page)).toContainText("version_mismatch");
  state.reason = ""; state.badReceipt = true; await verify(page).click(); await expect(result(page)).toContainText("Check unavailable");
  await expect(result(page)).not.toContainText("configuration_verified");
  expect(state.writes.every(w => w.path.endsWith("/agent-auth/verify"))).toBe(true); expect(errors).toEqual([]);
});
test("runtime restart and account disconnection invalidate in-flight proof @dashboard", async ({ page }) => {
  const { state, errors } = await open(page); await page.clock.install(); await expect(verify(page)).toBeEnabled();
  let release!: () => void; state.delay = () => new Promise<void>(r => { release = r; });
  await verify(page).click(); await expect.poll(() => state.writes.length).toBe(1);
  state.phase = "stopped"; await page.clock.fastForward(3100); await expect(page.locator(".code-runtime-status")).toHaveText("stopped"); await expect(verify(page)).toBeDisabled();
  const response = page.waitForResponse(r => r.url().endsWith("/agent-auth/verify")); release(); await (await response).finished(); await page.evaluate(() => new Promise(requestAnimationFrame));
  await expect(result(page)).toHaveCount(0);
  state.connected = false; state.phase = "ready"; await page.clock.fastForward(3100); await expect(verify(page)).toBeDisabled(); expect(errors).toEqual([]);
});
test("adapter catalog outage fails closed and recovers automatically @dashboard", async ({ page }) => {
  const { state, errors } = await open(page); await page.clock.install(); await expect(verify(page)).toBeEnabled(); state.unavailable = true; await page.clock.fastForward(3100);
  await expect(verify(page)).toBeDisabled();
  await expect(page.getByRole("button", { name: "Run agent", exact: true })).toBeDisabled();
  state.unavailable = false; await page.clock.fastForward(3100); await expect(verify(page)).toBeEnabled(); expect(state.writes).toHaveLength(0); expect(errors).toEqual([]);
});
