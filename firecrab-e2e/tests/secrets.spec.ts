import { installWasmClock } from "./support/wasmClock.js";
import { expect, test, type Page } from "@playwright/test";

const ID = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const CONNECTION = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const PLAN = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
const NEW = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
const SECRET = { id: ID, name: "DB_PASSWORD", description: "Database credential", version: 1, createdAt: 1_792_544_000_000, updatedAt: 1_792_544_000_000, source: null };
const LINK = { id: CONNECTION, name: "Production", provider: "firecrab", settings: { baseUrl: "https://secrets.example.com", region: "", prefix: "" }, version: 1, updatedAt: 1_792_544_000_000 };

async function openSecrets(page: Page, options: { empty?: boolean; failSave?: boolean; partial?: boolean; unavailable?: boolean; conflictApply?: boolean } = {}) {
  const writes: { path: string; method: string; body: Record<string, unknown> }[] = [];
  let rows: object[] = options.empty ? [] : [{ ...SECRET }];
  let connections: object[] = [{ ...LINK }];
  let direction = "import";
  await page.route(/^https?:\/\/[^/]+\/api\//, async (route) => {
    const path = new URL(route.request().url()).pathname;
    const method = route.request().method();
    if (options.unavailable && method === "GET" && ["/api/secrets", "/api/secrets/connections"].includes(path)) return route.fulfill({ status: 503, json: { error: { message: "PipeSecrets storage or its encryption key is unavailable" } } });
    if (path === "/api/vms") return route.fulfill({ json: [] });
    if (path === "/api/update") return route.fulfill({ json: { current: "0.3.1", latest: "0.3.1", updateAvailable: false } });
    if (path === "/api/update/progress") return route.fulfill({ json: { phase: "idle", percent: 0, updatedAtMs: 0 } });
    if (path === "/api/pipe/auth") return route.fulfill({ json: [] });
    if (path === "/api/secrets" && method === "GET") return route.fulfill({ json: rows });
    if (path === "/api/secrets/connections" && method === "GET") return route.fulfill({ json: connections });
    if (path.endsWith("/reveal")) return route.fulfill({ json: { value: "fixture-private-password", version: 1 } });
    if (method !== "GET") {
      const body = route.request().postData() ? route.request().postDataJSON() : {};
      writes.push({ path, method, body });
      if (path.endsWith("/preview")) {
        direction = body.direction;
        return route.fulfill({ json: { id: PLAN, connectionId: CONNECTION, direction, expiresAt: Date.now() + 600_000, changes: direction === "import" ? [{ name: "DB_PASSWORD", change: "update" }, { name: "NEW_TOKEN", change: "add" }, { name: "UNCHANGED", change: "unchanged" }] : [{ name: "DB_PASSWORD", change: "update" }] } });
      }
      if (path.endsWith("/apply")) {
        if (options.conflictApply) { options.conflictApply = false; return route.fulfill({ status: 409, json: { error: { message: "The values changed" } } }); }
        if (direction === "import" && body.names.includes("NEW_TOKEN")) rows.push({ ...SECRET, id: NEW, name: "NEW_TOKEN", source: { connectionId: CONNECTION, connectionName: "Production", provider: "firecrab", remoteKey: "NEW_TOKEN", importedAt: Date.now() } });
        return route.fulfill({ json: options.partial ? { applied: [], failed: [{ name: "DB_PASSWORD", message: "failed" }] } : { applied: body.names, failed: [] } });
      }
      if (path.includes("/connections")) {
        if (method === "DELETE") { connections = []; return route.fulfill({ json: { deleted: CONNECTION } }); }
        const saved = { ...LINK, ...body, version: method === "PUT" ? 2 : 1 };
        const { credentials: _credentials, ...summary } = saved;
        connections = [summary];
        return route.fulfill({ status: method === "POST" ? 201 : 200, json: summary });
      }
      if (options.failSave && method === "POST") return route.fulfill({ status: 400, json: { error: { code: "validation_failed", message: "request validation failed", fields: { name: "is already registered" } } } });
      if (method === "DELETE") { rows = []; return route.fulfill({ json: { deleted: ID } }); }
      const { value: _value, ...metadata } = body;
      rows = [{ ...SECRET, ...metadata, version: method === "PUT" ? 2 : 1 }];
      return route.fulfill({ status: method === "POST" ? 201 : 200, json: rows[0] });
    }
    return route.fulfill({ status: 404, json: { error: { message: "unused endpoint" } } });
  });
  await page.addInitScript(() => localStorage.setItem("firecrab.locale", "en"));
  await page.goto("/#/secrets");
  if (!options.unavailable) await expect(page.getByRole("button", { name: "New secret", exact: true })).toBeEnabled();
  return writes;
}

