import { test, before, after, beforeEach, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { chromium } from '../../firecrab-e2e/node_modules/playwright-core/index.mjs';
import { expect } from '../../firecrab-e2e/node_modules/@playwright/test/index.mjs';
import { createFixtureServer } from './fixture-server.mjs';
import { installCanvasHistoryFixture } from './canvas-fixture.mjs';
import { installCanvasEditorFixture } from './editor-fixture.mjs';
import { randomUUID } from 'node:crypto';

let fixture, browser, context, page, errors;
before(async () => {
  fixture = await createFixtureServer(fileURLToPath(new URL('../dist', import.meta.url)), 12);
  browser = await chromium.launch({ headless: true });
});
after(async () => { await browser?.close(); await fixture?.close(); });
beforeEach(async () => {
  fixture.reset(12);
  errors = [];
  context = await browser.newContext({ viewport: { width: 1440, height: 1000 } });
  page = await context.newPage();
  page.on('pageerror', error => errors.push(error.message));
});
afterEach(async () => {
  await context?.close();
  assert.deepEqual(errors, [], 'No runtime exceptions');
});


async function openCanvasHistory() {
  const canvas = installCanvasHistoryFixture(fixture);
  await page.goto(`${fixture.url}/#/ai-canvas`);
  await page.getByLabel('Project', { exact: true }).selectOption(canvas.project);
  await page.getByRole('button', { name: 'Open project', exact: true }).click();
  await page.getByRole('button', { name: 'Version history', exact: true }).click();
  await expect(page.locator('.cw-history li')).toHaveCount(2);
  return canvas;
}

test('canvas compares saved versions and rejects a stale restore without rebasing it', async () => {
  const canvas = await openCanvasHistory();
  await page.getByRole('button', { name: 'Compare', exact: true }).last().click();
  await expect(page.locator('.cw-comparison')).toContainText('saved before');
  await expect(page.locator('.cw-comparison')).toContainText('saved current');
  canvas.externalWrite();
  await page.getByRole('button', { name: 'Restore selected version', exact: true }).click();
  await expect(page.locator('#cw-error')).toContainText('Saved file changed');
  assert.equal(canvas.restoreCount, 0);
  assert.equal(canvas.restoreRequests[0].expectedVersion, 2);
  assert.equal(canvas.savedFiles.at(-1).version.id, 3);
  await expect(page.locator('.cw-history li')).toHaveCount(3);
  await page.getByRole('button', { name: 'Compare', exact: true }).last().click();
  await expect(page.locator('.cw-comparison')).toContainText('concurrent saved content');
  await page.getByRole('button', { name: 'Restore selected version', exact: true }).click();
  await expect(page.getByText('File restored as a new version', { exact: true })).toBeVisible();
  assert.equal(canvas.restoreCount, 1);
  assert.equal(canvas.restoreRequests[1].expectedVersion, 3);
  assert.equal(canvas.savedFiles.at(-1).version.restoredFrom, 1);
  assert.equal(canvas.savedFiles.length, 4);
});

test('canvas retains the identical restore request after a lost acknowledgement', async () => {
  const canvas = await openCanvasHistory();
  await page.getByRole('button', { name: 'Compare', exact: true }).last().click();
  await expect(page.locator('.cw-comparison')).toBeVisible();
  canvas.loseNextAcknowledgement();
  await page.getByRole('button', { name: 'Restore selected version', exact: true }).click();
  await expect(page.getByText('Unconfirmed restore: api/auth.txt', { exact: true })).toBeVisible();
  assert.equal(canvas.restoreCount, 1);
  assert.equal(canvas.restoreRequests.length, 1);
  await page.getByRole('button', { name: 'Retry the same restore', exact: true }).click();
  await expect(page.getByText('File restored as a new version', { exact: true })).toBeVisible();
  assert.deepEqual(canvas.restoreRequests[1], canvas.restoreRequests[0]);
  assert.equal(canvas.restoreCount, 1);
  assert.equal(canvas.savedFiles.length, 3);
});


test('canvas pages older retained versions and reloads without dispatching a restore', async () => {
  const canvas = await openCanvasHistory();
  for (let index=0; index<105; index++) canvas.externalWrite(`saved-${index}`);
  await page.getByRole('button', { name: 'Load retained history', exact: true }).click();
  await expect(page.locator('.cw-history li')).toHaveCount(100);
  await page.getByRole('button', { name: 'Load older history', exact: true }).click();
  await expect(page.locator('.cw-history li')).toHaveCount(107);
  await expect.poll(() => [...canvas.privateViews.values()]
    .some(view => view.document.nodes.some(node => node.kind === 'history')))
    .toBe(true);
  await page.reload();
  await page.getByRole('button', { name: 'Load retained history', exact: true }).click();
  await expect(page.locator('.cw-history li')).toHaveCount(100);
  assert.equal(canvas.restoreRequests.length, 0);
  assert.equal(canvas.restoreCount, 0);
});


// Controller-only browser regressions. The stub is not Web VS Code or a VM.
async function openEditorController() {
  const canvas=installCanvasEditorFixture(fixture);
  await page.route('**/*',async route=>{
    const url=new URL(route.request().url());
    if(!url.hostname.endsWith('.editor.localhost'))return route.continue();
    const id=url.searchParams.get('firecrabClient'),proof=randomUUID();canvas.activate(id,proof);
    return route.fulfill({contentType:'text/html',body:`<h1>Controller fixture · no native editor</h1><script>
      addEventListener('message',event=>{
        if(event.source===parent&&event.origin===${JSON.stringify(fixture.url)}&&event.data.type==='firecrab-window-proof')
          parent.postMessage({...event.data,windowProof:${JSON.stringify(proof)}},event.origin);
      });</script>`});
  });
  await page.goto(`${fixture.url}/#/ai-canvas`);
  await page.getByLabel('Project',{exact:true}).selectOption(canvas.project);
  await page.getByRole('button',{name:'Open project',exact:true}).click();
  await page.getByRole('button',{name:'Personal Page · Web VS Code',exact:true}).click();
  await expect(page.locator('iframe')).toHaveCount(1);
  await expect.poll(()=>[...canvas.clients.values()].some(c=>c.activations.length>0)).toBe(true);
  return {canvas,id:[...canvas.clients.keys()][0]};
}

test('closing an editor waits for a window-bound snapshot and preserves its native process',async()=>{
  const {canvas,id}=await openEditorController();
  await page.getByRole('button',{name:'Close editor',exact:true}).click();
  await expect.poll(()=>canvas.clients.get(id).command).toBe('close');
  await expect(page.locator('iframe')).toHaveAttribute('inert','');
  await expect(page.getByRole('tab')).toHaveCount(1);
  canvas.acknowledge(id);
  await expect(page.locator('iframe')).toHaveCount(0);
  assert.equal(canvas.clients.get(id).status,'detached');assert.equal(canvas.stopRequests,0);
  await page.getByRole('button',{name:'Personal Page · Web VS Code',exact:true}).click();
  await expect(page.locator('iframe')).toHaveCount(1);
  await expect.poll(()=>canvas.clients.size).toBe(2);
  assert.equal(canvas.editors.size,1);
});

test('failed tab close retains its iframe and Keep editing waits for a worker acknowledgement',async()=>{
  const {canvas,id}=await openEditorController();
  await page.getByRole('button',{name:'Close tab',exact:true}).click();
  await expect.poll(()=>canvas.clients.get(id).command).toBe('close');
  Object.assign(canvas.clients.get(id),{status:'error',error:'Private draft storage unavailable'});
  await expect(page.getByRole('button',{name:'Keep editing',exact:true})).toBeEnabled();
  await page.getByRole('button',{name:'Keep editing',exact:true}).click();
  await expect.poll(()=>canvas.clients.get(id).command).toBe('cancel');
  await expect(page.locator('iframe')).toHaveAttribute('inert','');
  canvas.reportActive(id);
  await expect(page.locator('iframe')).not.toHaveAttribute('inert','');
  await expect(page.getByRole('tab')).toHaveCount(1);
  await page.getByRole('button',{name:'Close tab',exact:true}).click();
  await expect.poll(()=>canvas.clients.get(id).command).toBe('close');canvas.acknowledge(id);
  await expect(page.locator('iframe')).toHaveCount(0);assert.equal(canvas.stopRequests,0);
  await expect(page.getByRole('heading',{name:'A new workspace',exact:true})).toBeVisible();
});

test('an accepted ambiguous close reply reconciles without reloading its iframe',async()=>{
  const {canvas,id}=await openEditorController();canvas.loseNextControlAck();
  const source=await page.locator('iframe').getAttribute('src');
  await page.getByRole('button',{name:'Close editor',exact:true}).click();
  await expect(page.getByRole('button',{name:'Retry close',exact:true})).toBeEnabled();
  await page.getByRole('button',{name:'Retry close',exact:true}).click();
  await expect(page.getByRole('button',{name:'Retry close',exact:true})).toBeDisabled();
  assert.equal(canvas.controls.length,1);
  assert.equal(await page.locator('iframe').getAttribute('src'),source);
  canvas.acknowledge(id);await expect(page.locator('iframe')).toHaveCount(0);
  assert.equal(canvas.stopRequests,0);
});


test('an unaccepted close reply retries its original operation identity',async()=>{
  const {canvas,id}=await openEditorController();const requests=[];
  await page.route('**/clients/*/control',async route=>{
    requests.push(route.request().postDataJSON());
    if(requests.length===1)return route.fulfill({status:503,contentType:'application/json',body:JSON.stringify({error:{message:'Request did not reach storage'}})});
    return route.fallback();
  });
  await page.getByRole('button',{name:'Close editor',exact:true}).click();
  await expect(page.getByRole('button',{name:'Retry close',exact:true})).toBeEnabled();
  await page.getByRole('button',{name:'Retry close',exact:true}).click();
  await expect.poll(()=>canvas.clients.get(id).command).toBe('close');
  assert.deepEqual(requests[0],requests[1]);canvas.acknowledge(id);
  await expect(page.locator('iframe')).toHaveCount(0);assert.equal(canvas.stopRequests,0);
});
