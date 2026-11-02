import { expect, test, type Page } from "@playwright/test";

const id="11111111-1111-4111-8111-111111111111", vm="22222222-2222-4222-8222-222222222222", network="33333333-3333-4333-8333-333333333333", boot="44444444-4444-4444-8444-444444444444";
const base="/api/code/workspaces", sha="a".repeat(40);
const spec={name:"web-app",github:{account:"octocat",owner:"team",repository:"web-app",branch:"feature/work"},runtime:"node_js",resources:{cpu:2,ram:4096,diskGb:16},agent:null};
async function open(page:Page, options:{locale?:"en"|"ko";setup?:boolean;running?:boolean}={}) {
  const state={records:options.setup?[]:[{id,revision:4,spec:structuredClone(spec)}] as any[], runtime:null as any, sequence:0, writes:[] as any[], clones:[] as any[], files:"initial", failure:"" as ""|"accepted"|"not_sent"|"bad_receipt", outage:false, github:true, delayRuntime:null as null|(()=>Promise<void>), reads:0,
    images:[{alias:"ubuntu-26.04",version:"v1",installed:true,minDiskGb:8},{alias:"alpine-3.23",version:"v2",installed:true,minDiskGb:1},{alias:"too-big",version:"v1",installed:true,minDiskGb:64},{alias:"not-installed",version:"v1",installed:false,minDiskGb:1},{alias:"__microboot",version:"v1",installed:true,minDiskGb:0}],
    networks:[{id:network,name:"Development",internetEnabled:true},{id:boot,name:"Isolated",internetEnabled:false}], storage:[{id:"fast",name:"Fast volume",kind:"env"},{id:"archive",name:"Archive",kind:"env"}]};
  const errors:string[]=[];page.on("pageerror",e=>errors.push(e.message));
  await page.addInitScript(locale=>localStorage.setItem("firecrab.locale",locale),options.locale??"en");
  function record(w:any){return {...structuredClone(w),connections:{github:state.github?"present_unverified":"missing",ai:"not_required"}};}
  function admit(workspace:any,action:string,placement?:any) {
    state.sequence++;
    state.runtime={workspaceId:workspace.id,vmId:vm,configurationRevision:workspace.revision,operationId:`55555555-5555-4555-8555-${String(state.sequence).padStart(12,"0")}`,action,phase:"queued",projectPath:"/home/fc-code/project",runtimeId:state.runtime?.runtimeId??null,initialCommit:state.runtime?.initialCommit??null,vmState:state.runtime?.vmState??"created",error:null,
      placement:state.runtime?.placement??{template:{name:placement.template,version:"v1"},microNetworkId:placement.microNetworkId,storageRoot:placement.storageRoot}};
  }
  function advance(phase:string) {
    state.runtime.phase=phase;
    if(phase==="ready") {state.runtime.vmState="running";state.runtime.runtimeId=boot;if(!state.runtime.initialCommit){state.runtime.initialCommit=sha;state.clones.push(structuredClone(state.records.find(w=>w.id===state.runtime.workspaceId).spec.github));}}
    if(phase==="stopped")state.runtime.vmState="stopped";
  }
  if(options.running){admit(state.records[0],"start",{template:"ubuntu-26.04",microNetworkId:network,storageRoot:"fast"});advance("ready");}
  await page.route(/^https?:\/\/[^/]+\/api\//,async r=>{
    const request=r.request(),path=new URL(request.url()).pathname,method=request.method();
    const send=(json:unknown,status=200)=>r.fulfill({json,status});
    if(method!=="GET")state.writes.push({path,method,body:request.postDataJSON()});
    if(method==="GET"&&/^\/api\/code\/workspaces\/[^/]+\/editor$/.test(path))return send(null);
    if(path==="/api/images")return state.outage?send({error:{code:"unavailable"}},503):send(state.images);
    if(path==="/api/micro-networks")return send(state.networks);
    if(path==="/api/storage")return send(state.storage);
    if(path==="/api/pipe/ai/connections")return send([]);
    if(path==="/api/pipe/auth")return send(state.github?[{provider:"github",account:"octocat"}]:[]);
    if(path==="/api/code/github/repositories")return send({account:"octocat",repositories:[{owner:"team",repository:"web-app",defaultBranch:"feature/work",private:true}],nextPage:null});
    if(path==="/api/code/github/branches")return send({account:"octocat",owner:"team",repository:"web-app",branches:["feature/work"],nextPage:null});
    if(path===base)return send(state.records.map(record));
    const match=path.match(/^\/api\/code\/workspaces\/([^/]+)(\/runtime(?:\/(?:start|stop))?)?$/);
    if(match){
      const key=match[1];let w=state.records.find(w=>w.id===key);
      if(!match[2]&&method==="PUT"){w={id:key,revision:1,spec:request.postDataJSON().spec};state.records.push(w);return send(record(w));}
      if(!w)return send({error:{code:"not_found"}},404);
      if(!match[2])return send(record(w));
      if(method==="GET"){
        state.reads++;const snapshot=structuredClone(state.runtime),delay=state.delayRuntime;state.delayRuntime=null;
        if(delay)await delay();return snapshot&&snapshot.workspaceId===key?send(snapshot):send({error:{code:"not_found"}},404);
      }
      const body=request.postDataJSON();if(body.expectedRevision!==w.revision)return send({error:{code:"code_workspace_revision_conflict"}},409);
      const failure=state.failure;state.failure="";if(failure==="not_sent")return r.abort("failed");
      admit(w,match[2].endsWith("/stop")?"stop":"start",body);
      if(failure==="accepted"){advance(state.runtime.action==="stop"?"stopped":"ready");return r.abort("failed");}
      if(failure==="bad_receipt")return send({...state.runtime,workspaceId:boot,phase:"ready"},202);
      return send(structuredClone(state.runtime),202);
    }
    if(path==="/api/update")return send({current:"0.4.1",latest:"0.4.1",updateAvailable:false});
    if(path==="/api/vms")return send([]);
    return send({error:{code:"not_found"}},404);
  });
  await page.goto(options.setup?"/#/code/new":`/#/code/${id}`);
  await page.locator(options.setup?"#code-setup-options > summary":"#code-overview-settings > summary").click();
  return {state,errors,advance,admit};
}
const create=(p:Page,ko=false)=>p.getByRole("button",{name:ko?"MicroVM 생성 및 시작":"Create and start MicroVM",exact:true});
const control=(p:Page,name:string)=>p.getByRole("button",{name,exact:true});

