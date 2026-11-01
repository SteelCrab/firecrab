import {expect,test,type Page} from '@playwright/test';
import {readFileSync} from 'node:fs';
const profiles=JSON.parse(readFileSync(new URL('../../firecrab-api/src/code_agent_auth/adapters.json',import.meta.url),'utf8'));
const id='11111111-1111-4111-8111-111111111111',account='22222222-2222-4222-8222-222222222222',vm='33333333-3333-4333-8333-333333333333',instance='44444444-4444-4444-8444-444444444444',operation='55555555-5555-4555-8555-555555555555';
async function open(page:Page,options:{locale?:string;agent?:string;provider?:string;authentication?:string}={}){
 const agent=options.agent??'codex',provider=options.provider??'openai',authentication=options.authentication??'api_key';
 const profile=profiles.find((p:any)=>p.agent===agent&&p.provider===provider&&p.authentication===authentication);
 const state={rows:[] as any[],writes:[] as any[],connected:true,phase:'ready',revision:4,historyFailure:false,omitAdmission:false,loseResponse:false,delay:null as null|(()=>Promise<void>),historyDelay:null as null|(()=>Promise<void>)};
 const errors:string[]=[];page.on('pageerror',e=>errors.push(e.message));
 await page.addInitScript(locale=>localStorage.setItem('firecrab.locale',locale),options.locale??'en');
 const record=()=>({id,revision:state.revision,spec:{name:'web-app',github:{account:'octocat',owner:'team',repository:'web-app',branch:'feature/work'},runtime:'node_js',resources:{cpu:2,ram:4096,diskGb:16},agent:{agent,connectionId:account}},connections:{github:'present_unverified',ai:state.connected?'present_unverified':'missing'}});
 await page.route(/^https?:\/\/[^/]+\/api\//,async r=>{
  const req=r.request(),path=new URL(req.url()).pathname,send=(json:unknown,status=200)=>r.fulfill({json,status});
  if(req.method()!=='GET')state.writes.push({path,body:req.postDataJSON()});
  if(path==='/api/code/agent-auth/adapters')return send(profiles);
  if(path==='/api/pipe/ai/connections')return send(state.connected?[{id:account,name:'Shared development AUTH',provider,authentication,models:[],streaming:false,toolCalls:false}]:[]);
  if(path==='/api/code/workspaces')return send([record()]);
  if(path===`/api/code/workspaces/${id}`)return send(record());
  if(path===`/api/code/workspaces/${id}/runtime`)return send({workspaceId:id,configurationRevision:4,vmId:vm,runtimeId:instance,operationId:operation,action:'start',phase:state.phase,initialCommit:'a'.repeat(40),projectPath:'/home/fc-code/project',vmState:state.phase==='ready'?'running':'stopped',error:null,placement:{template:{name:'ubuntu-26.04',version:'v1'},microNetworkId:vm,storageRoot:'default'}});
  if(path===`/api/code/workspaces/${id}/editor`)return send({workspaceId:id,revision:state.revision,vmId:vm,runtimeId:instance,operationId:operation,phase:'ready',version:'4.144.0'});
  if(path===`/api/code/workspaces/${id}/agent-runs`){const rows=structuredClone(state.rows);if(state.historyDelay)await state.historyDelay();return state.historyFailure?send({error:{code:'unavailable'}},503):send(rows);}
  const match=path.match(/\/agent-runs\/([0-9a-f-]+)(\/cancel)?$/);
  if(match){const runId=match[1];let row=state.rows.find(v=>v.id===runId);
   if(match[2]){if(row)row.phase='cancelled';return send({id:runId,workspaceId:id,run:row??null});}
   if(req.method()==='GET')return row?send(row):send({error:{code:'not_found'}},404);
   if(!state.omitAdmission&&!row){const b=req.postDataJSON();row={id:runId,workspaceId:id,revision:b.expectedRevision,connectionId:account,agent,adapterId:profile.id,cliVersion:profile.cliVersion,model:b.model,vmId:vm,runtimeId:instance,operationId:operation,phase:'queued',output:'',truncated:false,providerRequests:0,error:null,exitCode:null,createdAtMs:Date.now(),updatedAtMs:Date.now()};state.rows.unshift(row);}
   if(state.delay)await state.delay();
   if(state.loseResponse||state.omitAdmission)return r.abort('failed');return send(row,202);
  }
  if(path==='/api/vms'||path==='/api/pipe/auth')return send([]);
  if(path==='/api/update')return send({current:'0.4.1',latest:'0.4.1',updateAvailable:false});
  return send({error:{code:'not_found'}},404);
 });await page.goto(`/#/code/${id}/editor`);await expect(page.locator('#code-editor h2')).toHaveText('web-app');return{state,errors,profile};
}
const panel=(page:Page)=>page.locator('#code-agent-execution');
const start=(page:Page,ko=false)=>page.getByRole('button',{name:ko?'에이전트 실행':'Run agent',exact:true});
for(const locale of ['en','ko'])for(const width of [1280,390])test(`${locale} runs assigned AUTH in the same workspace at ${width}px @dashboard`,async({page},info)=>{
 await page.setViewportSize({width,height:1000});const {state,errors}=await open(page,{locale});const ko=locale==='ko';
 await expect(start(page,ko)).toBeDisabled();await page.getByLabel(ko?'작업 지시':'Task',{exact:true}).fill('Add a health endpoint and update its documentation.');await expect(start(page,ko)).toBeEnabled();expect(state.writes).toHaveLength(0);
 await start(page,ko).click();await expect(panel(page)).toContainText(ko?'대기 중':'Queued');expect(state.writes).toHaveLength(1);
 expect(state.writes[0].body).toEqual({expectedRevision:4,vmId:vm,runtimeId:instance,operationId:operation,model:'',prompt:'Add a health endpoint and update its documentation.'});
 state.rows[0].phase='running';state.rows[0].model='gpt-5.4';state.rows[0].providerRequests=2;state.rows[0].output='Updated src/routes/health.ts\nHealth endpoint is ready.\n';
 await expect(panel(page).getByRole('status')).toContainText(ko?'실행 중':'Running');await expect(page.getByLabel(ko?'에이전트 출력':'Agent output',{exact:true})).toContainText('Health endpoint is ready.');
 await expect(page.getByRole('link',{name:ko?'Web VS Code 열기':'Open Web VS Code',exact:true})).toHaveAttribute('href',`/api/code/workspaces/${id}/editor/open`);
 await expect(panel(page)).toContainText('Shared development AUTH');await expect(start(page,ko)).toBeDisabled();await panel(page).scrollIntoViewIfNeeded();expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);
 if(!ko)await page.screenshot({path:info.outputPath(`code-agent-run-en-${width}.png`),fullPage:true});
 await page.getByRole('button',{name:ko?'실행 취소':'Cancel run',exact:true}).click();await expect(panel(page)).toContainText(ko?'취소됨':'Cancelled');expect(state.writes).toHaveLength(2);expect(errors).toEqual([]);
});
for(const [agent,provider] of [['claude_code','anthropic'],['opencode','openai'],['opencode','anthropic']])test(`${agent}/${provider} uses only its saved assignment @dashboard`,async({page})=>{
 const {state,errors,profile}=await open(page,{agent,provider});await page.getByLabel('Task',{exact:true}).fill('Update README');await page.locator('#code-agent-options > summary').click();await page.getByLabel('Model ID (optional)',{exact:true}).fill('fixture-model');await expect(start(page)).toBeEnabled();await start(page).click();await expect(panel(page)).toContainText('Queued');
 expect(state.writes[0].body.model).toBe('fixture-model');expect(Object.keys(state.writes[0].body).sort()).toEqual(['expectedRevision','model','operationId','prompt','runtimeId','vmId']);expect(state.rows[0].adapterId).toBe(profile.id);expect(errors).toEqual([]);
});
test('lost start response and reload only observe the existing run @dashboard',async({page})=>{
 const {state,errors}=await open(page);state.loseResponse=true;await page.getByLabel('Task',{exact:true}).fill('Update README');await expect(start(page)).toBeEnabled();await start(page).click();await expect(panel(page)).toContainText('Queued');expect(state.writes).toHaveLength(1);await page.reload();await expect(panel(page)).toContainText('Queued');expect(state.writes).toHaveLength(1);expect(errors).toEqual([]);
});
test('unknown admission requires an explicit retry with the same ID and task @dashboard',async({page})=>{
 const {state,errors}=await open(page);state.omitAdmission=true;await page.getByLabel('Task',{exact:true}).fill('Update README');await expect(start(page)).toBeEnabled();await start(page).click();await expect(page.getByRole('button',{name:'Retry same request'})).toBeEnabled();await page.waitForTimeout(3300);expect(state.writes).toHaveLength(1);await page.reload();await expect(page.getByRole('button',{name:'Retry same request'})).toBeEnabled();expect(state.writes).toHaveLength(1);
 state.omitAdmission=false;await page.getByRole('button',{name:'Retry same request'}).click();await expect(panel(page)).toContainText('Queued');expect(state.writes).toHaveLength(2);expect(state.writes[1]).toEqual(state.writes[0]);expect(errors).toEqual([]);
});
test('unknown start can be retired before a late POST arrives @dashboard',async({page})=>{
 const {state,errors}=await open(page);state.omitAdmission=true;await page.getByLabel('Task',{exact:true}).fill('Update README');await expect(start(page)).toBeEnabled();await start(page).click();await expect(page.getByRole('button',{name:'Retry same request'})).toBeEnabled();await page.getByRole('button',{name:'Cancel run'}).click();await expect(page.getByRole('button',{name:'Retry same request'})).toHaveCount(0);expect(state.writes[1].path).toBe(state.writes[0].path+'/cancel');expect(errors).toEqual([]);
});
test('unsupported ChatGPT cannot start work @dashboard',async({page})=>{
 const {state,errors}=await open(page,{authentication:'chatgpt'});await page.getByLabel('Task',{exact:true}).fill('Update README');await expect(panel(page)).toContainText('Select a supported agent');await expect(start(page)).toBeDisabled();expect(state.writes).toHaveLength(0);expect(errors).toEqual([]);
});
test('output is escaped and an interrupted run never restarts after AUTH removal @dashboard',async({page})=>{
 const {state,errors}=await open(page);await page.getByLabel('Task',{exact:true}).fill('Update README');await expect(start(page)).toBeEnabled();await start(page).click();await expect(panel(page)).toContainText('Queued');state.rows[0].phase='interrupted';state.rows[0].error='bindings_changed';state.rows[0].output='<img src=x onerror="window.agentInjected=true">';state.connected=false;
 await expect(panel(page)).toContainText('Interrupted');await expect(page.getByLabel('Agent output',{exact:true})).toHaveText(state.rows[0].output);expect(await page.evaluate(()=>Boolean((window as any).agentInjected))).toBe(false);await expect(start(page)).toBeDisabled();expect(state.writes).toHaveLength(1);expect(errors).toEqual([]);
});
test('storage failure does not dispatch a billable request @dashboard',async({page})=>{
 const {state,errors}=await open(page);await page.getByLabel('Task',{exact:true}).fill('Update README');await expect(start(page)).toBeEnabled();await page.evaluate(()=>{Storage.prototype.setItem=()=>{throw new Error('blocked');};});await start(page).click();await expect(panel(page).getByRole('alert')).toContainText('session storage');expect(state.writes).toHaveLength(0);expect(errors).toEqual([]);
});

