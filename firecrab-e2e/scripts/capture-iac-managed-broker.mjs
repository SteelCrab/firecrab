/** Isolated actual API/React recovery; the original uncertainty and stopped guest are seeded. */
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
    "Usage: node scripts/capture-iac-managed-broker.mjs QA_INFO OUTPUT_DIRECTORY",
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
      "firecrab-e2e/scripts/capture-iac-managed-broker.mjs",
    ],
    { cwd: repo, encoding: "utf8" },
  ).trim(),
  "",
  "capture requires clean implementation and script",
);
const date = process.env.FIRECRAB_IAC_QA_DATE;
assert.match(date || "", /^\d{4}-\d{2}-\d{2}$/);
const output = path.resolve(outputPath);
await mkdir(output, { recursive: true });
const hash = async (file) =>
  createHash("sha256")
    .update(await readFile(file))
    .digest("hex");
const diskBefore = await hash(fixture.diskPath);
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
  assert.equal(before.action, "disconnect_database_broker");
  assert.equal(before.ownership.resource.id, "default");
  const page = await context.newPage();
  await page.addInitScript(() => localStorage.setItem("firecrab.locale", "en"));
  await page.goto(`${origin}#/stacks`);
  const work = page.getByRole("region", {
    name: "Native operations",
    exact: true,
  });
  const card = work.locator(`[data-operation-id="${fixture.operationId}"]`);
  await expect(
    card.getByText("Disconnect database broker", { exact: true }),
  ).toBeVisible();
  await expect(card.getByText("Needs recovery", { exact: true })).toBeVisible();
  const files = [];
  async function capture(name, locator) {
    const filename = `iac-548-managed-broker-${name}-${date}.png`;
    await locator.scrollIntoViewIfNeeded();
    await locator.screenshot({ path: path.join(output, filename) });
    files.push(filename);
  }
  await capture("desktop", card);
  await page.setViewportSize({ width: 390, height: 844 });
  await expect
    .poll(() =>
      page.evaluate(() => document.documentElement.scrollWidth <= innerWidth),
    )
    .toBe(true);
  await capture("mobile", card);
  await card
    .getByRole("button", { name: "Recover original operation", exact: true })
    .click();
  await expect(
    work.getByText("Original operation completed.", { exact: true }),
  ).toBeVisible();
  const after = await observe();
  assert.equal(after.state, "succeeded");
  assert.equal(after.executionGeneration, 1);
  assert.equal(after.operationId, before.operationId);
  assert.deepEqual(after.ownership, before.ownership);
  const selected = await context.request.get(
    `${origin}api/pipe/database-rabbitmq`,
  );
  assert.equal(selected.status(), 200);
  assert.equal((await selected.json()).configured, false);
  const retained = await context.request.get(
    `${origin}api/vms/${fixture.brokerId}`,
  );
  assert.equal(retained.status(), 200);
  assert.equal((await retained.json()).state, "stopped");
  const diskAfter = await hash(fixture.diskPath);
  assert.equal(diskAfter, diskBefore);
  await capture("recovery-mobile", work);
  await page.setViewportSize({ width: 1440, height: 1000 });
  await capture("recovery-desktop", work);
  const hashes = {};
  for (const filename of files)
    hashes[filename] = await hash(path.join(output, filename));
  assert(!JSON.stringify({ before, after }).includes("private-broker-fixture"));
  await writeFile(
    path.join(output, "iac-548-managed-broker-evidence.json"),
    JSON.stringify(
      {
        sourceCommit,
        sourceDirty: false,
        capturedOn: date,
        fixture: true,
        scope: fixture.scope,
        before,
        after,
        retainedBrokerId: fixture.brokerId,
        retainedDiskGeneration: fixture.diskGeneration,
        diskSha256Before: diskBefore,
        diskSha256After: diskAfter,
        screenshots: hashes,
      },
      null,
      2,
    ) + "\n",
  );
  console.log(
    JSON.stringify({
      sourceCommit,
      files,
      recovery: after.state,
      executionGeneration: after.executionGeneration,
      diskRetained: diskBefore === diskAfter,
    }),
  );
} finally {
  await browser.close();
  await writeFile(infoPath.replace(/\.[^.]+$/, ".stop"), "");
}
