import { expect, test, type Page } from "@playwright/test";
import type { StorageConnection, StorageInput } from "../../firecrab-frontend/src/api/pipeStorage";

const ID = "00000000-0000-0000-0000-000000000439";
const saved: StorageConnection = { id: ID, name: "Reports", bucket: "reports-bucket", region: "ap-northeast-2", prefix: "team/", credentialRef: "connection", services: [] };
async function open(page: Page, options: { empty?: boolean; overview?: boolean; locale?: "ko"; bound?: boolean; reject?: boolean; readOnly?: boolean } = {}) {
  let rows = options.empty ? [] : [{ ...saved, services: options.bound ? [{ id: "service", name: "web" }] : [] }];
  const writes: StorageInput[] = [];
  const operations: { method: string; key: string | null; overwrite: string | null; size: number }[] = [];
  const listings: string[] = [];
  await page.addInitScript(locale => localStorage.setItem("firecrab.locale", locale), options.locale ?? "en");
  await page.route(/^https?:\/\/[^/]+\/api\//, async route => {
    const request = route.request(); const url = new URL(request.url()); const method = request.method();
    if (url.pathname === "/api/pipe/storage") {
      if (method === "POST") {
        writes.push(request.postDataJSON());
        rows = [{ ...saved, ...request.postDataJSON(), credentials: undefined }];
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
      return route.fulfill({ json: { prefix: "team/", objects: [{ key: url.searchParams.has("cursor") ? "team/next.csv" : "team/한글 +%?.csv", size: 3, lastModified: "2026-10-23T00:00:00Z" }], prefixes: [], nextCursor: url.searchParams.has("cursor") ? null : "opaque+/=&cursor" } });
    }
    if (url.pathname.endsWith("/object")) {
      operations.push({ method, key: url.searchParams.get("key"), overwrite: url.searchParams.get("overwrite"), size: request.postDataBuffer()?.length ?? 0 });
      if (options.readOnly && method !== "GET") return route.fulfill({ status: 403, json: { error: { message: "S3 denied this operation" } } });
      return method === "GET" ? route.fulfill({ body: "abc", headers: { "Content-Type": "application/octet-stream", "Content-Length": "3" } }) : route.fulfill({ status: 204 });
    }
    if (["/api/pipe/auth", "/api/pipe/registries", "/api/pipe/databases", "/api/vms"].includes(url.pathname)) return route.fulfill({ json: [] });
    if (url.pathname === "/api/pipe") return route.fulfill({ json: { config: null, units: [], nginxAvailable: false, cloudflaredAvailable: false } });
    return route.fulfill({ status: 404, json: { error: { message: "Fixture route unavailable" } } });
  });
  await page.goto(options.overview ? "/#/pipe" : "/#/pipe/storage");
  return { writes, operations, listings };
}

