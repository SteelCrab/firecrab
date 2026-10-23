import { expect, test, type Page } from "@playwright/test";
import type { AiConnection, AiConnectionInput } from "../../firecrab-frontend/src/api/pipeAi";

const id = "11111111-1111-4111-8111-111111111111";
const loginId = "33333333-3333-4333-8333-333333333333";
const otherId = "44444444-4444-4444-8444-444444444444";
const saved: AiConnection = { id, name: "Personal AI", provider: "openai", authentication: "api_key", endpoint: "https://api.openai.com/v1/responses", models: ["server-only-model"], streaming: false, toolCalls: false };

async function open(page: Page, options: { locale?: "en" | "ko"; connected?: boolean; loginFailure?: boolean; extraConnection?: boolean; connectionsUnavailable?: boolean } = {}) {
  let connections: AiConnection[] = options.connected ? [{ ...saved }] : [];
  if (options.extraConnection) connections.push({ ...saved, id: otherId, name: "Other account" });
  let connectionsUnavailable = options.connectionsUnavailable ?? false;
  const writes: AiConnectionInput[] = [];
  const unexpectedAiRequests: string[] = [];
  let login: { id: string; name: string } | null = null;
  await page.context().route("https://auth.openai.com/**", route => route.fulfill({ body: "Mock consent page" }));
  await page.route(/^https?:\/\/[^/]+\/api\//, async route => {
    const path = new URL(route.request().url()).pathname;
    const method = route.request().method();
    if (path === "/api/vms") return route.fulfill({ json: [] });
    if (path === "/api/update") return route.fulfill({ json: { current: "0.3.1", latest: "0.3.1", updateAvailable: false } });
    if (path === "/api/pipe") return route.fulfill({ status: 503, json: { error: { message: "Ingress helper unavailable" } } });
    if (path === "/api/pipe/ai/connections") {
      if (connectionsUnavailable) return route.fulfill({ status: 503, json: { error: { message: "Connections unavailable" } } });
      return route.fulfill({ json: connections });
    }
    if (path.endsWith("/login")) {
      login = { ...route.request().postDataJSON(), id: path.split("/")[5] };
      return route.fulfill({ json: { loginId, authorizationUrl: "https://auth.openai.com/api/accounts/authorize?state=test", expiresIn: 600 } });
    }
    if (path === `/api/pipe/ai/logins/${loginId}`) {
      if (options.loginFailure) return route.fulfill({ json: { status: "failed", message: "ChatGPT plan usage permission was not granted" } });
      const connection: AiConnection = { ...saved, id: login!.id, name: login!.name, authentication: "chatgpt", models: [] };
      connections = [...connections.filter(value => value.id !== connection.id), connection];
      return route.fulfill({ json: { status: "connected", message: null } });
    }
    if (/\/connections\/[^/]+$/.test(path)) {
      const connectionId = path.split("/").at(-1)!;
      if (method === "DELETE") {
        connections = connections.filter(value => value.id !== connectionId);
        return route.fulfill({ json: { deleted: true, remoteRevocationConfirmed: null } });
      }
      const input = route.request().postDataJSON() as AiConnectionInput;
      writes.push(input);
      if (input.apiKey === "bad-key") return route.fulfill({ status: 401, json: { error: { code: "ai_authentication_expired", message: "Provider authentication was rejected" } } });
      const connection: AiConnection = { ...saved, id: connectionId, name: input.name, models: [] };
      connections = [...connections.filter(value => value.id !== connectionId), connection];
      return route.fulfill({ json: connection });
    }
    if (path.startsWith("/api/pipe/ai/")) unexpectedAiRequests.push(path);
    return route.fulfill({ status: 404 });
  });
  await page.addInitScript(locale => localStorage.setItem("firecrab.locale", locale), options.locale ?? "en");
  await page.goto("/#/pipe/ai");
  return { writes, unexpectedAiRequests, recoverConnections() { connectionsUnavailable = false; }, get login() { return login; } };
}

