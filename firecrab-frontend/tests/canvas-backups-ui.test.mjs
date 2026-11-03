// Compiled dashboard with a synthetic controller; no real DB or VM is restored here.
import {test,before,after,beforeEach,afterEach} from 'node:test';
import assert from 'node:assert/strict';
import {fileURLToPath} from 'node:url';
import {chromium} from '../../firecrab-e2e/node_modules/playwright-core/index.mjs';
import {expect} from '../../firecrab-e2e/node_modules/@playwright/test/index.mjs';
import {createFixtureServer} from './fixture-server.mjs';
import {installCanvasHistoryFixture} from './canvas-fixture.mjs';
let fixture,browser,context,page,canvas,rows,admissions,policy,lose,broken,generation,hidden,reject,retention,cleanups,cleanupAdmissions,loseCleanup,hideCleanup;
before(async()=>{fixture=await createFixtureServer(fileURLToPath(new URL('../dist',import.meta.url)),0);browser=await chromium.launch({headless:true});});
after(async()=>{await browser?.close();await fixture?.close();});
beforeEach(async()=>{
 fixture.reset(0);canvas=installCanvasHistoryFixture(fixture);rows=[];admissions=[];policy={enabled:false,intervalSeconds:3600,nextRunAt:0};lose=false;broken=false;generation=1;hidden=false;reject=false;retention={enabled:false,keepLatest:10,maxAgeDays:null,includeManual:false,nextRunAt:0};cleanups=[];cleanupAdmissions=[];loseCleanup=false;hideCleanup=false;
 const path=canvas.base+'/backups';
 fixture.handlers.set('GET '+canvas.base,({send})=>send(broken?409:200,broken?{error:{message:'Fixture database is damaged'}}:{projectId:canvas.project,status:'stopped',generation,writeGenerationRequired:generation>1}));
 fixture.handlers.set('GET '+path+'/cleanups',({send})=>send(200,hideCleanup?[]:cleanups));
 fixture.handlers.set('GET '+path+'/retention',({send})=>send(200,retention));
 fixture.handlers.set('PUT '+path+'/retention',({body,send})=>{retention={...retention,...body};return send(200,retention);});
 fixture.handlers.set('GET '+path+'/retention/preview',({send})=>send(200,{candidates:rows.filter(r=>!r.cleanupId).map(r=>({id:r.id,createdAt:r.createdAt,kind:r.request.restoreCheckpoint?'restore_originals':r.phase==='aborted'?'partial':'checkpoint',protected:!!r.protected,eligible:!r.protected&&rows.filter(r=>!r.cleanupId).length>1,reason:r.protected?'Fixture checkpoint is protected':null})),automaticTargetIds:rows.filter(r=>!r.cleanupId).slice(retention.keepLatest).filter(r=>!r.protected).map(r=>r.id)}));
 fixture.handlers.set('GET '+path,({send})=>send(200,hidden?[]:rows));fixture.handlers.set('GET '+path+'/binding',({send})=>send(200,{generation}));
 fixture.handlers.set('GET '+path+'/policy',({send})=>send(200,policy));fixture.handlers.set('PUT '+path+'/policy',({body,send})=>{policy={...policy,...body};return send(200,policy);});
 const inherited=fixture.matchHandler;
 fixture.matchHandler=({method,path:target})=>{
  if(target.startsWith(path+'/operations/')&&target.endsWith('/protection'))return({body,send})=>{const row=rows.find(r=>r.id===target.split('/').at(-2));if(row)row.protected=body.protected;return send(row?200:404,row||{});};
  if(target.startsWith(path+'/cleanups/'))return({body,send})=>{
   const id=target.split('/').at(target.endsWith('/retry')?-2:-1);let row=cleanups.find(r=>r.id===id);
   if(method==='POST'&&row){row.pendingReconciliation=false;row.phase='complete';delete row.error;return send(200,row);}
   if(method==='PUT'){
    cleanupAdmissions.push({id,body:structuredClone(body)});
    if(!row){row={id,request:structuredClone(body),phase:'complete',pendingReconciliation:false,createdAt:1770000000,logicalBytes:1024**3,removedFiles:4,automatic:false};cleanups.unshift(row);for(const targetId of body.targetIds){const targetRow=rows.find(r=>r.id===targetId);if(targetRow){targetRow.cleanupId=id;targetRow.cleanedAt=1770000001;}}}
    if(loseCleanup){loseCleanup=false;hideCleanup=true;return send(503,{error:{message:'Fixture cleanup acknowledgement lost'}});}
    hideCleanup=false;return send(200,row);
   }
   return send(row?200:404,row||{error:{message:'Missing fixture cleanup'}});
  };
  if(target.startsWith(path+'/operations/'))return({body,send})=>{
   const id=target.split('/').at(target.endsWith('/retry')?-2:-1);let row=rows.find(r=>r.id===id);
   if(method==='POST'&&row){row.pendingReconciliation=false;delete row.error;return send(200,row);}
   if(method==='PUT'){
    admissions.push({id,body:structuredClone(body)});
    if(reject){reject=false;return send(409,{error:{code:'workspace_conflict',message:'Fixture VM must be stopped'}});}
    if(!row){row={id,request:structuredClone(body),phase:'complete',createdAt:1770000000,manifest:{checkpoint:{id:body.restoreCheckpoint||id,revision:7,createdAt:1770000000},volumeBytes:1024**3}};if(body.restoreCheckpoint){generation++;row.restoredGeneration=generation;}rows.unshift(row);}
    if(lose){lose=false;hidden=true;return send(503,{error:{message:'Fixture acknowledgement lost'}});}
    hidden=false;if(!rows.some(r=>r.id===id))rows.unshift(row);
    return send(200,row);
   }
   return send(row?200:404,row||{error:{message:'Missing fixture operation'}});
  };
  return inherited?.({method,path:target});
 };
 context=await browser.newContext({viewport:{width:1440,height:1100}});page=await context.newPage();
 await page.goto(fixture.url+'/#/ai-canvas');await page.getByLabel('Project',{exact:true}).selectOption(canvas.project);await page.getByRole('button',{name:'Open project',exact:true}).click();
 await expect(page.getByRole('button',{name:'Backups & recovery',exact:true})).toBeVisible();
});
afterEach(async()=>{await context?.close();});
test('schedule, retained backup and explicit restore remain available when the private DB is damaged',async()=>{
 await page.getByRole('button',{name:'Backups & recovery',exact:true}).click();
 const dialog=page.getByRole('dialog');await expect(dialog).toContainText('Automatic backups wait for a stopped, idle VM');
 await dialog.getByRole('checkbox',{name:'Enable scheduled checkpoints'}).check();await dialog.getByRole('button',{name:'Save schedule',exact:true}).click();await expect.poll(()=>policy.enabled).toBe(true);
 await dialog.getByRole('button',{name:'Back up now',exact:true}).click();await expect(dialog.getByRole('list',{name:'Backup operation history',exact:true}).locator('li')).toHaveCount(1);
 await page.screenshot({path:fileURLToPath(new URL('../../test-results/canvas-backups/schedule-checkpoint.jpg',import.meta.url)),fullPage:true});
 broken=true;await dialog.getByRole('button',{name:'Refresh',exact:true}).click();
 await dialog.getByRole('button',{name:'Select for restoration',exact:true}).click();assert.equal(admissions.length,1);
 await expect(dialog.getByRole('region',{name:'Confirm restoration'})).toContainText('Current files are preserved privately');
 await page.screenshot({path:fileURLToPath(new URL('../../test-results/canvas-backups/restore-confirmation.jpg',import.meta.url)),fullPage:true});
 await dialog.getByRole('button',{name:'Restore this checkpoint',exact:true}).click();await expect.poll(()=>admissions.length).toBe(2);assert.equal(admissions[1].body.restoreCheckpoint,admissions[0].id);
 await expect(dialog).toContainText('Reload other open pages');
});
test('uncertain acknowledgement survives a reload and retries the same operation UUID and request',async()=>{
 await page.getByRole('button',{name:'Backups & recovery',exact:true}).click();lose=true;
 await page.getByRole('dialog').getByRole('button',{name:'Back up now',exact:true}).click();await expect(page.getByRole('button',{name:'Retry same request'})).toBeVisible();
 await page.reload();await page.getByRole('button',{name:'Backups & recovery',exact:true}).click();await page.getByRole('button',{name:'Retry same request',exact:true}).click();
 await expect.poll(()=>admissions.length).toBe(2);assert.deepEqual(admissions[0],admissions[1]);
});