test('history outage blocks new runs and fresh status recovers @dashboard',async({page})=>{
 const {state,errors}=await open(page);await page.getByLabel('Task',{exact:true}).fill('Update README');await expect(start(page)).toBeEnabled();state.historyFailure=true;await expect(panel(page).getByRole('alert')).toBeVisible();await expect(start(page)).toBeDisabled();state.historyFailure=false;await expect(start(page)).toBeEnabled();expect(state.writes).toHaveLength(0);expect(errors).toEqual([]);
});
test('navigation drops a delayed observation and resumes polling on return @dashboard',async({page})=>{
 const {state,errors}=await open(page);await page.getByLabel('Task',{exact:true}).fill('Update README');await expect(start(page)).toBeEnabled();let release!:()=>void;state.historyDelay=()=>new Promise<void>(r=>{release=r;});await expect.poll(()=>Boolean(release)).toBe(true);await page.evaluate(()=>{location.hash='#/vms';});state.historyDelay=null;release();await page.evaluate(id=>{location.hash='#/code/'+id+'/editor';},id);await expect(page.getByLabel('Task',{exact:true})).toHaveValue('');await page.getByLabel('Task',{exact:true}).fill('New task after returning');await expect(start(page)).toBeEnabled();expect(errors).toEqual([]);
});

test('task is the only visible input and details survive observation polling @dashboard',async({page})=>{
 const {state,errors}=await open(page);await expect(panel(page).getByText('Shared development AUTH',{exact:false})).toBeVisible();expect(await panel(page).locator('input:visible').count()).toBe(0);expect(await panel(page).locator('textarea:visible').count()).toBe(1);await expect(page.locator('#code-workspace-settings')).not.toHaveAttribute('open');
 await page.locator('#code-agent-options > summary').click();await page.getByLabel('Model ID (optional)',{exact:true}).fill('fixture-model');await page.waitForTimeout(3300);await expect(page.getByLabel('Model ID (optional)',{exact:true})).toBeVisible();await expect(page.getByLabel('Model ID (optional)',{exact:true})).toHaveValue('fixture-model');expect(state.writes).toHaveLength(0);expect(errors).toEqual([]);
});
