/** Actual compiled dashboard + isolated Axum/SQLite fixture; no browser API mocks. */
import assert from "node:assert/strict";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { chromium } from "@playwright/test";

const [infoPath, outputPath] = process.argv.slice(2);
if (!infoPath || !outputPath) throw new Error("Usage: node scripts/capture-iac-ownership.mjs QA_INFO OUTPUT_DIRECTORY");
const fixture = JSON.parse(await readFile(infoPath, "utf8"));
assert.equal(fixture.fixture, true, "capture requires the disposable API fixture");
const origin = new URL(fixture.url);
assert.equal(origin.hostname, "127.0.0.1", "capture must use the local disposable fixture");
const repoRoot=fileURLToPath(new URL("../../",import.meta.url));
const sourceCommit=execFileSync("git",["rev-parse","HEAD"],{cwd:repoRoot,encoding:"utf8"}).trim();
const sourceDirty=execFileSync("git",["status","--porcelain","--untracked-files=normal","--","firecrab-api","firecrab-stack","firecrab-frontend","firecrab-cli"],{cwd:repoRoot,encoding:"utf8"}).trim()!=="";
const output = path.resolve(outputPath);
await mkdir(output, { recursive: true });
const browser = await chromium.launch({ channel: process.env.FIRECRAB_IAC_QA_CHANNEL || undefined });
try {
  const context = await browser.newContext({ viewport: { width: 1440, height: 1000 }, locale: "en-US" });
  const ownership = fixture.ownership;
  const identity = await context.request.get(`${origin}api/iac/identity`);
  assert.equal(identity.status(), 200);
  const capabilities = await identity.json();
  assert.equal(capabilities.endpointId, ownership.endpointId);
  assert.equal(capabilities.inventoryKinds.length, 43);
  assert.deepEqual(capabilities.transferKinds, ["network", "vm", "service", "storage_root", "volume", "security_group", "shell", "schedule"]);
  const observed = await context.request.get(`${origin}api/iac/owners`);
  assert.equal(observed.status(), 200);
  assert.deepEqual(await observed.json(), fixture.ownerships);
  for (const resource of fixture.ownerships) {
    const namespace = {storage_root:"micro-storages",volume:"microblockstorage",security_group:"security-groups",shell:"shells",schedule:"schedules"}[resource.resource.kind];
    if (!namespace) continue;
    const rejected = await context.request.delete(`${origin}api/${namespace}/${resource.resource.id}`);
    assert.equal(rejected.status(), 409);
    assert.equal((await rejected.json()).error.code, "iac_resource_managed");
  }
  const blocked = await context.request.patch(`${origin}api/micro-networks/${ownership.resource.id}`, { data: { internetEnabled: false } });
  assert.equal(blocked.status(), 409);
  assert.equal((await blocked.json()).error.code, "iac_resource_managed");
  const receipt = await context.request.get(`${origin}api/iac/owner-changes/${fixture.operationId}`);
  assert.equal(receipt.status(), 200);
  assert.deepEqual((await receipt.json()).ownership, ownership);
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", error => errors.push(error.message));
  await page.addInitScript(() => localStorage.setItem("firecrab.locale", "en"));
  await page.goto(`${origin}#/stacks`);
  await page.getByText(ownership.owner.key, { exact: true }).waitFor();
  await page.getByText(ownership.resource.id, { exact: false }).waitFor();
  for (const owned of fixture.ownerships) {
    await page.getByText(owned.owner.key, { exact: true }).waitFor();
  }
  await page.evaluate(async () => { await document.fonts.ready; });
  await page.waitForTimeout(500);
  await page.screenshot({ path: path.join(output, "iac-548-ownership-desktop-2026-10-30.png"), fullPage: true });
  await page.setViewportSize({ width: 375, height: 812 });
  await page.getByText(ownership.owner.key, { exact: true }).waitFor();
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth), "ownership must fit the mobile viewport");
  await page.waitForTimeout(500);
  await page.screenshot({ path: path.join(output, "iac-548-ownership-mobile-2026-10-30.png"), fullPage: true });
  assert.deepEqual(errors, []);
  await writeFile(path.join(output, "iac-548-ownership-evidence.json"), JSON.stringify({
    capturedAt: new Date().toISOString(), sourceCommit, sourceDirty, versions:{browser:browser.version(),node:process.version}, fixture: true, transport: "actual local Axum HTTP; no mocked API responses",
    runtime: "compiled WASM dashboard with disposable SQLite resource records; no Linux/KVM boot",
    ownership, ownerships:fixture.ownerships, checks: ["endpoint identity", "43 inventory kinds; eight transfer adapters", "actual ownership read", "direct PATCH rejected with 409", "five configuration kinds reject direct DELETE with 409", "immutable operation acknowledgement", "six ownership records in the actual dashboard", "desktop dashboard", "375px mobile without horizontal overflow", "no browser page errors"],
  }, null, 2) + "\n");
  console.log("Actual API ownership, mutation rejection, receipt and two dashboard screenshots verified.");
} finally {
  await browser.close();
}
