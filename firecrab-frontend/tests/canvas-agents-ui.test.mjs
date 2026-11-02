import { test, before, after, beforeEach, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { chromium } from '../../firecrab-e2e/node_modules/playwright-core/index.mjs';
import { expect } from '../../firecrab-e2e/node_modules/@playwright/test/index.mjs';
import { createFixtureServer } from './fixture-server.mjs';
import { installCanvasAgentFixture } from './canvas-agent-fixture.mjs';
import { installCanvasEditorFixture } from './editor-fixture.mjs';
let fixture, browser, context, page, canvas, errors;
before(async () => {
  fixture = await createFixtureServer(fileURLToPath(new URL('../dist', import.meta.url)), 0);
  browser = await chromium.launch({ headless: true });
});
after(async () => { await browser?.close(); await fixture?.close(); });
beforeEach(async () => {
  fixture.reset(0); canvas = installCanvasAgentFixture(fixture); errors = [];
  context = await browser.newContext({ viewport: { width: 1440, height: 1000 } });
  page = await context.newPage(); page.on('pageerror', e => errors.push(e.message));
});
afterEach(async () => { await context?.close(); assert.deepEqual(errors, []); });
async function openAgent() {
  await page.goto(`${fixture.url}/#/ai-canvas`);
  await page.getByLabel('Project', { exact: true }).selectOption(canvas.project);
  await page.getByRole('button', { name: 'Open project', exact: true }).click();
  await page.getByRole('button', { name: 'Folder agent', exact: true }).click();
  await expect(page.getByLabel('Agent model', { exact: true })).toContainText('qa-chatgpt');
  await page.getByLabel('Agent model', { exact: true }).selectOption({ label: 'ChatGPT · UI fixture · no model execution · qa-chatgpt' });
  await page.getByLabel('Agent request', { exact: true }).fill('Inspect saved project files');
  await expect(page.getByRole('button', { name: 'Start file task', exact: true })).toBeEnabled();
}
test('agent requests checkpoint private IDs before dispatch and reconnect without a second task', async () => {
  await openAgent();
  await page.getByRole('button', { name: 'Start file task', exact: true }).click();
  await expect(page.getByText('Fixture result. No model or VM executed.', { exact: true })).toBeVisible();
  assert.equal(canvas.admissions.length, 1);
  const admission = canvas.admissions[0];
  const writes = fixture.requests.filter(r => r.method === 'PUT');
  const capture = writes.findIndex(r => r.path.includes('/instructions/snapshots/'));
  const checkpoint = writes.findIndex(r => r.path.includes('/views/') && r.body.document.nodes.some(n => n.kind === 'agent' && JSON.parse(n.content).pending?.id === admission.id));
  const dispatch = writes.findIndex(r => r.path.endsWith('/tasks/' + admission.id));
  assert.ok(checkpoint >= 0 && checkpoint < capture && capture < dispatch);
  await page.getByRole('button', { name: 'Task details', exact: true }).click();
  await expect(page.locator('.cw-agent-rules')).toContainText('AGENTS.md #7');
  await page.getByRole('button', { name: 'Save layout', exact: true }).click();
  await expect(page.locator('#cw-save-state')).toContainText('Saved privately');
  const count = fixture.requests.filter(r => r.method === 'PUT').length;
  await page.reload();
  await expect(page.getByText('Fixture result. No model or VM executed.', { exact: true })).toBeVisible();
  assert.equal(canvas.admissions.length, 1);
  assert.equal(fixture.requests.filter(r => r.method === 'PUT').length, count);
});
test('failed private storage blocks provider work until the identical saved request can be persisted', async () => {
  await openAgent();
  await page.getByRole('button', { name: 'Save layout', exact: true }).click();
  await expect(page.locator('#cw-save-state')).toContainText('Saved privately');
  canvas.rejectPrivateSave();
  await page.getByRole('button', { name: 'Start file task', exact: true }).click();
  await expect(page.getByRole('alert')).toContainText('Fixture private save rejected');
  assert.equal(canvas.admissions.length, 0); assert.equal(canvas.snapshots.size, 0);
  const rejected = fixture.requests.filter(r => r.method === 'PUT' && r.path.includes('/views/')).at(-1);
  const pending = JSON.parse(rejected.body.document.nodes.find(n => n.kind === 'agent').content).pending;
  canvas.rejectPrivateSave(false);
  await page.getByRole('button', { name: 'Retry saved request', exact: true }).click();
  await expect(page.getByText('Fixture result. No model or VM executed.', { exact: true })).toBeVisible();
  assert.equal(canvas.admissions[0].id, pending.id); assert.deepEqual(canvas.admissions[0].body, pending.request);
});
test('lost admission acknowledgement is recovered by read-only task lookup after reload', async () => {
  await openAgent(); canvas.loseNextAdmission();
  await page.getByRole('button', { name: 'Start file task', exact: true }).click();
  await expect(page.getByRole('button', { name: 'Retry saved request', exact: true })).toBeEnabled();
  assert.equal(canvas.admissions.length, 1);
  canvas.revealTasks(); await page.reload();
  await expect(page.getByText('Fixture result. No model or VM executed.', { exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Start file task', exact: true })).toBeVisible();
  assert.equal(canvas.admissions.length, 1); assert.equal(canvas.snapshots.size, 1);
});
test('ChatGPT and Claude require explicit selection and new tabs begin without agent sessions', async () => {
  canvas.requireModelVerification();
  await page.goto(`${fixture.url}/#/ai-canvas`);
  await page.getByLabel('Project', { exact: true }).selectOption(canvas.project);
  await page.getByRole('button', { name: 'Open project', exact: true }).click();
  await page.getByRole('button', { name: 'Folder agent', exact: true }).click();
  await expect(page.getByLabel('Agent model', { exact: true })).toContainText('qa-claude');
  await page.getByLabel('Agent model', { exact: true }).selectOption({ index: 2 });
  await page.getByLabel('Agent request', { exact: true }).fill('Claude file task');
  await expect(page.getByRole('button', { name: 'Start file task', exact: true })).toBeDisabled();
  await page.getByRole('button', { name: 'Verify file tools · sends a model probe', exact: true }).click();
  await page.getByRole('button', { name: 'Start file task', exact: true }).click();
  await expect(page.getByText('Fixture result. No model or VM executed.', { exact: true })).toBeVisible();
  assert.equal(canvas.admissions[0].body.selection.connectionId, canvas.accounts[1].id);
  await page.getByRole('button', { name: 'New empty canvas tab', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'A new workspace', exact: true })).toBeVisible();
  await expect(page.getByLabel('Folder agent session', { exact: true })).toHaveCount(0);
});

test('the editor opens the same retained agent session and switching views cannot dispatch work', async () => {
  fixture.reset(0); canvas = installCanvasAgentFixture(fixture, installCanvasEditorFixture(fixture));
  await page.route('**/*', async route => {
    const url = new URL(route.request().url());
    if (url.hostname.endsWith('.editor.localhost')) return route.fulfill({ status:200, contentType:'text/html', body:'<h1>Editor controller fixture</h1>' });
    return route.continue();
  });
  await openAgent();
  await page.getByRole('button', { name: 'Start file task', exact: true }).click();
  await expect(page.getByText('Fixture result. No model or VM executed.', { exact: true })).toBeVisible();
  await page.getByLabel('Agent request', { exact: true }).fill('Private next request');
  await page.getByRole('button', { name: 'Personal Page · Web VS Code', exact: true }).click();
  const dock = page.getByLabel('Editor folder agents', { exact: true });
  await expect(dock).toBeVisible();
  await expect(dock.getByLabel('Agent request', { exact: true })).toHaveValue('Private next request');
  await expect(dock.getByText('Fixture result. No model or VM executed.', { exact: true })).toBeVisible();
  await page.getByRole('button', { name: 'Back to canvas', exact: true }).click();
  await expect(page.getByLabel('Agent request', { exact: true })).toHaveValue('Private next request');
  assert.equal(canvas.admissions.length, 1);
});
