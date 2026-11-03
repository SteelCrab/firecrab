/** Actual local API/dashboard; the uncertain SQL checkpoint is explicitly injected. */
import assert from "node:assert/strict";
import { readFile, writeFile, mkdir } from "node:fs/promises";
import { createHash, randomUUID } from "node:crypto";
import { execFileSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { chromium, expect } from "@playwright/test";

const [infoPath, outputPath] = process.argv.slice(2);
if (!infoPath || !outputPath) throw new Error("Usage: node scripts/capture-iac-database.mjs QA_INFO OUTPUT_DIRECTORY");
const fixture = JSON.parse(await readFile(infoPath, "utf8"));
assert.equal(fixture.fixture, true);
const origin = new URL(fixture.url);
assert.equal(origin.hostname, "127.0.0.1");
const repo = fileURLToPath(new URL("../../", import.meta.url));
const sourceCommit = execFileSync("git", ["rev-parse", "HEAD"], {cwd:repo,encoding:"utf8"}).trim();
assert.equal(execFileSync("git", ["status", "--porcelain", "--", "firecrab-api", "firecrab-stack", "firecrab-frontend", "firecrab-e2e/scripts/capture-iac-database.mjs"], {cwd:repo,encoding:"utf8"}).trim(), "", "capture requires clean implementation and script");
const output = path.resolve(outputPath);
await mkdir(output,{recursive:true});
const date = process.env.FIRECRAB_IAC_QA_DATE;
assert.match(date || "", /^\d{4}-\d{2}-\d{2}$/);
const browser = await chromium.launch();
try {
  const context = await browser.newContext({viewport:{width:1440,height:1000},locale:"en-US"});
  const operationPath = `${origin}api/iac/native-operations/${fixture.databaseOperationId}`;
  async function observe() {
    const response = await context.request.get(operationPath);
    assert.equal(response.status(),200);
    return response.json();
  }
  const before = await observe();
  assert.equal(before.state,"needs_recovery");
  assert.equal(before.action,"update_database_connection");
  assert.equal(before.ownership.resource.id,fixture.databaseId);
  const page = await context.newPage();
  await page.addInitScript(() => localStorage.setItem("firecrab.locale","en"));
  await page.goto(`${origin}#/stacks`);
  const work = page.getByRole("region",{name:"Native operations",exact:true});
  const card = work.locator(`[data-operation-id="${fixture.databaseOperationId}"]`);
  await expect(card.getByText("Update database connection",{exact:true})).toBeVisible();
  await expect(card.getByText("Needs recovery",{exact:true})).toBeVisible();
  const files = [];
  async function capture(name) {
    const filename = `iac-548-database-${name}-${date}.png`;
    await card.scrollIntoViewIfNeeded();
    await card.screenshot({path:path.join(output,filename)});
    files.push(filename);
  }
  await capture("desktop");
  await page.setViewportSize({width:390,height:844});
  await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  await capture("mobile");
  await card.getByRole("button",{name:"Recover original operation",exact:true}).click();
  await expect(work.getByRole("alert")).toContainText("Original operation still needs recovery.",{timeout:20000});
  const after = await observe();
  assert.equal(after.state,"needs_recovery");
  assert.equal(after.operationId,before.operationId);
  assert.equal(after.executionGeneration,1);
  assert.deepEqual(after.ownership,before.ownership);
  await expect(card.locator("dt",{hasText:"Recovery attempt"}).locator("+ dd")).toHaveText("1");
  await capture("recovery-mobile");
  await page.setViewportSize({width:1440,height:1000});
  await work.screenshot({path:path.join(output,`iac-548-database-recovery-desktop-${date}.png`)});
  files.push(`iac-548-database-recovery-desktop-${date}.png`);

  // A real failed verification must not create a saved connection or leak credentials.
  const list = async () => (await (await context.request.get(`${origin}api/pipe/databases`)).json());
  const savedBefore = await list();
  const id = randomUUID();
  const body = {name:"Rejected isolated QA input",provider:"local",uri:`postgresql://fixture:private-create-credential@${fixture.databaseFixtureAddress}/iac_verification_fixture`,tlsMode:"disable",writeEnabled:false};
  const failed = await context.request.post(`${origin}api/pipe/databases`,{headers:{"x-firecrab-operation-id":id},data:body});
  assert.equal(failed.status(),502);
  assert.equal(failed.headers()["x-firecrab-operation-id"],id);
  const receipt = await context.request.get(`${origin}api/iac/native-operations/${id}`);
  assert.equal(receipt.status(),200);
  const failedOperation = await receipt.json();
  assert.equal(failedOperation.state,"failed");
  const retry = await context.request.post(`${origin}api/pipe/databases`,{headers:{"x-firecrab-operation-id":id},data:body});
  assert.equal(retry.status(),502);
  assert.deepEqual(await list(),savedBefore);
  const combined = JSON.stringify({before,after,failedOperation,error:await failed.json()});
  assert(!combined.includes("private-create-credential"));
  assert(!combined.includes("private-fixture-credential"));
  const hashes = {};
  for (const file of files) hashes[file] = createHash("sha256").update(await readFile(path.join(output,file))).digest("hex");
  const evidence = {sourceCommit,sourceDirty:false,capturedOn:date,fixture:true,scope:fixture.databaseFixture,
    before,after,failedOperation,savedConnectionCount:savedBefore.length,
    checks:["actual Axum/SQLite HTTP API and compiled React dashboard","same operation/resource UUID and ownership after recovery","recovery attempt advanced to 1","unavailable database remains uncertain, never falsely succeeded","known failed create and exact retry add no saved connection","public responses redact fixture credentials","mobile layout does not overflow"],
    notRun:["live PostgreSQL/MySQL or TLS verification","SQL execution","KVM guest boot","Terraform provider","expanded native Stack"],files:hashes};
  await writeFile(path.join(output,"iac-548-database-evidence.json"),JSON.stringify(evidence,null,2)+"\n");
  await context.close();
  console.log("Actual API checks and four English database recovery screenshots passed; no live SQL evidence claimed.");
} finally {
  await browser.close();
}
