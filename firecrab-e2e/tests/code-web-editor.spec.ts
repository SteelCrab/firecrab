import {expect,test,type Page} from '@playwright/test';
const id='11111111-1111-4111-8111-111111111111',vm='22222222-2222-4222-8222-222222222222',instance='33333333-3333-4333-8333-333333333333',operation='44444444-4444-4444-8444-444444444444';
async function open(page:Page,locale='en'){
 const state={phase:'ready',editor:null as any,revision:4,writes:[] as any[],failure:false,outage:false};
 const spec={name:'web-app',github:{account:'octocat',owner:'team',repository:'web-app',branch:'feature/work'},runtime:'node_js',resources:{cpu:2,ram:4096,diskGb:16},agent:null};
 const errors:string[]=[];page.on('pageerror',e=>errors.push(e.message));await page.addInitScript(locale=>localStorage.setItem('firecrab.locale',locale),locale);
 await page.route(/^https?:\/\/[^/]+\/api\//,async r=>{const req=r.request(),path=new URL(req.url()).pathname,send=(json:unknown,status=200)=>r.fulfill({json,status});
  if(req.method()!=='GET')state.writes.push({path,body:req.postDataJSON()});
  if(path==='/api/code/workspaces')return send([{id,revision:state.revision,spec,connections:{github:'present_unverified',ai:'not_required'}}]);
  if(path===`/api/code/workspaces/${id}`)return send({id,revision:state.revision,spec,connections:{github:'present_unverified',ai:'not_required'}});
  if(path===`/api/code/workspaces/${id}/runtime`)return send({workspaceId:id,configurationRevision:4,vmId:vm,runtimeId:instance,operationId:operation,action:'start',phase:state.phase,initialCommit:'a'.repeat(40),projectPath:'/home/fc-code/project',vmState:state.phase==='ready'?'running':'stopped',error:null,placement:{template:{name:'ubuntu-26.04',version:'v1'},microNetworkId:vm,storageRoot:'default'}});
  if(path===`/api/code/workspaces/${id}/editor`){if(req.method()==='POST'){state.editor={workspaceId:id,revision:state.revision,vmId:vm,runtimeId:instance,operationId:operation,phase:'preparing',version:'4.144.0'};if(state.failure)return r.abort('failed');return send(state.editor,202);}return state.outage?send({error:{code:'unavailable'}},503):send(state.editor);}
  if(path==='/api/vms'||path==='/api/pipe/ai/connections'||path==='/api/pipe/auth')return send([]);
  if(path==='/api/update')return send({current:'0.4.1',latest:'0.4.1',updateAvailable:false});return send({error:{code:'not_found'}},404);
 });await page.goto(`/#/code/${id}/editor`);return{state,errors};
}
for(const locale of ['en','ko'])for(const width of [1280,390])test(`${locale} prepares the same editor and terminal at ${width}px @dashboard`,async({page},info)=>{
 await page.setViewportSize({width,height:1000});const{state,errors}=await open(page,locale);const ko=locale==='ko';
 const prepare=page.getByRole('button',{name:ko?'편집 도구 준비':'Prepare editor tools',exact:true});await expect(prepare).toBeEnabled();await prepare.click();
 await expect(page.locator('.code-tools')).toContainText(ko?'편집기 준비 중':'Preparing editor');
 expect(state.writes).toEqual([{path:`/api/code/workspaces/${id}/editor`,body:{expectedRevision:4,vmId:vm,runtimeId:instance,operationId:operation}}]);
 state.editor.phase='ready';await expect(page.getByRole('link',{name:ko?'Web VS Code 열기':'Open Web VS Code',exact:true})).toHaveAttribute('href',`/api/code/workspaces/${id}/editor/open`);
 const terminal=page.getByRole('link',{name:ko?'터미널 열기':'Open terminal',exact:true});await expect(terminal).toHaveAttribute('href',`/api/code/workspaces/${id}/terminal/open`);await expect(terminal).toHaveAttribute('rel','noreferrer');
 await expect(page.getByRole('button',{name:ko?'에이전트 실행':'Run agent',exact:true})).toBeDisabled();expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);
 if(!ko)await page.screenshot({path:info.outputPath(`code-web-editor-en-${width}.png`),fullPage:true});expect(errors).toEqual([]);
});
test('lost preparation responses observe the existing job without replay @dashboard',async({page})=>{const{state,errors}=await open(page);state.failure=true;await page.getByRole('button',{name:'Prepare editor tools',exact:true}).click();await expect(page.locator('.code-tools')).toContainText('Preparing editor');expect(state.writes).toHaveLength(1);expect(errors).toEqual([]);});
test('stale receipts and stopped runtimes cannot open browser tools @dashboard',async({page})=>{const{state,errors}=await open(page);await expect(page.getByRole('button',{name:'Prepare editor tools',exact:true})).toBeEnabled();state.editor={workspaceId:id,revision:3,vmId:vm,runtimeId:instance,operationId:operation,phase:'ready',version:'4.144.0'};await page.reload();await expect(page.getByRole('link',{name:'Open Web VS Code',exact:true})).toHaveCount(0);state.phase='stopped';await page.reload();await expect(page.getByRole('button',{name:'Prepare editor tools',exact:true})).toHaveCount(0);expect(state.writes).toHaveLength(0);expect(errors).toEqual([]);});
test('metadata outages remove previously enabled editor links @dashboard',async({page})=>{const{state,errors}=await open(page);await page.getByRole('button',{name:'Prepare editor tools',exact:true}).click();state.editor.phase='ready';await expect(page.getByRole('link',{name:'Open terminal',exact:true})).toBeVisible();state.outage=true;await expect(page.getByRole('link',{name:'Open terminal',exact:true})).toHaveCount(0);expect(errors).toEqual([]);});
for(const width of [1280,390])test(`standalone terminal streams input and resize at ${width}px @dashboard`,async({page},info)=>{
 await page.setViewportSize({width,height:900});const frames:(string|Buffer)[]=[];
 await page.routeWebSocket('**/_fc/terminal/socket',ws=>{ws.onMessage(data=>frames.push(data));ws.send(Buffer.from('fc-code /home/fc-code/project\r\n$ '));});
 await page.goto('/code-terminal.html');await expect(page.locator('#terminal')).toContainText('/home/fc-code/project');
 await page.locator('.xterm-helper-textarea').focus();await page.keyboard.type('pwd');await page.keyboard.press('Enter');
 await expect.poll(()=>frames.filter(Buffer.isBuffer).map(b=>b.toString()).join('')).toContain('pwd');expect(frames.some(v=>typeof v==='string'&&JSON.parse(v).cols>1)).toBe(true);
 expect(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth)).toBe(true);await page.screenshot({path:info.outputPath(`code-terminal-en-${width}.png`),fullPage:true});
 await page.getByRole('button',{name:'Disconnect',exact:true}).click();await expect(page.locator('#terminal-status')).toContainText('Disconnected');
});
test('standalone terminal splits large UTF-8 paste into bounded binary input @dashboard',async({page})=>{
 const frames:Buffer[]=[];await page.routeWebSocket('**/_fc/terminal/socket',ws=>{ws.onMessage(data=>{if(Buffer.isBuffer(data))frames.push(data);});ws.send(Buffer.from('$ '));});
 await page.goto('/code-terminal.html');await expect(page.locator('#terminal')).toContainText('$');
 await page.locator('.xterm-helper-textarea').focus();const text='한'.repeat(12000);await page.locator('.xterm-helper-textarea').evaluate((el,text)=>{const data=new DataTransfer();data.setData('text/plain',text);el.dispatchEvent(new ClipboardEvent('paste',{clipboardData:data,bubbles:true,cancelable:true}));},text);
 await expect.poll(()=>Buffer.concat(frames).toString('utf8').length).toBe(text.length);expect(Buffer.concat(frames).toString('utf8')).toBe(text);expect(frames.every(f=>f.length<=16384)).toBe(true);
});
