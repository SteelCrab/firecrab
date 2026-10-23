import { expect, test, type Page } from "@playwright/test";
import type {
  DatabaseInput,
  DatabasePreview,
  DatabaseTable,
  PipeDatabase,
} from "../types/api/pipeDatabases";

const id = "11111111-1111-4111-8111-111111111111";
const saved: PipeDatabase = {
  id,
  name: "Application DB",
  provider: "local",
  engine: "postgres",
  host: "localhost",
  port: 5432,
  database: "app",
  tlsMode: "disable",
};

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
  const previews: {
    schema: string;
    table: string;
    limit: number;
    offset: number;
  }[] = [];
  const removes: string[] = [];
  let tableReads = 0;
  let unavailable = options.unavailable ?? false;
  let previewFailure = options.previewFailure ?? false;
  let tableFailure = options.tableFailure ?? false;
  let deleteFailure = options.deleteFailure ?? false;
  let release: () => void = () => {};
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  const pause = async (operation: Options["hold"]) => {
    if (options.hold === operation) await gate;
  };
  await page.route(/^https?:\/\/[^/]+\/api\//, async (route) => {
    const path = new URL(route.request().url()).pathname;
    const method = route.request().method();
    if (path === "/api/vms") return route.fulfill({ json: [] });
    if (path === "/api/update")
      return route.fulfill({
        json: { current: "0.3.1", latest: "0.3.1", updateAvailable: false },
      });
    if (path === "/api/pipe")
      return route.fulfill({
        json: {
          config: null,
          units: [],
          nginxAvailable: false,
          cloudflaredAvailable: false,
        },
      });
    if (path === "/api/pipe/databases/test") {
      checks.push(route.request().postDataJSON());
      await pause("test");
      return options.failure
        ? route.fulfill({
            status: 502,
            json: {
              error: { message: "Cannot read the database; check credentials" },
            },
          })
        : route.fulfill({ json: { connected: true } });
    }
    if (path === "/api/pipe/databases") {
      if (method === "GET")
        return unavailable
          ? route.fulfill({
              status: 503,
              json: { error: { message: "Database list unavailable" } },
            })
          : route.fulfill({ json: connections });
      const input = route.request().postDataJSON() as DatabaseInput;
      creates.push(input);
      await pause("create");
      if (options.validation)
        return route.fulfill({
          status: 400,
          json: {
            error: {
              message: "request validation failed",
              fields: options.validation,
            },
          },
        });
      if (options.failure)
        return route.fulfill({
          status: 502,
          json: {
            error: { message: "Cannot read the database; check credentials" },
          },
        });
      const uri = new URL(input.uri);
      const connection: PipeDatabase = {
        ...saved,
        name: input.name,
        provider: input.provider,
        engine: uri.protocol === "mysql:" ? "mysql" : "postgres",
        host: uri.hostname,
        port: Number(uri.port || (uri.protocol === "mysql:" ? 3306 : 5432)),
        database: uri.pathname.slice(1),
        tlsMode:
          input.tlsMode ??
          (input.provider === "local" ? "disable" : "verify_full"),
      };
      connections = [...connections, connection];
      return route.fulfill({ status: 201, json: connection });
    }
    if (path === `/api/pipe/databases/${id}/tables`) {
      tableReads++;
      await pause("tables");
      if (tableFailure)
        return route.fulfill({
          status: 502,
          json: { error: { message: "Cannot list database tables" } },
        });
      return route.fulfill({
        json: {
          tables: options.tables ?? [
            { schema: "public", name: "users" },
            { schema: "public", name: "empty" },
          ],
          truncated: options.truncated ?? false,
        },
      });
    }
    if (path === `/api/pipe/databases/${id}/preview`) {
      const input = route.request().postDataJSON();
      previews.push(input);
      await pause("preview");
      if (previewFailure)
        return route.fulfill({
          status: 502,
          json: {
            error: {
              message: "Cannot read the database; check SELECT permissions",
            },
          },
        });
      if (options.preview) return route.fulfill({ json: options.preview });
      return route.fulfill({
        json: {
          columns: [
            { name: "id", dataType: "integer" },
            { name: "name", dataType: "text" },
          ],
          rows:
            input.table === "empty"
              ? []
              : input.offset
                ? [["3", "Next page"]]
                : [
                    ["1", "Ada"],
                    ["2", null],
                  ],
          offset: input.offset,
          hasMore: input.table !== "empty" && !input.offset,
        },
      });
    }
    if (path === `/api/pipe/databases/${id}` && method === "DELETE") {
      removes.push(id);
      await pause("delete");
      if (deleteFailure)
        return route.fulfill({
          status: 503,
          json: { error: { message: "Cannot remove database connection" } },
        });
      connections = [];
      return route.fulfill({ json: { deleted: true } });
    }
    return route.fulfill({ status: 404 });
  });
  await page.addInitScript(
    (locale) => localStorage.setItem("firecrab.locale", locale),
    options.locale ?? "en",
  );
  await page.goto("/#/pipe/database");
  return {
    creates,
    checks,
    previews,
    removes,
    release,
    get tableReads() {
      return tableReads;
    },
    recover: () => {
      unavailable = false;
    },
    recoverPreview: () => {
      previewFailure = false;
    },
    failPreview: () => {
      previewFailure = true;
    },
    recoverTables: () => {
      tableFailure = false;
    },
    failTables: () => {
      tableFailure = true;
    },
    recoverDelete: () => {
      deleteFailure = false;
    },
  };
}

