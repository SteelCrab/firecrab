import { expect, test, type Page } from "@playwright/test";
import type { StorageConnection, StorageInput } from "../types/api/pipeStorage";

const ID = "00000000-0000-0000-0000-000000000439";
const UNICODE_FILE = "한글 +%?.csv";
const saved: StorageConnection = {
  id: ID,
  name: "Reports",
  bucket: "reports-bucket",
  region: "ap-northeast-2",
  prefix: "team/",
  credentialRef: "connection",
  services: [],
};
interface Options {
  empty?: boolean;
  overview?: boolean;
  locale?: "ko";
  bound?: boolean;
  reject?: boolean;
  readOnly?: boolean;
  failUploads?: number;
  folders?: boolean;
}

async function open(page: Page, options: Options = {}) {
  let rows = options.empty
    ? []
    : [
        {
          ...saved,
          services: options.bound ? [{ id: "service", name: "web" }] : [],
        },
      ];
  const writes: StorageInput[] = [];
  const operations: {
    method: string;
    key: string | null;
    overwrite: string | null;
    size: number;
  }[] = [];
  const listings: string[] = [];
  const extraObjects = new Map<string, number>();
  const removed = new Set<string>();
  let remainingUploadFailures = options.failUploads ?? 0;
  await page.addInitScript(
    (locale) => localStorage.setItem("firecrab.locale", locale),
    options.locale ?? "en",
  );
  await page.route(/^https?:\/\/[^/]+\/api\//, async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const method = request.method();
    if (url.pathname === "/api/pipe/storage") {
      if (method === "POST") {
        const input: StorageInput = request.postDataJSON();
        writes.push(input);
        rows = [
          {
            ...saved,
            name: input.name || input.bucket,
            bucket: input.bucket,
            region: input.region,
            prefix: input.prefix,
            credentialRef: input.credentialRef,
          },
        ];
        return route.fulfill({ status: 201, json: rows[0] });
      }
      return route.fulfill({ json: rows });
    }
    if (url.pathname === `/api/pipe/storage/${ID}`) {
      if (method === "DELETE") {
        rows = [];
        return route.fulfill({ status: 204 });
      }
      writes.push(request.postDataJSON());
      if (options.reject)
        return route.fulfill({
          status: 401,
          json: { error: { message: "AWS rejected the signing credentials" } },
        });
      rows = [
        { ...rows[0], ...request.postDataJSON(), credentials: undefined },
      ];
      return route.fulfill({ json: rows[0] });
    }
    if (url.pathname.endsWith("/verify"))
      return route.fulfill({
        json: {
          list: "allowed",
          read: "unknown",
          write: "unknown",
          delete: "unknown",
        },
      });
    if (url.pathname.endsWith("/objects")) {
      listings.push(url.search);
      const prefix = url.searchParams.get("prefix") ?? saved.prefix;
      if (!prefix.startsWith(saved.prefix))
        return route.fulfill({
          status: 400,
          json: { error: { message: "The folder must stay within team/" } },
        });
      const root = prefix === saved.prefix;
      const cursor = url.searchParams.get("cursor");
      const key = root
        ? `${prefix}${cursor ? "next.csv" : UNICODE_FILE}`
        : `${prefix}report.csv`;
      const objects = [
        { key, size: 3, lastModified: "2026-10-23T00:00:00Z" },
        ...Array.from(extraObjects, ([objectKey, size]) => ({
          key: objectKey,
          size,
          lastModified: "2026-10-23T00:00:00Z",
        })),
      ].filter(
        (object) =>
          object.key.startsWith(prefix) &&
          !object.key.slice(prefix.length).includes("/") &&
          !removed.has(object.key),
      );
      const prefixes = options.folders
        ? root
          ? ["team/archive/"]
          : prefix === "team/archive/"
            ? ["team/archive/2025/"]
            : []
        : [];
      return route.fulfill({
        json: {
          prefix,
          objects,
          prefixes,
          nextCursor: root && !cursor ? "opaque+/=&cursor" : null,
        },
      });
    }
    if (url.pathname.endsWith("/object")) {
      const key = url.searchParams.get("key");
      operations.push({
        method,
        key,
        overwrite: url.searchParams.get("overwrite"),
        size: request.postDataBuffer()?.length ?? 0,
      });
      if (options.readOnly && method !== "GET")
        return route.fulfill({
          status: 403,
          json: { error: { message: "S3 denied this operation" } },
        });
      if (method === "PUT" && remainingUploadFailures-- > 0)
        return route.fulfill({
          status: 503,
          json: {
            error: { message: "S3 is temporarily unavailable. Try again." },
          },
        });
      if (method === "PUT" && key)
        extraObjects.set(key, request.postDataBuffer()?.length ?? 0);
      if (method === "DELETE" && key) removed.add(key);
      return method === "GET"
        ? route.fulfill({
            body: "abc",
            headers: {
              "Content-Type": "application/octet-stream",
              "Content-Length": "3",
            },
          })
        : route.fulfill({ status: 204 });
    }
    if (
      [
        "/api/pipe/auth",
        "/api/pipe/registries",
        "/api/pipe/databases",
        "/api/vms",
      ].includes(url.pathname)
    )
      return route.fulfill({ json: [] });
    if (url.pathname === "/api/pipe")
      return route.fulfill({
        json: {
          config: null,
          units: [],
          nginxAvailable: false,
          cloudflaredAvailable: false,
        },
      });
    return route.fulfill({
      status: 404,
      json: { error: { message: "Fixture route unavailable" } },
    });
  });
  await page.goto(options.overview ? "/#/pipe" : "/#/pipe/storage");
  return { writes, operations, listings };
}

