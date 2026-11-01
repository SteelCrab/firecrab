import { expect, test, type Page } from "@playwright/test";

const id = "11111111-1111-4111-8111-111111111111", other = "22222222-2222-4222-8222-222222222222";
const key = "33333333-3333-4333-8333-333333333333", chat = "44444444-4444-4444-8444-444444444444", claude = "55555555-5555-4555-8555-555555555555";
const loginId = "66666666-6666-4666-8666-666666666666", base = "/api/code/workspaces";
const account = (id: string, name: string, provider = "openai", authentication = "api_key") =>
  ({ id, name, provider, authentication, endpoint: provider === "openai" ? "https://api.openai.com/v1" : "https://api.anthropic.com", models: [], streaming: false, toolCalls: false });
const record = (workspaceId = id, name = "web-app") => ({ id: workspaceId, revision: 4, createdAtMs: 1, updatedAtMs: 2,
  spec: { name, github: { account: "octocat", owner: "team", repository: name, branch: "main" }, runtime: "node_js", resources: { cpu: 2, ram: 4096, diskGb: 16 }, agent: { agent: "codex", connectionId: key } as null | { agent: string; connectionId: string } } });
async function open(page: Page, options: { locale?: "en" | "ko"; editor?: boolean; missing?: boolean } = {}) {
  const state = {
    records: [record(), record(other, "worker")],
    accounts: [account(key, "Shared OpenAI"), account(chat, "Personal ChatGPT", "openai", "chatgpt"), account(claude, "Team Anthropic", "anthropic")],
    writes: [] as { method: string; path: string; body: any }[],
    accountsUnavailable: false, probeUnavailable: false, rejectSave: false, failure: "" as "" | "committed" | "not_saved",
    failureReason: "", approved: false, cancelled: false, loginAccount: "", loginName: "", listCalls: 0,
    delayList: null as null | (() => Promise<void>), delayAccounts: null as null | (() => Promise<void>),
  };
  if (options.missing) state.accounts = state.accounts.filter(a => a.id !== key);
  const errors: string[] = []; page.on("pageerror", e => errors.push(e.message));
  await page.addInitScript(locale => localStorage.setItem("firecrab.locale", locale), options.locale ?? "en");
  await page.context().route("https://auth.openai.com/**", r => r.fulfill({ body: "Test approval" }));
  function project(w: ReturnType<typeof record>) {
    const b = w.spec.agent, a = state.accounts.find(a => a.id === b?.connectionId);
    const compatible = a && (b?.agent === "codex" ? a.provider === "openai" : b?.agent === "claude_code" ? a.provider === "anthropic" && a.authentication === "api_key" : a.authentication === "api_key");
    return structuredClone({ ...w, connections: { github: "present_unverified", ai: !b ? "not_required" : !a ? "missing" : compatible ? "present_unverified" : "incompatible" } });
  }
  await page.route(/^https?:\/\/[^/]+\/api\//, async route => {
    const request = route.request(), path = new URL(request.url()).pathname, method = request.method();
    const send = (json: unknown, status = 200) => route.fulfill({ json, status });
    if (method === "GET" && ["/api/images", "/api/micro-networks", "/api/storage"].includes(path)) return send([]);
    if (method !== "GET") state.writes.push({ method, path, body: request.postData() ? request.postDataJSON() : null });
    if (path === "/api/pipe/ai/connections") {
      const rows = structuredClone(state.accounts), delay = state.delayAccounts; state.delayAccounts = null;
      if (delay) await delay();
      return state.accountsUnavailable ? send({ error: { code: "unavailable" } }, 503) : send(rows);
    }
    if (path.endsWith("/login")) {
      state.loginAccount = path.split("/")[5]; state.loginName = request.postDataJSON().name; state.cancelled = false;
      return send({ loginId, authorizationUrl: "https://auth.openai.com/authorize?state=test", expiresIn: 600 });
    }
    if (path === `/api/pipe/ai/logins/${loginId}`) {
      if (method === "DELETE") state.cancelled = true;
      if (state.cancelled) return send({ status: "failed", message: "Sign-in cancelled." });
      if (!state.approved) return send({ status: "pending", message: null });
      state.accounts = [...state.accounts.filter(a => a.id !== state.loginAccount), account(state.loginAccount, state.loginName, "openai", "chatgpt")];
      return send({ status: "connected", message: null });
    }
    if (/^\/api\/pipe\/ai\/connections\/[^/]+$/.test(path) && method === "DELETE") {
      state.accounts = state.accounts.filter(a => a.id !== path.split("/").at(-1));
      return send({ deleted: true, remoteRevocationConfirmed: true });
    }
    if (path === base) {
      state.listCalls++;
      const rows = state.records.map(project), delay = state.delayList; state.delayList = null;
      if (delay) await delay(); return send(rows);
    }
    const match = path.match(/^\/api\/code\/workspaces\/([^/]+)(\/(?:runtime|verify))?$/);
    if (match) {
      const w = state.records.find(w => w.id === match[1]); if (!w) return send({ error: { code: "not_found" } }, 404);
      if (match[2] === "/runtime") return send({ error: { code: "not_found" } }, 404);
      if (match[2] === "/verify") return send({ id: w.id, revision: w.revision, checkedAtMs: Date.now(), github: { status: "verified", commitSha: "a".repeat(40) }, ai: state.failureReason ? { status: "failed", reason: state.failureReason } : { status: "credentials_verified", method: "openai_api_key" } });
      if (method === "PUT") {
        const body = request.postDataJSON();
        if (state.rejectSave) return send({ error: { code: "validation_failed" } }, 400);
        if (body.expectedRevision !== w.revision) return send({ error: { code: "code_workspace_revision_conflict" } }, 409);
        const failure = state.failure; state.failure = "";
        if (failure !== "not_saved") { w.spec = body.spec; w.revision++; }
        if (failure) return route.abort("failed"); return send(project(w));
      }
      return state.probeUnavailable ? send({ error: { code: "unavailable" } }, 503) : send(project(w));
    }
    if (path === "/api/update") return send({ current: "0.4.1", latest: "0.4.1", updateAvailable: false });
    if (path === "/api/vms") return send([]);
    return send({ error: { code: "not_found" } }, 404);
  });
  await page.goto(`/#/code/${id}${options.editor ? "/editor" : ""}`);
  await expect(page.locator(options.editor ? "#code-editor h2" : "#code-details h2")).toHaveText("web-app");
  return { state, errors };
}
const assignment = (page: Page) => page.locator(".code-binding");
const choose = (page: Page) => page.getByRole("button", { name: "Choose AI AUTH", exact: true }).click();
const save = (page: Page) => page.getByRole("button", { name: "Save assignment", exact: true });

