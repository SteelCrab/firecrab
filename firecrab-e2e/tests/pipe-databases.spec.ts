import { expect, test, type Page } from "@playwright/test";
import type { DatabaseInput, DatabasePreview, DatabaseTable, PipeDatabase } from "./support/database-types";

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
  await page.goto("/#/pipe/database");
  return { creates, checks, previews, removes, release, get tableReads() { return tableReads; }, recover: () => { unavailable = false; }, recoverPreview: () => { previewFailure = false; }, failPreview: () => { previewFailure = true; }, recoverTables: () => { tableFailure = false; }, failTables: () => { tableFailure = true; }, recoverDelete: () => { deleteFailure = false; } };
}

test.describe("PIPE databases @dashboard", () => {
  test("keeps long mobile cells compact and scrollable", async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    const value = "x".repeat(2048);
    await open(page, { saved: true, preview: { columns: [{ name: "long_text", dataType: "text" }], rows: [[value]], hasMore: false, offset: 0 } });
    const panel = page.getByRole("region", { name: "Databases", exact: true });
    await panel.getByRole("button", { name: "Browse Application DB", exact: true }).click();
    await panel.getByLabel("Table", { exact: true }).selectOption({index:1});
    const cell = panel.locator(".pipe-database-cell");
    await expect(cell).toHaveText(value);
    await expect(cell).toHaveAttribute("tabindex", "0");
    const dimensions = await cell.evaluate(element => ({ height: element.clientHeight, contentHeight: element.scrollHeight }));
    expect(dimensions.height).toBeLessThanOrEqual(160);
    expect(dimensions.contentHeight).toBeGreaterThan(dimensions.height);
    await cell.focus();
    await cell.press("End");
    await expect.poll(() => cell.evaluate(element => element.scrollTop)).toBeGreaterThan(0);
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
    await panel.getByText("TLS & certificates", { exact: true }).click();
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
    await page.getByRole("link", { name: "MicroVM", exact: true }).click();
    await expect(page).toHaveURL(/#\/pipe\/database$/);
    await expect(panel.getByLabel("Connection name", { exact: true })).toHaveValue("Unfinished DB");
    expect(dialogs).toBe(1);
    await panel.getByRole("button", { name: "Cancel", exact: true }).click();
    await page.getByRole("link", { name: "MicroVM", exact: true }).click();
    await expect(page).toHaveURL(/#\/vms$/);
    expect(dialogs).toBe(1);
  });

  test("clears stale rows after a table refresh failure and allows retry", async ({ page }) => {
    const mock = await open(page, { saved: true });
    const panel = page.getByRole("region", { name: "Databases", exact: true });
    await panel.getByRole("button", { name: "Browse Application DB", exact: true }).click();
    await panel.getByLabel("Table", { exact: true }).selectOption({index:1});
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
    await panel.getByLabel("Table", { exact: true }).selectOption({index:1});
    await panel.getByRole("button", { name: "Application DB actions", exact: true }).click();
    page.once("dialog",dialog=>dialog.accept());
    await page.getByRole("menuitem", { name: "Remove connection Application DB", exact: true }).click();
    await expect(panel.getByRole("alert")).toContainText("Cannot remove database connection");
    await expect(panel.getByRole("cell", { name: "Application DB", exact: true })).toBeVisible();
    await expect(panel.getByRole("cell", { name: "Ada", exact: true })).toBeVisible();
    mock.recoverDelete();
    await panel.getByRole("button", { name: "Application DB actions", exact: true }).click();
    page.once("dialog",dialog=>dialog.accept());
    await page.getByRole("menuitem", { name: "Remove connection Application DB", exact: true }).click();
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
    await panel.getByLabel("Table", { exact: true }).selectOption({index:1});
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
    await panel.getByLabel("Table", { exact: true }).selectOption({index:1});
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

  test("browses tables, preserves NULL, paginates and removes only the connection", async ({ page }) => {
    const mock = await open(page, { saved: true });
    const panel = page.getByRole("region", { name: "Databases", exact: true });
    await panel.getByRole("button", { name: "Browse Application DB", exact: true }).click();
    await panel.getByLabel("Table", { exact: true }).selectOption({index:1});
    await expect(panel.getByRole("cell", { name: "Ada", exact: true })).toBeVisible();
    await expect(panel.getByRole("cell", { name: "NULL", exact: true })).toBeVisible();
    await page.setViewportSize({ width: 390, height: 844 });
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    expect(mock.previews[0]).toEqual({ schema: "public", table: "users", limit: 50, offset: 0 });
    await panel.getByRole("button", { name: "Next rows", exact: true }).click();
    await expect(panel.getByRole("cell", { name: "Next page", exact: true })).toBeVisible();
    expect(mock.previews[1].offset).toBe(50);
    await panel.getByRole("button", { name: "Previous rows", exact: true }).click();
    await expect(panel.getByRole("cell", { name: "Ada", exact: true })).toBeVisible();
    await panel.getByLabel("Table", { exact: true }).selectOption({index:2});
    await expect(panel.getByText("This table has no rows on this page.")).toBeVisible();
    await panel.getByRole("button", { name: "Application DB actions", exact: true }).click();
    page.once("dialog",dialog=>dialog.accept());
    await page.getByRole("menuitem", { name: "Remove connection Application DB", exact: true }).click();
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
    await panel.getByLabel("Table", { exact: true }).selectOption({index:1});
    await expect(panel.getByRole("alert")).toContainText("SELECT permissions");
    mock.recoverPreview();
    await panel.getByRole("button", { name: "Refresh rows", exact: true }).click();
    await expect(panel.getByRole("cell", { name: "Ada", exact: true })).toBeVisible();
  });

  test("supports English connection forms in a narrow viewport", async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await open(page);
    const panel = page.getByRole("region", { name: "Databases", exact: true });
    await panel.getByRole("button", { name: "Add database", exact: true }).click();
    await panel.getByLabel("Database source", { exact: true }).selectOption("rds");
    await expect(panel.getByLabel("TLS", { exact: true })).not.toBeVisible();
    await expect(panel.getByLabel("CA certificate (PEM, optional)", { exact: true })).not.toBeVisible();
    const security = panel.getByText("TLS & certificates", { exact: true });
    await security.focus();
    await security.press("Enter");
    await expect(panel.getByLabel("TLS", { exact: true }).getByRole("option", { name: "Disabled", exact: true })).toHaveCount(0);
    await expect(panel.getByLabel("CA certificate (PEM, optional)", { exact: true })).toBeVisible();
    const clippedControls = await panel.locator(".pipe-database-form").evaluate(form => {
      const bounds = form.getBoundingClientRect();
      return [...form.querySelectorAll("input, select, textarea, button")].filter(control => {
        const rect = control.getBoundingClientRect();
        return rect.left < bounds.left - 1 || rect.right > bounds.right + 1;
      }).map(control => control.tagName);
    });
    expect(clippedControls).toEqual([]);
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  });
});

async function openWritable(page: Page, failure?: "unknown" | "conflict") {
  const mutations: Record<string, unknown>[] = [], bindings: Record<string, unknown>[] = [], triggers: Record<string, unknown>[] = [], jobs: Record<string, unknown>[] = [], rabbitmq: unknown[] = [], acknowledgements: unknown[] = [];
  const serviceId = "22222222-2222-4222-8222-222222222222";
  let configured = false;
  const columns = [
    { name: "id", dataType: "bigint", nullable: false, generated: true, primaryKey: true, binary: false, defaultValue: null },
    { name: "name", dataType: "text", nullable: false, generated: false, primaryKey: false, binary: false, defaultValue: null },
    { name: "metadata", dataType: "jsonb", nullable: true, generated: false, primaryKey: false, binary: false, defaultValue: null },
  ];
  await page.route(/^https?:\/\/[^/]+\/api\//, async route => {
    const path = new URL(route.request().url()).pathname, method = route.request().method();
    const input = () => route.request().postDataJSON();
    if (path === "/api/vms") return route.fulfill({ json: [] });
    if (path === "/api/update") return route.fulfill({ json: { current: "0.3.1", latest: "0.3.1", updateAvailable: false } });
    if (path === "/api/pipe") return route.fulfill({ json: { config: null, units: [], nginxAvailable: false, cloudflaredAvailable: false } });
    if (path === "/api/pipe/databases") return route.fulfill({ json: [{ ...saved, writeEnabled: true }] });
    if (path.endsWith("/tables")) return route.fulfill({ json: { tables: [{ schema: "public", name: "users" }], truncated: false } });
    if (path.endsWith("/rows")) return route.fulfill({ json: { definition: { columns, primaryKey: ["id"], transactional: true }, rows: [{ values: { id: "9007199254740993", name: "Ada", metadata: null }, key: { id: "9007199254740993" }, version: "abc123", truncatedColumns: [] }], offset: 0, hasMore: false } });
    if (path.endsWith("/mutations")) {
      const body = input(); mutations.push(body);
      if (failure && mutations.length === 1) return route.fulfill({ status: failure === "conflict" ? 409 : 503, json: { error: { message: failure === "conflict" ? "The row changed; refresh it before saving" : "Request result is unknown" } } });
      return route.fulfill({ json: { requestId: body.requestId, affectedRows: 1 } });
    }
    if (path === "/api/micro-services") return route.fulfill({ json: [{ id: serviceId, name: "Worker" }] });
    if (path.endsWith("/bindings")) {
      if (method === "POST") bindings.push({ id: input().requestId, databaseId: id, envName: "DATABASE_URL", ready: true, tableCount: 1, ...input() });
      return route.fulfill({ json: method === "GET" ? bindings : bindings.at(-1) });
    }
    if (path.endsWith("/triggers")) {
      if (method === "POST") triggers.push({ id: input().requestId, databaseId: id, paused: false, installed: true, detached: false, pending: 0, failed: 0, delivered: 0, ...input() });
      return route.fulfill({ json: method === "GET" ? triggers : triggers.at(-1) });
    }
    if (path.endsWith("/failures")) return route.fulfill({ json: [{ eventId: `${triggers[0].id}:1`, attempts: 12, error: "payload_too_large" }] });
    if (path.includes("/acknowledge/")) { acknowledgements.push(input()); triggers[0].failed = 0; triggers[0].skipped = 1; return route.fulfill({ json: { acknowledged: true } }); }
    if (path === "/api/pipe/database-rabbitmq") {
      if (method === "PUT") { rabbitmq.push(input()); configured = true; }
      return route.fulfill({ json: { configured, brokers: configured ? ["broker:5671"] : undefined, tls: true, development: false } });
    }
    if (path === "/api/pipe/database-rabbitmq/managed") {
      if (method === "POST") { rabbitmq.push(input()); configured = true; }
      return route.fulfill({ json: configured ? { phase: "ready", development: false } : null });
    }
    if (path === "/api/pipe/database-maintenance/tools") return route.fulfill({ json: { postgres: true, mysql: true } });
    if (path.endsWith("/jobs")) return route.fulfill({ json: jobs });
    if (path.includes("/database-jobs/") && path.endsWith("/backup") && method==="DELETE") {
      const job=jobs.find(j=>path.includes(j.id as string));if(job)job.status="expired";
      return route.fulfill({json:{deleted:true}});
    }
    if (path.endsWith("/backups")) {
      jobs.push({ id: input().requestId, databaseId: id, kind: "backup", status: "succeeded", createdAt: 1, checksum: "abc", bytes: 100, backupId: null, safetyBackupId: null, version: null, error: null });
      return route.fulfill({ status: 202, json: jobs.at(-1) });
    }
    if (path.endsWith("/restore")) { jobs.push({ ...input(), id: input().requestId, kind: "restore", status: "queued", createdAt: 2, bytes: null }); return route.fulfill({ status: 202, json: jobs.at(-1) }); }
    return route.fulfill({ status: 404 });
  });
  await page.addInitScript(() => localStorage.setItem("firecrab.locale", "en"));
  await page.goto("/#/pipe/database");
  await page.getByRole("button", { name: "Browse Application DB", exact: true }).click();
  await page.getByLabel("Table", { exact: true }).selectOption({index:1});
  await expect(page.getByRole("button", { name: "Add row", exact: true })).toBeVisible();
  return { mutations, bindings, triggers, jobs, rabbitmq, acknowledgements, serviceId };
}

test("writes JSON text without rounding and keeps unknown-result retries identical @dashboard", async ({ page }) => {
  const mock = await openWritable(page, "unknown");
  await page.getByRole("button", { name: "Add row", exact: true }).click();
  const editor = page.getByRole("form", { name: "Row editor" });
  await editor.getByLabel("name value mode", { exact: true }).selectOption("value");
  await editor.getByLabel("name value", { exact: true }).fill("New user");
  await editor.getByLabel("metadata value mode", { exact: true }).selectOption("value");
  await editor.getByLabel("metadata value", { exact: true }).fill('{"amount":9007199254740993.123456789}');
  await editor.getByRole("button", { name: "Save row", exact: true }).click();
  await expect(page.getByRole("alert")).toContainText("unknown");
  await expect(editor.getByRole("button", { name: "Cancel", exact: true })).toBeDisabled();
  await expect(editor.getByLabel("name value", { exact: true })).toBeDisabled();
  await editor.getByRole("button", { name: "Retry same request", exact: true }).click();
  await expect(editor).toHaveCount(0);
  expect(mock.mutations).toHaveLength(2);
  expect(mock.mutations[0]).toEqual(mock.mutations[1]);
  expect(mock.mutations[0].values).toEqual({ name: "New user", metadata: '{"amount":9007199254740993.123456789}' });
});

test("keeps original primary keys and releases edits after a definitive conflict @dashboard", async ({ page }) => {
  const mock = await openWritable(page, "conflict");
  await page.getByRole("button", { name: "Edit row", exact: true }).click();
  const editor = page.getByRole("form", { name: "Row editor" });
  await editor.getByLabel("name value", { exact: true }).fill("Changed");
  await editor.getByRole("button", { name: "Save row", exact: true }).click();
  await expect(page.getByRole("alert")).toContainText("row changed");
  await expect(editor.getByLabel("name value", { exact: true })).toBeEnabled();
  expect(mock.mutations[0]).toMatchObject({ key: { id: "9007199254740993" }, version: "abc123", operation: "update" });
  await editor.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(editor).toHaveCount(0);
});

test("uses minimal service and RabbitMQ defaults and confirms snapshot restore @dashboard", async ({ page }) => {
  const mock = await openWritable(page);
  await page.getByText("Services & reliable events", { exact: true }).click();
  await page.getByRole("button", { name: "Connect service", exact: true }).click();
  await page.getByLabel("Service", { exact: true }).selectOption(mock.serviceId);
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect.poll(() => mock.bindings.length).toBe(1);
  expect(mock.bindings[0]).toMatchObject({ serviceId: mock.serviceId, access: "read", envName: "DATABASE_URL" });
  await page.getByRole("button", { name: "Prepare RabbitMQ automatically", exact: true }).click();
  await expect.poll(() => mock.rabbitmq.length).toBe(1);
  expect(mock.rabbitmq[0]).toEqual({ development: false });
  await page.getByRole("button", { name: "Enable events", exact: true }).click();
  await page.getByLabel("Source table", { exact: true }).selectOption({label:"public.users"});
  await page.getByLabel("Handler service", { exact: true }).selectOption(mock.serviceId);
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect.poll(() => mock.triggers.length).toBe(1);
  expect(mock.triggers[0]).toMatchObject({ schema: "public", table: "users", serviceId: mock.serviceId, path: "/events/database" });
  await page.getByText("Backups & schema changes", { exact: true }).click();
  await page.getByRole("button", { name: "Create backup", exact: true }).click();
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect.poll(() => mock.jobs.length).toBe(1);
  await page.getByRole("button", { name: "Restore snapshot", exact: true }).click();
  await page.getByLabel("Backup", {exact:true}).selectOption(mock.jobs[0].id as string);
  await page.getByRole("button", {name:"Save",exact:true}).click();
  expect(mock.jobs).toHaveLength(1);
  await expect(page.getByLabel("Confirm database name",{exact:true})).toHaveAttribute("required","");
  await page.getByLabel("Confirm database name", { exact: true }).fill("app");
  await page.getByLabel("I stopped all external database writers.", { exact: true }).check();
  page.once("dialog",dialog=>dialog.accept());
  await page.getByRole("button", {name:"Save",exact:true}).click();
  await expect.poll(() => mock.jobs.length).toBe(2);
  expect(mock.jobs[1]).toMatchObject({ backupId: mock.jobs[0].id, confirmDatabase: "app", externalWritersStopped: true });
});

test("restores production TLS and requires confirmation to skip a failed delivery @dashboard", async ({ page }) => {
  const mock = await openWritable(page);
  await page.getByText("Services & reliable events", { exact: true }).click();
  await page.getByText("Use existing RabbitMQ", { exact: true }).click();
  await page.getByRole("button", {name:"Configure RabbitMQ",exact:true}).click();
  await page.getByLabel("Single-node development only", { exact: true }).check();
  await page.getByLabel("TLS", { exact: true }).uncheck();
  await page.getByLabel("Single-node development only", { exact: true }).uncheck();
  await expect(page.getByLabel("TLS", { exact: true })).toBeChecked();
  await expect(page.getByLabel("TLS", { exact: true })).toBeDisabled();
  await page.getByLabel("RabbitMQ endpoints", { exact: true }).fill("broker-1:5671, broker-2:5671, broker-3:5671");
  await page.getByLabel("Username", { exact: true }).fill("firecrab");
  await page.getByLabel("Password", { exact: true }).fill("qa-password");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect.poll(() => mock.rabbitmq.length).toBe(1);
  expect(mock.rabbitmq[0]).toMatchObject({ development: false, tls: true });
  await page.getByRole("button", { name: "Enable events", exact: true }).click();
  await page.getByLabel("Source table", { exact: true }).selectOption({label:"public.users"});
  await page.getByLabel("Handler service", { exact: true }).selectOption(mock.serviceId);
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect.poll(() => mock.triggers.length).toBe(1);
  mock.triggers[0].failed = 1;
  await page.getByRole("button", { name: "Failed events", exact: true }).click();
  await expect(page.getByText("payload_too_large", { exact: true })).toBeVisible();
  page.once("dialog", dialog => dialog.dismiss());
  await page.getByRole("button", { name: "Skip failed event", exact: true }).click();
  expect(mock.acknowledgements).toHaveLength(0);
  page.once("dialog", dialog => dialog.accept());
  await page.getByRole("button", { name: "Skip failed event", exact: true }).click();
  await expect.poll(() => mock.acknowledgements.length).toBe(1);
  expect(mock.acknowledgements[0]).toEqual({ confirmEventId: `${mock.triggers[0].id}:1` });
  await expect(page.getByText(/Skipped 1/)).toBeVisible();
});

test("removes a retained backup only after confirmation @dashboard",async({page})=>{
  const mock=await openWritable(page);
  await page.getByText("Backups & schema changes",{exact:true}).click();
  await page.getByRole("button",{name:"Create backup",exact:true}).click();
  await page.getByRole("button",{name:"Save",exact:true}).click();
  await expect.poll(()=>mock.jobs.length).toBe(1);
  page.once("dialog",d=>d.dismiss());
  await page.getByRole("button",{name:"Remove backup",exact:true}).click();
  expect(mock.jobs[0].status).toBe("succeeded");
  page.once("dialog",d=>d.accept());
  await page.getByRole("button",{name:"Remove backup",exact:true}).click();
  await expect.poll(()=>mock.jobs[0].status).toBe("expired");
});