async function browse(page: Page) {
  await page
    .getByRole("button", { name: "Open files in Reports", exact: true })
    .click();
  await expect(
    page.getByRole("region", { name: "S3 object browser", exact: true }),
  ).toBeVisible();
}
async function connect(page: Page) {
  await page
    .getByRole("button", { name: "Connect bucket", exact: true })
    .click();
  await page.getByLabel("Bucket", { exact: true }).fill("reports-bucket");
}
const editor = (page: Page) => page.locator("#editor");
async function save(page: Page) {
  await page.getByRole("button", { name: "Save", exact: true }).click();
}
test.describe("PipeStorage @dashboard", () => {
  test("opens S3 from its card and protects service-bound connections", async ({
    page,
  }) => {
    await open(page, { overview: true, bound: true });
    await page.locator('#content a[href="#/pipe/storage"]').click();
    await expect(
      page.getByRole("region", { name: "PipeStorage", exact: true }),
    ).toContainText("web");
    await expect(
      page.getByRole("button", { name: "Edit connection", exact: true }),
    ).toBeDisabled();
    await expect(
      page.getByRole("button", { name: "Disconnect", exact: true }),
    ).toBeDisabled();
    await expect(
      page.getByRole("button", { name: "Check connection", exact: true }),
    ).toBeEnabled();
    await browse(page);
  });
  test("uses shared credentials and normalizes the configured prefix", async ({
    page,
  }) => {
    const m = await open(page, { empty: true });
    await connect(page);
    await page.getByLabel("Folder (optional)", { exact: true }).fill("team");
    await expect(page.getByLabel("Region", { exact: true })).toHaveValue(
      "ap-northeast-2",
    );
    await save(page);
    await expect(editor(page)).toHaveCount(0);
    expect(m.writes[0]).toMatchObject({
      name: "reports-bucket",
      bucket: "reports-bucket",
      region: "ap-northeast-2",
      prefix: "team/",
      credentialRef: "pipeAuthAws",
    });
    expect(m.writes[0]).not.toHaveProperty("credentials");
  });
  test("keeps separate credentials input-only and preserves stored keys on edit", async ({
    page,
  }) => {
    const m = await open(page, { empty: true });
    await connect(page);
    await page.getByLabel("Name (optional)", { exact: true }).fill("Reports");
    await page
      .getByLabel("Credentials", { exact: true })
      .selectOption("connection");
    await page
      .getByLabel("AWS access key ID", { exact: true })
      .fill("FIXTURE_KEY");
    await page
      .getByLabel("AWS secret access key", { exact: true })
      .fill("private-secret");
    await page
      .getByLabel("Session token (optional)", { exact: true })
      .fill("private-session");
    await save(page);
    await expect(editor(page)).toHaveCount(0);
    expect(m.writes[0].credentials).toEqual({
      accessKeyId: "FIXTURE_KEY",
      secretAccessKey: "private-secret",
      sessionToken: "private-session",
    });
    await page
      .getByRole("button", { name: "Edit connection", exact: true })
      .click();
    await expect(page.locator("#field-secretAccessKey")).toHaveValue("");
    await page.getByLabel("Name (optional)", { exact: true }).fill("Changed");
    await save(page);
    await expect.poll(() => m.writes.length).toBe(2);
    expect(m.writes[1]).not.toHaveProperty("credentials");
    expect(
      await page.evaluate(
        () => JSON.stringify(localStorage) + JSON.stringify(sessionStorage),
      ),
    ).not.toContain("private-secret");
  });
  test("paginates with an opaque cursor, downloads Unicode keys and deletes explicitly", async ({
    page,
  }) => {
    const m = await open(page);
    await browse(page);
    await page.getByRole("button", { name: "Load more", exact: true }).click();
    await expect(
      page.getByRole("cell", { name: "next.csv", exact: true }),
    ).toBeVisible();
    expect(new URLSearchParams(m.listings[1]).get("cursor")).toBe(
      "opaque+/=&cursor",
    );
    const row = page
      .getByRole("row")
      .filter({
        has: page.getByRole("cell", { name: UNICODE_FILE, exact: true }),
      });
    const download = page.waitForEvent("download");
    await row.getByRole("button", { name: "Download", exact: true }).click();
    expect((await download).suggestedFilename()).toMatch(/^한글 \+%[?_]\.csv$/);
    page.once("dialog", (d) => d.accept());
    await row.getByRole("button", { name: "Delete file", exact: true }).click();
    await expect(row).toHaveCount(0);
    expect(m.operations.map((o) => [o.method, o.key])).toEqual([
      ["GET", `team/${UNICODE_FILE}`],
      ["DELETE", `team/${UNICODE_FILE}`],
    ]);
  });
  test("retains rejected credential edits and denies file changes without losing uploads", async ({
    page,
  }) => {
    await open(page, { reject: true, readOnly: true });
    await page
      .getByRole("button", { name: "Edit connection", exact: true })
      .click();
    await page.getByLabel("Name (optional)", { exact: true }).fill("Changed");
    await save(page);
    await expect(page.getByRole("alert")).toContainText("AWS rejected");
    await expect(
      page.getByLabel("Name (optional)", { exact: true }),
    ).toHaveValue("Changed");
    await page.getByRole("button", { name: "Cancel", exact: true }).click();
    await browse(page);
    await page
      .getByLabel("File", { exact: true })
      .setInputFiles({
        name: "denied.bin",
        mimeType: "application/octet-stream",
        buffer: Buffer.from("denied"),
      });
    await page.getByRole("button", { name: "Upload", exact: true }).click();
    await expect(page.getByRole("alert")).toContainText("S3 denied");
    await expect(page.getByLabel("File name", { exact: true })).toHaveValue(
      "denied.bin",
    );
  });
  test("retries the same upload and confirms an overwrite", async ({
    page,
  }) => {
    const m = await open(page, { failUploads: 1 });
    await browse(page);
    await page
      .getByLabel("File", { exact: true })
      .setInputFiles({
        name: "upload.bin",
        mimeType: "application/octet-stream",
        buffer: Buffer.from("bytes"),
      });
    await page.getByRole("button", { name: "Upload", exact: true }).click();
    await expect(page.getByRole("alert")).toContainText(
      "temporarily unavailable",
    );
    await page.getByLabel("Allow overwrite", { exact: true }).check();
    page.once("dialog", (d) => d.accept());
    await page.getByRole("button", { name: "Upload", exact: true }).click();
    await expect(
      page.getByRole("cell", { name: "upload.bin", exact: true }),
    ).toBeVisible();
    expect(m.operations.filter((o) => o.method === "PUT")).toMatchObject([
      { key: "team/upload.bin", size: 5, overwrite: "false" },
      { key: "team/upload.bin", size: 5, overwrite: "true" },
    ]);
  });
  test("navigates folders within its root and rejects an outside prefix", async ({
    page,
  }) => {
    const m = await open(page, { folders: true });
    await browse(page);
    await page
      .getByRole("button", { name: "team/archive/", exact: true })
      .click();
    await expect(
      page.getByRole("cell", { name: "report.csv", exact: true }),
    ).toBeVisible();
    await page
      .getByRole("button", { name: "team/archive/2025/", exact: true })
      .click();
    await expect(
      page.getByLabel("Directory prefix", { exact: true }),
    ).toHaveValue("team/archive/2025/");
    await page
      .getByRole("button", { name: "Root folder", exact: true })
      .click();
    await expect(
      page.getByLabel("Directory prefix", { exact: true }),
    ).toHaveValue("team/");
    await page.getByLabel("Directory prefix", { exact: true }).fill("outside/");
    await page
      .getByRole("button", { name: "Open prefix", exact: true })
      .click();
    await expect(page.getByRole("alert")).toContainText("within team/");
    expect(
      m.listings.some(
        (q) => new URLSearchParams(q).get("prefix") === "team/archive/2025/",
      ),
    ).toBe(true);
  });
  test("protects an unsaved connection and file selection during navigation", async ({
    page,
  }) => {
    await open(page);
    await connect(page);
    await page.getByLabel("Bucket", { exact: true }).fill("draft-bucket");
    page.once("dialog", (d) => d.dismiss());
    await page.getByRole("link", { name: "MicroVM", exact: true }).click();
    await expect(page.getByLabel("Bucket", { exact: true })).toHaveValue(
      "draft-bucket",
    );
    await page.getByRole("button", { name: "Cancel", exact: true }).click();
    await browse(page);
    await page
      .getByLabel("File", { exact: true })
      .setInputFiles({
        name: "unsaved.bin",
        mimeType: "application/octet-stream",
        buffer: Buffer.from("bytes"),
      });
    page.once("dialog", (d) => d.dismiss());
    await page.getByRole("link", { name: "MicroVM", exact: true }).click();
    await expect(page.getByLabel("File name", { exact: true })).toHaveValue(
      "unsaved.bin",
    );
  });
  for (const locale of ["en", "ko"] as const) {
    test(`keeps connection editing and file actions within mobile bounds in ${locale}`, async ({
      page,
    }) => {
      await page.setViewportSize({ width: 390, height: 844 });
      await open(page, { locale: locale === "ko" ? "ko" : undefined });
      await page.locator("[data-action=create][data-id=pipe-storage]").click();
      await expect(editor(page)).toBeVisible();
      expect(
        await page.evaluate(
          () => document.documentElement.scrollWidth <= innerWidth,
        ),
      ).toBe(true);
      await page.locator("[data-action=cancel]").click();
      await page.locator("[data-action=storage-browse]").click();
      await expect(
        page.getByRole("region", { name: "S3 object browser", exact: true }),
      ).toBeVisible();
      expect(
        await page.evaluate(
          () => document.documentElement.scrollWidth <= innerWidth,
        ),
      ).toBe(true);
    });
  }
});