for (const locale of ["en", "ko"] as const) for (const width of [1280, 390]) test(`${locale} reuse and remove an editor assignment at ${width}px @dashboard`, async ({ page }, info) => {
  const ko = locale === "ko"; await page.setViewportSize({ width, height: 1000 });
  const { state, errors } = await open(page, { locale, editor: true });
  await expect(page.locator("h1")).toHaveText(ko ? "편집기" : "Editor");
  await expect(page.locator("#code-search")).toHaveCount(0);
  await page.getByRole("button", { name: ko ? "AI AUTH 선택" : "Choose AI AUTH", exact: true }).click();
  await page.getByRole("combobox", { name: ko ? "AI AUTH 계정" : "AI AUTH account", exact: true }).selectOption(chat);
  await expect(assignment(page)).toContainText(ko ? "다시 로그인하지 않고" : "without signing in again");
  await expect(page.getByRole("button", { name: ko ? "에이전트 실행" : "Run agent", exact: true })).toBeDisabled();
  await expect(page.getByRole("button", { name: ko ? "Web VS Code 열기" : "Open Web VS Code", exact: true })).toBeDisabled();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  await page.getByRole("button", { name: ko ? "할당 저장" : "Save assignment", exact: true }).scrollIntoViewIfNeeded();
  if (!ko) await page.screenshot({ path: info.outputPath(`code-auth-states-en-${width}.png`), fullPage: true });
  await page.getByRole("button", { name: ko ? "할당 저장" : "Save assignment", exact: true }).click();
  await expect(assignment(page)).toHaveCount(0); await expect(page.locator("#code-editor-auth")).toContainText(chat);
  expect(state.records[0].revision).toBe(5); expect(state.records[0].spec.agent?.connectionId).toBe(chat);
  expect(state.records[1].spec.agent?.connectionId).toBe(key);
  expect(state.writes).toHaveLength(1); expect(state.writes[0]).toEqual({ method: "PUT", path: `${base}/${id}`, body: { expectedRevision: 4, spec: state.records[0].spec } });
  await page.getByRole("button", { name: ko ? "AI AUTH 선택" : "Choose AI AUTH", exact: true }).click();
  await page.getByRole("combobox", { name: ko ? "에이전트" : "Agent", exact: true }).selectOption("");
  await page.getByRole("button", { name: ko ? "할당 저장" : "Save assignment", exact: true }).click();
  await expect(assignment(page)).toHaveCount(0); await expect(page.locator("#code-editor-auth")).toContainText(ko ? "선택된 에이전트 없음" : "No agent selected");
  expect(state.accounts).toHaveLength(3); expect(state.records[0].spec.agent).toBeNull();
  expect(state.writes.every(w => w.method === "PUT" && w.path.startsWith(base))).toBe(true); expect(errors).toEqual([]);
});