for(const locale of ["en","ko"] as const)for(const width of [1280,390])test(`${locale} saved creation starts the selected checkout and resumes retained files at ${width}px @dashboard`,async({page},info)=>{
  test.setTimeout(45000);
  const ko=locale==="ko";await page.setViewportSize({width,height:1000});const {state,errors,advance}=await open(page,{locale,setup:true});
  await page.getByRole("textbox",{name:ko?"워크스페이스 이름":"Workspace name",exact:true}).fill("web-app");
  await page.getByRole("combobox",{name:ko?"GitHub 저장소":"GitHub repository",exact:true}).selectOption("team/web-app");
  await control(page,ko?"워크스페이스 생성":"Create workspace").click();await expect(page.locator("#code-editor h2")).toHaveText("web-app");await page.locator("#code-workspace-settings > summary").click();
  await expect(create(page,ko)).toBeEnabled();expect(state.writes).toHaveLength(1);
  const images=page.getByRole("combobox",{name:ko?"설치된 이미지":"Installed image",exact:true});await expect(images.locator("option")).toHaveCount(2);
  await page.getByRole("combobox",{name:ko?"스토리지 루트":"Storage root",exact:true}).selectOption("archive");
  await create(page,ko).scrollIntoViewIfNeeded();expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);
  if(!ko)await page.screenshot({path:info.outputPath(`code-runtime-en-${width}.png`),fullPage:true});
  await create(page,ko).click();await expect(page.locator(".code-runtime-status")).toHaveText(ko?"대기 중":"Queued");
  const target=state.records[0].id;expect(state.writes[1]).toEqual({path:`${base}/${target}/runtime`,method:"POST",body:{expectedRevision:1,template:"ubuntu-26.04",microNetworkId:network,storageRoot:"archive"}});
  for(const phase of ["provisioning","preparing","ready"]){advance(phase);await expect(page.locator(".code-runtime-status")).toHaveText(phase==="provisioning"?(ko?"MicroVM 생성 중":"Creating MicroVM"):phase==="preparing"?(ko?"프로젝트 준비 중":"Preparing project"):(ko?"MicroVM 준비 완료":"MicroVM ready"));}
  expect(state.clones).toEqual([state.records[0].spec.github]);state.files="dirty local edits";
  await control(page,ko?"MicroVM 중지":"Stop MicroVM").click();await expect(page.locator(".code-runtime-status")).toHaveText(ko?"대기 중":"Queued");advance("stopped");
  await expect(control(page,ko?"MicroVM 시작":"Start MicroVM")).toBeEnabled();await control(page,ko?"MicroVM 시작":"Start MicroVM").click();await expect(page.locator(".code-runtime-status")).toHaveText(ko?"대기 중":"Queued");advance("ready");
  await expect(page.locator(".code-runtime-status")).toHaveText(ko?"MicroVM 준비 완료":"MicroVM ready");
  if(!ko)await page.screenshot({path:info.outputPath(`code-runtime-ready-en-${width}.png`),fullPage:true});
  expect(state.runtime.vmId).toBe(vm);expect(state.files).toBe("dirty local edits");expect(state.clones).toHaveLength(1);
  expect(state.writes.slice(2).map(w=>w.body)).toEqual([{expectedRevision:1},{expectedRevision:1}]);expect(state.writes.every(w=>w.path.startsWith(base))).toBe(true);expect(errors).toEqual([]);
});

