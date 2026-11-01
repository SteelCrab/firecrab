import { expect, test, type Page } from "@playwright/test";

const accountId = "11111111-1111-4111-8111-111111111111", workspaceId = "22222222-2222-4222-8222-222222222222", loginId = "33333333-3333-4333-8333-333333333333";
const connection = { id: accountId, name: "Shared AI account", provider: "openai", authentication: "api_key", endpoint: "https://api.openai.com/v1/responses", models: [], streaming: false, toolCalls: false };
const spec = { name: "web-app", github: { account: "octocat", owner: "team", repository: "web-app", branch: "main" }, runtime: "node_js", resources: { cpu: 2, ram: 4096, diskGb: 16 }, agent: { agent: "codex", connectionId: accountId } };
async function open(page: Page, options: { locale?: "en" | "ko"; setup?: boolean; empty?: boolean; chatgpt?: boolean } = {}) {
  const state = { connections: options.empty ? [] : [{ ...connection, authentication: options.chatgpt ? "chatgpt" : "api_key" }], writes: [] as { method: string; path: string; body: any }[], configs: [] as any[], approved: false, cancelled: false, failSave: false, unavailable: false, loginConnection: "", loginName: "", reports: [] as any[], delayCheck: null as null | (() => Promise<void>) };
  const errors: string[] = []; page.on("pageerror", e => errors.push(e.message));
  await page.addInitScript(locale => localStorage.setItem("firecrab.locale", locale), options.locale ?? "en");
  await page.context().route("https://auth.openai.com/**", r => r.fulfill({ body: "Mock ChatGPT consent page" }));
  await page.route(/^https?:\/\/[^/]+\/api\//, async route => {
    const request = route.request(), path = new URL(request.url()).pathname, method = request.method();
    const send = (json: unknown, status = 200) => route.fulfill({ json, status });
    if (method === "GET" && ["/api/images", "/api/micro-networks", "/api/storage"].includes(path)) return send([]);
    if (method !== "GET") state.writes.push({ method, path, body: request.postData() ? request.postDataJSON() : null });
    if (path === "/api/pipe/ai/connections") return state.unavailable ? send({ error: { code: "unavailable" } }, 503) : send(state.connections);
    if (path.endsWith("/login")) { state.loginConnection = path.split("/")[5]; state.loginName = request.postDataJSON().name; state.cancelled = false; return send({ loginId, authorizationUrl: "https://auth.openai.com/api/accounts/authorize?state=test", expiresIn: 600 }); }
    if (path === `/api/pipe/ai/logins/${loginId}`) {
      if (method === "DELETE") { state.cancelled = true; return send({ status: "failed", message: "Sign-in cancelled." }); }
      if (state.cancelled) return send({ status: "failed", message: "Sign-in cancelled." });
      if (!state.approved) return send({ status: "pending", message: null });
      state.connections = [...state.connections.filter(c => c.id !== state.loginConnection), { ...connection, id: state.loginConnection, name: state.loginName, authentication: "chatgpt" }];
      return send({ status: "connected", message: null });
    }
    const ai = path.match(/^\/api\/pipe\/ai\/connections\/([^/]+)$/);
    if (ai) {
      if (method === "DELETE") { state.connections = state.connections.filter(c => c.id !== ai[1]); return send({ deleted: true, remoteRevocationConfirmed: false }); }
      if (state.failSave) return send({ error: { code: "ai_authentication", message: "Provider rejected credentials" } }, 401);
      const body = request.postDataJSON();
      const saved = { ...connection, id: ai[1], name: body.name, provider: body.provider };
      state.connections = [...state.connections.filter(c => c.id !== saved.id), saved]; return send(saved);
    }
    if (path === `/api/code/workspaces/${workspaceId}/verify`) {
      state.reports.push(request.postDataJSON()); if (state.delayCheck) await state.delayCheck();
      return send({ id: workspaceId, revision: 4, checkedAtMs: Date.now(), github: { status: "verified", commitSha: "a".repeat(40) }, ai: { status: "credentials_verified", method: options.chatgpt ? "chatgpt_plan" : "openai_api_key" } });
    }
    const record = { id: workspaceId, revision: 4, spec, createdAtMs: 1, updatedAtMs: 2, connections: { github: "present_unverified", ai: state.connections.some(c => c.id === accountId) ? "present_unverified" : "missing" } };
    if (path === "/api/code/workspaces") return send(options.setup ? state.configs : [record]);
    if (/^\/api\/code\/workspaces\/[^/]+\/runtime$/.test(path)) return send({ error: { code: "not_found" } }, 404);
    if (/^\/api\/code\/workspaces\/[^/]+$/.test(path)) {
      if (method === "PUT") { const body = request.postDataJSON(); const saved = { ...record, id: path.split("/").at(-1)!, revision: 1, spec: body.spec }; state.configs.push(saved); return send(saved); }
      return send(state.configs.find(c => c.id === path.split("/").at(-1)) ?? record);
    }
    if (path === "/api/pipe/auth") return send([{ provider: "github", account: "octocat" }]);
    if (path === "/api/code/github/repositories") return send({ account: "octocat", repositories: [{ owner: "team", repository: "web-app", defaultBranch: "main", private: true }], nextPage: null });
    if (path === "/api/code/github/branches") return send({ account: "octocat", owner: "team", repository: "web-app", branches: ["main"], nextPage: null });
    if (path === "/api/update") return send({ current: "0.4.1", latest: "0.4.1", updateAvailable: false });
    if (path === "/api/vms") return send([]);
    return send({ error: { code: "not_found" } }, 404);
  });
  await page.goto(options.setup ? "/#/code/new" : `/#/code/${workspaceId}`); return { state, errors };
}
async function manage(page: Page, ko = false) { await page.getByRole("button", { name: ko ? "AI AUTH 관리" : "Manage AI AUTH", exact: true }).click(); }

