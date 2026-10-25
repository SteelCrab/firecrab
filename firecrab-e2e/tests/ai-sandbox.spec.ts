import { expect, test, type Page } from "@playwright/test";
import type { Project, SandboxTask, TaskInput } from "../../firecrab-frontend/src/api/aiSandbox";
const connection = "11111111-1111-4111-8111-111111111111", vm = "33333333-3333-4333-8333-333333333333";
const selection = { connectionId: connection, model: "qa-native-model" };
const project: Project = { id: "22222222-2222-4222-8222-222222222222", spec: { name: "Firecrab Dashboard", sourcePath: "/workspaces/firecrab-demo", vmId: vm, selection, instructions: "Keep the interface simple." }, createdAt: 1 };
async function open(page: Page, options: { locale?: "en" | "ko"; empty?: boolean; hold?: boolean; failed?: boolean; failSaveOnce?: boolean } = {}) {
  let projects = options.empty ? [] : [structuredClone(project)], tasks: SandboxTask[] = [], failure = options.failed, conflict = false, verified = !options.empty;
  const starts: TaskInput[] = []; const saves: string[] = []; let saveFailed = false;
  await page.route(/^https?:\/\/[^/]+\/api\//, async route => {
    const path = new URL(route.request().url()).pathname, method = route.request().method();
    const respond = (json: unknown, status = 200) => route.fulfill({ json, status });
    if (path === "/api/vms") return respond([{ id: vm, name: "sandbox-dev", state: "running", purpose: "instance", cpu: 1, ram: 512, usageHistory: [], startupTimeline: [], env: {}, shellRefs: [], portForwards: [] }]);
    if (path === "/api/update") return respond({ current: "0.3.1", latest: "0.3.1", updateAvailable: false });
    if (path === "/api/pipe/ai/connections") return respond([{ id: connection, name: "QA connected account", provider: "openai", authentication: "api_key", endpoint: "https://api.openai.com/v1/responses", models: [], streaming: false, toolCalls: false }]);
    if (path.endsWith("/models")) return respond({ discoveryAvailable: true, models: [{ id: selection.model, name: "QA native model", verified, toolsVerified: verified }] });
    if (path.endsWith("/verify")) { verified = true; return respond({ id: selection.model, name: "QA native model", verified: true, toolsVerified: true }); }
    if (path === "/api/ai-sandbox/projects") return failure ? respond({ error: { message: "Sandbox temporarily unavailable" } }, 503) : respond(projects);
    if (/\/projects\/[^/]+$/.test(path)) {
      const id = path.split("/").at(-1)!;
      if (method === "DELETE") { projects = projects.filter(p => p.id !== id); tasks = []; return respond({ deleted: true }); }
      const saved = { id, spec: route.request().postDataJSON(), createdAt: 1 }; saves.push(id); projects = [...projects.filter(p => p.id !== id), saved]; if (options.failSaveOnce && !saveFailed) { saveFailed = true; return route.abort("failed"); } return respond(saved);
    }
    if (/\/projects\/[^/]+\/tasks$/.test(path)) return respond(tasks.map(t => ({ ...t, changes: undefined, events: undefined, changeCount: t.changes.length, eventCount: t.events.length, lastTool: t.events.at(-1)?.name ?? null })));
    if (/\/projects\/[^/]+\/tasks\/[^/]+$/.test(path)) {
      const input = route.request().postDataJSON() as TaskInput; starts.push(input);
      const t: SandboxTask = { id: path.split("/").at(-1)!, projectId: projects[0].id, attemptId: crypto.randomUUID(), request: input, selection: input.selection ?? selection, state: options.hold ? "in_progress" : "result", createdAt: Date.now(), updatedAt: Date.now(), result: options.hold ? "" : input.kind === "code" ? "Added the health endpoint.\nValidation: 4 tests passed." : "Start with a small health endpoint. Then add a regression test.", error: null, errorCode: null, events: input.kind === "code" ? [{ sequence: 0, at: 1, name: "run_command", input: '{"command":"npm test","timeout_seconds":120}', output: "4 tests passed", stderr: "", exitCode: 0 }] : [], changes: input.kind === "code" ? [{ path: "src/health.ts", before: "export const health = () => null;", after: 'export const health = () => ({ status: "ok" });', beforeMode: 420, afterMode: 420, applied: false }] : [] };
      tasks.push(t); return respond(t, 202);
    }
    const task = tasks.find(t => t.id === path.split("/")[4]);
    if (task) {
      if (path.endsWith("/cancel") || path.endsWith("/reconcile")) { task.state = "cancelled"; task.error = path.endsWith("/cancel") ? "Task cancelled by the user" : "Interrupted attempt reconciled"; task.updatedAt++; }
      if (path.endsWith("/apply")) {
        if (conflict) return respond({ error: { message: "Source file changed; review the conflict" } }, 409);
        for (const c of task.changes) c.applied = true; task.state = "merged"; task.updatedAt++;
      }
      return respond(task);
    }
    return route.fulfill({ status: 404 });
  });
  await page.addInitScript(locale => localStorage.setItem("firecrab.locale", locale), options.locale ?? "en"); await page.goto("/#/ai-sandbox");
  return { starts, saves, recover() { failure = false; }, conflict(value: boolean) { conflict = value; }, interrupt() { const task = tasks.at(-1)!; task.state = "interrupted"; task.error = "Guest outcome needs reconciliation"; task.updatedAt++; } };
}