for(const width of [1280,390])test(`editor lifecycle controls retain the same runtime at ${width}px @dashboard`,async({page},info)=>{
  await page.setViewportSize({width,height:1000});
  const {state,errors,advance}=await open(page,{running:true});
  await page.getByRole("link",{name:"Open workspace",exact:true}).click();await page.locator("#code-workspace-settings > summary").click();
  await expect(page.locator("#code-editor .code-runtime-status")).toHaveText("MicroVM ready");
  await control(page,"Stop MicroVM").click();await expect(page.locator(".code-runtime-status")).toHaveText("Queued");advance("stopped");
  await expect(control(page,"Start MicroVM")).toBeEnabled();await control(page,"Start MicroVM").click();await expect(page.locator(".code-runtime-status")).toHaveText("Queued");advance("ready");
  await expect(page.locator(".code-runtime-status")).toHaveText("MicroVM ready");
  expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);
  expect(state.runtime.vmId).toBe(vm);expect(state.clones).toHaveLength(1);expect(state.writes.map(w=>w.path)).toEqual([`${base}/${id}/runtime/stop`,`${base}/${id}/runtime/start`]);expect(errors).toEqual([]);
  await page.screenshot({path:info.outputPath(`code-runtime-editor-en-${width}.png`),fullPage:true});
});