for (const locale of ["en", "ko"] as const) for (const width of [1280, 390]) test(`${locale} connect and reuse AUTH in setup at ${width}px @dashboard`, async ({ page }, info) => {
  const ko = locale === "ko"; await page.setViewportSize({ width, height: 1000 });
  const { state, errors } = await open(page, { locale, setup: true, empty: true });
  await page.getByRole("textbox", { name: ko ? "워크스페이스 이름" : "Workspace name", exact: true }).fill("retained-draft");
  await page.getByRole("combobox", { name: ko ? "GitHub 저장소" : "GitHub repository", exact: true }).selectOption("team/web-app");
  await page.getByRole("combobox", { name: ko ? "에이전트" : "Agent", exact: true }).selectOption("codex");
  await manage(page, ko);
  await page.getByRole("button", { name: ko ? "API 키로 연결" : "Use API key", exact: true }).click();
  await page.getByRole("textbox", { name: ko ? "연결 이름" : "Connection name", exact: true }).fill("Shared CODE key");
  await page.getByLabel(ko ? "OpenAI API 키" : "OpenAI API key", { exact: true }).fill("code-auth-private-key");
  await page.getByRole("button", { name: ko ? "검증 후 저장" : "Verify and save", exact: true }).click();
  await expect(page.getByRole("table")).toContainText("Shared CODE key");
  expect(state.connections).toHaveLength(1);
  if (!ko) await page.screenshot({ path: info.outputPath(`code-auth-en-${width}.png`), fullPage: true });
  await page.getByRole("button", { name: ko ? "워크스페이스로 돌아가기" : "Back to workspace", exact: true }).click();
  await expect(page.getByRole("textbox", { name: ko ? "워크스페이스 이름" : "Workspace name", exact: true })).toHaveValue("retained-draft");
  const select = page.getByRole("combobox", { name: ko ? "AI AUTH 계정" : "AI AUTH account", exact: true });
  await select.selectOption(state.connections[0].id);
  await page.getByRole("button", { name: ko ? "워크스페이스 생성" : "Create workspace", exact: true }).click();
  await expect(page.locator("#code-details h2")).toHaveText("retained-draft");
  expect(state.configs[0].spec.agent.connectionId).toBe(state.connections[0].id);
  expect(JSON.stringify(state.configs)).not.toContain("code-auth-private-key");
  expect(await page.evaluate(() => JSON.stringify({ ...localStorage, ...sessionStorage }))).not.toContain("code-auth-private-key");
  expect(state.writes.filter(w => w.path.startsWith("/api/code/")).every(w => !JSON.stringify(w.body).includes("apiKey"))).toBe(true);
  expect(errors).toEqual([]);
});

