// Compiled dashboard with a synthetic controller; no real DB or VM is restored here.
import {test,before,after,beforeEach,afterEach} from 'node:test';
import assert from 'node:assert/strict';
import {fileURLToPath} from 'node:url';
import {chromium} from '../../firecrab-e2e/node_modules/playwright-core/index.mjs';
import {expect} from '../../firecrab-e2e/node_modules/@playwright/test/index.mjs';
import {createFixtureServer} from './fixture-server.mjs';
import {installCanvasHistoryFixture} from './canvas-fixture.mjs';
let fixture,browser,context,page,canvas,rows,admissions,policy,lose,broken,generation,hidden;
before(async()=>{fixture=await createFixtureServer(fileURLToPath(new URL('../dist',import.meta.url)),0);browser=await chromium.launch({headless:true});});
after(async()=>{await browser?.close();await fixture?.close();});
beforeEach(async()=>{
 fixture.reset(0);canvas=installCanvasHistoryFixture(fixture);rows=[];admissions=[];policy={enabled:false,intervalSeconds:3600,nextRunAt:0};lose=false;broken=false;generation=1;hidden=false;
 const path=canvas.base+'/backups';
 fixture.handlers.set('GET '+canvas.base,({send})=>send(broken?409:200,broken?{error:{message:'Fixture database is damaged'}}:{projectId:canvas.project,status:'stopped',generation,writeGenerationRequired:generation>1}));
 fixture.handlers.set('GET '+path,({send})=>send(200,hidden?[]:rows));fixture.handlers.set('GET '+path+'/binding',({send})=>send(200,{generation}));
 fixture.handlers.set('GET '+path+'/policy',({send})=>send(200,policy));fixture.handlers.set('PUT '+path+'/policy',({body,send})=>{policy={...policy,...body};return send(200,policy);});
 const inherited=fixture.matchHandler;
 fixture.matchHandler=({method,path:target})=>{
  if(target.startsWith(path+'/operations/'))return({body,send})=>{
   const id=target.split('/').at(-1);let row=rows.find(r=>r.id===id);
   if(method==='PUT'){
    admissions.push({id,body:structuredClone(body)});
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
 await dialog.getByRole('button',{name:'Back up now',exact:true}).click();await expect(dialog.locator('.cw-backup-records li')).toHaveCount(1);
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
