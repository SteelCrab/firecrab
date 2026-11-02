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
  assert.deepEqual(capabilities.transferKinds, ["network", "vm", "service", "storage_root", "volume", "security_group", "shell", "schedule", "pool", "queue", "database_connection", "storage_connection", "secret", "secret_connection", "auth", "registry", "ai_connection", "ai_model", "sandbox_environment", "sandbox_project", "stack_source", "sandbox_access", "queue_engine", "cluster_host", "cluster_placement", "cluster_service", "service_otlp", "service_deploy_token"]);
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
  const states = new Map();
  for (const record of fixture.ownerships) {
    if (!states.has(record.owner.id)) states.set(record.owner.id,[]);
    states.get(record.owner.id).push(record);
  }
  assert.ok(states.size>=2,"fixture must exercise separate Terraform state owners");
  assert.equal(await page.locator('.stack-card[data-manager="terraform"]').count(),states.size);
  for(const [id,records] of states) {
    const card=page.locator(`.stack-card[data-manager="terraform"][data-owner-id="${id}"]`);
    assert.equal(await card.count(),1);
    assert.deepEqual((await card.locator('.stack-resource').evaluateAll(items=>items.map(item=>item.dataset.resourceId))).sort(),records.map(item=>item.resource.id).sort());
    for(const kind of new Set(records.map(item=>item.resource.kind))) {
      const section=card.locator(`.stack-resource-group[data-kind="${kind}"]`);
      assert.equal(await section.count(),1);
      assert.equal(await section.locator('.stack-resource-heading svg').count(),1);
      assert.equal(Number(await section.locator('.stack-resource-count').innerText()),records.filter(item=>item.resource.kind===kind).length);
    }
  }
  const nativeCard=page.locator(`.stack-card[data-manager="stack"][data-owner-id="${fixture.nativeSourceStackId}"]`);
  assert.equal(await nativeCard.locator(`.stack-resource[data-resource-id="${fixture.nativeSourceNetworkId}"]`).count(),1,"native bindings and ownership must not duplicate the resource");
  await page.evaluate(async () => { await document.fonts.ready; });
  await page.getByText(sourceName,{exact:true}).first().evaluate(element=>element.scrollIntoView({block:"start"}));
  await page.waitForTimeout(500);
  await page.screenshot({path:path.join(output,`iac-548-ownership-source-${captureDate}.png`),fullPage:true});
  await page.locator('.stack-card[data-manager="terraform"]').first().evaluate(element => element.scrollIntoView({ block: "start" }));
  await page.waitForTimeout(500);
  await page.screenshot({ path: path.join(output, `iac-548-ownership-overview-${captureDate}.png`), fullPage: true });
  const focus = fixture.ownerships.find(owned => owned.resource.kind === "queue_engine") || ownership;
  async function focusOwnership() {
    const resource=page.locator(`.stack-resource[data-resource-id="${focus.resource.id}"]`);
    await resource.evaluate(element => element.scrollIntoView({ block: "center" }));
    const generation = await resource.getByText(String(focus.generation), { exact: true }).boundingBox();
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
  const access=fixture.ownerships.find(item=>item.resource.kind==="sandbox_access");
  assert.ok(access,"fixture must include an owned access policy");
  const accessPath=`${origin}api/ai-sandbox/access/${access.resource.id}`;
  const originalPolicy=command(["sandbox","get-access",access.resource.id]);
  assert.ok(originalPolicy.grants.length>0);
  const replacement=await context.request.put(accessPath,{data:originalPolicy});
  assert.equal(replacement.status(),409);
  assert.equal((await replacement.json()).error.code,"iac_ownership_conflict");
  assert.deepEqual(command(["sandbox","revoke-access",access.resource.id]),{grants:[]});
  const accessOperations=cli(["native-operations","--kind","sandbox_access","--id",access.resource.id]);
  assert.equal(accessOperations.length,1);
  const accessOperation=accessOperations[0];
  assert.equal(accessOperation.action,"revoke_sandbox_access");
  assert.equal(accessOperation.state,"succeeded");
  assert.deepEqual(accessOperation.ownership,access);
  const released=cli(["owner","--kind","sandbox_access","--id",access.resource.id]);
  assert.equal(released.owner,null);
  assert.equal(released.generation,access.generation+1);
  assert.deepEqual(cli(["recover-native-operation","--id",accessOperation.operationId]),accessOperation);
  assert.deepEqual(command(["sandbox","get-access",access.resource.id]),{grants:[]});
  await page.locator(`.stack-resource[data-resource-id="${access.resource.id}"]`).waitFor({state:"detached"});
  const accessRevocation={operation:accessOperation,released,checks:["actual CLI policy observation and revocation","managed grant replacement rejected","atomic deny and authority release","immutable completed recovery","React poll removes revoked ownership"]};
  const engineOwner=fixture.ownerships.find(item=>item.resource.kind==="queue_engine");
  assert.ok(engineOwner,"fixture must include the exact reserved broker owner");
  for(const action of ["start","stop"]) {
    const denied=await context.request.post(`${origin}api/queue/engine/${action}`);
    assert.equal(denied.status(),409);
    assert.equal((await denied.json()).error.code,"iac_ownership_conflict");
  }
  const releaseFile=path.join(output,"queue-engine-release.json");
  const release={operationId:randomUUID(),expectedEndpointId:engineOwner.endpointId,resource:engineOwner.resource,
    expectedGeneration:engineOwner.generation,expectedOwner:engineOwner.owner,owner:null};
  await writeFile(releaseFile,JSON.stringify(release));
  const engineReleased=cli(["change-owner","--file",releaseFile]);
  assert.equal(engineReleased.owner,null);
  const stopId=randomUUID();
  const accepted=await context.request.post(`${origin}api/queue/engine/stop`,{headers:{"x-firecrab-operation-id":stopId}});
  assert.equal(accepted.status(),202);
  assert.equal(accepted.headers()["x-firecrab-operation-id"],stopId);
  async function engineFailure() {
    const deadline=Date.now()+5000;
    while(Date.now()<deadline) {
      const operation=cli(["native-operation","--id",stopId]);
      if(operation.state==="needs_recovery") return operation;
      await new Promise(resolve=>setTimeout(resolve,25));
    }
    throw new Error("expected the deliberately absent reserved VM to retain a recovery fence");
  }
  const failedStop=await engineFailure();
  assert.equal(failedStop.ownership.resource.id,engineOwner.resource.id);
  assert.equal(failedStop.action,"stop_queue_engine");
  assert.equal(failedStop.errorCode,"queue_engine_incomplete");
  const resumed=cli(["recover-native-operation","--id",stopId]);
  assert.equal(resumed.operationId,stopId);
  assert.equal(resumed.executionGeneration,1);
  const failedRecovery=await engineFailure();
  assert.equal(failedRecovery.executionGeneration,1);
  assert.deepEqual(failedRecovery.ownership,failedStop.ownership);
  const retryStop=await context.request.post(`${origin}api/queue/engine/stop`,{headers:{"x-firecrab-operation-id":stopId}});
  assert.equal(retryStop.status(),202);
  assert.deepEqual(cli(["native-operation","--id",stopId]),failedRecovery);
  await page.setViewportSize({width:1440,height:1000});
  await page.goto(`${origin}#/queues`);
  await page.getByRole("button",{name:"Recover engine",exact:true}).waitFor();
  await page.screenshot({path:path.join(output,`iac-548-queue-recovery-desktop-${captureDate}.png`),fullPage:true});
  await page.setViewportSize({width:375,height:812});
  assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=window.innerWidth));
  await page.screenshot({path:path.join(output,`iac-548-queue-recovery-mobile-${captureDate}.png`),fullPage:true});
  const queueEngine={owned:engineOwner,released:engineReleased,failedStop,resumed,failedRecovery,
    fixtureObservation:"the broker VM is intentionally absent; its checkpoint says previously created, so native stop/recovery must fail without creating a replacement. No Linux/KVM boot or RabbitMQ connection occurred",
    checks:["Terraform-owned start/stop rejected","actual CLI explicit release","caller-retained stop operation UUID","native missing-VM failure retains fence","actual CLI original-operation recovery increments execution generation","same reserved VM identity","HTTP retry does not redispatch","actual React recovery button","375px without horizontal overflow"]};
  assert.deepEqual(errors, []);
  await writeFile(path.join(output, "iac-548-ownership-evidence.json"), JSON.stringify({
    capturedAt: new Date().toISOString(), captureDate, sourceCommit, sourceDirty, focusResource:focus.resource, versions:{browser:browser.version(),node:process.version}, fixture: true, transport: "actual local Axum HTTP; no mocked API responses",
    runtime: "compiled React dashboard with disposable SQLite resource records; no Linux/KVM boot",
    ownership, ownerships:fixture.ownerships,nativeRecovery,sourceRemoval,accessRevocation,queueEngine, checks: ["endpoint identity", `43 inventory kinds; ${capabilities.transferKinds.length} transfer adapters`, "actual ownership read", "direct PATCH rejected with 409", `${blockedConfigurations} configuration kinds reject direct DELETE with 409`, "immutable operation acknowledgement", `${fixture.ownerships.length} Terraform ownership records and one retained native binding in the actual dashboard before operator revocation`, `${states.size} Terraform states remain separate; each resource kind has its own icon section and count`, "native bindings are displayed once", "desktop dashboard", "375px mobile without horizontal overflow", "no browser page errors"],
  }, null, 2) + "\n");
  console.log("Actual API ownership, separate state cards, icon sections and six dashboard screenshots verified.");
} finally {
  await browser.close();
}