test("shared key replacement and disconnection retain the connection ID and invalidate checks @dashboard", async ({ page }) => {
  const { state, errors } = await open(page);
  await page.getByRole("button", { name: "Check saved connections", exact: true }).click();
  await expect(page.locator(".code-credential-result")).toContainText("Provider credentials verified");
  await expect(page.locator("#content")).toContainText("Agent authentication remains unverified.");
  expect(state.reports).toEqual([{ expectedRevision: 4 }]);
  await manage(page); await page.getByRole("button", { name: "Replace API key", exact: true }).click();
  await page.getByLabel("OpenAI API key", { exact: true }).fill("code-auth-private-replacement");
  await page.getByRole("button", { name: "Verify and save", exact: true }).click(); await expect(page.getByRole("table")).toContainText(connection.name);
  expect(state.writes.find(w => w.method === "PUT")?.path).toBe(`/api/pipe/ai/connections/${accountId}`);
  await page.getByRole("button", { name: "Disconnect", exact: true }).click(); await expect(page.getByRole("table")).toHaveCount(0);
  await expect(page.locator("#content")).toContainText("Remote revocation was not confirmed");
  await page.getByRole("button", { name: "Back to workspace", exact: true }).click();
  await expect(page.locator("#code-details")).toContainText("Account missing or disconnected");
  await expect(page.locator(".code-credential-result")).toHaveCount(0);
  expect(state.writes.filter(w => w.method === "PUT" && w.path.startsWith("/api/code/"))).toHaveLength(0); expect(errors).toEqual([]);
});

test("ChatGPT approval uses the shared login service and returns to the same draft @dashboard", async ({ page }) => {
  const { state, errors } = await open(page, { setup: true, empty: true }); await page.clock.install();
  await page.getByRole("textbox", { name: "Workspace name", exact: true }).fill("kept-on-sign-in"); await manage(page);
  await page.getByRole("button", { name: "Connect ChatGPT", exact: true }).click();
  await page.getByRole("button", { name: "Continue with ChatGPT", exact: true }).click();
  await expect(page.getByRole("link", { name: "Open ChatGPT sign-in", exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "Back to workspace", exact: true })).toBeDisabled();
  const start = state.writes.find(w => w.path.endsWith("/login"))!; expect(start.body).toEqual({ name: "ChatGPT" });
  state.approved = true; await page.clock.fastForward(1600);
  await expect(page.getByRole("table")).toContainText("ChatGPT account");
  await page.getByRole("button", { name: "Back to workspace", exact: true }).click();
  await expect(page.getByRole("textbox", { name: "Workspace name", exact: true })).toHaveValue("kept-on-sign-in"); expect(errors).toEqual([]);
});

test("cancelled re-login preserves a shared account and pending callbacks cannot change the UI @dashboard", async ({ page }) => {
  const { state, errors } = await open(page, { chatgpt: true }); await page.clock.install(); await manage(page);
  await page.getByRole("button", { name: "Sign in again", exact: true }).click();
  await page.getByRole("button", { name: "Continue with ChatGPT", exact: true }).click();
  await page.getByRole("button", { name: "Cancel sign-in", exact: true }).click();
  await expect(page.locator("#content")).toContainText("Sign-in cancelled.");
  await expect(page.getByRole("button", { name: "Back to workspace", exact: true })).toBeEnabled();
  state.approved = true; await page.clock.fastForward(5000);
  expect(state.connections[0].id).toBe(accountId); expect(state.connections[0].name).toBe(connection.name);
  expect(state.writes.filter(w => w.method === "DELETE").map(w => w.path)).toEqual([`/api/pipe/ai/logins/${loginId}`]); expect(errors).toEqual([]);
});

test("failed key rotation clears the secret input and retains the existing account @dashboard", async ({ page }) => {
  const { state, errors } = await open(page); state.failSave = true; await manage(page);
  await page.getByRole("button", { name: "Replace API key", exact: true }).click();
  await page.getByLabel("OpenAI API key", { exact: true }).fill("code-auth-private-rejected");
  await page.getByRole("button", { name: "Verify and save", exact: true }).click();
  await expect(page.locator("#content [role=alert]")).toContainText("Provider rejected");
  await expect(page.getByLabel("OpenAI API key", { exact: true })).toHaveValue(""); expect(state.connections[0]).toEqual(connection);
  await page.getByRole("button", { name: "Cancel", exact: true }).click(); await page.getByRole("button", { name: "Back to workspace", exact: true }).click(); expect(errors).toEqual([]);
});

