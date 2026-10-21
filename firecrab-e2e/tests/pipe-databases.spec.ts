import { expect, test, type Page } from "@playwright/test";
import type { DatabaseInput, DatabasePreview, DatabaseTable, PipeDatabase } from "../../firecrab-frontend/src/api/pipeDatabases";

const id = "11111111-1111-4111-8111-111111111111";
const saved: PipeDatabase = { id, name: "Application DB", provider: "local", engine: "postgres", host: "localhost", port: 5432, database: "app", tlsMode: "disable" };

interface Options {
  saved?: boolean;
  failure?: boolean;
  unavailable?: boolean;
  locale?: string;
  previewFailure?: boolean;
  tableFailure?: boolean;
  deleteFailure?: boolean;
  hold?: "test" | "create" | "tables" | "preview" | "delete";
  validation?: Record<string, string>;
  tables?: DatabaseTable[];
  truncated?: boolean;
  preview?: DatabasePreview;
}

async function open(page: Page, options: Options = {}) {
  let connections: PipeDatabase[] = options.saved ? [saved] : [];
  const creates: DatabaseInput[] = [];
  const checks: DatabaseInput[] = [];
  const previews: { schema: string; table: string; limit: number; offset: number }[] = [];
  const removes: string[] = [];
  let tableReads = 0;
  let unavailable = options.unavailable ?? false;
  let previewFailure = options.previewFailure ?? false;
  let tableFailure = options.tableFailure ?? false;
  let deleteFailure = options.deleteFailure ?? false;
  let release: () => void = () => {};
  const gate = new Promise<void>(resolve => { release = resolve; });
  const pause = async (operation: Options["hold"]) => { if (options.hold === operation) await gate; };
  await page.route(/^https?:\/\/[^/]+\/api\//, async route => {
    const path = new URL(route.request().url()).pathname;
    const method = route.request().method();
    if (path === "/api/vms") return route.fulfill({ json: [] });
    if (path === "/api/update") return route.fulfill({ json: { current: "0.3.1", latest: "0.3.1", updateAvailable: false } });
    if (path === "/api/pipe") return route.fulfill({ json: { config: null, units: [], nginxAvailable: false, cloudflaredAvailable: false } });
    if (path === "/api/pipe/databases/test") {
      checks.push(route.request().postDataJSON());
      await pause("test");
      return options.failure ? route.fulfill({ status: 502, json: { error: { message: "Cannot read the database; check credentials" } } }) : route.fulfill({ json: { connected: true } });
    }
    if (path === "/api/pipe/databases") {
      if (method === "GET") return unavailable ? route.fulfill({ status: 503, json: { error: { message: "Database list unavailable" } } }) : route.fulfill({ json: connections });
      const input = route.request().postDataJSON() as DatabaseInput;
      creates.push(input);
      await pause("create");
      if (options.validation) return route.fulfill({ status: 400, json: { error: { message: "request validation failed", fields: options.validation } } });
      if (options.failure) return route.fulfill({ status: 502, json: { error: { message: "Cannot read the database; check credentials" } } });
      const uri = new URL(input.uri);
      const connection: PipeDatabase = { ...saved, name: input.name, provider: input.provider, engine: uri.protocol === "mysql:" ? "mysql" : "postgres", host: uri.hostname, port: Number(uri.port || (uri.protocol === "mysql:" ? 3306 : 5432)), database: uri.pathname.slice(1), tlsMode: input.tlsMode ?? (input.provider === "local" ? "disable" : "verify_full") };
      connections = [...connections, connection];
      return route.fulfill({ status: 201, json: connection });
    }
    if (path === `/api/pipe/databases/${id}/tables`) {
      tableReads++;
      await pause("tables");
      if (tableFailure) return route.fulfill({ status: 502, json: { error: { message: "Cannot list database tables" } } });
      return route.fulfill({ json: { tables: options.tables ?? [{ schema: "public", name: "users" }, { schema: "public", name: "empty" }], truncated: options.truncated ?? false } });
    }
    if (path === `/api/pipe/databases/${id}/preview`) {
      const input = route.request().postDataJSON(); previews.push(input);
      await pause("preview");
      if (previewFailure) return route.fulfill({ status: 502, json: { error: { message: "Cannot read the database; check SELECT permissions" } } });
      if (options.preview) return route.fulfill({ json: options.preview });
      return route.fulfill({ json: { columns: [{ name: "id", dataType: "integer" }, { name: "name", dataType: "text" }], rows: input.table === "empty" ? [] : input.offset ? [["3", "Next page"]] : [["1", "Ada"], ["2", null]], offset: input.offset, hasMore: input.table !== "empty" && !input.offset } });
    }
    if (path === `/api/pipe/databases/${id}` && method === "DELETE") {
      removes.push(id);
      await pause("delete");
      if (deleteFailure) return route.fulfill({ status: 503, json: { error: { message: "Cannot remove database connection" } } });
      connections = [];
      return route.fulfill({ json: { deleted: true } });
    }
    return route.fulfill({ status: 404 });
  });
  await page.addInitScript(locale => localStorage.setItem("firecrab.locale", locale), options.locale ?? "en");
  await page.goto("/#/pipe");
  return { creates, checks, previews, removes, release, get tableReads() { return tableReads; }, recover: () => { unavailable = false; }, recoverPreview: () => { previewFailure = false; }, failPreview: () => { previewFailure = true; }, recoverTables: () => { tableFailure = false; }, failTables: () => { tableFailure = true; }, recoverDelete: () => { deleteFailure = false; } };
}

