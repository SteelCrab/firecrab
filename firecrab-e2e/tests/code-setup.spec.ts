import { expect, test, type Page } from "@playwright/test";

const key = "11111111-1111-4111-8111-111111111111", chat = "22222222-2222-4222-8222-222222222222", anthropic = "33333333-3333-4333-8333-333333333333";
const repo = { owner: "team", repository: "web-app", defaultBranch: "main", private: true };
const worker = { ...repo, repository: "worker" };
type Workspace = { id: string; revision: number; spec: any; connections: { github: string; ai: string }; createdAtMs: number; updatedAtMs: number };
async function open(page: Page, locale: "en" | "ko" = "en", options: { missing?: boolean; paged?: boolean; failure?: "committed" | "not_saved" } = {}) {
  const state = { github: options.missing ? "" : "octocat", accounts: [
    { id: key, name: "OpenAI team", provider: "openai", authentication: "api_key" },
    { id: chat, name: "Personal ChatGPT", provider: "openai", authentication: "chatgpt" },
    { id: anthropic, name: "Anthropic team", provider: "anthropic", authentication: "api_key" },
  ], records: [] as Workspace[], writes: [] as { id: string; body: any }[], failure: options.failure, rejected: false,
    delay: null as null | (() => Promise<void>), branchRequests: [] as string[], mutations: [] as string[], errors: [] as string[] };
  page.on("pageerror", e => state.errors.push(e.message));
  await page.addInitScript(language => localStorage.setItem("firecrab.locale", language), locale);
  await page.route(/^https?:\/\/[^/]+\/api\//, async route => {
    const request = route.request(), url = new URL(request.url()), path = url.pathname, method = request.method();
    const send = (json: unknown, status = 200) => route.fulfill({ json, status });
    if (method !== "GET") state.mutations.push(`${method} ${path}`);
    if (path === "/api/pipe/auth") return send(state.github ? [{ provider: "github", account: state.github }] : []);
    if (path === "/api/pipe/ai/connections") return send(state.accounts);
    if (path === "/api/code/github/repositories") return send({ account: state.github, repositories: options.paged ? [url.searchParams.get("page") === "2" ? worker : repo] : [repo, worker], nextPage: options.paged && url.searchParams.get("page") !== "2" ? 2 : null });
    if (path === "/api/code/github/branches") {
      const name = url.searchParams.get("repository")!; state.branchRequests.push(name);
      if (name === "web-app" && state.delay) await state.delay();
      return send({ account: state.github, owner: "team", repository: name, branches: options.paged ? [url.searchParams.get("page") === "2" ? "feature/login" : "main"] : name === "worker" ? ["worker-main"] : ["main", "feature/login"], nextPage: options.paged && url.searchParams.get("page") !== "2" ? 2 : null });
    }
    if (path === "/api/code/workspaces") return send(state.records);
    const match = path.match(/^\/api\/code\/workspaces\/([^/]+)(\/runtime)?$/);
    if (match) {
      const id = match[1];
      if (method === "PUT") {
        const body = request.postDataJSON(); state.writes.push({ id, body });
        if (state.rejected) return send({ error: { code: "validation_failed", fields: { "spec.agent.connectionId": "account disconnected" } } }, 400);
        if (state.records.some(r => r.id === id)) return send({ error: { code: "code_workspace_revision_conflict" } }, 409);
        if (state.failure !== "not_saved") state.records.push({ id, revision: 1, spec: body.spec, createdAtMs: 1, updatedAtMs: 1, connections: { github: "present_unverified", ai: body.spec.agent ? "present_unverified" : "not_required" } });
        if (state.failure) { state.failure = undefined; return route.abort("failed"); }
        return send(state.records.at(-1));
      }
      if (match[2]) return send({ error: { code: "not_found" } }, 404);
      const record = state.records.find(r => r.id === id);
      return record ? send(record) : send({ error: { code: "not_found" } }, 404);
    }
    if (path === "/api/update") return send({ current: "0.4.1", latest: "0.4.1", updateAvailable: false });
    if (path === "/api/vms") return send([]);
    return send({ error: { code: "not_found" } }, 404);
  });
  await page.goto("/#/code/new"); return state;
}
async function choose(page: Page, locale: "en" | "ko" = "en") {
  await page.getByRole("textbox", { name: locale === "en" ? "Workspace name" : "워크스페이스 이름", exact: true }).fill("web-app");
  await page.getByRole("combobox", { name: locale === "en" ? "GitHub repository" : "GitHub 저장소", exact: true }).selectOption("team/web-app");
  await expect(page.getByRole("combobox", { name: locale === "en" ? "Branch" : "브랜치", exact: true })).toHaveValue("main");
}
for (const locale of ["en", "ko"] as const) for (const width of [1280, 390]) test(`${locale} one CODE setup flow at ${width}px @dashboard`, async ({ page }, info) => {
  await page.setViewportSize({ width, height: 1000 }); const state = await open(page, locale); const ko = locale === "ko";
  await choose(page, locale);
  await page.getByRole("combobox", { name: ko ? "브랜치" : "Branch", exact: true }).selectOption("feature/login");
  await page.getByRole("combobox", { name: ko ? "에이전트" : "Agent", exact: true }).selectOption("codex");
  await page.getByRole("combobox", { name: ko ? "AI AUTH 계정" : "AI AUTH account", exact: true }).selectOption(chat);
  await page.getByRole("combobox", { name: ko ? "개발 언어" : "Runtime", exact: true }).selectOption("python");
  await page.getByRole("spinbutton", { name: "vCPU", exact: true }).fill("4");
  await page.getByRole("combobox", { name: ko ? "메모리" : "Memory", exact: true }).selectOption("8192");
  await page.getByRole("spinbutton", { name: ko ? "디스크 (GiB)" : "Disk (GiB)", exact: true }).fill("32");
  const create = page.getByRole("button", { name: ko ? "워크스페이스 생성" : "Create workspace", exact: true });
  await expect(create).toBeEnabled(); await create.scrollIntoViewIfNeeded();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  if (!ko) await page.screenshot({ path: info.outputPath(`code-setup-en-${width}.png`), fullPage: true });
  await create.click(); await expect(page).toHaveURL(/#\/code\/[0-9a-f-]{36}$/);
  await expect(page.locator("#code-details h2")).toHaveText("web-app");
  await expect(page.locator("#code-details")).toContainText(ko ? "MicroVM 생성 전" : "MicroVM not provisioned");
  expect(state.writes).toHaveLength(1);
  expect(state.writes[0].body).toEqual({ expectedRevision: 0, spec: { name: "web-app", github: { account: "octocat", owner: "team", repository: "web-app", branch: "feature/login" }, runtime: "python", resources: { cpu: 4, ram: 8192, diskGb: 32 }, agent: { agent: "codex", connectionId: chat } } });
  expect(state.mutations.every(call => /^PUT \/api\/code\/workspaces\/[0-9a-f-]{36}$/.test(call))).toBe(true);
  expect(JSON.stringify(state.writes)).not.toMatch(/apiKey|accessToken|vmId|command|endpoint/); expect(state.errors).toEqual([]);
});
test("account compatibility matches each agent and explicit no-agent creation @dashboard", async ({ page }) => {
  const state = await open(page); await choose(page);
  const agent = page.getByRole("combobox", { name: "Agent", exact: true }); await agent.selectOption("claude_code");
  const account = page.getByRole("combobox", { name: "AI AUTH account", exact: true });
  await expect(account.locator("option")).toHaveText(["Select a compatible account", "Anthropic team · Anthropic API key"]);
  await agent.selectOption("opencode"); await expect(account).not.toContainText("Personal ChatGPT"); await expect(account.locator("option")).toHaveCount(3);
  state.accounts = state.accounts.filter(c => c.id === chat);
  await page.getByRole("button", { name: "Reload connections", exact: true }).click();
  await expect(page.getByRole("button", { name: "Create workspace", exact: true })).toBeDisabled();
  await expect(page.locator("#content")).toContainText("No compatible account");
  await agent.selectOption(""); await expect(account).toHaveCount(0);
  await page.getByRole("button", { name: "Create workspace", exact: true }).click(); await expect(page.locator("#code-details h2")).toHaveText("web-app");
  expect(state.writes[0].body.spec.agent).toBeNull(); expect(state.errors).toEqual([]);
});
test("missing GitHub, pagination and changed connections preserve entered settings @dashboard", async ({ page }) => {
  const state = await open(page, "en", { missing: true, paged: true });
  await page.getByRole("textbox", { name: "Workspace name", exact: true }).fill("retained-name");
  await expect(page.getByRole("button", { name: "Create workspace", exact: true })).toBeDisabled(); state.github = "octocat";
  await page.getByRole("button", { name: "Reload connections", exact: true }).click();
  await page.getByRole("combobox", { name: "GitHub repository", exact: true }).selectOption("team/web-app");
  await expect(page.getByRole("combobox", { name: "Branch", exact: true })).toHaveValue("main");
  await page.getByRole("button", { name: "Load more branches", exact: true }).click(); await page.getByRole("combobox", { name: "Branch", exact: true }).selectOption("feature/login");
  await page.getByRole("button", { name: "Load more repositories", exact: true }).click();
  await expect(page.getByRole("combobox", { name: "GitHub repository", exact: true }).locator("option")).toHaveCount(3);
  await expect(page.getByRole("combobox", { name: "Branch", exact: true })).toHaveValue("feature/login"); expect(state.branchRequests).toHaveLength(2);
  state.github = "other-login"; await page.getByRole("button", { name: "Reload connections", exact: true }).click();
  await expect(page.getByRole("combobox", { name: "GitHub repository", exact: true })).toHaveValue("");
  await expect(page.getByRole("textbox", { name: "Workspace name", exact: true })).toHaveValue("retained-name");
  await expect(page.getByRole("button", { name: "Create workspace", exact: true })).toBeDisabled(); expect(state.errors).toEqual([]);
});
test("late branches cannot retarget the repository selected next @dashboard", async ({ page }) => {
  const state = await open(page); let release!: () => void; const waiting = new Promise<void>(r => { release = r; }); state.delay = () => waiting;
  await page.getByRole("textbox", { name: "Workspace name", exact: true }).fill("worker");
  const repos = page.getByRole("combobox", { name: "GitHub repository", exact: true });
  await repos.selectOption("team/web-app"); await expect.poll(() => state.branchRequests.length).toBe(1); await repos.selectOption("team/worker");
  await expect(page.getByRole("combobox", { name: "Branch", exact: true }).locator("option")).toHaveText(["Select a branch", "worker-main"]); release();
  await expect(page.getByRole("combobox", { name: "Branch", exact: true })).not.toContainText("feature/login"); await expect(repos).toHaveValue("team/worker"); expect(state.errors).toEqual([]);
});
for (const failure of ["committed", "not_saved"] as const) test(`unknown save (${failure}) reuses the original UUID and body across reload @dashboard`, async ({ page }) => {
  const state = await open(page, "en", { failure }); await choose(page); await page.getByRole("button", { name: "Create workspace", exact: true }).click();
  await expect(page.getByRole("button", { name: "Check saved workspace", exact: true })).toBeEnabled();
  await expect(page.getByRole("textbox", { name: "Workspace name", exact: true })).toBeDisabled(); const first = structuredClone(state.writes[0]);
  page.once("dialog", d => d.accept()); await page.reload();
  await expect(page.getByRole("button", { name: "Check saved workspace", exact: true })).toBeEnabled();
  if (failure === "not_saved") await expect(page.locator("#api-status")).toHaveText("Live");
  await page.getByRole("button", { name: "Check saved workspace", exact: true }).click(); await expect(page.locator("#code-details h2")).toHaveText("web-app");
  expect(state.writes).toHaveLength(failure === "committed" ? 1 : 2); expect(state.writes.every(write => JSON.stringify(write) === JSON.stringify(first))).toBe(true); expect(state.records).toHaveLength(1);
  expect(await page.evaluate(() => sessionStorage.getItem("firecrab.code.create.v1"))).toBeNull(); expect(state.errors).toEqual([]);
});
test("a rejected save keeps fields editable and never claims a workspace was created @dashboard", async ({ page }) => {
  const state = await open(page); await choose(page); state.rejected = true;
  await page.getByRole("button", { name: "Create workspace", exact: true }).click(); await expect(page.locator("#code-setup-error")).toContainText("Check the workspace fields");
  await expect(page.getByRole("textbox", { name: "Workspace name", exact: true })).toBeEnabled(); expect(state.records).toHaveLength(0);
  expect(await page.evaluate(() => sessionStorage.getItem("firecrab.code.create.v1"))).toBeNull(); expect(state.errors).toEqual([]);
});

test("an externally changed saved workspace is reviewed without another write @dashboard", async ({ page }) => {
  const state = await open(page, "en", { failure: "committed" }); await choose(page);
  await page.getByRole("button", { name: "Create workspace", exact: true }).click();
  await expect(page.getByRole("button", { name: "Check saved workspace", exact: true })).toBeEnabled();
  state.records[0].spec.name = "renamed-by-operator"; state.records[0].revision = 2;
  await page.getByRole("button", { name: "Check saved workspace", exact: true }).click();
  await expect(page.locator("#code-setup-error")).toContainText("saved workspace changed");
  await page.getByRole("button", { name: "Review saved workspace", exact: true }).click();
  await expect(page.locator("#code-details h2")).toHaveText("renamed-by-operator");
  expect(state.writes).toHaveLength(1); expect(state.errors).toEqual([]);
});