test.describe("AI Sandbox @dashboard", () => {
  for (const locale of ["en", "ko"] as const) for (const width of [1280, 390]) test(`${locale} conversation, code review and apply at ${width}px`, async ({ page }, info) => {
    await page.setViewportSize({ width, height: 900 }); const mock = await open(page, { locale }), ko = locale === "ko";
    const box = page.getByRole("textbox", { name: ko ? "대화 또는 작업 내용" : "Message or task", exact: true });
    await box.fill("Plan a small health endpoint"); await page.getByRole("button", { name: ko ? "메시지 전송" : "Send message", exact: true }).click();
    await expect(page.getByText("Start with a small health endpoint.", { exact: false })).toBeVisible(); await page.reload(); await expect(page.getByText("Plan a small health endpoint", { exact: true })).toBeVisible();
    await page.screenshot({ path: info.outputPath(`sandbox-conversation-${locale}-${width}.png`), fullPage: true });
    await page.getByRole("combobox", { name: ko ? "실행 모드" : "Execution mode", exact: true }).selectOption("code");
    await box.fill("Add a health check endpoint and cover it with tests"); await page.getByRole("button", { name: ko ? "코드 작업 실행" : "Run code task", exact: true }).click();
    const board = page.locator(".sb-board"); await expect(board.getByText("Add a health check endpoint and cover it with tests")).toBeVisible();
    await page.screenshot({ path: info.outputPath(`sandbox-board-${locale}-${width}.png`), fullPage: true });
    await board.getByRole("button").click(); const dialog = page.getByRole("dialog", { name: ko ? "작업 결과" : "Task result", exact: true });
    await expect(dialog.getByText("src/health.ts", { exact: true })).toBeVisible(); await expect(dialog.getByText('export const health = () => ({ status: "ok" });', { exact: true })).toBeVisible();
    await page.screenshot({ path: info.outputPath(`sandbox-review-${locale}-${width}.png`), fullPage: true });
    mock.conflict(true); await dialog.getByRole("button", { name: ko ? "선택한 파일 반영" : "Apply selected files", exact: true }).click(); await expect(dialog.getByRole("alert")).toContainText("Source file changed");
    mock.conflict(false); await dialog.getByRole("button", { name: ko ? "선택한 파일 반영" : "Apply selected files", exact: true }).click(); await expect(dialog.getByText(ko ? "반영됨" : "Applied", { exact: true })).toBeVisible();
    await dialog.getByRole("tab", { name: ko ? /실행 기록/ : /Activity/ }).click(); await expect(dialog.getByText("4 tests passed", { exact: true })).toBeVisible(); await expect(dialog.getByText(ko ? "종료 코드 0" : "exit 0", { exact: true })).toBeVisible();
    await dialog.getByRole("button", { name: ko ? "닫기" : "Close", exact: true }).click(); expect(mock.starts.map(s => s.kind)).toEqual(["chat", "code"]);
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  });
  test("creates a project using verified models without credentials", async ({ page }) => {
    await open(page, { empty: true }); await page.getByRole("button", { name: "Create a project", exact: true }).click(); const dialog = page.getByRole("dialog");
    await dialog.getByRole("textbox", { name: "Project name", exact: true }).fill("New workspace"); await dialog.getByRole("combobox", { name: "Model ID", exact: true }).fill(selection.model);
    await dialog.getByRole("button", { name: "Verify access", exact: true }).click(); await expect(dialog.getByText("Conversation + code tasks", { exact: true })).toBeVisible();
    await expect(dialog.locator('input[type="password"]')).toHaveCount(0); await dialog.getByRole("button", { name: "Save project", exact: true }).click(); await expect(page.getByRole("heading", { name: "New workspace", exact: true })).toBeVisible();
  });
  test("cancels, creates a distinct retry and reconciles interruption", async ({ page }) => {
    const mock = await open(page, { hold: true }); await page.getByRole("textbox", { name: "Message or task", exact: true }).fill("Long analysis"); await page.getByRole("button", { name: "Send message", exact: true }).click(); await page.getByRole("button", { name: "Stop task", exact: true }).click();
    await expect(page.getByRole("button", { name: "Retry as a new attempt", exact: true })).toBeEnabled(); await page.getByRole("button", { name: "Retry as a new attempt", exact: true }).click();
    expect(mock.starts[1].retryOf).toBeTruthy(); mock.interrupt(); await expect(page.getByRole("button", { name: "Reconcile attempt", exact: true })).toBeVisible(); await page.getByRole("button", { name: "Reconcile attempt", exact: true }).click(); await expect(page.getByText("Interrupted attempt reconciled", { exact: true })).toBeVisible();
  });
  test("recovers a failed list without displaying a false empty state", async ({ page }) => {
    const mock = await open(page, { failed: true }); await expect(page.getByRole("alert")).toContainText("Sandbox temporarily unavailable"); await expect(page.getByRole("button", { name: "Create a project", exact: true })).toHaveCount(0);
    mock.recover(); await page.getByRole("button", { name: "Retry", exact: true }).click(); await expect(page.getByRole("heading", { name: "Firecrab Dashboard", exact: true })).toBeVisible();
  });
  test("tabs support arrow keys and settings restore keyboard focus", async ({ page }) => {
    await open(page); const conversation = page.getByRole("tab", { name: "Conversation", exact: true });
    await conversation.focus(); await page.keyboard.press("ArrowRight"); await expect(page.getByRole("tab", { name: /Tasks/ })).toHaveAttribute("aria-selected", "true");
    await page.keyboard.press("Home"); await expect(conversation).toHaveAttribute("aria-selected", "true");
    const settings = page.getByRole("button", { name: "Project settings", exact: true }); await settings.click(); await expect(page.getByRole("dialog")).toBeVisible(); await page.keyboard.press("Escape"); await expect(page.getByRole("dialog")).toHaveCount(0); await expect(settings).toBeFocused();
  });

  test("an uncertain project save retries the same identity", async ({ page }) => {
    const mock = await open(page, { empty: true, failSaveOnce: true }); await page.getByRole("button", { name: "Create a project", exact: true }).click(); const dialog = page.getByRole("dialog");
    await dialog.getByRole("textbox", { name: "Project name", exact: true }).fill("Stable project"); await dialog.getByRole("button", { name: "Save project", exact: true }).click();
    await expect(dialog.getByRole("alert")).toBeVisible(); await dialog.getByRole("button", { name: "Save project", exact: true }).click(); await expect(dialog).toHaveCount(0);
    expect(mock.saves).toHaveLength(2); expect(mock.saves[0]).toBe(mock.saves[1]);
  });

});