test.describe("PipeStorage @dashboard", () => {
  test("opens the S3 card and protects service-bound connections", async ({ page }) => {
    await open(page, { overview: true, bound: true });
    const card = page.getByRole("link", { name: "PipeStorage", exact: true });
    await expect(card).toContainText("1 saved");
    await card.click();
    await expect(page.getByRole("heading", { name: "PipeStorage · Amazon S3" })).toBeVisible();
    await expect(page.getByRole("cell", { name: "web", exact: true })).toBeVisible();
    await expect(page.getByRole("button", { name: "Disconnect", exact: true })).toBeDisabled();
    await expect(page.getByRole("button", { name: "Edit", exact: true })).toBeDisabled();
  });

  test("saves input-only keys and clears them from the editor", async ({ page }) => {
    const mock = await open(page, { empty: true });
    await page.getByRole("button", { name: "Add S3 connection" }).click();
    await page.getByLabel("Name", { exact: true }).fill("Reports");
    await page.getByLabel("Bucket", { exact: true }).fill("reports-bucket");
    await page.getByLabel("Credentials", { exact: true }).selectOption("connection");
    await page.getByLabel("AWS access key ID", { exact: true }).fill("AKIAIOSFODNN7EXAMPLE");
    await page.getByLabel("AWS secret access key", { exact: true }).fill("private-secret");
    await page.getByLabel("Session token (optional)", { exact: true }).fill("private-session");
    await expect(page.getByLabel("AWS secret access key", { exact: true })).toHaveAttribute("type", "password");
    await page.getByRole("button", { name: "Verify and save", exact: true }).click();
    await expect(page.getByRole("status")).toContainText("Connection verified and saved");
    expect(mock.writes[0].credentials?.sessionToken).toBe("private-session");
    await expect(page.getByLabel("AWS secret access key", { exact: true })).toHaveCount(0);
    expect(await page.evaluate(() => JSON.stringify(localStorage))).not.toContain("private-secret");
    await page.getByRole("button", { name: "Verify", exact: true }).click();
    await expect(page.getByRole("status")).toContainText("Listing is allowed");
  });

  test("browses pages, downloads, uploads and explicitly deletes one Unicode key", async ({ page }) => {
    const mock = await open(page);
    await page.getByRole("button", { name: "Browse", exact: true }).click();
    const download = page.waitForEvent("download");
    await page.getByRole("button", { name: "Download", exact: true }).click();
    expect((await download).suggestedFilename()).toBe("한글 +%_.csv");
    expect(mock.operations[0].key).toBe("team/한글 +%?.csv");
    await page.getByRole("button", { name: "Next page", exact: true }).click();
    await expect(page.getByRole("cell", { name: "team/next.csv", exact: true })).toBeVisible();
    expect(new URLSearchParams(mock.listings.at(-1)).get("cursor")).toBe("opaque+/=&cursor");
    await page.getByLabel("File", { exact: true }).setInputFiles({ name: "upload +%.bin", mimeType: "application/octet-stream", buffer: Buffer.from("bytes") });
    await page.getByRole("button", { name: "Upload", exact: true }).click();
    await expect(page.getByRole("status")).toContainText("Upload complete");
    expect(mock.operations.find(operation => operation.method === "PUT")).toMatchObject({ key: "team/upload +%.bin", overwrite: "false", size: 5 });
    page.once("dialog", dialog => dialog.accept());
    await page.getByRole("button", { name: "Delete object", exact: true }).click();
    await expect.poll(() => mock.operations.some(operation => operation.method === "DELETE")).toBe(true);
  });

  test("keeps the saved connection after rejected keys and shows read-only errors", async ({ page }) => {
    const mock = await open(page, { reject: true, readOnly: true });
    await page.getByRole("button", { name: "Edit", exact: true }).click();
    await page.getByLabel("Replace saved keys", { exact: true }).check();
    await page.getByLabel("AWS access key ID", { exact: true }).fill("AKIAIOSFODNN7EXAMPLE");
    await page.getByLabel("AWS secret access key", { exact: true }).fill("invalid-secret");
    await page.getByRole("button", { name: "Verify and save", exact: true }).click();
    await expect(page.getByRole("alert")).toContainText("AWS rejected");
    await expect(page.getByRole("cell", { name: "Reports", exact: true })).toBeVisible();
    await page.getByRole("button", { name: "Cancel", exact: true }).click();
    await page.getByRole("button", { name: "Browse", exact: true }).click();
    await page.getByLabel("File", { exact: true }).setInputFiles({ name: "denied.bin", mimeType: "application/octet-stream", buffer: Buffer.from("denied") });
    await page.getByRole("button", { name: "Upload", exact: true }).click();
    await expect(page.getByRole("alert")).toContainText("S3 denied");
    expect(mock.operations.filter(operation => operation.method === "DELETE")).toHaveLength(0);
  });

  for (const locale of ["en", "ko"] as const) {
    test(`fits desktop and mobile connection forms in ${locale}`, async ({ page }, info) => {
      await page.setViewportSize({ width: 1280, height: 900 });
      await open(page, { locale: locale === "ko" ? "ko" : undefined });
      await page.getByRole("button", { name: locale === "ko" ? "S3 연결 추가" : "Add S3 connection", exact: true }).click();
      await page.screenshot({ path: info.outputPath(`s3-desktop-${locale}.png`), fullPage: true });
      await page.setViewportSize({ width: 390, height: 844 });
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
      await page.screenshot({ path: info.outputPath(`s3-mobile-${locale}.png`), fullPage: true });
      await page.getByRole("button", { name: locale === "ko" ? "취소" : "Cancel", exact: true }).click();
      await page.getByRole("button", { name: locale === "ko" ? "조회" : "Browse", exact: true }).click();
      await expect(page.getByRole("cell", { name: "team/한글 +%?.csv", exact: true })).toBeVisible();
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
      await page.screenshot({ path: info.outputPath(`s3-browser-mobile-${locale}.png`), fullPage: true });
    });
  }
});