test.describe("Secrets web console @dashboard", () => {
  test("SEC1: creates and edits without loading stored values into the editor", async ({ page }) => {
    const writes = await openSecrets(page, { empty: true });
    await page.getByRole("button", { name: "New secret", exact: true }).click();
    await page.getByLabel("Secret name", { exact: true }).fill("DB_PASSWORD");
    const value = "typed-secret-value\n두 번째 줄";
    await page.getByLabel("Secret value", { exact: true }).fill(value);
    expect(await page.getByLabel("Secret value", { exact: true }).evaluate(e => getComputedStyle(e).getPropertyValue('-webkit-text-security'))).toBe('disc');
    await page.getByLabel("Show entered value", { exact: true }).check();
    await expect(page.getByLabel("Secret value", { exact: true })).toHaveValue(value);
    await page.getByRole("button", { name: "Save secret", exact: true }).click();
    await expect(page.getByRole("heading", { name: "New secret", exact: true })).toHaveCount(0);
    await expect(page.getByLabel("DB_PASSWORD value", { exact: true })).toHaveText("••••••••");
    expect(writes[0].body.value).toBe(value);
    await page.getByRole("button", { name: "Edit DB_PASSWORD", exact: true }).click();
    await expect(page.getByLabel("Secret value", { exact: true })).toBeHidden();
    await expect(page.getByLabel("Secret value", { exact: true })).toHaveValue("");
    await page.getByLabel("Description", { exact: true }).fill("Updated description");
    await page.getByRole("button", { name: "Save secret", exact: true }).click();
    expect(writes[1].body).toEqual({ name: "DB_PASSWORD", description: "Updated description", version: 1 });
    await expect(page.getByText("Updated description", { exact: true })).toBeVisible();
    await page.getByRole("button", { name: "Edit DB_PASSWORD", exact: true }).click();
    await page.getByLabel("Replace secret value", { exact: true }).check();
    await expect(page.getByLabel("Secret value", { exact: true })).toBeVisible();
    await expect(page.getByLabel("Secret value", { exact: true })).toHaveValue("");
    await page.getByRole("button", { name: "Save secret", exact: true }).click();
    expect(writes[2].body.value).toBe("");
    expect(writes[2].body).not.toHaveProperty("replaceValue");
  });

  test("unavailable encrypted storage shows one error and blocks creation", async ({ page }) => {
    await openSecrets(page, { unavailable: true });
    await expect(page.getByRole("alert")).toHaveCount(1);
    await expect(page.getByRole("alert")).toContainText("encryption key is unavailable");
    await expect(page.getByRole("button", { name: "New secret", exact: true })).toBeDisabled();
  });

  test("SEC2: reveals only on request and hides values again", async ({ page }) => {
    await openSecrets(page);
    await expect(page.getByText("fixture-private-password")).toHaveCount(0);
    await page.getByRole("button", { name: "Reveal DB_PASSWORD", exact: true }).click();
    await expect(page.getByLabel("DB_PASSWORD value", { exact: true })).toHaveText("fixture-private-password");
    await page.getByRole("button", { name: "Hide DB_PASSWORD", exact: true }).click();
    await expect(page.getByLabel("DB_PASSWORD value", { exact: true })).toHaveText("••••••••");
    await page.getByRole("button", { name: "Reveal DB_PASSWORD", exact: true }).click();
    await expect(page.getByLabel("DB_PASSWORD value", { exact: true })).toHaveText("fixture-private-password");
    await page.evaluate(() => window.dispatchEvent(new Event("blur")));
    await expect(page.getByLabel("DB_PASSWORD value", { exact: true })).toHaveText("••••••••");
  });

  test("retains upload selection and destination when a stale preview is retried", async ({ page }) => {
    const writes = await openSecrets(page, { conflictApply: true });
    await page.getByLabel("Select DB_PASSWORD", { exact: true }).check();
    await page.getByLabel("Upload destination", { exact: true }).selectOption(CONNECTION);
    await page.getByRole("button", { name: "Preview upload", exact: true }).click();
    await page.getByLabel("Apply DB_PASSWORD", { exact: true }).check();
    await page.getByRole("button", { name: "Upload selected (1)", exact: true }).click();
    await expect(page.getByRole("alert")).toContainText("The values changed");
    await expect(page.getByLabel("Select DB_PASSWORD", { exact: true })).toBeChecked();
    await expect(page.getByLabel("Upload destination", { exact: true })).toHaveValue(CONNECTION);
    await page.getByRole("button", { name: "Preview upload", exact: true }).click();
    await page.getByLabel("Apply DB_PASSWORD", { exact: true }).check();
    await page.getByRole("button", { name: "Upload selected (1)", exact: true }).click();
    await expect(page.getByRole("status")).toContainText("1 secret(s) applied.");
    expect(writes.filter(write => write.path.endsWith("/preview")).map(write => write.body.secretIds)).toEqual([[ID], [ID]]);
  });

  test("SEC3: previews imports and requires selection before replacing existing values", async ({ page }, testInfo) => {
    const writes = await openSecrets(page);
    await page.getByRole("tab", { name: /External connections/ }).click();
    await page.getByRole("button", { name: "Import from Production", exact: true }).click();
    await expect(page.getByRole("heading", { name: "Import preview", exact: true })).toBeVisible();
    await expect(page.getByLabel("Apply DB_PASSWORD", { exact: true })).not.toBeChecked();
    await expect(page.getByLabel("Apply NEW_TOKEN", { exact: true })).toBeChecked();
    await expect(page.getByLabel("Apply UNCHANGED", { exact: true })).toBeDisabled();
    await page.screenshot({ path: testInfo.outputPath("secrets-import-desktop.png"), fullPage: true });
    await page.getByRole("button", { name: "Import selected (1)", exact: true }).click();
    expect(writes.find((write) => write.path.endsWith("/apply"))?.body).toEqual({ names: ["NEW_TOKEN"] });
    await expect(page.getByText("1 secret(s) applied.", { exact: true })).toBeVisible();
    await page.getByRole("tab", { name: /Internal/ }).click();
    await expect(page.getByText("NEW_TOKEN", { exact: true })).toBeVisible();
    await expect(page.getByText("fixture-private-password")).toHaveCount(0);
  });

  test("SEC4: uploads selected local IDs and reports partial failure", async ({ page }) => {
    const writes = await openSecrets(page, { partial: true });
    await page.getByLabel("Select DB_PASSWORD", { exact: true }).check();
    await page.getByLabel("Upload destination", { exact: true }).selectOption(CONNECTION);
    await page.getByRole("button", { name: "Preview upload", exact: true }).click();
    expect(writes.find((write) => write.path.endsWith("/preview"))?.body).toEqual({ direction: "export", secretIds: [ID] });
    await page.getByLabel("Apply DB_PASSWORD", { exact: true }).check();
    await page.getByRole("button", { name: "Upload selected (1)", exact: true }).click();
    await expect(page.getByRole("alert")).toContainText("Upload failed: DB_PASSWORD");
    await expect(page.getByText("0 secret(s) applied.", { exact: true })).toBeVisible();
  });

  test("SEC5: configures both providers and keeps credentials out of edit forms", async ({ page }) => {
    const writes = await openSecrets(page);
    await page.getByRole("tab", { name: /External connections/ }).click();
    await page.getByRole("button", { name: "Add connection", exact: true }).click();
    await page.getByLabel("Connection name", { exact: true }).fill("AWS production");
    await page.getByLabel("Provider", { exact: true }).selectOption("awsSecretsManager");
    await page.getByLabel("AWS access key ID", { exact: true }).fill("FIXTURE_KEY");
    await page.getByLabel("AWS secret access key", { exact: true }).fill("fixture-credential");
    await page.getByRole("button", { name: "Save connection", exact: true }).click();
    expect(writes[0].body.provider).toBe("awsSecretsManager");
    await page.getByRole("button", { name: "Edit connection AWS production", exact: true }).click();
    await expect(page.getByLabel("AWS secret access key", { exact: true })).toHaveValue("");
    await page.getByLabel("Name prefix (optional)", { exact: true }).fill("production/");
    await page.getByRole("button", { name: "Save connection", exact: true }).click();
    expect(writes[1].body.credentials).toEqual({});
    await page.getByRole("button", { name: "Add connection", exact: true }).click();
    await page.getByLabel("Provider", { exact: true }).selectOption("firecrab");
    await expect(page.getByLabel("Base URL", { exact: true })).toBeVisible();
    await expect(page.getByLabel("Bearer token (optional)", { exact: true })).toHaveAttribute("type", "password");
    await expect(page.getByLabel("Provider", { exact: true }).locator("option")).toHaveText(["Firecrab · PipeSecrets", "AWS Secrets Manager"]);
    await expect(page.getByLabel("Project ID", { exact: true })).toHaveCount(0);
    await expect(page.getByLabel("Secret path", { exact: true })).toHaveCount(0);
  });

  test("SEC6: keeps a failed save editable and protects unsaved forms during navigation", async ({ page }) => {
    await openSecrets(page, { failSave: true });
    await page.getByRole("button", { name: "New secret", exact: true }).click();
    await page.getByLabel("Secret name", { exact: true }).fill("DB_PASSWORD");
    await page.getByLabel("Secret value", { exact: true }).fill("typed-private-value");
    await page.getByRole("button", { name: "Save secret", exact: true }).click();
    await expect(page.getByRole("alert")).toContainText("is already registered");
    await expect(page.getByLabel("Secret value", { exact: true })).toHaveValue("typed-private-value");
    page.once("dialog", (dialog) => dialog.dismiss());
    await page.getByRole("link", { name: "MicroVM", exact: true }).click();
    await expect(page).toHaveURL(/#\/secrets$/);
    await expect(page.getByRole("heading", { name: "New secret", exact: true })).toBeVisible();
  });

  test("SEC7: supports mobile layouts and Korean labels without page overflow", async ({ page }, testInfo) => {
    const errors: string[] = [];
    page.on("pageerror", (error) => errors.push(error.message));
    await openSecrets(page);
    await page.screenshot({ path: testInfo.outputPath("secrets-internal-desktop.png"), fullPage: true });
    await page.setViewportSize({ width: 390, height: 844 });
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    await page.getByRole("tab", { name: /External connections/ }).click();
    await page.getByRole("button", { name: "Add connection", exact: true }).click();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    await page.screenshot({ path: testInfo.outputPath("secrets-mobile.png"), fullPage: true });
    await page.getByRole("button", { name: "Open menu", exact: true }).click();
    await page.getByRole("combobox", { name: "Language", exact: true }).selectOption("ko");
    await page.getByRole("navigation", { name: "주요 메뉴", exact: true }).getByRole("link", { name: "Secrets", exact: true }).click();
    await expect(page.getByRole("heading", { name: "외부 연결 추가", exact: true })).toBeVisible();
    expect(errors).toEqual([]);
  });

  test("SEC8: automatically masks revealed values and disables expired previews", async ({ page }) => {
    await installWasmClock(page);
    const writes = await openSecrets(page);
    await page.getByRole("button", { name: "Reveal DB_PASSWORD", exact: true }).click();
    await expect(page.getByLabel("DB_PASSWORD value", { exact: true })).toHaveText("fixture-private-password");
    await page.clock.fastForward(30_001);
    await expect(page.getByLabel("DB_PASSWORD value", { exact: true })).toHaveText("••••••••");
    await page.getByRole("button", { name: "Reveal DB_PASSWORD", exact: true }).click();
    await expect(page.getByLabel("DB_PASSWORD value", { exact: true })).toHaveText("fixture-private-password");
    await page.evaluate(() => document.dispatchEvent(new Event("visibilitychange")));
    await expect(page.getByLabel("DB_PASSWORD value", { exact: true })).toHaveText("••••••••");
    await page.getByRole("tab", { name: /External connections/ }).click();
    await page.getByRole("button", { name: "Import from Production", exact: true }).click();
    await expect(page.getByRole("button", { name: "Import selected (1)", exact: true })).toBeEnabled();
    await page.clock.fastForward(600_001);
    await expect(page.getByRole("alert")).toHaveText("This preview expired. Create a new preview.");
    await expect(page.getByRole("button", { name: "Import selected (1)", exact: true })).toBeDisabled();
    expect(writes.filter((write) => write.path.endsWith("/apply"))).toEqual([]);
  });
});