test.describe("PIPE databases @dashboard", () => {
  test("keeps long mobile cells compact and scrollable", async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    const value = "x".repeat(2048);
    await open(page, { saved: true, preview: { columns: [{ name: "long_text", dataType: "text" }], rows: [[value]], hasMore: false, offset: 0 } });
    const panel = page.getByRole("region", { name: "Databases", exact: true });
    await panel.getByRole("button", { name: "Browse Application DB", exact: true }).click();
    await panel.getByLabel("Table", { exact: true }).selectOption("0");
    const cell = panel.locator(".pipe-database-cell");
    await expect(cell).toHaveText(value);
    await expect(cell).toHaveAttribute("tabindex", "0");
    const dimensions = await cell.evaluate(element => ({ height: element.clientHeight, contentHeight: element.scrollHeight }));
    expect(dimensions.height).toBeLessThanOrEqual(160);
    expect(dimensions.contentHeight).toBeGreaterThan(dimensions.height);
    await cell.focus();
    await cell.press("End");
    expect(await cell.evaluate(element => element.scrollTop)).toBeGreaterThan(0);
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  });

  test("requires a nonblank name and URI before testing or saving", async ({ page }) => {
    const mock = await open(page);
    const panel = page.getByRole("region", { name: "Databases", exact: true });
    await panel.getByRole("button", { name: "Add database", exact: true }).click();
    const save = panel.getByRole("button", { name: "Save connection", exact: true });
    const check = panel.getByRole("button", { name: "Test connection", exact: true });
    await expect(save).toBeDisabled();
    await expect(check).toBeDisabled();
    await panel.getByLabel("Connection name", { exact: true }).fill("   ");
    await panel.getByLabel("Database URI", { exact: true }).fill("mysql://user:secret@localhost/app");
    await expect(save).toBeDisabled();
    await panel.getByLabel("Connection name", { exact: true }).fill("App");
    await expect(save).toBeEnabled();
    await panel.getByLabel("Database URI", { exact: true }).fill("  ");
    await expect(check).toBeDisabled();
    expect(mock.creates).toHaveLength(0);
    expect(mock.checks).toHaveLength(0);
  });

  test("forwards TLS and PEM settings and resets TLS when switching sources", async ({ page }) => {
    const mock = await open(page);
    const panel = page.getByRole("region", { name: "Databases", exact: true });
    await panel.getByRole("button", { name: "Add database", exact: true }).click();
    await panel.getByLabel("Connection name", { exact: true }).fill("RDS TLS");
    await panel.getByLabel("Database URI", { exact: true }).fill("mysql://user:secret@db.rds.amazonaws.com/app");
    await panel.getByLabel("TLS", { exact: true }).selectOption("disable");
    await panel.getByLabel("Database source", { exact: true }).selectOption("rds");
    await expect(panel.getByLabel("TLS", { exact: true })).toHaveValue("");
    await expect(panel.getByLabel("TLS", { exact: true }).getByRole("option", { name: "Disabled", exact: true })).toHaveCount(0);
    await panel.getByLabel("TLS", { exact: true }).selectOption("verify_full");
    const pem = "-----BEGIN CERTIFICATE-----\nTEST-FIXTURE\n-----END CERTIFICATE-----";
    await panel.getByLabel("CA certificate (PEM, optional)", { exact: true }).fill(`  ${pem}\n`);
    await panel.getByRole("button", { name: "Save connection", exact: true }).click();
    await expect(panel.getByRole("cell", { name: "Verified", exact: true })).toBeVisible();
    expect(mock.creates[0]).toMatchObject({ provider: "rds", tlsMode: "verify_full", caCertificate: pem });
    await expect(panel).not.toContainText("TEST-FIXTURE");
  });

  test("masks the URI and keeps credentials out of browser storage", async ({ page }) => {
    await open(page);
    const panel = page.getByRole("region", { name: "Databases", exact: true });
    await panel.getByRole("button", { name: "Add database", exact: true }).click();
    const field = panel.getByLabel("Database URI", { exact: true });
    await field.fill("postgres://user:private-password@localhost/app");
    await expect(field).toHaveAttribute("autocomplete", "off");
    await panel.getByRole("button", { name: "Show URI", exact: true }).click();
    await expect(field).toHaveAttribute("type", "text");
    await panel.getByRole("button", { name: "Hide URI", exact: true }).click();
    await expect(field).toHaveAttribute("type", "password");
    const storage = await page.evaluate(() => JSON.stringify([Object.entries(localStorage), Object.entries(sessionStorage)]));
    expect(storage).not.toContain("private-password");
    await panel.getByRole("button", { name: "Cancel", exact: true }).click();
    await panel.getByRole("button", { name: "Add database", exact: true }).click();
    await expect(field).toHaveValue("");
    await expect(field).toHaveAttribute("type", "password");
  });

  test("connection checks do not save and clear success after an input change", async ({ page }) => {
    const mock = await open(page);
    const panel = page.getByRole("region", { name: "Databases", exact: true });
    await panel.getByRole("button", { name: "Add database", exact: true }).click();
    await panel.getByLabel("Connection name", { exact: true }).fill("Check only");
    await panel.getByLabel("Database URI", { exact: true }).fill("postgres://user:secret@localhost/app");
    await panel.getByRole("button", { name: "Test connection", exact: true }).click();
    await expect(panel.getByRole("status")).toHaveText("Database connection succeeded.");
    expect(mock.checks).toHaveLength(1);
    expect(mock.creates).toHaveLength(0);
    await panel.getByLabel("Database URI", { exact: true }).fill("postgres://user:changed@localhost/app");
    await expect(panel.getByRole("status")).toHaveCount(0);
    await expect(panel.getByRole("cell", { name: "Check only", exact: true })).toHaveCount(0);
  });

  test("blocks duplicate saves and editing while a connection is pending", async ({ page }) => {
    const mock = await open(page, { hold: "create" });
    const panel = page.getByRole("region", { name: "Databases", exact: true });
    await panel.getByRole("button", { name: "Add database", exact: true }).click();
    await panel.getByLabel("Connection name", { exact: true }).fill("Pending DB");
    await panel.getByLabel("Database URI", { exact: true }).fill("postgres://user:secret@localhost/app");
    await panel.getByRole("button", { name: "Save connection", exact: true }).dblclick({ force: true });
    await expect.poll(() => mock.creates.length).toBe(1);
    await expect(panel.getByLabel("Database URI", { exact: true })).toBeDisabled();
    await expect(panel.getByLabel("Database source", { exact: true })).toBeDisabled();
    await expect(panel.getByRole("button", { name: "Test connection", exact: true })).toBeDisabled();
    await expect(panel.getByRole("button", { name: "Cancel", exact: true })).toBeDisabled();
    mock.release();
    await expect(panel.getByRole("cell", { name: "Pending DB", exact: true })).toBeVisible();
    expect(mock.creates).toHaveLength(1);
  });

  test("preserves inputs and shows server validation details", async ({ page }) => {
    const mock = await open(page, { validation: { database: "Supabase requires a PostgreSQL URI" } });
    const panel = page.getByRole("region", { name: "Databases", exact: true });
    await panel.getByRole("button", { name: "Add database", exact: true }).click();
    await panel.getByLabel("Connection name", { exact: true }).fill("Invalid provider");
    await panel.getByLabel("Database source", { exact: true }).selectOption("supabase");
    await panel.getByLabel("Database URI", { exact: true }).fill("mysql://user:secret@localhost/app");
    await panel.getByRole("button", { name: "Save connection", exact: true }).click();
    await expect(panel.getByRole("alert")).toContainText("Supabase requires a PostgreSQL URI");
    await expect(panel.getByLabel("Database URI", { exact: true })).toHaveValue("mysql://user:secret@localhost/app");
    expect(mock.creates).toHaveLength(1);
  });

  test("warns before leaving a DB draft and clears the warning after cancel", async ({ page }) => {
    await open(page);
    const panel = page.getByRole("region", { name: "Databases", exact: true });
    await panel.getByRole("button", { name: "Add database", exact: true }).click();
    await panel.getByLabel("Connection name", { exact: true }).fill("Unfinished DB");
    let dialogs = 0;
    page.on("dialog", async dialog => { dialogs++; await dialog.dismiss(); });
    await page.getByRole("button", { name: "MicroVM", exact: true }).click();
    await expect(page).toHaveURL(/#\/pipe$/);
    await expect(panel.getByLabel("Connection name", { exact: true })).toHaveValue("Unfinished DB");
    expect(dialogs).toBe(1);
    await panel.getByRole("button", { name: "Cancel", exact: true }).click();
    await page.getByRole("button", { name: "MicroVM", exact: true }).click();
    await expect(page).toHaveURL(/#\/vms$/);
    expect(dialogs).toBe(1);
  });

  test("clears stale rows after a table refresh failure and allows retry", async ({ page }) => {
    const mock = await open(page, { saved: true });
    const panel = page.getByRole("region", { name: "Databases", exact: true });
    await panel.getByRole("button", { name: "Browse Application DB", exact: true }).click();
    await panel.getByLabel("Table", { exact: true }).selectOption("0");
    await expect(panel.getByRole("cell", { name: "Ada", exact: true })).toBeVisible();
    mock.failTables();
    await panel.getByRole("button", { name: "Refresh tables", exact: true }).click();
    await expect(panel.getByRole("alert")).toContainText("Cannot list database tables");
    await expect(panel.getByRole("cell", { name: "Ada", exact: true })).toHaveCount(0);
    await expect(panel.getByLabel("Table", { exact: true })).toBeDisabled();
    mock.recoverTables();
    await panel.getByRole("button", { name: "Refresh tables", exact: true }).click();
    await expect(panel.getByLabel("Table", { exact: true })).toBeEnabled();
    expect(mock.tableReads).toBe(3);
  });

  test("retains the connection and preview when removal fails", async ({ page }) => {
    const mock = await open(page, { saved: true, deleteFailure: true });
    const panel = page.getByRole("region", { name: "Databases", exact: true });
    await panel.getByRole("button", { name: "Browse Application DB", exact: true }).click();
    await panel.getByLabel("Table", { exact: true }).selectOption("0");
    await panel.getByRole("button", { name: "Remove connection Application DB", exact: true }).click();
    await expect(panel.getByRole("alert")).toContainText("Cannot remove database connection");
    await expect(panel.getByRole("cell", { name: "Application DB", exact: true })).toBeVisible();
    await expect(panel.getByRole("cell", { name: "Ada", exact: true })).toBeVisible();
    mock.recoverDelete();
    await panel.getByRole("button", { name: "Remove connection Application DB", exact: true }).click();
    await expect(panel.getByRole("cell", { name: "Application DB", exact: true })).toHaveCount(0);
    expect(mock.removes).toHaveLength(2);
  });

  test("shows empty table lists and disables table actions", async ({ page }) => {
    await open(page, { saved: true, tables: [] });
    const panel = page.getByRole("region", { name: "Databases", exact: true });
    await panel.getByRole("button", { name: "Browse Application DB", exact: true }).click();
    await expect(panel.getByText("No accessible tables. Check SELECT permissions or refresh the table list.")).toBeVisible();
    await expect(panel.getByLabel("Table", { exact: true })).toBeDisabled();
    await expect(panel.getByRole("button", { name: "Refresh rows", exact: true })).toBeDisabled();
    await panel.getByRole("button", { name: "Close browser", exact: true }).click();
    await expect(panel.getByLabel("Database browser", { exact: true })).toHaveCount(0);
  });

  test("renders unusual identifiers and HTML as text and distinguishes empty strings from NULL", async ({ page }) => {
    const table = { schema: 'schema"quoted', name: 'table`quoted' };
    const html = '<img src=x onerror="window.qaInjected=true">';
    const mock = await open(page, { saved: true, tables: [table], truncated: true, preview: {
      columns: [{ name: '<script>column</script>', dataType: "text" }], rows: [[html], [""], [null]], hasMore: false, offset: 0,
    } });
    const panel = page.getByRole("region", { name: "Databases", exact: true });
    await panel.getByRole("button", { name: "Browse Application DB", exact: true }).click();
    await expect(panel.getByText("Showing the first 500 tables.")).toBeVisible();
    await panel.getByLabel("Table", { exact: true }).selectOption("0");
    await expect(panel.getByRole("cell", { name: html, exact: true })).toBeVisible();
    await expect(panel.locator(".pipe-database-preview tbody tr").nth(1).getByRole("cell")).toHaveText("");
    await expect(panel.getByRole("cell", { name: "NULL", exact: true })).toBeVisible();
    await expect(panel.locator("img, script")).toHaveCount(0);
    expect(mock.previews[0]).toMatchObject({ schema: table.schema, table: table.name });
    await expect(panel.getByRole("button", { name: "Next rows", exact: true })).toBeDisabled();
  });

  test("hides old rows on failed page requests and retries the selected table", async ({ page }) => {
    const mock = await open(page, { saved: true });
    const panel = page.getByRole("region", { name: "Databases", exact: true });
    await panel.getByRole("button", { name: "Browse Application DB", exact: true }).click();
    await panel.getByLabel("Table", { exact: true }).selectOption("0");
    await expect(panel.getByRole("cell", { name: "Ada", exact: true })).toBeVisible();
    mock.failPreview();
    await panel.getByRole("button", { name: "Next rows", exact: true }).click();
    await expect(panel.getByRole("alert")).toContainText("SELECT permissions");
    await expect(panel.getByRole("cell", { name: "Ada", exact: true })).toHaveCount(0);
    mock.recoverPreview();
    await panel.getByRole("button", { name: "Refresh rows", exact: true }).click();
    await expect(panel.getByRole("cell", { name: "Ada", exact: true })).toBeVisible();
    expect(mock.previews.at(-1)?.offset).toBe(0);
  });

  for (const [provider, uri, engine] of [
    ["local", "mysql://user:private-password@localhost:3306/app", "MySQL"],
    ["local", "postgresql://user:private-password@localhost:5432/app", "PostgreSQL"],
    ["supabase", "postgresql://postgres.project:private-password@pooler.supabase.com:5432/postgres", "PostgreSQL"],
    ["rds", "postgresql://user:private-password@db.rds.amazonaws.com:5432/app", "PostgreSQL"],
    ["rds", "mysql://user:private-password@db.rds.amazonaws.com:3306/app", "MySQL"],
  ] as const) {
    test(`tests and saves ${provider} ${engine} without Nginx`, async ({ page }) => {
      const mock = await open(page);
      const panel = page.getByRole("region", { name: "Databases", exact: true });
      await panel.getByRole("button", { name: "Add database", exact: true }).click();
      await panel.getByLabel("Connection name", { exact: true }).fill("Application DB");
      await panel.getByLabel("Database source", { exact: true }).selectOption(provider);
      const field = panel.getByLabel("Database URI", { exact: true });
      await field.fill(uri);
      await expect(field).toHaveAttribute("type", "password");
      await panel.getByRole("button", { name: "Test connection", exact: true }).click();
      await expect(panel.getByRole("status")).toHaveText("Database connection succeeded.");
      expect(mock.checks[0]).toMatchObject({ name: "Application DB", provider, uri });
      await panel.getByRole("button", { name: "Save connection", exact: true }).click();
      await expect(panel.getByRole("cell", { name: "Application DB", exact: true })).toBeVisible();
      await expect(panel.getByRole("cell", { name: engine, exact: true })).toBeVisible();
      await expect(field).toHaveCount(0);
      await expect(panel).not.toContainText("private-password");
      expect(mock.creates[0]).toEqual(mock.checks[0]);
      await page.reload();
      await expect(panel.getByRole("cell", { name: "Application DB", exact: true })).toBeVisible();
    });
  }

  test("browses tables, preserves NULL, paginates and removes only the connection", async ({ page }, testInfo) => {
    const mock = await open(page, { saved: true });
    const panel = page.getByRole("region", { name: "Databases", exact: true });
    await panel.getByRole("button", { name: "Browse Application DB", exact: true }).click();
    await panel.getByLabel("Table", { exact: true }).selectOption("0");
    await expect(panel.getByRole("cell", { name: "Ada", exact: true })).toBeVisible();
    await expect(panel.getByRole("cell", { name: "NULL", exact: true })).toBeVisible();
    await panel.screenshot({ path: testInfo.outputPath("database-preview-desktop.png") });
    expect(mock.previews[0]).toEqual({ schema: "public", table: "users", limit: 50, offset: 0 });
    await panel.getByRole("button", { name: "Next rows", exact: true }).click();
    await expect(panel.getByRole("cell", { name: "Next page", exact: true })).toBeVisible();
    expect(mock.previews[1].offset).toBe(2);
    await panel.getByRole("button", { name: "Previous rows", exact: true }).click();
    await expect(panel.getByRole("cell", { name: "Ada", exact: true })).toBeVisible();
    await panel.getByLabel("Table", { exact: true }).selectOption("1");
    await expect(panel.getByText("This table has no rows on this page.")).toBeVisible();
    await panel.getByRole("button", { name: "Remove connection Application DB", exact: true }).click();
    await expect(panel.getByText("Connection removed. The source database is unchanged.")).toBeVisible();
    expect(mock.removes).toEqual([id]);
    await expect(panel.getByLabel("Database browser", { exact: true })).toHaveCount(0);
  });

  test("keeps failed connection inputs and clears credentials on cancel", async ({ page }) => {
    const mock = await open(page, { failure: true });
    const panel = page.getByRole("region", { name: "Databases", exact: true });
    await panel.getByRole("button", { name: "Add database", exact: true }).click();
    await panel.getByLabel("Connection name", { exact: true }).fill("Failed DB");
    await panel.getByLabel("Database URI", { exact: true }).fill("mysql://user:private-password@localhost/app");
    await panel.getByRole("button", { name: "Save connection", exact: true }).click();
    await expect(panel.getByRole("alert")).toContainText("Cannot read the database");
    await expect(panel.getByLabel("Database URI", { exact: true })).toHaveValue("mysql://user:private-password@localhost/app");
    expect(mock.creates).toHaveLength(1);
    await panel.getByRole("button", { name: "Cancel", exact: true }).click();
    await panel.getByRole("button", { name: "Add database", exact: true }).click();
    await expect(panel.getByLabel("Database URI", { exact: true })).toHaveValue("");
  });

  test("retries unavailable lists and failed table previews", async ({ page }) => {
    const mock = await open(page, { saved: true, unavailable: true, previewFailure: true });
    const panel = page.getByRole("region", { name: "Databases", exact: true });
    await expect(panel.getByRole("button", { name: "Add database", exact: true })).toBeDisabled();
    mock.recover();
    await panel.getByRole("button", { name: "Retry database list", exact: true }).click();
    await panel.getByRole("button", { name: "Browse Application DB", exact: true }).click();
    await panel.getByLabel("Table", { exact: true }).selectOption("0");
    await expect(panel.getByRole("alert")).toContainText("SELECT permissions");
    mock.recoverPreview();
    await panel.getByRole("button", { name: "Refresh rows", exact: true }).click();
    await expect(panel.getByRole("cell", { name: "Ada", exact: true })).toBeVisible();
  });

  test("supports Korean and a narrow viewport", async ({ page }, testInfo) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await open(page, { locale: "ko" });
    const panel = page.getByRole("region", { name: "데이터베이스", exact: true });
    await panel.getByRole("button", { name: "DB 가져오기", exact: true }).click();
    await panel.getByLabel("DB 위치", { exact: true }).selectOption("rds");
    await expect(panel.getByLabel("TLS", { exact: true }).getByRole("option", { name: "사용 안 함", exact: true })).toHaveCount(0);
    await expect(panel.getByLabel("CA 인증서 (PEM, 선택)", { exact: true })).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    await panel.screenshot({ path: testInfo.outputPath("database-form-mobile.png") });
  });
});