test.describe("Pipe AI accounts @dashboard", () => {
  test("saves a masked API key without a model or browser persistence", async ({ page }) => {
    const mock = await open(page);
    const panel = page.getByRole("region", { name: "Pipe AI", exact: true });
    await panel.getByRole("button", { name: "Use API key", exact: true }).click();
    await panel.getByLabel("Connection name", { exact: true }).fill("API account");
    const key = panel.getByLabel("OpenAI API key", { exact: true });
    await expect(key).toHaveAttribute("type", "password");
    await expect(key).toHaveAttribute("autocomplete", "off");
    await key.fill("test-private-key");
    await expect(panel.getByRole("textbox")).toHaveCount(2);
    await panel.getByRole("button", { name: "Verify and save", exact: true }).click();
    await expect(panel.locator(".pipe-ai-connections strong")).toHaveText("API account");
    expect(mock.writes[0]).toEqual({ name: "API account", provider: "openai", apiKey: "test-private-key" });
    await expect(panel).not.toContainText("test-private-key");
    await expect(page.getByText("Ingress helper unavailable", { exact: true })).toHaveCount(0);
    expect(await page.evaluate(() => JSON.stringify({ local: { ...localStorage }, session: { ...sessionStorage } }))).not.toContain("test-private-key");
    await page.reload();
    await expect(panel.locator(".pipe-ai-connections strong")).toHaveText("API account");
    expect(mock.unexpectedAiRequests).toEqual([]);
  });

  test("preserves a connection after a failed credential replacement", async ({ page }) => {
    await open(page, { connected: true });
    const panel = page.getByRole("region", { name: "Pipe AI", exact: true });
    await panel.getByRole("button", { name: "Replace API key", exact: true }).click();
    await panel.getByLabel("OpenAI API key", { exact: true }).fill("bad-key");
    await panel.getByRole("button", { name: "Verify and save", exact: true }).click();
    await expect(panel.getByRole("alert")).toContainText("Provider authentication was rejected");
    await expect(panel.locator(".pipe-ai-connections strong")).toHaveText("Personal AI");
    await expect(panel.getByRole("button", { name: "Disconnect", exact: true })).toBeDisabled();
    await panel.getByRole("button", { name: "Cancel", exact: true }).click();
    await expect(panel.getByRole("button", { name: "Disconnect", exact: true })).toBeEnabled();
  });

  test("shows account management without model, project, conversation, or task controls", async ({ page }) => {
    const mock = await open(page, { connected: true });
    const panel = page.getByRole("region", { name: "Pipe AI", exact: true });
    await expect(panel.locator(".pipe-ai-connections strong")).toHaveText("Personal AI");
    await expect(panel.getByText("Connected", { exact: true })).toBeVisible();
    await expect(panel.getByRole("combobox")).toHaveCount(0);
    await expect(panel.getByRole("textbox")).toHaveCount(0);
    await expect(panel.locator("textarea")).toHaveCount(0);
    await expect(panel).not.toContainText("server-only-model");
    await expect(panel.getByRole("button", { name: /Send|Save project default|Verify inference/ })).toHaveCount(0);
    await panel.getByRole("button", { name: "Add connection", exact: true }).click();
    await expect(panel.getByRole("textbox")).toHaveCount(1);
    await expect(panel.getByLabel("Model ID", { exact: true })).toHaveCount(0);
    expect(mock.unexpectedAiRequests).toEqual([]);
  });

  for (const failed of [false, true]) test(`handles ChatGPT sign-in ${failed ? "denial" : "completion"} without a key or model`, async ({ page }) => {
    const mock = await open(page, { loginFailure: failed });
    const panel = page.getByRole("region", { name: "Pipe AI", exact: true });
    await panel.getByRole("button", { name: "Connect ChatGPT", exact: true }).click();
    await panel.getByLabel("Connection name", { exact: true }).fill("My ChatGPT");
    await expect(panel.getByRole("textbox")).toHaveCount(1);
    await panel.getByRole("button", { name: "Continue with ChatGPT", exact: true }).click();
    if (failed) await expect(panel.getByRole("alert")).toContainText("ChatGPT plan usage permission was not granted");
    else {
      await expect(panel.locator(".pipe-ai-connections strong")).toHaveText("My ChatGPT");
      await expect(panel.getByText("ChatGPT account connected.", { exact: true })).toBeVisible();
    }
    expect(mock.login).toEqual({ id: expect.any(String), name: "My ChatGPT" });
    expect(mock.writes).toHaveLength(0);
    expect(mock.unexpectedAiRequests).toEqual([]);
  });

  test("returns to the connection step after removing the last account", async ({ page }) => {
    await open(page, { connected: true });
    const panel = page.getByRole("region", { name: "Pipe AI", exact: true });
    await panel.getByRole("button", { name: "Disconnect", exact: true }).click();
    await expect(panel.getByRole("button", { name: "Connect ChatGPT", exact: true })).toBeVisible();
    await expect(panel.locator(".pipe-ai-connections")).toHaveCount(0);
  });

  test("disconnects only the selected account", async ({ page }) => {
    await open(page, { connected: true, extraConnection: true });
    const panel = page.getByRole("region", { name: "Pipe AI", exact: true });
    const account = panel.locator(".pipe-ai-connections li").filter({ hasText: "Personal AI" });
    await account.getByRole("button", { name: "Disconnect", exact: true }).click();
    await expect(panel.locator(".pipe-ai-connections strong")).toHaveText("Other account");
    await expect(panel.getByRole("button", { name: "Disconnect", exact: true })).toBeEnabled();
  });

  test("clears keys when switching authentication and cancels the draft", async ({ page }) => {
    await open(page);
    const panel = page.getByRole("region", { name: "Pipe AI", exact: true });
    await panel.getByRole("button", { name: "Use API key", exact: true }).click();
    await expect(panel.getByLabel("Connection name", { exact: true })).toHaveValue("OpenAI");
    await panel.getByLabel("OpenAI API key", { exact: true }).fill("discarded-private-key");
    await panel.getByRole("radio", { name: "ChatGPT", exact: true }).focus();
    await page.keyboard.press("Space");
    await expect(panel.getByLabel("OpenAI API key", { exact: true })).toHaveCount(0);
    await expect(panel.getByLabel("Connection name", { exact: true })).toHaveValue("ChatGPT");
    await panel.getByRole("radio", { name: "API key", exact: true }).focus();
    await page.keyboard.press("Space");
    await expect(panel.getByLabel("OpenAI API key", { exact: true })).toHaveValue("");
    await panel.getByRole("button", { name: "Cancel", exact: true }).click();
    await expect(panel.getByRole("button", { name: "Connect ChatGPT", exact: true })).toBeVisible();
  });

  test("retries unavailable connections without showing an empty account state", async ({ page }) => {
    const mock = await open(page, { connectionsUnavailable: true });
    const panel = page.getByRole("region", { name: "Pipe AI", exact: true });
    await expect(panel.getByRole("alert")).toContainText("Connections unavailable");
    await expect(panel.getByRole("button", { name: "Connect ChatGPT", exact: true })).toHaveCount(0);
    mock.recoverConnections();
    await panel.getByRole("button", { name: "Retry", exact: true }).click();
    await expect(panel.getByRole("button", { name: "Connect ChatGPT", exact: true })).toBeVisible();
  });

  for (const locale of ["en", "ko"] as const) for (const width of [1280, 390]) test(`${locale} account management fits a ${width}px viewport`, async ({ page }) => {
    await page.setViewportSize({ width, height: 844 });
    await open(page, { connected: true, locale });
    const panel = page.getByRole("region", { name: "Pipe AI", exact: true });
    await expect(panel.locator(".pipe-ai-connections strong")).toHaveText("Personal AI");
    const disconnect = panel.getByRole("button", { name: locale === "ko" ? "연결 해제" : "Disconnect", exact: true });
    await expect(disconnect).toBeInViewport();
    await expect(panel.getByRole("combobox")).toHaveCount(0);
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  });

  test("keeps the mobile connection form usable with both authentication methods", async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await open(page, { locale: "ko" });
    const panel = page.getByRole("region", { name: "Pipe AI", exact: true });
    await panel.getByRole("button", { name: "ChatGPT 연결", exact: true }).click();
    await expect(panel.getByRole("button", { name: "Continue with ChatGPT", exact: true })).toBeVisible();
    await panel.getByRole("radio", { name: "API 키", exact: true }).focus();
    await page.keyboard.press("Space");
    await expect(panel.getByLabel("OpenAI API 키", { exact: true })).toBeVisible();
    const form = await panel.getByRole("form", { name: "AI 연결", exact: true }).boundingBox();
    expect(form!.x + form!.width).toBeLessThanOrEqual(390);
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  });
});
