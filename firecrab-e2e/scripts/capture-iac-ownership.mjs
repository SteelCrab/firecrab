/** Actual compiled dashboard + isolated Axum/SQLite fixture; no browser API mocks. */
import assert from "node:assert/strict";
import { mkdir, readFile, writeFile, unlink } from "node:fs/promises";
import { createHash, randomUUID } from "node:crypto";
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
const captureDate = process.env.FIRECRAB_IAC_QA_DATE || new Intl.DateTimeFormat("en-CA").format(new Date());
assert.match(captureDate, /^\d{4}-\d{2}-\d{2}$/);
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
  assert.deepEqual(capabilities.transferKinds, ["network", "vm", "service", "storage_root", "volume", "security_group", "shell", "schedule", "pool", "queue", "database_connection", "storage_connection", "secret", "secret_connection", "auth", "registry", "ai_connection", "ai_model", "sandbox_environment", "sandbox_project", "stack_source"]);
  const observed = await context.request.get(`${origin}api/iac/owners`);
  assert.equal(observed.status(), 200);
  assert.deepEqual((await observed.json()).filter(item=>item.owner.manager==="terraform"), fixture.ownerships);
  const cliPath = process.env.FIRECRAB_IAC_QA_CLI;
  assert.ok(cliPath && path.isAbsolute(cliPath), "set the compiled CLI absolute path for native recovery evidence");
  function command(args) {return JSON.parse(execFileSync(cliPath,["--api",fixture.url,...args],{encoding:"utf8"}));}
  function cli(args) {return command(["iac",...args]);}
  const project=fixture.nativeRecoveryProjectId;
  assert.match(project,/^[0-9a-f-]{36}$/);
  const failure=await context.request.delete(`${origin}api/ai-sandbox/projects/${project}`);
  assert.equal(failure.status(),503);
  const pending=cli(["native-operations","--kind","sandbox_project","--id",project]);
  assert.equal(pending.length,1);
  const nativeBefore=pending[0];
  assert.equal(nativeBefore.state,"needs_recovery");
  assert.equal(nativeBefore.errorCode,"project_cleanup_incomplete");
  assert.equal(nativeBefore.ownership.resource.id,project);
  assert.deepEqual(cli(["native-operation","--id",nativeBefore.operationId]),nativeBefore);
  assert.equal(path.basename(fixture.nativeRecoveryWorkspace),project);
  assert.equal(await readFile(fixture.nativeRecoveryWorkspace,"utf8"),"fixture cleanup failure");
  await unlink(fixture.nativeRecoveryWorkspace);
  await mkdir(fixture.nativeRecoveryWorkspace);
  await writeFile(path.join(fixture.nativeRecoveryWorkspace,"remove.txt"),"private fixture output");
  const nativeAfter=cli(["recover-native-operation","--id",nativeBefore.operationId]);
  assert.equal(nativeAfter.state,"succeeded");
  assert.equal(nativeAfter.operationId,nativeBefore.operationId);
  assert.deepEqual(nativeAfter.ownership,nativeBefore.ownership);
  assert.deepEqual(cli(["recover-native-operation","--id",nativeBefore.operationId]),nativeAfter);
  assert.equal(await readFile(fixture.nativeRecoveryBorrowedFile,"utf8"),"borrowed contents");
  const nativeRecovery={before:nativeBefore,after:nativeAfter,checks:["real filesystem failure","CLI status discovery","same operation and project UUID after recovery","immutable completed retry","borrowed source retained"],cliSha256:createHash("sha256").update(await readFile(cliPath)).digest("hex")};
  const sourceName=fixture.nativeSourceStackName;
  const sourceBefore=command(["stack","source","status",sourceName]);
  assert.equal(sourceBefore.generation,1);
  const removalFile=path.join(output,"source-removal-request.json");
  const removal={operationId:randomUUID(),expectedEndpointId:ownership.endpointId,expectedStackId:fixture.nativeSourceStackId,expectedGeneration:1};
  await writeFile(removalFile,JSON.stringify(removal));
  const sourceRemoved=command(["stack","source","remove",sourceName,"--file",removalFile]);
  assert.equal(sourceRemoved.state,"succeeded");assert.equal(sourceRemoved.action,"delete_stack_source");
  assert.equal(command(["stack","source","generation",sourceName]).generation,2);
  assert.equal(command(["stack","source","history",sourceName]).length,1);
  const sourceFile=path.join(output,"source-recreated.json");
  await writeFile(sourceFile,JSON.stringify(sourceBefore.source));
  const recreated=command(["stack","source","configure",sourceName,"--file",sourceFile,"--expected-generation","2"]);
  assert.equal(recreated.generation,3);
  assert.deepEqual(command(["stack","source","remove",sourceName,"--file",removalFile]),sourceRemoved);
  assert.equal(command(["stack","source","status",sourceName]).generation,3);
  const retained=await context.request.get(`${origin}api/micro-networks/${fixture.nativeSourceNetworkId}`);
  assert.equal(retained.status(),200);assert.equal((await retained.json()).id,fixture.nativeSourceNetworkId);
  const sourceRemoval={operation:sourceRemoved,recreatedGeneration:3,retainedNetworkId:fixture.nativeSourceNetworkId,checks:["actual CLI source removal","generation retained at 2","history retained","recreated source generation 3","old delete retry preserves recreated source","bound network retained"],fixtureObservation:"initial Git check metadata was created locally; no GitHub repository fetch"};
  let blockedConfigurations = 0;
  for (const resource of fixture.ownerships) {
    const namespace = {storage_root:"micro-storages",volume:"microblockstorage",security_group:"security-groups",shell:"shells",schedule:"schedules",pool:"pools",queue:"queues",database_connection:"pipe/databases",storage_connection:"pipe/storage",secret:"secrets",secret_connection:"secrets/connections",sandbox_project:"ai-sandbox/projects"}[resource.resource.kind];
    if (!namespace) continue;
    const rejected = await context.request.delete(`${origin}api/${namespace}/${resource.resource.id}`);
    assert.equal(rejected.status(), 409);
    assert.equal((await rejected.json()).error.code, "iac_resource_managed");
    blockedConfigurations++;
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
  await page.getByText(sourceName,{exact:true}).first().evaluate(element=>element.scrollIntoView({block:"start"}));
  await page.waitForTimeout(500);
  await page.screenshot({path:path.join(output,`iac-548-ownership-source-${captureDate}.png`),fullPage:true});
  await page.getByText(fixture.ownerships[0].owner.key, { exact: true }).evaluate(element => element.scrollIntoView({ block: "center" }));
  await page.waitForTimeout(500);
  await page.screenshot({ path: path.join(output, `iac-548-ownership-overview-${captureDate}.png`), fullPage: true });
  const focus = fixture.ownerships.find(owned => owned.resource.kind === "volume") || ownership;
  async function focusOwnership() {
    await page.getByText(focus.owner.key, { exact: true }).evaluate(element => element.scrollIntoView({ block: "center" }));
    const generation = await page.getByText(String(focus.generation), { exact: true }).last().boundingBox();
    assert.ok(generation && generation.y >= 0 && generation.y + generation.height <= page.viewportSize().height, "the focused ownership generation must be fully visible");
  }
  await focusOwnership();
  await page.waitForTimeout(500);
  await page.screenshot({ path: path.join(output, `iac-548-ownership-desktop-${captureDate}.png`), fullPage: true });
  await page.setViewportSize({ width: 375, height: 812 });
  await page.getByText(ownership.owner.key, { exact: true }).waitFor();
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth), "ownership must fit the mobile viewport");
  await focusOwnership();
  await page.waitForTimeout(500);
  await page.screenshot({ path: path.join(output, `iac-548-ownership-mobile-${captureDate}.png`), fullPage: true });
  assert.deepEqual(errors, []);
  await writeFile(path.join(output, "iac-548-ownership-evidence.json"), JSON.stringify({
    capturedAt: new Date().toISOString(), captureDate, sourceCommit, sourceDirty, focusResource:focus.resource, versions:{browser:browser.version(),node:process.version}, fixture: true, transport: "actual local Axum HTTP; no mocked API responses",
    runtime: "compiled WASM dashboard with disposable SQLite resource records; no Linux/KVM boot",
    ownership, ownerships:fixture.ownerships,nativeRecovery,sourceRemoval, checks: ["endpoint identity", `43 inventory kinds; ${capabilities.transferKinds.length} transfer adapters`, "actual ownership read", "direct PATCH rejected with 409", `${blockedConfigurations} configuration kinds reject direct DELETE with 409`, "immutable operation acknowledgement", `${fixture.ownerships.length} Terraform ownership records and one retained native binding in the actual dashboard`, "desktop dashboard", "375px mobile without horizontal overflow", "no browser page errors"],
  }, null, 2) + "\n");
  console.log("Actual API ownership, mutation rejection, receipt and three dashboard screenshots verified.");
} finally {
  await browser.close();
}
