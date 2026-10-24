import { expect, test, type Page } from "@playwright/test";
import type { StorageConnection, StorageInput } from "../../firecrab-frontend/src/api/pipeStorage";

const ID = "00000000-0000-0000-0000-000000000439";
const UNICODE_FILE = "한글 +%?.csv";
const saved: StorageConnection = { id: ID, name: "Reports", bucket: "reports-bucket", region: "ap-northeast-2", prefix: "team/", credentialRef: "connection", services: [] };
interface Options { empty?: boolean; overview?: boolean; locale?: "ko"; bound?: boolean; reject?: boolean; readOnly?: boolean; failUploads?: number; folders?: boolean }

async function open(page: Page, options: Options = {}) {
  let rows = options.empty ? [] : [{ ...saved, services: options.bound ? [{ id: "service", name: "web" }] : [] }];
  const writes: StorageInput[] = [];
  const operations: { method: string; key: string | null; overwrite: string | null; size: number }[] = [];
  const listings: string[] = [];
  const extraObjects = new Map<string, number>();
  const removed = new Set<string>();
  let remainingUploadFailures = options.failUploads ?? 0;
  await page.addInitScript(locale => localStorage.setItem("firecrab.locale", locale), options.locale ?? "en");
  await page.route(/^https?:\/\/[^/]+\/api\//, async route => {
    const request = route.request(); const url = new URL(request.url()); const method = request.method();
    if (url.pathname === "/api/pipe/storage") {
      if (method === "POST") {
        const input: StorageInput = request.postDataJSON();
        writes.push(input);
        rows = [{ ...saved, name: input.name || input.bucket, bucket: input.bucket, region: input.region, prefix: input.prefix, credentialRef: input.credentialRef }];
        return route.fulfill({ status: 201, json: rows[0] });
      }
      return route.fulfill({ json: rows });
    }
    if (url.pathname === `/api/pipe/storage/${ID}`) {
      if (method === "DELETE") { rows = []; return route.fulfill({ status: 204 }); }
      writes.push(request.postDataJSON());
      if (options.reject) return route.fulfill({ status: 401, json: { error: { message: "AWS rejected the signing credentials" } } });
      rows = [{ ...rows[0], ...request.postDataJSON(), credentials: undefined }];
      return route.fulfill({ json: rows[0] });
    }
    if (url.pathname.endsWith("/verify")) return route.fulfill({ json: { list: "allowed", read: "unknown", write: "unknown", delete: "unknown" } });
    if (url.pathname.endsWith("/objects")) {
      listings.push(url.search);
      const prefix = url.searchParams.get("prefix") ?? saved.prefix;
      if (!prefix.startsWith(saved.prefix)) return route.fulfill({ status: 400, json: { error: { message: "The folder must stay within team/" } } });
      const root = prefix === saved.prefix;
      const cursor = url.searchParams.get("cursor");
      const key = root ? `${prefix}${cursor ? "next.csv" : UNICODE_FILE}` : `${prefix}report.csv`;
      const objects = [{ key, size: 3, lastModified: "2026-10-23T00:00:00Z" }, ...Array.from(extraObjects, ([objectKey, size]) => ({ key: objectKey, size, lastModified: "2026-10-23T00:00:00Z" }))]
        .filter(object => object.key.startsWith(prefix) && !object.key.slice(prefix.length).includes("/") && !removed.has(object.key));
      const prefixes = options.folders ? root ? ["team/archive/"] : prefix === "team/archive/" ? ["team/archive/2025/"] : [] : [];
      return route.fulfill({ json: { prefix, objects, prefixes, nextCursor: root && !cursor ? "opaque+/=&cursor" : null } });
    }
    if (url.pathname.endsWith("/object")) {
      const key = url.searchParams.get("key");
      operations.push({ method, key, overwrite: url.searchParams.get("overwrite"), size: request.postDataBuffer()?.length ?? 0 });
      if (options.readOnly && method !== "GET") return route.fulfill({ status: 403, json: { error: { message: "S3 denied this operation" } } });
      if (method === "PUT" && remainingUploadFailures-- > 0) return route.fulfill({ status: 503, json: { error: { message: "S3 is temporarily unavailable. Try again." } } });
      if (method === "PUT" && key) extraObjects.set(key, request.postDataBuffer()?.length ?? 0);
      if (method === "DELETE" && key) removed.add(key);
      return method === "GET" ? route.fulfill({ body: "abc", headers: { "Content-Type": "application/octet-stream", "Content-Length": "3" } }) : route.fulfill({ status: 204 });
    }
    if (["/api/pipe/auth", "/api/pipe/registries", "/api/pipe/databases", "/api/vms"].includes(url.pathname)) return route.fulfill({ json: [] });
    if (url.pathname === "/api/pipe") return route.fulfill({ json: { config: null, units: [], nginxAvailable: false, cloudflaredAvailable: false } });
    return route.fulfill({ status: 404, json: { error: { message: "Fixture route unavailable" } } });
  });
  await page.goto(options.overview ? "/#/pipe" : "/#/pipe/storage");
  return { writes, operations, listings };
}