test("workspace and editor reflect shared disconnection and explicit recovery @dashboard", async ({ page }) => {
  const { state, errors } = await open(page); await page.clock.install();
  await page.getByRole("link", { name: "Editor status", exact: true }).click();
  await page.getByRole("button", { name: "Manage AI AUTH", exact: true }).click();
  await page.getByRole("row").filter({ hasText: "Shared OpenAI" }).getByRole("button", { name: "Disconnect", exact: true }).click();
  await page.getByRole("button", { name: "Back to editor", exact: true }).click();
  await expect(page.locator("#code-editor-auth")).toContainText("Account missing or disconnected");
  await expect(page.getByRole("button", { name: "Run agent", exact: true })).toBeDisabled();
  expect(state.records[0].spec.agent?.connectionId).toBe(key); expect(state.records[1].spec.agent?.connectionId).toBe(key);
  await choose(page); await page.getByRole("combobox", { name: "AI AUTH account", exact: true }).selectOption(chat); await save(page).click();
  await expect(assignment(page)).toHaveCount(0);
  await page.getByRole("link", { name: "Back to workspace", exact: true }).click();
  await expect(page.locator("#code-details")).toContainText(chat); await expect(page.locator("#code-details")).toContainText("Account present · access not verified");
  await page.goto(`/#/code/${other}/editor`); await expect(page.locator("#code-editor-auth")).toContainText("Account missing or disconnected");
  expect(state.writes.filter(w => w.path.startsWith(base))).toHaveLength(1); expect(errors).toEqual([]);
});

test("approval cancellation and reconnection return without automatic assignment @dashboard", async ({ page }) => {
  const { state, errors } = await open(page, { editor: true }); await page.clock.install(); state.failureReason = "consent_required";
  await page.getByRole("button", { name: "Check saved connections", exact: true }).click(); await expect(page.locator("#code-editor-auth")).toContainText("Approval required");
  await page.getByRole("button", { name: "Manage AI AUTH", exact: true }).click();
  const renew = () => page.getByRole("row").filter({ hasText: "Personal ChatGPT" }).getByRole("button", { name: "Sign in again", exact: true }).click();
  await renew(); await page.getByRole("button", { name: "Continue with ChatGPT", exact: true }).click();
  await expect(page.locator("#content")).toContainText("Waiting for sign-in"); await expect(page.getByRole("button", { name: "Back to editor", exact: true })).toBeDisabled();
  await page.getByRole("button", { name: "Cancel sign-in", exact: true }).click(); await expect(page.locator("#content")).toContainText("Sign-in cancelled");
  await renew(); await page.getByRole("button", { name: "Continue with ChatGPT", exact: true }).click(); state.approved = true; await page.clock.fastForward(1600);
  await expect(page.locator("#content")).toContainText("ChatGPT account connected"); await page.getByRole("button", { name: "Back to editor", exact: true }).click();
  await expect(page.locator("#code-editor-auth")).toContainText(key); expect(state.records[0].spec.agent?.connectionId).toBe(key);
  expect(state.writes.filter(w => w.path.startsWith(base))).toHaveLength(1); expect(state.accounts.some(a => a.id === chat)).toBe(true); expect(errors).toEqual([]);
});

