/** Isolated actual API/React capture. The uncertain SQL checkpoint is injected. */
import assert from "node:assert/strict";
import { readFile, writeFile, mkdir } from "node:fs/promises";
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { chromium, expect } from "@playwright/test";

const [infoPath, outputPath] = process.argv.slice(2);
if (!infoPath || !outputPath)
  throw new Error(
    "Usage: node scripts/capture-iac-database-events.mjs QA_INFO OUTPUT_DIRECTORY",
  );
const fixture = JSON.parse(await readFile(infoPath, "utf8"));
assert.equal(fixture.fixture, true);
const origin = new URL(fixture.url);
assert.equal(origin.hostname, "127.0.0.1");
const repo = fileURLToPath(new URL("../../", import.meta.url));
const sourceCommit = execFileSync("git", ["rev-parse", "HEAD"], {
  cwd: repo,
  encoding: "utf8",
}).trim();
assert.equal(
  execFileSync(
    "git",
    [
      "status",
      "--porcelain",
      "--",
      "firecrab-api",
      "firecrab-stack",
      "firecrab-frontend",
      "firecrab-e2e/scripts/capture-iac-database-events.mjs",
    ],
    { cwd: repo, encoding: "utf8" },
  ).trim(),
  "",
  "capture requires clean implementation and script",
);
const output = path.resolve(outputPath);
await mkdir(output, { recursive: true });
const date = process.env.FIRECRAB_IAC_QA_DATE;
assert.match(date || "", /^\d{4}-\d{2}-\d{2}$/);
const browser = await chromium.launch();
try {
  const context = await browser.newContext({
    viewport: { width: 1440, height: 1000 },
    locale: "en-US",
  });
  const operationPath = `${origin}api/iac/native-operations/${fixture.operationId}`;
  async function observe() {
    const response = await context.request.get(operationPath);
    assert.equal(response.status(), 200);
    return response.json();
  }
  const before = await observe();
  assert.equal(before.state, "needs_recovery");
  assert.equal(before.action, "detach_database_events");
  assert.equal(
    before.ownership.resource.id,
    `${fixture.databaseId}:${fixture.triggerId}`,
  );
  const page = await context.newPage();
  await page.addInitScript(() => localStorage.setItem("firecrab.locale", "en"));
  await page.goto(`${origin}#/stacks`);
  const work = page.getByRole("region", {
    name: "Native operations",
    exact: true,
  });
  const card = work.locator(`[data-operation-id="${fixture.operationId}"]`);
  await expect(
    card.getByText("Detach database events", { exact: true }),
  ).toBeVisible();
  await expect(card.getByText("Needs recovery", { exact: true })).toBeVisible();
  const files = [];
  async function capture(name) {
    const filename = `iac-548-database-events-${name}-${date}.png`;
    await card.scrollIntoViewIfNeeded();
    await card.screenshot({ path: path.join(output, filename) });
    files.push(filename);
  }
  await capture("desktop");
  await page.setViewportSize({ width: 390, height: 844 });
  await expect
    .poll(() =>
      page.evaluate(() => document.documentElement.scrollWidth <= innerWidth),
    )
    .toBe(true);
  await capture("mobile");
  await card
    .getByRole("button", { name: "Recover original operation", exact: true })
    .click();
  await expect(work.getByRole("alert")).toContainText(
    "Original operation still needs recovery.",
    { timeout: 20000 },
  );
  const after = await observe();
  assert.equal(after.state, "needs_recovery");
  assert.equal(after.executionGeneration, 1);
  assert.equal(after.operationId, before.operationId);
  assert.deepEqual(after.ownership, before.ownership);
  await expect(
    card.locator("dt", { hasText: "Recovery attempt" }).locator("+ dd"),
  ).toHaveText("1");
  await capture("recovery-mobile");
  await page.setViewportSize({ width: 1440, height: 1000 });
  await capture("recovery-desktop");
  const combined = JSON.stringify({ before, after });
  assert(!combined.includes("private-event-fixture"));
  assert(!combined.includes("postgresql"));
  const hashes = {};
  for (const filename of files)
    hashes[filename] = createHash("sha256")
      .update(await readFile(path.join(output, filename)))
      .digest("hex");
  await writeFile(
    path.join(output, "iac-548-database-events-evidence.json"),
    JSON.stringify(
      {
        sourceCommit,
        sourceDirty: false,
        capturedOn: date,
        fixture: true,
        scope: fixture.scope,
        before,
        after,
        checks: [
          "actual Axum/SQLite API and compiled React dashboard",
          "original qualified capture and operation UUID retained",
          "actual reconnect failure leaves uncertainty and advances attempt to 1",
          "fixture credentials absent from public receipts",
          "English desktop/mobile cards without page overflow",
        ],
        notRun: [
          "live PostgreSQL/MySQL",
          "source SQL",
          "RabbitMQ queue operations",
          "KVM guest boot",
          "expanded native Stack",
          "Terraform provider",
        ],
        files: hashes,
      },
      null,
      2,
    ) + "\n",
  );
  await context.close();
  console.log(
    "Four English event recovery screenshots and original-identity API checks passed; no live SQL or broker execution claimed.",
  );
} finally {
  await browser.close();
}
