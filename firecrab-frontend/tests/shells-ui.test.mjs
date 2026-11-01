import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";
import { expect } from "@playwright/test";
import { createFixtureServer } from "./fixture-server.mjs";

test("shell revisions show their original script and publication refreshes the version", async () => {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  let version = 2;
  const rev = (n) => ({
    id: `rev-${n}`,
    version: n,
    contentSha256: "abcdef0123456789",
    sizeBytes: 10,
    createdAtMs: 1,
  });
  fixture.state.shells = [{ id: "shell-1", name: "example", latestVersion: 2 }];
  fixture.handlers.set("GET /api/shells/shell-1", ({ send }) =>
    send(200, {
      id: "shell-1",
      name: "example",
      revisions: Array.from({ length: version }, (_, i) => rev(version - i)),
      latestContent: `echo v${version}`,
    }),
  );
  fixture.handlers.set("GET /api/shells/shell-1/revisions/rev-1", ({ send }) =>
    send(200, {
      shellId: "shell-1",
      revisionId: "rev-1",
      version: 1,
      content: "echo v1",
      contentSha256: "abc",
      createdAtMs: 1,
    }),
  );
  fixture.handlers.set("POST /api/shells/shell-1/revisions", ({ send }) => {
    version = 3;
    fixture.state.shells[0].latestVersion = 3;
    send(200, {
      shellId: "shell-1",
      revisionId: "rev-3",
      version: 3,
      content: "echo v3",
      contentSha256: "abc",
      createdAtMs: 1,
    });
  });
  try {
    await page.goto(`${fixture.url}/#/shells`);
    await page.getByText("example", { exact: true }).click();
    await page.getByRole("button", { name: /v1/ }).click();
    await expect(page.locator("#shell-revise")).toHaveValue("echo v1");
    await page.locator("#shell-revise").fill("echo v3");
    await page.getByRole("button", { name: "Publish new revision" }).click();
    await expect(page.locator("#shell-revise")).toHaveValue("echo v3");
    await expect(page.getByRole("complementary")).toContainText("v3");
    await expect(page.locator("tbody")).toContainText("v3");
  } finally {
    await page.close();
    await browser.close();
    await fixture.close();
  }
});
