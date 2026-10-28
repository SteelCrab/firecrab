import { test, expect } from "@playwright/test";
import { readFile } from "node:fs/promises";

test("local AWS account: real Auth and shared S3 connection roundtrip", async ({
  page,
}, info) => {
  test.skip(
    !process.env.FIRECRAB_STORAGE_QA_ACCOUNT_CREDENTIALS,
    "Requires explicit local-account QA",
  );
  const credentials = JSON.parse(
    process.env.FIRECRAB_STORAGE_QA_ACCOUNT_CREDENTIALS!,
  );
  await page.addInitScript(() => localStorage.setItem("firecrab.locale", "en"));
  await page.goto("/#/pipe/auth");
  await page.getByRole("button", { name: "Connect AWS", exact: true }).click();
  await page
    .getByLabel("Access key ID", { exact: true })
    .fill(credentials.AccessKeyId);
  await page
    .getByLabel("Secret access key", { exact: true })
    .fill(credentials.SecretAccessKey);
  if (credentials.SessionToken)
    await page
      .getByLabel("Session token", { exact: true })
      .fill(credentials.SessionToken);
  await page
    .getByLabel("Region", { exact: true })
    .fill(process.env.FIRECRAB_STORAGE_QA_REGION!);
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(
    page.getByRole("cell", {
      name: new RegExp(process.env.FIRECRAB_STORAGE_QA_ACCOUNT!),
    }),
  ).toBeVisible();
  await expect(page.locator('input[type="password"]')).toHaveCount(0);
  await page.screenshot({
    path: info.outputPath("aws-account-connected.png"),
    fullPage: true,
  });
  await page.goto("/#/pipe/storage");
  const panel = page.getByRole("region", { name: "PipeStorage", exact: true });
  await panel
    .getByRole("button", { name: "Connect bucket", exact: true })
    .click();
  const editor = page.locator("#editor");
  await editor
    .getByLabel("Bucket", { exact: true })
    .fill(process.env.FIRECRAB_STORAGE_QA_BUCKET!);
  await editor
    .getByLabel("Region", { exact: true })
    .fill(process.env.FIRECRAB_STORAGE_QA_REGION!);
  await editor.getByLabel("Folder", { exact: true }).fill("qa/");
  await editor
    .getByLabel("Name", { exact: true })
    .fill("Local AWS account QA");
  await editor.getByRole("button", { name: "Save", exact: true }).click();
  await expect(panel.getByRole("status")).toContainText("Saved");
  await expect(page.locator('input[type="password"]')).toHaveCount(0);
  await panel
    .getByRole("button", {
      name: "Open files in Local AWS account QA",
      exact: true,
    })
    .click();
  const data = Buffer.from(
    "Actual local AWS account / Auth / PipeStorage / S3 roundtrip\n",
  );
  await page
    .getByLabel("File", { exact: true })
    .setInputFiles({
      name: "account-console-proof.txt",
      mimeType: "text/plain",
      buffer: data,
    });
  const upload = panel;
  await upload.getByRole("button", { name: "Upload", exact: true }).click();
  await expect(page.locator("#storage-name")).toHaveValue("");
  await expect(panel.getByRole("status")).toContainText("Upload complete");
  const row = page
    .getByRole("row")
    .filter({
      has: page.getByRole("cell", { name: /^account-console-proof\.txt(\s|$)/ }),
    });
  const file = row.getByRole("button", { name: "Download", exact: true });
  const pending = page.waitForEvent("download");
  await file.click();
  const downloaded = await (await pending).path();
  expect(await readFile(downloaded!)).toEqual(data);
  await page
    .getByRole("region", { name: "S3 object browser", exact: true })
    .screenshot({
      path: info.outputPath("pipe-storage-account-roundtrip.png"),
    });
  await panel
    .getByRole("heading", { name: "Local AWS account QA", exact: true })
    .scrollIntoViewIfNeeded();
  await page.screenshot({
    path: info.outputPath("pipe-storage-account-desktop.png"),
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
    path: info.outputPath("pipe-storage-account-mobile.png"),
    fullPage: true,
  });
  page.once("dialog", (dialog) => dialog.accept());
  await row.getByRole("button", { name: "Delete file", exact: true }).click();
  await expect(file).toHaveCount(0);
});