const editor = (page: Page) => page.locator("#editor");
async function connect(page: Page) {
  await page
    .getByRole("button", { name: "Connect database", exact: true })
    .click();
}
async function draft(
  page: Page,
  uri = "postgres://reader:private-password@localhost:5432/app",
) {
  await connect(page);
  await page.getByLabel("Name", { exact: true }).fill("Application DB");
  await page.getByLabel("Connection URI", { exact: true }).fill(uri);
}
async function browse(page: Page) {
  await page
    .getByRole("row")
    .filter({
      has: page.getByRole("cell", { name: "Application DB", exact: true }),
    })
    .getByRole("button", { name: "Details", exact: true })
    .click();
  await page.getByRole("button", { name: "Tables", exact: true }).click();
}
async function users(page: Page) {
  await page.getByRole("button", { name: "public.users", exact: true }).click();
}
test.describe("PIPE databases @dashboard", () => {
  test("keeps long mobile cells compact and scrollable", async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    const value = "x".repeat(2048);
    await open(page, {
      saved: true,
      preview: {
        columns: [{ name: "long_text", dataType: "text" }],
        rows: [[value]],
        hasMore: false,
        offset: 0,
      },
    });
    await browse(page);
    await users(page);
    const cell = page.locator(".pipe-database-cell");
    await expect(cell).toHaveText(value);
    await expect(cell).toHaveAttribute("tabindex", "0");
    expect(await cell.evaluate((e) => e.clientHeight)).toBeLessThanOrEqual(160);
    expect(await cell.evaluate((e) => e.scrollHeight > e.clientHeight)).toBe(
      true,
    );
  });
  test("requires a name and URI before checking or saving", async ({
    page,
  }) => {
    const m = await open(page);
    await connect(page);
    await page
      .getByRole("button", { name: "Test connection", exact: true })
      .click();
    expect(m.checks).toEqual([]);
    await expect(page.locator("[aria-invalid=true]")).toHaveCount(2);
    await page.getByLabel("Name", { exact: true }).fill("DB");
    await page.getByRole("button", { name: "Save", exact: true }).click();
    expect(m.creates).toEqual([]);
  });
  test("forwards TLS and PEM settings and resets TLS on provider changes", async ({
    page,
  }) => {
    const m = await open(page);
    await draft(page);
    await page.getByLabel("Provider", { exact: true }).selectOption("supabase");
    await expect(page.getByLabel("TLS", { exact: true })).toHaveValue(
      "verify_full",
    );
    await page
      .getByLabel("CA certificate (optional)", { exact: true })
      .fill("-----BEGIN CERTIFICATE-----\nfixture\n-----END CERTIFICATE-----");
    await page
      .getByRole("button", { name: "Test connection", exact: true })
      .click();
    await expect.poll(() => m.checks.length).toBe(1);
    expect(m.checks[0]).toMatchObject({
      provider: "supabase",
      tlsMode: "verify_full",
      caCertificate:
        "-----BEGIN CERTIFICATE-----\nfixture\n-----END CERTIFICATE-----",
    });
    await page.getByLabel("Provider", { exact: true }).selectOption("local");
    await expect(page.getByLabel("TLS", { exact: true })).toHaveValue(
      "disable",
    );
  });
  test("masks the URI and keeps credentials out of browser storage", async ({
    page,
  }) => {
    const m = await open(page);
    await draft(page);
    await expect(
      page.getByLabel("Connection URI", { exact: true }),
    ).toHaveAttribute("type", "password");
    await page.getByRole("button", { name: "Save", exact: true }).click();
    await expect(editor(page)).toHaveCount(0);
    expect(m.creates[0].uri).toContain("private-password");
    expect(
      await page.evaluate(
        () => JSON.stringify(localStorage) + JSON.stringify(sessionStorage),
      ),
    ).not.toContain("private-password");
  });
  test("connection checks do not save and input changes clear their success", async ({
    page,
  }) => {
    const m = await open(page);
    await draft(page);
    await page
      .getByRole("button", { name: "Test connection", exact: true })
      .click();
    await expect(page.locator("#form-result")).toContainText("Connected");
    expect(m.creates).toEqual([]);
    await page.getByLabel("Name", { exact: true }).fill("Renamed");
    await expect(page.locator("#form-result")).toBeEmpty();
  });
  test("blocks duplicate saves and edits while a connection is pending", async ({
    page,
  }) => {
    const m = await open(page, { hold: "create" });
    await draft(page);
    await page.getByRole("button", { name: "Save", exact: true }).click();
    await expect(page.getByLabel("Name", { exact: true })).toBeDisabled();
    await expect(
      page.getByRole("button", { name: "Save", exact: true }),
    ).toBeDisabled();
    expect(m.creates).toHaveLength(1);
    m.release();
    await expect(editor(page)).toHaveCount(0);
  });
  test("preserves input and server field validation details", async ({
    page,
  }) => {
    await open(page, { validation: { uri: "invalid connection URI" } });
    await draft(page);
    await page.getByRole("button", { name: "Save", exact: true }).click();
    await expect(page.getByRole("alert")).toContainText(
      "invalid connection URI",
    );
    await expect(
      page.getByLabel("Connection URI", { exact: true }),
    ).toHaveValue("postgres://reader:private-password@localhost:5432/app");
  });
  test("warns before leaving a draft and cancellation clears its credentials", async ({
    page,
  }) => {
    await open(page);
    await draft(page);
    page.once("dialog", (d) => d.dismiss());
    await page.getByRole("link", { name: "MicroVM", exact: true }).click();
    await expect(editor(page)).toBeVisible();
    await page.getByRole("button", { name: "Cancel", exact: true }).click();
    await expect(page.locator("input[type=password]")).toHaveCount(0);
  });
  test("clears old rows after a table refresh failure and retries", async ({
    page,
  }) => {
    const m = await open(page, { saved: true });
    await browse(page);
    await users(page);
    await expect(
      page.getByRole("cell", { name: "Ada", exact: true }),
    ).toBeVisible();
    m.failTables();
    await page.getByRole("button", { name: "Tables", exact: true }).click();
    await expect(page.getByRole("alert")).toContainText(
      "Cannot list database tables",
    );
    await expect(
      page.getByRole("cell", { name: "Ada", exact: true }),
    ).toHaveCount(0);
    m.recoverTables();
    await page.getByRole("button", { name: "Tables", exact: true }).click();
    await expect(
      page.getByRole("button", { name: "public.users", exact: true }),
    ).toBeVisible();
  });
  test("retains the connection and preview when removal fails", async ({
    page,
  }) => {
    await open(page, { saved: true, deleteFailure: true });
    await browse(page);
    await users(page);
    page.once("dialog", (d) => d.accept());
    await page.getByRole("button", { name: "Disconnect", exact: true }).click();
    await expect(page.getByRole("alert")).toContainText(
      "Cannot remove database connection",
    );
    await expect(
      page.getByRole("cell", { name: "Ada", exact: true }),
    ).toBeVisible();
  });
  test("shows empty table lists without creating preview requests", async ({
    page,
  }) => {
    const m = await open(page, { saved: true, tables: [] });
    await browse(page);
    await expect(page.locator("#detail-result")).toContainText("No tables");
    expect(m.previews).toEqual([]);
  });
  test("renders unusual identifiers and HTML as text and preserves NULL", async ({
    page,
  }) => {
    const m = await open(page, {
      saved: true,
      tables: [{ schema: "odd|schema", name: "<script>table</script>" }],
      preview: {
        columns: [{ name: "<img src=x>", dataType: "text" }],
        rows: [["<script>text</script>"], [""], [null]],
        offset: 0,
        hasMore: false,
      },
    });
    await browse(page);
    await page
      .getByRole("button", {
        name: "odd|schema.<script>table</script>",
        exact: true,
      })
      .click();
    await expect(
      page.getByRole("cell", { name: "<script>text</script>", exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("cell", { name: "NULL", exact: true }),
    ).toBeVisible();
    await expect(page.locator(".pipe-database-preview script")).toHaveCount(0);
    expect(m.previews[0]).toMatchObject({
      schema: "odd|schema",
      table: "<script>table</script>",
    });
  });
  test("hides old rows after a failed page request and retries", async ({
    page,
  }) => {
    const m = await open(page, { saved: true });
    await browse(page);
    await users(page);
    m.failPreview();
    await page
      .getByRole("button", { name: "Next 50 rows", exact: true })
      .click();
    await expect(page.getByRole("alert")).toContainText(
      "Cannot read the database",
    );
    await expect(
      page.getByRole("cell", { name: "Ada", exact: true }),
    ).toHaveCount(0);
    m.recoverPreview();
    await page.getByRole("button", { name: "Tables", exact: true }).click();
    await users(page);
    await expect(
      page.getByRole("cell", { name: "Ada", exact: true }),
    ).toBeVisible();
  });
  test("paginates both directions and removes only the connection", async ({
    page,
  }) => {
    const m = await open(page, { saved: true });
    await browse(page);
    await users(page);
    await page
      .getByRole("button", { name: "Next 50 rows", exact: true })
      .click();
    await expect(
      page.getByRole("cell", { name: "Next page", exact: true }),
    ).toBeVisible();
    await page
      .getByRole("button", { name: "Previous 50 rows", exact: true })
      .click();
    await expect(
      page.getByRole("cell", { name: "Ada", exact: true }),
    ).toBeVisible();
    expect(m.previews.map((p) => p.offset)).toEqual([0, 50, 0]);
    page.once("dialog", (d) => d.accept());
    await page.getByRole("button", { name: "Disconnect", exact: true }).click();
    await expect(
      page.getByRole("cell", { name: "Application DB", exact: true }),
    ).toHaveCount(0);
    expect(m.removes).toEqual([id]);
  });
  test("keeps failed connection input editable and discards it on cancel", async ({
    page,
  }) => {
    await open(page, { failure: true });
    await draft(page);
    await page
      .getByRole("button", { name: "Test connection", exact: true })
      .click();
    await expect(page.getByRole("alert")).toContainText(
      "Cannot read the database",
    );
    await expect(
      page.getByLabel("Connection URI", { exact: true }),
    ).toHaveValue("postgres://reader:private-password@localhost:5432/app");
    await page.getByRole("button", { name: "Cancel", exact: true }).click();
    await expect(editor(page)).toHaveCount(0);
  });
  test("recovers unavailable connection lists", async ({ page }) => {
    const m = await open(page, { saved: true, unavailable: true });
    await expect(page.getByRole("alert")).toContainText(
      "Database list unavailable",
    );
    m.recover();
    await page.getByRole("button", { name: "Refresh", exact: true }).click();
    await expect(
      page.getByRole("cell", { name: "Application DB", exact: true }),
    ).toBeVisible();
    await expect(page.getByRole("alert")).toHaveCount(0);
  });
  test("keeps connection forms within a narrow viewport", async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await open(page);
    await draft(page);
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    ).toBe(true);
  });
});