test("late verification results cannot survive AUTH management or navigation @dashboard", async ({ page }) => {
  const { state, errors } = await open(page); let release!: () => void; const held = new Promise<void>(r => { release = r; }); state.delayCheck = () => held;
  await page.getByRole("button", { name: "Check saved connections", exact: true }).click(); await expect.poll(() => state.reports.length).toBe(1);
  await manage(page); release(); await expect(page.getByRole("table")).toBeVisible();
  await page.getByRole("button", { name: "Back to workspace", exact: true }).click(); await expect(page.locator(".code-credential-result")).toHaveCount(0); expect(errors).toEqual([]);
});

test("Anthropic uses the same account service and ChatGPT checks stay distinct from agent authentication @dashboard", async ({ page }) => {
  const { state, errors } = await open(page, { chatgpt: true });
  await page.getByRole("button", { name: "Check saved connections", exact: true }).click();
  await expect(page.locator(".code-credential-result")).toContainText("ChatGPT plan");
  await manage(page); await page.getByRole("button", { name: "Add connection", exact: true }).click();
  await page.getByRole("radio", { name: "API key", exact: true }).check();
  await page.getByRole("combobox", { name: "Provider", exact: true }).selectOption("anthropic");
  await page.getByLabel("Anthropic API key", { exact: true }).fill("code-auth-private-anthropic");
  await page.getByRole("button", { name: "Verify and save", exact: true }).click();
  await expect(page.getByRole("table")).toContainText("Anthropic API key");
  const write = state.writes.find(w => w.method === "PUT")!;
  expect(write.path).toMatch(/^\/api\/pipe\/ai\/connections\/[0-9a-f-]{36}$/);
  expect(write.body.provider).toBe("anthropic"); expect(write.body.model).toBeUndefined(); expect(state.configs).toHaveLength(0); expect(errors).toEqual([]);
});

test("failed account reads are retryable and do not erase a CODE draft @dashboard", async ({ page }) => {
  const { state, errors } = await open(page, { setup: true });
  await page.getByRole("textbox", { name: "Workspace name", exact: true }).fill("kept-through-outage");
  state.unavailable = true; await manage(page);
  await expect(page.locator("#content [role=alert]")).toContainText("Could not load AI AUTH accounts");
  state.unavailable = false; await page.getByRole("button", { name: "Retry", exact: true }).click();
  await expect(page.getByRole("table")).toContainText(connection.name);
  await page.getByRole("button", { name: "Back to workspace", exact: true }).click();
  await expect(page.getByRole("textbox", { name: "Workspace name", exact: true })).toHaveValue("kept-through-outage"); expect(errors).toEqual([]);
});

test("known disconnection invalidates a delayed successful credential report @dashboard", async ({ page }) => {
  const { state, errors } = await open(page); await page.clock.install();
  let release!: () => void; const held = new Promise<void>(r => { release = r; }); state.delayCheck = () => held;
  await page.getByRole("button", { name: "Check saved connections", exact: true }).click(); await expect.poll(() => state.reports.length).toBe(1);
  state.connections = []; await page.clock.fastForward(3100);
  await expect(page.locator("#code-details")).toContainText("Account missing or disconnected"); release();
  await expect(page.locator("#code-details [role=alert]")).toContainText("Connections changed"); await expect(page.locator(".code-credential-result")).toHaveCount(0); expect(errors).toEqual([]);
});

test("cancelling the shared account form keeps the workspace's unsaved-change guard @dashboard", async ({ page }) => {
  const { errors } = await open(page, { setup: true, empty: true });
  await page.getByRole("textbox", { name: "Workspace name", exact: true }).fill("unsaved-draft"); await manage(page);
  await page.getByRole("button", { name: "Use API key", exact: true }).click();
  await page.getByRole("button", { name: "Cancel", exact: true }).click();
  await page.getByRole("button", { name: "Back to workspace", exact: true }).click();
  let guarded = false; page.once("dialog", async d => { guarded = true; await d.dismiss(); });
  await page.locator('a[href="#/vms"]').click(); await expect.poll(() => guarded).toBe(true);
  await expect(page).toHaveURL(/#\/code\/new$/); await expect(page.getByRole("textbox", { name: "Workspace name", exact: true })).toHaveValue("unsaved-draft"); expect(errors).toEqual([]);
});
