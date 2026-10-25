import { test, expect } from "@playwright/test";
import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";

// The runner creates a bucket-scoped read-only session and cleans it up in finally.
test("real S3 read-only connection: browse, download, denied upload/delete and responsive UI", async ({
  page,
}, info) => {
  test.skip(
    !process.env.FIRECRAB_STORAGE_QA_SHA256,
    "Requires the disposable AWS QA runner",
  );
  await page.addInitScript(() => localStorage.setItem("firecrab.locale", "en"));
  await page.goto("/#/pipe/storage");
  const panel = page.getByRole("region", { name: "PipeStorage", exact: true });
  const listing = page.waitForResponse((response) =>
    new URL(response.url()).pathname.endsWith("/objects"),
  );
  await panel
    .getByRole("button", { name: "Open files in Read-only S3 QA", exact: true })
    .click();
  const listingResponse = await listing;
  expect(listingResponse.status(), await listingResponse.text()).toBe(200);
  expect(
    (await listingResponse.json()).objects.map(
      (object: { key: string }) => object.key,
    ),
  ).toContain("qa/한글 +%?.bin");
  const row = page
    .getByRole("row")
    .filter({
      has: page.getByRole("cell", { name: "한글 +%?.bin", exact: true }),
    });
  const file = row.getByRole("button", { name: "Download", exact: true });
  const download = page.waitForEvent("download");
  await file.click();
  const completed = await download;
  const downloaded = await completed.path();
  expect(downloaded).not.toBeNull();
  expect(
    createHash("sha256")
      .update(await readFile(downloaded!))
      .digest("hex"),
  ).toBe(process.env.FIRECRAB_STORAGE_QA_SHA256);
  await page
    .getByLabel("File", { exact: true })
    .setInputFiles({
      name: "denied-browser.bin",
      mimeType: "application/octet-stream",
      buffer: Buffer.from("must be denied"),
    });
  const upload = panel;
  await upload.getByRole("button", { name: "Upload", exact: true }).click();
  await expect(upload.getByRole("alert")).toContainText(
    "S3 denied this operation",
  );
  await expect(upload.getByLabel("File name", { exact: true })).toHaveValue(
    "denied-browser.bin",
  );
  await upload
    .getByRole("button", { name: "Cancel upload", exact: true })
    .click();
  page.once("dialog", (dialog) => dialog.accept());
  await row.getByRole("button", { name: "Delete file", exact: true }).click();
  await expect(panel.getByRole("alert")).toContainText(
    "S3 denied this operation",
  );
  await expect(file).toBeVisible();
  await page.screenshot({
    path: info.outputPath("s3-live-desktop.png"),
    fullPage: true,
  });
  await page.setViewportSize({ width: 390, height: 844 });
  expect(
    await page.evaluate(() => {
      const browser = document.querySelector<HTMLElement>(
        '[aria-label="S3 object browser"]',
      )!;
      return (
        document.documentElement.scrollWidth <= innerWidth &&
        browser.scrollWidth <= browser.clientWidth + 1
      );
    }),
  ).toBe(true);
  await page.screenshot({
    path: info.outputPath("s3-live-mobile.png"),
    fullPage: true,
  });
});