test('Korean recovery controls are available after reopening a damaged project',async()=>{
 broken=true;await page.reload();
 await page.locator('#cw-language').selectOption('ko');
 await page.getByRole('button',{name:'백업·복구',exact:true}).click();
 const dialog=page.getByRole('dialog');await expect(dialog).toContainText('프로젝트 백업·복구');
 await expect(dialog.getByRole('button',{name:'지금 백업',exact:true})).toBeEnabled();
 await expect(dialog.getByRole('checkbox',{name:'정기 백업 사용',exact:true})).toBeVisible();
 await page.screenshot({path:fileURLToPath(new URL('../../test-results/canvas-backups/korean-recovery.jpg',import.meta.url)),fullPage:true});
});

test('a committed receipt with pending cleanup stays resumable and blocks new operations',async()=>{
 rows.push({id:canvas.project,phase:'complete',pendingReconciliation:true,createdAt:1770000000,request:{expectedGeneration:1,restoreCheckpoint:null},error:'Fixture cleanup needs retry'});
 await page.getByRole('button',{name:'Backups & recovery',exact:true}).click();
 const dialog=page.getByRole('dialog');await expect(dialog).toContainText('Reconciliation pending');
 await expect(dialog.getByRole('button',{name:'Back up now',exact:true})).toBeDisabled();
 await dialog.getByRole('button',{name:'Resume',exact:true}).click();
 await expect(dialog.getByRole('button',{name:'Back up now',exact:true})).toBeEnabled();
 assert.equal(rows[0].pendingReconciliation,false);assert.equal(admissions.length,0);
});