async function connectionMenu(page: Page) {
  await page.getByRole("button", { name: "Reports actions", exact: true }).click();
}
async function openFiles(page: Page) {
  await page.getByRole("button", { name: "Open files in Reports", exact: true }).click();
  await expect(page.getByRole("region", { name: "S3 object browser", exact: true })).toBeVisible();
}
async function expectNoHorizontalScroll(page: Page) {
  expect(await page.evaluate(() => {
    const panel = document.querySelector<HTMLElement>('[aria-label="PipeStorage"]');
    const browser = document.querySelector<HTMLElement>('[aria-label="S3 object browser"], [aria-label="S3 파일 조회"]');
    return document.documentElement.scrollWidth <= innerWidth && [panel, browser].every(element => !element || element.scrollWidth <= element.clientWidth + 1);
  })).toBe(true);
}

test.describe("PipeStorage @dashboard", () => {
  test("opens the S3 card and explains why service-bound connections cannot change", async ({ page }) => {
    await open(page, { overview: true, bound: true });
    const card = page.getByRole("link", { name: "PipeStorage", exact: true });
    await expect(card).toContainText("1 saved");
    await card.click();
    const panel = page.getByRole("region", { name: "PipeStorage", exact: true });
    await expect(panel.getByRole("heading", { name: "S3 Storage", exact: true })).toBeVisible();
    await expect(panel).toContainText("web");
    await expect(panel).toContainText(/unbind|remove.*service|service.*before/i);
    await connectionMenu(page);
    await expect(page.getByRole("menuitem", { name: "Disconnect", exact: true })).toBeDisabled();
    await expect(page.getByRole("menuitem", { name: "Edit connection", exact: true })).toBeDisabled();
    await expect(page.getByRole("menuitem", { name: "Check connection", exact: true })).toBeEnabled();
    await page.keyboard.press("Escape");
    await openFiles(page);
  });

  test("starts with a short connection form and keeps advanced settings optional", async ({ page }) => {
    const mock = await open(page, { empty: true });
    await page.getByRole("button", { name: "Connect bucket", exact: true }).click();
    const dialog = page.getByRole("dialog", { name: "Connect S3 bucket", exact: true });
    await expect(dialog.getByLabel("Bucket", { exact: true })).toBeVisible();
    await expect(dialog.getByLabel("Region", { exact: true })).toHaveValue("ap-northeast-2");
    await expect(dialog.getByLabel("Folder (optional)", { exact: true })).toBeVisible();
    await expect(dialog.getByLabel("Name (optional)", { exact: true })).toBeHidden();
    await expect(dialog.getByLabel("AWS secret access key", { exact: true })).toHaveCount(0);
    await dialog.getByLabel("Bucket", { exact: true }).fill("reports-bucket");
    await dialog.getByLabel("Folder (optional)", { exact: true }).fill("team");
    await dialog.getByRole("button", { name: "Connect bucket", exact: true }).click();
    await expect(dialog).toHaveCount(0);
    await expect(page.getByRole("status")).toContainText("Connection verified and saved");
    expect(mock.writes[0]).toMatchObject({ name: "reports-bucket", bucket: "reports-bucket", region: "ap-northeast-2", prefix: "team/", credentialRef: "pipeAuthAws" });
    await expect(page.getByRole("button", { name: "Open files in reports-bucket", exact: true })).toBeVisible();
    await expect(page.getByRole("region", { name: "S3 object browser", exact: true })).toHaveCount(0);
  });

  test("saves input-only keys and clears them from the editor", async ({ page }) => {
    const mock = await open(page, { empty: true });
    await page.getByRole("button", { name: "Connect bucket", exact: true }).click();
    const dialog = page.getByRole("dialog", { name: "Connect S3 bucket", exact: true });
    await dialog.getByLabel("Bucket", { exact: true }).fill("reports-bucket");
    await dialog.getByText("More options", { exact: true }).click();
    await dialog.getByLabel("Name (optional)", { exact: true }).fill("Reports");
    await dialog.getByRole("combobox", { name: "Credentials", exact: true }).selectOption("connection");
    await dialog.getByLabel("AWS access key ID", { exact: true }).fill("AKIAIOSFODNN7EXAMPLE");
    await dialog.getByLabel("AWS secret access key", { exact: true }).fill("private-secret");
    await dialog.getByLabel("Session token (optional)", { exact: true }).fill("private-session");
    await expect(dialog.getByLabel("AWS secret access key", { exact: true })).toHaveAttribute("type", "password");
    await dialog.getByRole("button", { name: "Connect bucket", exact: true }).click();
    await expect(page.getByRole("status")).toContainText("Connection verified and saved");
    expect(mock.writes[0].credentials?.sessionToken).toBe("private-session");
    await expect(page.getByLabel("AWS secret access key", { exact: true })).toHaveCount(0);
    expect(await page.evaluate(() => JSON.stringify(localStorage))).not.toContain("private-secret");
    await connectionMenu(page);
    await page.getByRole("menuitem", { name: "Check connection", exact: true }).click();
    await expect(page.getByRole("status")).toContainText("Listing is allowed");
  });

  test("accumulates pages, downloads a Unicode key and deletes through its file menu", async ({ page }) => {
    const mock = await open(page);
    await openFiles(page);
    await expect(page.getByRole("button", { name: "Connect bucket", exact: true })).toHaveCount(0);
    const download = page.waitForEvent("download");
    await page.getByRole("button", { name: UNICODE_FILE, exact: true }).click();
    expect((await download).suggestedFilename()).toBe("한글 +%_.csv");
    expect(mock.operations[0].key).toBe(`team/${UNICODE_FILE}`);
    await page.getByRole("button", { name: "Load more", exact: true }).click();
    await expect(page.getByRole("button", { name: "next.csv", exact: true })).toBeVisible();
    await expect(page.getByRole("button", { name: UNICODE_FILE, exact: true })).toBeVisible();
    expect(new URLSearchParams(mock.listings.at(-1)).get("cursor")).toBe("opaque+/=&cursor");
    await page.getByLabel("File", { exact: true }).setInputFiles({ name: "upload +%.bin", mimeType: "application/octet-stream", buffer: Buffer.from("bytes") });
    const upload = page.getByRole("dialog", { name: "Upload file", exact: true });
    await expect(upload.getByLabel("File name", { exact: true })).toHaveValue("upload +%.bin");
    await upload.getByRole("button", { name: "Upload", exact: true }).click();
    await expect(upload).toHaveCount(0);
    await expect(page.getByRole("status")).toContainText("Upload complete");
    expect(mock.operations.find(operation => operation.method === "PUT")).toMatchObject({ key: "team/upload +%.bin", overwrite: "false", size: 5 });
    await page.getByRole("button", { name: `${UNICODE_FILE} actions`, exact: true }).click();
    page.once("dialog", dialog => dialog.accept());
    await page.getByRole("menuitem", { name: "Delete file", exact: true }).click();
    await expect.poll(() => mock.operations.some(operation => operation.method === "DELETE")).toBe(true);
    await expect(page.getByRole("button", { name: UNICODE_FILE, exact: true })).toHaveCount(0);
  });

  test("keeps rejected edits and explains denied file operations without losing the file", async ({ page }) => {
    const mock = await open(page, { reject: true, readOnly: true });
    await connectionMenu(page);
    await page.getByRole("menuitem", { name: "Edit connection", exact: true }).click();
    const editor = page.getByRole("dialog", { name: "Edit S3 connection", exact: true });
    await editor.getByText("More options", { exact: true }).click();
    await editor.getByLabel("Replace saved keys", { exact: true }).check();
    await editor.getByLabel("AWS access key ID", { exact: true }).fill("AKIAIOSFODNN7EXAMPLE");
    await editor.getByLabel("AWS secret access key", { exact: true }).fill("invalid-secret");
    await editor.getByRole("button", { name: "Save changes", exact: true }).click();
    await expect(editor.getByRole("alert")).toContainText("AWS rejected");
    await expect(editor.getByLabel("AWS secret access key", { exact: true })).toHaveValue("invalid-secret");
    await editor.getByRole("button", { name: "Cancel", exact: true }).click();
    await expect(page.getByRole("button", { name: "Open files in Reports", exact: true })).toBeVisible();
    await openFiles(page);
    await page.getByLabel("File", { exact: true }).setInputFiles({ name: "denied.bin", mimeType: "application/octet-stream", buffer: Buffer.from("denied") });
    const upload = page.getByRole("dialog", { name: "Upload file", exact: true });
    await upload.getByRole("button", { name: "Upload", exact: true }).click();
    await expect(upload.getByRole("alert")).toContainText("S3 denied");
    await expect(upload.getByLabel("File name", { exact: true })).toHaveValue("denied.bin");
    await upload.getByRole("button", { name: "Cancel", exact: true }).click();
    await page.getByRole("button", { name: `${UNICODE_FILE} actions`, exact: true }).click();
    page.once("dialog", dialog => dialog.accept());
    await page.getByRole("menuitem", { name: "Delete file", exact: true }).click();
    await expect(page.getByRole("region", { name: "PipeStorage", exact: true }).getByRole("alert")).toContainText("S3 denied");
    await expect(page.getByRole("button", { name: UNICODE_FILE, exact: true })).toBeVisible();
    expect(mock.operations.filter(operation => operation.method === "DELETE")).toHaveLength(1);
  });

  test("retries the selected upload and requires explicit overwrite confirmation", async ({ page }) => {
    const mock = await open(page, { failUploads: 1 });
    await openFiles(page);
    await expect(page.getByRole("button", { name: "Upload", exact: true })).toHaveCount(0);
    await page.getByLabel("File", { exact: true }).setInputFiles({ name: "retry.bin", mimeType: "application/octet-stream", buffer: Buffer.from("retry") });
    const dialog = page.getByRole("dialog", { name: "Upload file", exact: true });
    await dialog.getByLabel("File name", { exact: true }).fill("renamed +%.bin");
    await dialog.getByLabel("Allow overwrite", { exact: true }).check();
    page.once("dialog", prompt => prompt.dismiss());
    await dialog.getByRole("button", { name: "Upload", exact: true }).click();
    expect(mock.operations).toHaveLength(0);
    await expect(dialog.getByLabel("File name", { exact: true })).toHaveValue("renamed +%.bin");
    page.once("dialog", prompt => prompt.accept());
    await dialog.getByRole("button", { name: "Upload", exact: true }).click();
    await expect(dialog.getByRole("alert")).toContainText("temporarily unavailable");
    await expect(dialog.getByLabel("Allow overwrite", { exact: true })).toBeChecked();
    page.once("dialog", prompt => prompt.accept());
    await dialog.getByRole("button", { name: "Upload", exact: true }).click();
    await expect(dialog).toHaveCount(0);
    await expect(page.getByRole("status")).toContainText("Upload complete");
    expect(mock.operations.filter(operation => operation.method === "PUT")).toEqual([
      { method: "PUT", key: "team/renamed +%.bin", overwrite: "true", size: 5 },
      { method: "PUT", key: "team/renamed +%.bin", overwrite: "true", size: 5 },
    ]);
  });

  test("navigates folders and breadcrumbs within the configured root", async ({ page }) => {
    const mock = await open(page, { folders: true });
    await openFiles(page);
    const navigation = page.getByRole("navigation", { name: "Folder navigation", exact: true });
    await expect(navigation.getByRole("button", { name: "team/", exact: true })).toBeVisible();
    await expect(page.getByRole("button", { name: "Parent directory", exact: true })).toHaveCount(0);
    await page.getByRole("button", { name: "archive", exact: true }).click();
    await page.getByRole("button", { name: "2025", exact: true }).click();
    await expect(navigation.getByRole("button", { name: "2025", exact: true })).toBeVisible();
    await navigation.getByRole("button", { name: "archive", exact: true }).click();
    await expect(navigation.getByRole("button", { name: "2025", exact: true })).toHaveCount(0);
    await navigation.getByRole("button", { name: "team/", exact: true }).click();
    await expect(page.getByRole("button", { name: UNICODE_FILE, exact: true })).toBeVisible();
    expect(mock.listings.map(query => new URLSearchParams(query).get("prefix"))).toEqual(["team/", "team/archive/", "team/archive/2025/", "team/archive/", "team/"]);
    await expect(page.getByLabel("Directory prefix", { exact: true })).toHaveCount(0);
    await page.getByRole("button", { name: "Enter folder path", exact: true }).click();
    await page.getByLabel("Directory prefix", { exact: true }).fill("team/archive/2025");
    await page.getByRole("button", { name: "Open prefix", exact: true }).click();
    await expect(page.getByRole("button", { name: "report.csv", exact: true })).toBeVisible();
    expect(new URLSearchParams(mock.listings.at(-1)).get("prefix")).toBe("team/archive/2025/");
    await expect(page.getByLabel("File name", { exact: true })).toHaveCount(0);
    await page.getByRole("button", { name: "Enter folder path", exact: true }).click();
    await page.getByLabel("Directory prefix", { exact: true }).fill("");
    await page.getByRole("button", { name: "Open prefix", exact: true }).click();
    await expect(page.getByRole("button", { name: UNICODE_FILE, exact: true })).toBeVisible();
    expect(new URLSearchParams(mock.listings.at(-1)).get("prefix")).toBe("team/");
  });

  test("keeps keyboard focus in the dialog and restores it on Escape", async ({ page }) => {
    await open(page);
    const trigger = page.getByRole("button", { name: "Connect bucket", exact: true });
    await trigger.click();
    const dialog = page.getByRole("dialog", { name: "Connect S3 bucket", exact: true });
    await expect(dialog.getByLabel("Bucket", { exact: true })).toBeFocused();
    for (const key of ["Shift+Tab", "Shift+Tab", ...Array<string>(12).fill("Tab")]) {
      await page.keyboard.press(key);
      const focus = await dialog.evaluate(element => ({ inside: element.contains(document.activeElement), tag: document.activeElement?.tagName, hasFocus: document.hasFocus() }));
      if (!focus.inside) {
        expect(focus).toMatchObject({ tag: "BODY", hasFocus: false });
        await page.keyboard.press("Tab");
        expect(await dialog.evaluate(element => element.contains(document.activeElement))).toBe(true);
      }
    }
    for (const key of ["Control+k", "Meta+k"]) {
      await page.keyboard.press(key);
      await expect(page.getByRole("dialog", { name: "Command palette", exact: true, includeHidden: true })).toHaveCount(0);
      await expect(dialog).toBeVisible();
      expect(await dialog.evaluate(element => element.contains(document.activeElement))).toBe(true);
    }
    await page.keyboard.press("Escape");
    await expect(dialog).toHaveCount(0);
    await expect(page.getByRole("dialog", { name: "Command palette", exact: true, includeHidden: true })).toHaveCount(0);
    await expect(page.getByRole("dialog", { includeHidden: true })).toHaveCount(0);
    await expect(trigger).toBeFocused();
    await connectionMenu(page);
    await page.getByRole("menuitem", { name: "Edit connection", exact: true }).click();
    const editor = page.getByRole("dialog", { name: "Edit S3 connection", exact: true });
    await expect(editor.getByLabel("Bucket", { exact: true })).toBeFocused();
    await page.keyboard.press("Escape");
    await expect(editor).toHaveCount(0);
    await expect(page.getByRole("button", { name: "Reports actions", exact: true })).toBeFocused();
  });

  for (const locale of ["en", "ko"] as const) {
    test(`keeps desktop and mobile forms and file actions within the panel in ${locale}`, async ({ page }) => {
      await page.setViewportSize({ width: 1280, height: 900 });
      await open(page, { locale: locale === "ko" ? "ko" : undefined });
      await page.getByRole("button", { name: locale === "ko" ? "버킷 연결" : "Connect bucket", exact: true }).click();
      const editor = page.getByRole("dialog");
      await expect(editor).toBeVisible();
      await page.setViewportSize({ width: 390, height: 844 });
      await expectNoHorizontalScroll(page);
      await editor.getByRole("button", { name: locale === "ko" ? "취소" : "Cancel", exact: true }).click();
      await expectNoHorizontalScroll(page);
      await page.getByRole("button", { name: locale === "ko" ? "Reports 파일 열기" : "Open files in Reports", exact: true }).click();
      await expect(page.getByRole("button", { name: UNICODE_FILE, exact: true })).toBeVisible();
      await expectNoHorizontalScroll(page);
      await page.getByRole("button", { name: `${UNICODE_FILE} ${locale === "ko" ? "작업" : "actions"}`, exact: true }).click();
      await expect(page.getByRole("menuitem", { name: locale === "ko" ? "다운로드" : "Download", exact: true })).toBeVisible();
      await expect(page.getByRole("menuitem", { name: locale === "ko" ? "파일 삭제" : "Delete file", exact: true })).toBeVisible();
      const menu = await page.getByRole("menu").boundingBox();
      expect(menu).not.toBeNull();
      expect(menu!.x).toBeGreaterThanOrEqual(0);
      expect(menu!.x + menu!.width).toBeLessThanOrEqual(390);
      await page.keyboard.press("Escape");
      await page.getByLabel(locale === "ko" ? "파일" : "File", { exact: true }).setInputFiles({ name: "mobile.bin", mimeType: "application/octet-stream", buffer: Buffer.from("mobile") });
      await expect(page.getByRole("dialog")).toBeVisible();
      await expectNoHorizontalScroll(page);
      await page.getByRole("dialog").getByRole("button", { name: locale === "ko" ? "취소" : "Cancel", exact: true }).click();
    });
  }
});
