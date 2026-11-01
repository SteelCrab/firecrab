import { test, before, after, beforeEach, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { chromium } from '../../firecrab-e2e/node_modules/playwright-core/index.mjs';
import { expect } from '../../firecrab-e2e/node_modules/@playwright/test/index.mjs';
import { createFixtureServer } from './fixture-server.mjs';
import { installCanvasHistoryFixture } from './canvas-fixture.mjs';

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
  await page.reload();
  await page.getByRole('button', { name: 'Load retained history', exact: true }).click();
  await expect(page.locator('.cw-history li')).toHaveCount(100);
  assert.equal(canvas.restoreRequests.length, 0);
  assert.equal(canvas.restoreCount, 0);
});