test('a fresh definitive rejection with a missing receipt releases only that unadmitted request',async()=>{
 await page.getByRole('button',{name:'Backups & recovery',exact:true}).click();reject=true;
 const dialog=page.getByRole('dialog');await dialog.getByRole('button',{name:'Back up now',exact:true}).click();
 await expect(dialog.getByRole('alert')).toContainText('Fixture VM must be stopped');
 await expect(dialog.getByRole('button',{name:'Retry same request',exact:true})).toHaveCount(0);
 await expect(dialog.getByRole('button',{name:'Back up now',exact:true})).toBeEnabled();
});
test('an earlier uncertain backup keeps its UUID after a rejected retry and absent receipt',async()=>{
 await page.getByRole('button',{name:'Backups & recovery',exact:true}).click();lose=true;
 const dialog=page.getByRole('dialog');await dialog.getByRole('button',{name:'Back up now',exact:true}).click();
 await expect(dialog.getByRole('button',{name:'Retry same request',exact:true})).toBeVisible();
 rows=[];reject=true;await dialog.getByRole('button',{name:'Retry same request',exact:true}).click();
 await expect(dialog.getByRole('alert')).toContainText('Fixture VM must be stopped');
 await expect(dialog.getByRole('button',{name:'Retry same request',exact:true})).toBeVisible();
 assert.equal(admissions[0].id,admissions[1].id);
});