test("compatible choices missing selections and metadata outages keep explicit intent @dashboard", async ({ page }) => {
  const { state, errors } = await open(page, { missing: true }); await choose(page);
  await expect(assignment(page)).toContainText("Assigned account unavailable"); await expect(save(page)).toBeDisabled();
  const agent = page.getByRole("combobox", { name: "Agent", exact: true }), accounts = page.getByRole("combobox", { name: "AI AUTH account", exact: true });
  await agent.selectOption("claude_code"); await expect(accounts.locator("option")).toHaveText(["Select a compatible account", "Team Anthropic · Anthropic API key"]);
  await accounts.selectOption(claude); state.accountsUnavailable = true; await page.getByRole("button", { name: "Reload connections", exact: true }).click();
  await expect(assignment(page)).toContainText("could not be loaded"); await expect(save(page)).toBeDisabled(); await expect(accounts).toHaveValue(claude);
  state.accountsUnavailable = false; await page.getByRole("button", { name: "Reload connections", exact: true }).click(); await expect(save(page)).toBeEnabled();
  await save(page).click(); await expect(assignment(page)).toHaveCount(0); expect(state.records[0].spec.agent).toEqual({ agent: "claude_code", connectionId: claude }); expect(errors).toEqual([]);
});

test("concurrent configuration edits cannot be overwritten @dashboard", async ({ page }) => {
  const { state, errors } = await open(page); await page.clock.install(); await choose(page);
  await page.getByRole("combobox", { name: "AI AUTH account", exact: true }).selectOption(chat); state.records[0].revision++; state.records[0].spec.github.branch = "external-change";
  await page.clock.fastForward(3100); await expect(assignment(page)).toContainText("Workspace settings changed"); await expect(save(page)).toBeDisabled();
  await page.getByRole("button", { name: "Cancel assignment", exact: true }).click(); await expect(page.locator("#code-details")).toContainText("external-change"); expect(state.writes).toHaveLength(0); expect(errors).toEqual([]);
});

for (const failure of ["committed", "not_saved"] as const) test(`interrupted ${failure} save reconciles the original revision and request @dashboard`, async ({ page }) => {
  const { state, errors } = await open(page); await choose(page);
  await page.getByRole("combobox", { name: "AI AUTH account", exact: true }).selectOption(chat); state.failure = failure; await save(page).click();
  if (failure === "not_saved") {
    await expect(assignment(page)).toContainText("save result is unknown"); await expect(page.getByRole("combobox", { name: "AI AUTH account", exact: true })).toBeDisabled();
    await page.getByRole("button", { name: "Check saved assignment", exact: true }).click();
  }
  await expect(assignment(page)).toHaveCount(0); expect(state.records[0].revision).toBe(5); expect(state.records[0].spec.agent?.connectionId).toBe(chat);
  const writes = state.writes.filter(w => w.method === "PUT"); expect(writes).toHaveLength(failure === "committed" ? 1 : 2);
  expect(writes.every(w => JSON.stringify(w.body) === JSON.stringify(writes[0].body))).toBe(true); expect(errors).toEqual([]);
});

test("late account metadata cannot enter a reopened assignment form @dashboard", async ({ page }) => {
  const { state, errors } = await open(page); let release!: () => void; const held = new Promise<void>(r => { release = r; }); state.delayAccounts = () => held;
  await choose(page); await expect(assignment(page)).toContainText("Loading accounts"); await page.getByRole("button", { name: "Cancel assignment", exact: true }).click();
  state.accounts = state.accounts.filter(a => a.id !== chat); await choose(page);
  await expect(page.getByRole("combobox", { name: "AI AUTH account", exact: true })).not.toContainText("Personal ChatGPT");
  const response = page.waitForResponse(r => new URL(r.url()).pathname === "/api/pipe/ai/connections"); release(); await (await response).finished();
  await page.evaluate(() => new Promise(requestAnimationFrame));
  await expect(page.getByRole("combobox", { name: "AI AUTH account", exact: true })).not.toContainText("Personal ChatGPT"); expect(errors).toEqual([]);
});

test("late workspace observations cannot restore the old saved assignment @dashboard", async ({ page }) => {
  const { state, errors } = await open(page); await page.clock.install(); await choose(page);
  await page.getByRole("combobox", { name: "AI AUTH account", exact: true }).selectOption(chat);
  let release!: () => void; state.delayList = () => new Promise<void>(r => { release = r; }); const count = state.listCalls;
  await page.clock.fastForward(3100); await expect.poll(() => state.listCalls).toBeGreaterThan(count); await save(page).click();
  await expect(assignment(page)).toHaveCount(0); await expect(page.locator("#code-details")).toContainText(chat);
  const response = page.waitForResponse(r => new URL(r.url()).pathname === base); release(); await (await response).finished();
  await page.evaluate(() => new Promise(requestAnimationFrame));
  await expect(page.locator("#code-details .code-account-state code")).toHaveText(chat); expect(errors).toEqual([]);
});