test("placement reads fail closed and preserve explicitly selected unavailable choices @dashboard",async({page})=>{
  const {state,errors}=await open(page);await expect(create(page)).toBeEnabled();
  state.images=[];await control(page,"Reload placement").click();await expect(create(page)).toBeDisabled();
  await expect(page.locator(".code-runtime-controls")).toContainText("Install an image");
  state.outage=true;await control(page,"Reload placement").click();await expect(page.locator(".code-runtime-controls")).toContainText("Could not load");expect(state.writes).toHaveLength(0);expect(errors).toEqual([]);
});
test("a lost response after completed provisioning never duplicates the VM @dashboard",async({page})=>{
  const {state,errors}=await open(page);await expect(create(page)).toBeEnabled();state.failure="accepted";await create(page).click();
  await expect(page.locator(".code-runtime-status")).toHaveText("MicroVM ready");await expect(control(page,"Check operation")).toHaveCount(0);
  expect(state.writes).toHaveLength(1);expect(state.sequence).toBe(1);expect(errors).toEqual([]);
});
test("an unreceived provisioning request survives reload and retries its exact original target @dashboard",async({page})=>{
  const {state,errors}=await open(page);await expect(create(page)).toBeEnabled();state.failure="not_sent";await create(page).click();
  await expect(page.locator(".code-runtime-controls")).toContainText("request result is unknown");await page.reload();
  await control(page,"Check operation").click();await expect(control(page,"Retry same request")).toBeVisible();await control(page,"Retry same request").click();
  await expect(page.locator(".code-runtime-status")).toHaveText("Queued");expect(state.writes).toHaveLength(2);expect(state.writes[1]).toEqual(state.writes[0]);expect(state.sequence).toBe(1);expect(errors).toEqual([]);
});
test("changed workspace configuration blocks an old creation intent before mutation @dashboard",async({page})=>{
  const {state,errors}=await open(page);await expect(create(page)).toBeEnabled();state.records[0].revision++;state.records[0].spec.github.branch="external-change";await create(page).click();
  await expect(page.locator(".code-runtime-controls")).toContainText("workspace or runtime changed");expect(state.writes).toHaveLength(0);expect(errors).toEqual([]);
});
test("another lifecycle action cannot be overwritten during interrupted stop recovery @dashboard",async({page})=>{
  const {state,errors,admit,advance}=await open(page,{running:true});state.failure="not_sent";await control(page,"Stop MicroVM").click();
  await expect(page.locator(".code-runtime-controls")).toContainText("request result is unknown");admit(state.records[0],"start");advance("ready");
  await control(page,"Check operation").click();await expect(page.locator(".code-runtime-controls")).toContainText("workspace or runtime changed");
  await expect(control(page,"Retry same request")).toHaveCount(0);expect(state.writes).toHaveLength(1);expect(errors).toEqual([]);
});
test("a mismatched admission response requires a fresh observation @dashboard",async({page})=>{
  const {state,errors}=await open(page);await expect(create(page)).toBeEnabled();state.failure="bad_receipt";await create(page).click();
  await expect(page.locator(".code-runtime-controls")).toContainText("request result is unknown");await expect(page.locator(".code-runtime-status")).toHaveText("Queued");
  await control(page,"Check operation").click();await expect(control(page,"Check operation")).toHaveCount(0);expect(state.writes).toHaveLength(1);expect(errors).toEqual([]);
});
test("late ready observations cannot restore readiness after stop admission @dashboard",async({page})=>{
  await page.clock.install();const {state,errors}=await open(page,{running:true});let release!:()=>void;state.delayRuntime=()=>new Promise<void>(r=>{release=r;});const before=state.reads;
  await page.clock.fastForward(3100);await expect.poll(()=>state.reads).toBeGreaterThan(before);await control(page,"Stop MicroVM").click();await expect(page.locator(".code-runtime-status")).toHaveText("Queued");
  const response=page.waitForResponse(r=>r.url().endsWith("/runtime"));release();await(await response).finished();await page.evaluate(()=>new Promise(requestAnimationFrame));
  await expect(page.locator(".code-runtime-status")).toHaveText("Queued");expect(errors).toEqual([]);
});
test("sync failure retains the running VM and offers only an explicit stop retry @dashboard",async({page})=>{
  await page.clock.install();const {state,errors}=await open(page,{running:true});state.runtime.action="stop";state.runtime.phase="failed";state.runtime.error="sync_failed";await page.clock.fastForward(3100);
  await expect(control(page,"Retry stop")).toBeEnabled();await expect(control(page,"Retry / recover MicroVM")).toBeDisabled();await control(page,"Retry stop").click();await expect(page.locator(".code-runtime-status")).toHaveText("Queued");
  expect(state.runtime.vmId).toBe(vm);expect(state.writes).toHaveLength(1);expect(state.writes[0].path).toBe(`${base}/${id}/runtime/stop`);expect(errors).toEqual([]);
});
test("first checkout needs GitHub while a confirmed checkout can resume without it @dashboard",async({page})=>{
  await page.clock.install();const {state,errors,admit,advance}=await open(page);await expect(create(page)).toBeEnabled();state.github=false;await page.clock.fastForward(3100);await expect(create(page)).toBeDisabled();
  admit(state.records[0],"start",{template:"ubuntu-26.04",microNetworkId:network,storageRoot:"fast"});advance("ready");state.runtime.phase="stopped";state.runtime.vmState="stopped";await page.clock.fastForward(3100);
  await expect(control(page,"Start MicroVM")).toBeEnabled();expect(state.writes).toHaveLength(0);expect(errors).toEqual([]);
});