function seedRetentionBackups() {
 for(let i=1;i<=3;i++)rows.push({id:`55000000-0000-4000-8000-${String(i).padStart(12,'0')}`,request:{expectedGeneration:1,restoreCheckpoint:null},phase:'complete',createdAt:1770000000-i,manifest:{checkpoint:{id:canvas.project,revision:7,createdAt:1770000000},volumeBytes:1024**3}});
}
test('retention policy, protection and permanent cleanup require an explicit target confirmation',async()=>{
 seedRetentionBackups();await page.getByRole('button',{name:'Backups & recovery',exact:true}).click();
 const region=page.getByRole('region',{name:'Backup retention and cleanup',exact:true});
 await region.getByRole('checkbox',{name:'Enable automatic cleanup',exact:true}).check();
 await region.getByRole('spinbutton',{name:'Keep recent checkpoints',exact:true}).fill('2');
 await region.getByRole('spinbutton',{name:'Minimum age (days)',exact:true}).fill('7');
 await region.getByRole('button',{name:'Save retention policy',exact:true}).click();
 await expect.poll(()=>retention.enabled).toBe(true);assert.equal(retention.keepLatest,2);assert.equal(retention.maxAgeDays,7);assert.equal(retention.expectedGeneration,1);
 await region.getByRole('button',{name:'Select policy candidates',exact:true}).click();await expect(region.getByRole('checkbox',{name:'Select cleanup '+rows[2].id,exact:true})).toBeChecked();
 await region.getByRole('button',{name:'Protect checkpoint',exact:true}).first().click();await expect.poll(()=>rows[0].protected).toBe(true);
 await expect(region.getByRole('checkbox',{name:'Select cleanup '+rows[0].id,exact:true})).toBeDisabled();
 await region.getByRole('heading',{name:'Retention & cleanup',exact:true}).scrollIntoViewIfNeeded();await page.screenshot({path:fileURLToPath(new URL('../../test-results/canvas-retention/retention-policy.jpg',import.meta.url)),fullPage:true});
 const target=rows[2].id;await region.getByRole('checkbox',{name:'Select cleanup '+target,exact:true}).check();
 await region.getByRole('button',{name:'Delete selected artifacts',exact:true}).click();assert.equal(cleanupAdmissions.length,0);
 const confirm=region.getByRole('region',{name:'Confirm permanent cleanup',exact:true});await expect(confirm).toContainText(target);await expect(confirm).toContainText('Current project files remain');
 await confirm.scrollIntoViewIfNeeded();await page.screenshot({path:fileURLToPath(new URL('../../test-results/canvas-retention/cleanup-confirmation.jpg',import.meta.url)),fullPage:true});
 await confirm.getByRole('button',{name:'Permanently delete selected files',exact:true}).click();await expect.poll(()=>cleanupAdmissions.length).toBe(1);assert.deepEqual(cleanupAdmissions[0].body,{expectedGeneration:1,targetIds:[target]});
 await expect(region).toContainText('Cleanup complete');await expect(page.getByRole('dialog')).toContainText('Artifacts cleaned; receipt retained');
});
test('uncertain deletion acknowledgement survives reload and preserves the same target list and UUID',async()=>{
 seedRetentionBackups();await page.getByRole('button',{name:'Backups & recovery',exact:true}).click();
 let region=page.getByRole('region',{name:'Backup retention and cleanup',exact:true});const target=rows[2].id;
 await region.getByRole('checkbox',{name:'Select cleanup '+target,exact:true}).check();await region.getByRole('button',{name:'Delete selected artifacts',exact:true}).click();loseCleanup=true;
 await region.getByRole('button',{name:'Permanently delete selected files',exact:true}).click();await expect(region.getByRole('button',{name:'Retry same cleanup request',exact:true})).toBeVisible();
 await page.reload();await page.getByRole('button',{name:'Backups & recovery',exact:true}).click();region=page.getByRole('region',{name:'Backup retention and cleanup',exact:true});
 await region.getByRole('button',{name:'Retry same cleanup request',exact:true}).click();await expect.poll(()=>cleanupAdmissions.length).toBe(2);assert.deepEqual(cleanupAdmissions[0],cleanupAdmissions[1]);
 await expect(region.getByRole('button',{name:'Retry same cleanup request',exact:true})).toHaveCount(0);
});
test('pending cleanup remains resumable and prevents a new backup even after its data phase committed',async()=>{
 cleanups.push({id:canvas.project,phase:'complete',pendingReconciliation:true,createdAt:1770000000,request:{expectedGeneration:1,targetIds:[]},logicalBytes:1024,removedFiles:1,error:'Fixture maintenance cleanup pending'});
 await page.getByRole('button',{name:'Backups & recovery',exact:true}).click();const dialog=page.getByRole('dialog');await expect(dialog.getByRole('button',{name:'Back up now',exact:true})).toBeDisabled();
 await dialog.getByRole('button',{name:'Resume cleanup',exact:true}).click();await expect(dialog.getByRole('button',{name:'Back up now',exact:true})).toBeEnabled();assert.equal(cleanupAdmissions.length,0);
});
test('Korean retention settings reject an empty minimum and keep the final checkpoint disabled',async()=>{
 seedRetentionBackups();rows.splice(1);await page.locator('#cw-language').selectOption('ko');await page.getByRole('button',{name:'백업·복구',exact:true}).click();
 const region=page.getByRole('region',{name:'백업 보관·정리',exact:true});await expect(region.getByRole('checkbox',{name:'정리 대상 선택 '+rows[0].id,exact:true})).toBeDisabled();
 await region.getByRole('spinbutton',{name:'최근 백업 보존 개수',exact:true}).fill('0');await expect(region.getByRole('button',{name:'보관 정책 저장',exact:true})).toBeDisabled();
 await region.getByRole('spinbutton',{name:'최근 백업 보존 개수',exact:true}).fill('1');await expect(region.getByRole('button',{name:'보관 정책 저장',exact:true})).toBeEnabled();
});