test("rejected assignments and cancelled selection keep saved references @dashboard", async ({ page }) => {
  const { state, errors } = await open(page); await choose(page);
  await page.getByRole("combobox", { name: "AI AUTH account", exact: true }).selectOption(chat); state.rejectSave = true; await save(page).click(); await expect(assignment(page)).toContainText("Assignment was rejected");
  await page.getByRole("button", { name: "Cancel assignment", exact: true }).click(); await expect(page.locator("#code-details .code-account-state code")).toHaveText(key);
  expect(state.records[0].revision).toBe(4); expect(state.accounts).toHaveLength(3); expect(errors).toEqual([]);
});

test("assignment survives account management and retains its leave guard @dashboard", async ({ page }) => {
  const { state, errors } = await open(page, { editor: true }); await choose(page);
  await page.getByRole("combobox", { name: "AI AUTH account", exact: true }).selectOption(chat);
  await page.getByRole("button", { name: "Manage AI AUTH", exact: true }).click();
  await page.getByRole("button", { name: "Add connection", exact: true }).click(); await page.getByRole("button", { name: "Cancel", exact: true }).click();
  await page.getByRole("button", { name: "Back to editor", exact: true }).click();
  await expect(page.getByRole("combobox", { name: "AI AUTH account", exact: true })).toHaveValue(chat);
  let guarded = false; page.once("dialog", async d => { guarded = true; await d.dismiss(); });
  await page.getByRole("link", { name: "Back to workspace", exact: true }).click();
  await expect.poll(() => guarded).toBe(true); await expect(page).toHaveURL(new RegExp(`${id}/editor$`));
  expect(state.writes).toHaveLength(0); expect(errors).toEqual([]);
});

test("an uncertain save never overwrites a newer configuration during recovery @dashboard", async ({ page }) => {
  const { state, errors } = await open(page); await choose(page);
  await page.getByRole("combobox", { name: "AI AUTH account", exact: true }).selectOption(chat); state.failure = "not_saved"; await save(page).click();
  await expect(assignment(page)).toContainText("save result is unknown"); state.records[0].revision += 2; state.records[0].spec.name = "newer-workspace";
  await page.getByRole("button", { name: "Check saved assignment", exact: true }).click();
  await expect(assignment(page)).toContainText("Workspace settings changed"); await expect(save(page)).toBeDisabled();
  expect(state.writes.filter(w => w.method === "PUT")).toHaveLength(1); expect(state.records[0].spec.name).toBe("newer-workspace"); expect(errors).toEqual([]);
});

test("incompatible replacement is visible in the editor and removing an assignment needs no account metadata @dashboard", async ({ page }) => {
  const { state, errors } = await open(page, { editor: true }); await page.clock.install();
  state.accounts.find(a => a.id === key)!.provider = "anthropic"; await page.clock.fastForward(3100);
  await expect(page.locator("#code-editor-auth")).toContainText("Account is incompatible with the selected agent");
  state.accountsUnavailable = true; await choose(page); await expect(assignment(page)).toContainText("could not be loaded");
  await page.getByRole("combobox", { name: "Agent", exact: true }).selectOption(""); await expect(save(page)).toBeEnabled(); await save(page).click();
  await expect(assignment(page)).toHaveCount(0); expect(state.records[0].spec.agent).toBeNull(); expect(state.accounts).toHaveLength(3); expect(errors).toEqual([]);
});

test("list reordering keeps the assignment and subsequent check on the chosen workspace @dashboard", async ({ page }) => {
  const { state, errors } = await open(page); await page.goto("/#/code"); await page.clock.install();
  await expect(page.locator("#code-details h2")).toHaveText("web-app"); await choose(page);
  await page.getByRole("combobox", { name: "AI AUTH account", exact: true }).selectOption(chat);
  state.records.reverse(); await page.clock.fastForward(3100); await expect(save(page)).toBeEnabled(); await save(page).click();
  await expect(assignment(page)).toHaveCount(0); await expect(page.locator("#code-details h2")).toHaveText("web-app");
  await page.getByRole("button", { name: "Check saved connections", exact: true }).click();
  await expect(page.locator(".code-credential-result")).toContainText("Provider credentials verified");
  expect(state.records.find(w => w.id === id)!.spec.agent?.connectionId).toBe(chat);
  expect(state.records.find(w => w.id === other)!.spec.agent?.connectionId).toBe(key); expect(errors).toEqual([]);
});
