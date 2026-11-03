/** Actual API/React and isolated Linux broker ownership acknowledgement recovery. */
import assert from "node:assert/strict";
import { readFile,writeFile,mkdir } from "node:fs/promises";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { chromium,expect } from "@playwright/test";
const [infoPath,outputPath]=process.argv.slice(2);
assert(infoPath && outputPath,"Usage: capture-iac-ingress.mjs QA_INFO OUTPUT_DIRECTORY");
const fixture=JSON.parse(await readFile(infoPath,"utf8"));assert.equal(fixture.fixture,true);
assert.match(fixture.container,/^firecrab-548-ingress-evidence-[a-zA-Z0-9-]+$/);
const origin=new URL(fixture.url);assert.equal(origin.hostname,"127.0.0.1");
const repo=fileURLToPath(new URL("../../",import.meta.url));
const sourceCommit=execFileSync("git",["rev-parse","HEAD"],{cwd:repo,encoding:"utf8"}).trim();
assert.equal(execFileSync("git",["status","--porcelain","--","firecrab-api","firecrab-cli","firecrab-stack","firecrab-helper","firecrab-helper-protocol","firecrab-frontend","firecrab-e2e/scripts/capture-iac-ingress.mjs"],{cwd:repo,encoding:"utf8"}).trim(),"","capture requires clean implementation and script");
const date=process.env.FIRECRAB_IAC_QA_DATE;assert.match(date||"",/^\d{4}-\d{2}-\d{2}$/);
const output=path.resolve(outputPath);await mkdir(output,{recursive:true});
const hash=bytes=>createHash("sha256").update(bytes).digest("hex");
const manifest=()=>execFileSync("docker",["exec",fixture.container,"cat","/fixture/conf/pipe/manifest.json"]);
const beforeHash=hash(manifest());
const browser=await chromium.launch();
try {
 const context=await browser.newContext({viewport:{width:1440,height:1000},locale:"en-US"});
 const page=await context.newPage();await page.addInitScript(()=>localStorage.setItem("firecrab.locale","en"));
 await page.goto(`${origin}#/stacks`);
 await expect(page.locator('[data-kind="ingress"]')).toHaveCount(2);
 const files=[];
 async function capture(name,target=page.getByRole("main")){const filename=`iac-548-ingress-${name}-${date}.png`;await target.scrollIntoViewIfNeeded();await target.screenshot({path:path.join(output,filename)});files.push(filename);}
 await capture("desktop");await page.setViewportSize({width:390,height:844});
 await expect.poll(()=>page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);await capture("mobile",page.locator(`[data-owner-id="${fixture.transfer.expectedOwner.id}"]`));
 assert.equal((await context.request.post(`${origin}qa/lose-next-owner-ack`)).status(),204);
 const failed=await context.request.post(`${origin}api/iac/owners/transition`,{data:fixture.transfer});assert.equal(failed.status(),503);
 const recovered=await context.request.get(`${origin}api/iac/owner-changes/${fixture.transfer.operationId}`);assert.equal(recovered.status(),200);
 const result=await recovered.json();assert.equal(result.operationId,fixture.transfer.operationId);assert.equal(result.ownership.generation,2);assert.deepEqual(result.ownership.owner,fixture.transfer.owner);
 const peer=await context.request.get(`${origin}api/iac/owners/ingress/${encodeURIComponent(fixture.ownerships[1].resource.id)}`);assert.deepEqual(await peer.json(),fixture.ownerships[1]);
 const retry=await context.request.post(`${origin}api/iac/owners/transition`,{data:fixture.transfer});assert.equal(retry.status(),200);assert.deepEqual(await retry.json(),result.ownership);
 assert.equal(hash(manifest()),beforeHash);
 await page.reload();await expect(page.locator('[data-kind="ingress"]')).toHaveCount(1);await expect(page.getByText("firecrab_ingress.app",{exact:true})).toBeVisible();await expect(page.getByText("firecrab_ingress.peer",{exact:true})).toBeVisible();
 await capture("recovery-mobile",page.locator(`[data-owner-id="${fixture.transfer.owner.id}"]`));await page.setViewportSize({width:1440,height:1000});await capture("recovery-desktop");
 const screenshots={};for(const name of files)screenshots[name]=hash(await readFile(path.join(output,name)));
 await writeFile(path.join(output,"iac-548-ingress-evidence.json"),JSON.stringify({sourceCommit,sourceDirty:false,capturedOn:date,fixture:true,scope:fixture.scope,originalRequest:fixture.transfer,recovered:result,peerOwnership:fixture.ownerships[1],manifestSha256Before:beforeHash,manifestSha256After:hash(manifest()),screenshots},null,2)+"\n");
 console.log(JSON.stringify({sourceCommit,files,operationId:result.operationId,generation:result.ownership.generation}));
} finally { await browser.close();await writeFile(infoPath.replace(/\.[^.]+$/,".stop"),""); }
