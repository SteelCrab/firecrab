import { test, before, after, beforeEach, afterEach } from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { chromium } from '../../firecrab-e2e/node_modules/playwright-core/index.mjs';
import { expect } from '../../firecrab-e2e/node_modules/@playwright/test/index.mjs';
import { createFixtureServer } from './fixture-server.mjs';

let fixture, browser, context, page, errors;
before(async () => {
  fixture = await createFixtureServer(fileURLToPath(new URL('../../firecrab-frontend/dist', import.meta.url)), 12);
  browser = await chromium.launch({ headless: true });
});
after(async () => { await browser?.close(); await fixture?.close(); });
beforeEach(async () => {
  fixture.reset(12); errors = [];
  context = await browser.newContext({ viewport: { width: 1440, height: 1000 } });
  page = await context.newPage();
  page.on('pageerror', error => errors.push(error.message));
  page.on('console', message => { if (message.type() === 'error' && message.text().includes('panicked')) process.stderr.write(message.text() + '\n'); });
  await page.route('http://127.0.0.1:5524/**', route => route.fulfill({ status: 404, json: { error: 'No controller in fixture' } }));
});
afterEach(async () => { await context?.close(); assert.deepEqual(errors, [], 'No runtime exceptions'); });
async function open(route = 'vms') {
  await page.goto(`${fixture.url}/#/${route}`);
  if (route.startsWith('console/')) return;
  await expect(page.locator('#content')).toHaveAttribute('data-loaded', 'true');
}
async function detail(index = 1) {
  await open();
  const response = page.waitForResponse(response => response.url().endsWith(`/api/vms/${fixture.state.vms[index].id}`));
  await page.locator('.vm-row-name').nth(index).click();
  await response;
  await expect(page.getByRole('region', { name: 'Detail', exact: true })).toBeVisible();
}
function mutation(path, method = 'POST') { return fixture.requests.findLast(request => request.path === path && request.method === method); }

test('cluster preserves an unknown placement across reload and routes lifecycle and drain through the controller', async () => {
  await page.setViewportSize({ width: 1440, height: 1150 });
  const hostIds = ['00000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000002'];
  const networkIds = ['00000000-0000-0000-0000-000000000011', '00000000-0000-0000-0000-000000000012'];
  const image = { alias: 'fixture', version: 'v1', kernelSha256: 'a'.repeat(64), rootfsSha256: 'b'.repeat(64), bootArgsSha256: 'c'.repeat(64) };
  const hosts = hostIds.map((id, i) => ({ report: { id, name: `worker-${i + 1}`, endpoint: `https://worker-${i + 1}.test:5523`, architecture: 'aarch64',
    capacity: { cpu: 4, ramMib: 4096 }, unmanaged: { cpu: 0, ramMib: 0 }, images: [image], labels: {},
    networks: [{ id: networkIds[i], name: 'private' }], storages: [{ id: 'default', capacityGib: 40, unmanagedGib: 0 }] },
    lastSeenMs: 0, draining: false, online: true, reserved: { cpu: 0, ramMib: 0 }, reservedDiskGib: {} }));
  const calls = []; let placement = null; let attempts = 0;
  await page.route('**/api/cluster/**', async route => {
    const request = route.request(); const path = new URL(request.url()).pathname;
    if (request.method() === 'GET' && path === '/api/cluster/hosts') {
      hosts.forEach(host => host.lastSeenMs++);
      return route.fulfill({ json: hosts });
    }
    if (request.method() === 'GET' && path === '/api/cluster/placements') return route.fulfill({ json: placement ? [placement] : [] });
    calls.push({ path, method: request.method(), key: request.headers()['idempotency-key'], body: request.postData() ? request.postDataJSON() : null });
    if (path === '/api/cluster/placements' && request.method() === 'POST') {
      if (++attempts === 1) return route.abort('failed');
      const spec = request.postDataJSON(); const id = '00000000-0000-0000-0000-000000000099';
      placement = { id, idempotencyKey: request.headers()['idempotency-key'], vm: { hostId: hostIds[0], resourceId: id },
        target: spec.targets[0], spec, generation: 0, action: 'create', phase: 'confirmed', observed: { id, name: spec.name, state: 'created' }, error: null, createdAtMs: 1 };
      hosts[0].reserved = { cpu: 1, ramMib: 512 }; hosts[0].reservedDiskGib = { default: 2 };
      return route.fulfill({ status: 202, json: placement });
    }
    if (request.method() === 'PATCH') {
      const host = hosts.find(host => path.endsWith(host.report.id));
      host.draining = request.postDataJSON().draining;
      return route.fulfill({ json: host });
    }
    if (path.endsWith('/stop')) { placement.action = 'stop'; placement.generation++; placement.observed.state = 'stopped'; return route.fulfill({ status: 202, json: placement }); }
    return route.fulfill({ status: 404, json: {} });
  });
  await open('cluster');
  await expect(page.locator('#detail-panel')).toBeHidden();
  await expect(page.locator('#panel-toggle')).toHaveCount(0);
  // The placement form is on the placements tab.
  await page.getByRole('tab', { name: /VM placements/ }).click();
  const name = page.getByLabel('VM name', { exact: true });
  await name.fill('resilient-vm'); await name.focus();
  const heartbeat = page.waitForResponse(response => new URL(response.url()).pathname === '/api/cluster/hosts');
  await heartbeat; await expect(name).toBeFocused(); await expect(name).toHaveValue('resilient-vm');
  await page.getByRole('button', { name: 'Place VM', exact: true }).click();
  await expect(page.getByRole('button', { name: 'Retry saved placement' })).toBeVisible();
  const saved = await page.evaluate(() => sessionStorage.getItem('firecrab.cluster.placement.v1'));
  assert.ok(saved); await page.reload();
  await page.getByRole('tab', { name: /VM placements/ }).click();
  await page.getByRole('button', { name: 'Retry saved placement' }).click();
  const placed = page.locator('[data-cluster-placement]'); await expect(placed).toContainText('resilient-vm');
  const submissions = calls.filter(call => call.path === '/api/cluster/placements');
  assert.equal(submissions.length, 2); assert.equal(submissions[0].key, submissions[1].key); assert.deepEqual(submissions[0].body, submissions[1].body);
  assert.equal(submissions[0].body.targets.length, 2);
  assert.deepEqual(submissions[0].body.targets[1], { network: { hostId: hostIds[1], resourceId: networkIds[1] }, storage: { hostId: hostIds[1], resourceId: 'default' } });
  assert.equal(await page.evaluate(() => sessionStorage.getItem('firecrab.cluster.placement.v1')), null);
  await page.getByRole('tab', { name: /Workload hosts/ }).click();
  await page.locator(`[data-cluster-host="${hostIds[0]}"]`).getByRole('button', { name: 'Drain host' }).click();
  await expect(page.locator(`[data-cluster-host="${hostIds[0]}"]`).getByRole('button', { name: 'Resume placement' })).toBeVisible();
  await page.getByRole('tab', { name: /VM placements/ }).click();
  const candidate = page.locator(`[data-cluster-input="candidate-${hostIds[0]}"]`); await expect(candidate).toBeDisabled(); await expect(candidate).not.toBeChecked();
  await placed.getByRole('button', { name: 'Stop', exact: true }).click(); await expect(placed).toContainText('stopped');
  assert.ok(calls.some(call => call.path === `/api/cluster/placements/${placement.id}/stop`));
  assert.ok(calls.some(call => call.method === 'PATCH' && call.path === `/api/cluster/hosts/${hostIds[0]}` && call.body.draining));
  if (process.env.FIRECRAB_CLUSTER_SCREENSHOT) {
    await page.evaluate(() => { document.querySelector('#content').scrollTop = 0; });
    await page.screenshot({ path: process.env.FIRECRAB_CLUSTER_SCREENSHOT, fullPage: true });
  }
  await page.setViewportSize({ width: 390, height: 844 });
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), 'Cluster view fits a mobile viewport');
});

test('WASM owns the app and filters the same names, images, addresses, and state tabs', async () => {
  await open();
  await expect(page.locator('#root')).toHaveAttribute('data-engine', 'wasm');
  await expect(page.locator('.vm-list tbody tr[data-id]:visible')).toHaveCount(12);
  await page.getByRole('tab', { name: /^Running/ }).click();
  await expect(page.locator('.vm-list tbody tr[data-id]:visible')).toHaveCount(3);
  await page.getByRole('tab', { name: /^All/ }).click();
  const search = page.getByRole('searchbox', { name: 'Filter MicroVMs' });
  await search.fill(' VM-00009 '); await expect(page.locator('.vm-list tbody tr[data-id]:visible')).toHaveCount(1);
  await search.fill('UBUNTU'); await expect(page.locator('.vm-list tbody tr[data-id]:visible')).toHaveCount(12);
  await search.fill('10.42.0.2'); await expect(page.locator('.vm-list tbody tr[data-id]:visible')).toHaveCount(1);
});

test('the precompressed gzip fallback boots when Brotli is unavailable', async () => {
  await context.setExtraHTTPHeaders({ 'Accept-Encoding': 'gzip' });
  const loaded = page.waitForResponse(response => response.url().endsWith('.wasm'));
  await open();
  assert.equal((await loaded).headers()['content-encoding'], 'gzip');
  await expect(page.locator('#root')).toHaveAttribute('data-engine', 'wasm');
  await expect(page.locator('.vm-list tbody tr[data-id]:visible')).toHaveCount(12);
});

test('narrow searches restore consecutive and separated rows in server order', async () => {
  fixture.reset(200);
  for (const index of [2, 3, 9, 99, 150]) fixture.state.vms[index].template = "shared-image";
  await open();
  const rows = page.locator('#vm-rows tr[data-id]:visible');
  const originalIds = fixture.state.vms.map(vm => vm.id);
  await page.evaluate(() => { window.originalVmRows = [...document.querySelectorAll('#vm-rows tr')]; });
  const search = page.getByRole('searchbox', { name: 'Filter MicroVMs' });
  for (const query of ['vm-0001', 'vm-001', 'vm-00019', 'shared-image', 'missing', 'vm-000', 'ubuntu', '']) {
    await search.fill(query);
    const expected = fixture.state.vms.filter(vm => [vm.name, vm.template].some(value => value.includes(query))).map(vm => vm.id);
    await expect(rows).toHaveCount(expected.length);
    assert.deepEqual(await rows.evaluateAll(rows => rows.map(row => row.dataset.id)), expected);
    await expect(search).toBeFocused();
  }
  assert.deepEqual(await rows.evaluateAll(rows => rows.map(row => row.dataset.id)), originalIds);
  assert.equal(await page.evaluate(() => [...document.querySelectorAll('#vm-rows tr')].every((row, i) => row === window.originalVmRows[i])), true);
});

test('search preserves browser Unicode lowercasing for VM names and palette results', async () => {
  const names = ['ÉCOLE', 'ΟΣ', 'İSTANBUL', '한글 서버'];
  names.forEach((name, i) => { fixture.state.vms[i].name = name; });
  await open();
  const search = page.getByRole('searchbox', { name: 'Filter MicroVMs' });
  for (const name of names) {
    await search.fill(name.toLowerCase());
    await expect(page.locator('#vm-rows tr[data-id]:visible')).toHaveCount(1);
    await expect(page.locator('.vm-row-name')).toHaveText(name);
  }
  await search.fill('');
  await page.keyboard.press('ControlOrMeta+k');
  await page.locator('#palette-query').fill('école');
  await expect(page.locator('#palette-items')).toContainText('ÉCOLE');
});

test('state filters expose only matching rows to the screen and accessibility tree', async () => {
  fixture.reset(240);
  await open();
  const search = page.getByRole('searchbox', { name: 'Filter MicroVMs' });
  const table = page.locator('.vm-list');
  const originalRows = await table.locator('tbody tr').elementHandles();
  await page.locator('.vm-row-name').nth(1).click();
  await expect(page.locator('#detail-panel')).toContainText('vm-00001');
  for (const [filter, label, count] of [['running', 'Running', 60], ['stopped', 'Stopped', 60], ['error', 'Error', 60], ['all', 'All', 240]]) {
    await page.getByRole('tab', { name: new RegExp(`^${label}`) }).click();
    await expect(table.getByRole('row')).toHaveCount(count + 1);
    await expect(table.locator('tbody tr[data-id]:visible')).toHaveCount(count);
    assert.deepEqual(await table.locator('tbody tr[data-id]:visible').evaluateAll(rows => rows.map(row => row.dataset.id)), fixture.state.vms.filter(vm => filter === 'all' || vm.state === filter).map(vm => vm.id));
    await search.fill('ubuntu');
    await expect(table.getByRole('row')).toHaveCount(count + 1);
    await search.fill('');
  }
  assert.equal(await page.evaluate(rows => rows.every((row, i) => row === document.querySelectorAll('#vm-rows tr')[i]), originalRows), true);
  await page.getByRole('tab', { name: /^Running/ }).click();
  fixture.state.vms[1].state = 'running';
  const refreshed = page.waitForResponse(response => response.url().endsWith('/api/vms'));
  await refreshed;
  await expect(table.getByRole('row')).toHaveCount(62);
});

test('large lists keep all VMs reachable through scrolling, palette, keyboard and filters', async () => {
  fixture.reset(5000);
  await open();
  const table = page.locator('.vm-list');
  const port = page.locator('#vm-scroll');
  await expect(table).toHaveAttribute('aria-rowcount', '5001');
  assert.ok(await table.locator('tr[data-id]').count() < 100, 'Large lists keep a bounded DOM window');
  const focusedRow = page.getByRole('button', { name: 'vm-00010', exact: true });
  await focusedRow.focus();
  await port.evaluate(async port => { port.scrollTop = 864; await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))); });
  assert.equal(await port.evaluate(port => port.scrollTop), 864);
  await expect(focusedRow).toBeFocused();
  await port.evaluate(port => { port.scrollTop = port.scrollHeight; });
  await expect(page.locator('.vm-row-name').last()).toHaveText('vm-04999');
  await expect(page.locator('#vm-rows tr[data-id]').last()).toHaveAttribute('aria-rowindex', '5001');
  await page.setViewportSize({ width: 600, height: 600 });
  await port.evaluate(port => { port.scrollTop = port.scrollHeight; });
  await expect(page.locator('.vm-row-name').last()).toHaveText('vm-04999');
  await page.setViewportSize({ width: 1440, height: 1000 });
  await page.keyboard.press('ControlOrMeta+k');
  await page.locator('#palette-query').fill('vm-02505');
  await page.locator('#palette-query').press('Enter');
  await expect(page.locator('#detail-panel')).toContainText('vm-02505');
  await expect(page.getByRole('button', { name: 'vm-02505', exact: true })).toBeVisible();
  await page.keyboard.press('ArrowDown');
  await expect(page.locator('#detail-panel')).toContainText('vm-02506');
  const previousTop = await port.evaluate(port => port.scrollTop);
  fixture.state.vms[2506].cpuUsagePercent = 25;
  fixture.state.vms[2506].template = 'ubuntu-24.04-refreshed';
  const updated = page.waitForResponse(response => response.url().endsWith('/api/vms'));
  await updated;
  await expect(page.getByRole('button', { name: 'vm-02506', exact: true })).toBeVisible();
  await expect(page.locator('#vm-rows tr[data-id]').filter({ has: page.getByRole('button', { name: 'vm-02506', exact: true }) })).toContainText('ubuntu-24.04-refreshed');
  assert.equal(await port.evaluate(port => port.scrollTop), previousTop);
  const search = page.getByRole('searchbox', { name: 'Filter MicroVMs' });
  await search.fill('vm-00000');
  await expect(table).toHaveAttribute('aria-rowcount', '2');
  await expect(page.locator('.vm-row-name')).toHaveText('vm-00000');
  await expect(search).toBeFocused();
  await search.fill('');
  await expect(table).toHaveAttribute('aria-rowcount', '5001');
  await search.fill('vm-00000');
  await page.evaluate(() => { window.refreshedFirstRow = document.querySelector('#vm-rows tr[data-id]'); });
  await search.fill('');
  assert.equal(await page.evaluate(() => window.refreshedFirstRow === document.querySelector('#vm-rows tr[data-id]')), true);
  await page.getByRole('tab', { name: /^Running/ }).click();
  await expect(table).toHaveAttribute('aria-rowcount', '1251');
  assert.equal(await table.locator('tr[data-id]').evaluateAll(rows => rows.every(row => Number(row.dataset.id.slice(-12)) % 4 === 0)), true);
  await port.evaluate(port => { port.scrollTop = port.scrollHeight; });
  await expect(page.locator('.vm-row-name').last()).toHaveText('vm-04996');
  fixture.state.vms[1].state = 'running';
  const refreshed = page.waitForResponse(response => response.url().endsWith('/api/vms'));
  await refreshed;
  await expect(table).toHaveAttribute('aria-rowcount', '1252');
});

test('medium lists stay bounded when a state filter leaves a smaller result set', async () => {
  fixture.reset(1000);
  await open();
  const table = page.locator('.vm-list');
  await page.getByRole('tab', { name: /^Running/ }).click();
  await expect(table).toHaveAttribute('aria-rowcount', '251');
  assert.ok(await table.locator('tr[data-id]').count() < 100);
  await page.locator('#vm-scroll').evaluate(port => { port.scrollTop = port.scrollHeight; });
  await expect(page.locator('.vm-row-name').last()).toHaveText('vm-00996');
  await page.getByRole('tab', { name: /^All/ }).click();
  await expect(table).toHaveAttribute('aria-rowcount', '1001');
  await expect(page.locator('.vm-row-name').first()).toHaveText('vm-00000');
  const search = page.getByRole('searchbox', { name: 'Filter MicroVMs' });
  await search.fill('ubuntu');
  assert.ok(await table.locator('tr[data-id]').count() < 100);
  await search.fill('vm-00009');
  await expect(table).toHaveAttribute('aria-rowcount', '2');
  await expect(page.locator('.vm-row-name')).toHaveText('vm-00009');
});

test('VM creation sends numeric resources, optional storage, environment and port rules', async () => {
  await open('vms/new');
  await page.getByLabel('Name', { exact: true }).fill('new-wasm-vm');
  await page.locator('details.form-advanced summary').click();
  await page.getByLabel('Environment', { exact: true }).fill('URL=https://example.com/?a=b');
  await page.getByLabel('Port forwarding', { exact: true }).fill('8080:80');
  await page.getByRole('button', { name: 'Create MicroVM', exact: true }).click();
  await expect(page.locator('.vm-row-name').filter({ hasText: 'new-wasm-vm' })).toHaveCount(1);
  const body = mutation('/api/vms').body;
  assert.equal(body.ram, 1024); assert.equal(body.cpu, 2); assert.equal(body.storageRoot, null);
  assert.deepEqual(body.env, { URL: 'https://example.com/?a=b' });
  assert.deepEqual(body.portForwards, [{ hostPort: 8080, guestPort: 80, protocol: 'tcp' }]);
  // CreateVmRequest rejects unknown fields: nothing the page only shows may travel in the body.
  assert.deepEqual(Object.keys(body).sort(), ['blockStorages', 'cpu', 'diskGb', 'egressPolicy', 'env', 'microNetworkId', 'name', 'portForwards', 'ram', 'shellIds', 'storageRoot', 'template']);
});

test('API failures preserve the entered form and allow retry', async () => {
  await open('vms/new');
  await page.getByLabel('Name', { exact: true }).fill('retry-vm');
  fixture.failNext = { path: '/api/vms', method: 'POST', message: 'Name already exists' };
  await page.getByRole('button', { name: 'Create MicroVM', exact: true }).click();
  await expect(page.getByRole('alert')).toContainText('Name already exists');
  await expect(page.getByLabel('Name', { exact: true })).toHaveValue('retry-vm');
  await page.getByLabel('Name', { exact: true }).fill('retry-vm-2');
  await page.getByRole('button', { name: 'Create MicroVM', exact: true }).click();
  await expect(page.locator('.vm-row-name').filter({ hasText: 'retry-vm-2' })).toHaveCount(1);
});

test('VM creation opens advanced settings when an advanced field is invalid', async () => {
  await open('vms/new');
  await page.getByLabel('Name', { exact: true }).fill('bad-env');
  await page.locator('details.form-advanced summary').click();
  await page.getByLabel('Environment', { exact: true }).fill('A=1\nA=2');
  await page.locator('details.form-advanced summary').click();
  await page.getByRole('button', { name: 'Create MicroVM', exact: true }).click();
  await expect(page.locator('details.form-advanced')).toHaveAttribute('open', '');
  await expect(page.locator('#error-env')).toBeVisible();
});

test('VM resource edits preserve isolation and environment edits use the real PUT contract', async () => {
  fixture.state.vms[1].egressPolicy = 'isolated';
  await detail(1);
  await page.getByLabel('More VM actions', { exact: true }).click();
  await page.getByRole('button', { name: 'Resources', exact: true }).click();
  await page.getByLabel('CPU', { exact: true }).fill('4');
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.locator('#editor')).toHaveCount(0);
  assert.equal(mutation(`/api/vms/${fixture.state.vms[1].id}`, 'PUT').body.egressPolicy, 'isolated');
  await page.locator('.vm-row-name').nth(1).click();
  await page.getByLabel('More VM actions', { exact: true }).click();
  await page.getByRole('button', { name: 'Environment', exact: true }).click();
  await page.getByLabel('Environment (NAME=value)', { exact: true }).fill('HELLO=world');
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.locator('#editor')).toHaveCount(0);
  assert.deepEqual(mutation(`/api/vms/${fixture.state.vms[1].id}`, 'PUT').body.env, { HELLO: 'world' });
});

test('VM lifecycle actions refresh server state and deletion requires confirmation', async () => {
  await open();
  await page.locator('.vm-list tbody tr[data-id]:visible').nth(1).getByRole('button', { name: 'Start', exact: true }).click();
  await expect(page.locator('.vm-list tbody tr[data-id]:visible').nth(1)).toContainText('Running');
  await page.locator('.vm-list tbody tr[data-id]:visible').nth(1).getByRole('button', { name: 'Stop', exact: true }).click();
  await expect(page.locator('.vm-list tbody tr[data-id]:visible').nth(1)).toContainText('Stopped');
  await page.locator('.vm-row-name').nth(1).click();
  page.once('dialog', dialog => dialog.dismiss());
  await page.getByLabel('More VM actions', { exact: true }).click();
  await page.getByRole('button', { name: 'Delete', exact: true }).click();
  assert.equal(fixture.state.vms.length, 12);
  page.once('dialog', dialog => dialog.accept());
  await page.getByLabel('More VM actions', { exact: true }).click();
  await page.getByRole('button', { name: 'Delete', exact: true }).click();
  await expect(page.locator('.vm-list tbody tr[data-id]:visible')).toHaveCount(11);
});

test('all thirteen main destinations and eight Pipe destinations load without React', async () => {
  await open();
  for (const [route, title] of [['pools','Pools'],['schedules','Schedules'],['security-groups','Security groups'],['secrets','Secrets'],['services','MicroService'],['networks','Networks'],['storages','VM disk storage'],['images','Images'],['kernels','Kernels'],['shells','Shells'],['host','Host'],['pipe','Pipe'],['vms','MicroVM']]) {
    await page.locator(`nav a.sidebar-item[href="#/${route}"]`).click();
    await expect(page.getByRole('heading', { level: 1 })).toHaveText(title);
    await expect(page.locator('#content')).toHaveAttribute('data-loaded', 'true');
  }
  for (const card of ['auth','route','registry','database','storage','firecrab-tunnel','quick-tunnel','named-tunnel']) {
    await page.goto(`${fixture.url}/#/pipe/${card}`);
    await expect(page.locator('#content')).toHaveAttribute('data-loaded', 'true');
    await expect(page.locator('#content')).not.toBeEmpty();
  }
});

test('network, storage and Shell creation send the expected wire fields', async () => {
  for (const [route, values, path] of [
    ['networks', { Name: 'net-from-wasm', 'IPv4 subnet (CIDR)': '172.31.30.0/24' }, '/api/micro-networks'],
    ['storages', { Name: 'data', 'Host directory': '/srv/data' }, '/api/micro-storages'],
    ['shells', { Name: 'setup', 'Shell script': '#!/bin/sh\necho hello' }, '/api/shells'],
  ]) {
    await open(route);
    await page.getByRole('button', { name: { networks: 'New network', storages: 'Register disk location', shells: 'New shell' }[route], exact: true }).click();
    for (const [label, value] of Object.entries(values)) await page.getByLabel(label, { exact: true }).fill(value);
    await page.locator('#editor').getByRole('button', { name: route === 'storages' ? 'Register' : 'Save', exact: true }).click();
    await expect(page.locator('#editor')).toHaveCount(0);
    assert.equal(mutation(path).body.name, Object.values(values)[0]);
  }
});

test('MicroService deployment collects hostPort instead of sending unsupported replicas', async () => {
  fixture.handlers.set('GET /api/micro-services/service-1/deployment', ({ send }) => send(200, null));
  await open('services');
  await page.getByRole('button', { name: 'Open web', exact: true }).click();
  await expect(page.locator('#service-status').getByRole('status')).toHaveText('Not deployed');
  await page.getByRole('complementary', { name: 'MicroService detail' }).getByRole('button', { name: 'Deploy', exact: true }).click();
  const deploy = page.getByRole('button', { name: 'Deploy and run', exact: true });
  await expect(deploy).toBeEnabled();
  const checkedPort = page.waitForResponse(response =>
    new URL(response.url()).pathname === '/api/micro-services/service-1/deployment/check' &&
    response.request().method() === 'POST' && response.request().postDataJSON()?.hostPort === 30123);
  await page.getByLabel('First host port', { exact: true }).fill('30123');
  assert.equal((await checkedPort).status(), 200);
  await expect(deploy).toBeEnabled();
  await deploy.click();
  await expect(page.locator('#editor')).toHaveCount(0);
  assert.deepEqual(mutation('/api/micro-services/service-1/deployment').body, { hostPort: 30123, microNetworkId: null });
});

test('shell publishing uses latestContent and revision endpoints', async () => {
  await open('shells');
  await page.getByRole('button', { name: 'Details', exact: true }).click();
  await page.getByRole('button', { name: 'Publish revision', exact: true }).click();
  await expect(page.getByLabel('Shell script', { exact: true })).toHaveValue('#!/bin/sh\necho fixture');
  await page.getByLabel('Shell script', { exact: true }).fill('#!/bin/sh\necho next');
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.locator('#editor')).toHaveCount(0);
  assert.equal(mutation('/api/shells/shell-1/revisions').body.content, '#!/bin/sh\necho next');
});

test('image registration and OCI import expose completion', async () => {
  await open('images');
  await page.getByRole('tab', { name: /Packages/ }).click();
  await page.getByRole('button', { name: 'Register', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Progress', exact: true })).toBeVisible();
  assert.deepEqual(mutation('/api/microregistry/register').body, { alias: 'ubuntu-24.04', version: '2026.09.1' });
  await page.getByRole('button', { name: 'Import OCI image', exact: true }).click();
  await page.getByLabel('OCI reference', { exact: true }).fill('nginx:1.27');
  await page.locator('#editor').getByRole('button', { name: 'Import', exact: true }).click();
  await expect(page.locator('#editor')).toHaveCount(0);
  assert.equal(mutation('/api/oci/import').body.reference, 'nginx:1.27');
});

test('kernel reload restores a running install and keeps long logs in a scrollable panel', async () => {
  fixture.state.kernels = [{ version: '7.2.13', architecture: 'aarch64', installed: false, inUse: false }];
  let phase = 'running';
  let log = '[12:00:00] building Linux 7.2.13 for aarch64\n' + Array.from({ length: 300 }, (_, i) => `[12:00:00] CC kernel/source-${i}.c`).join('\n');
  fixture.handlers.set('GET /api/kernels/7.2.13/install', ({ send }) => send(200, {
    version: '7.2.13', status: phase, log, startedAtMs: 1000, downloadedBytes: 160478200,
  }));
  await open('kernels');
  await expect(page.getByRole('cell', { name: 'Building kernel', exact: true })).toBeVisible();
  const job = page.locator('.kernel-job[data-kernel-version="7.2.13"]');
  await expect(job.getByRole('progressbar', { name: 'Installation stages', exact: true })).toHaveAttribute('aria-valuenow', '2');
  await expect(job).toContainText('2 / 4 stages complete');
  await page.getByRole('cell', { name: '7.2.13', exact: true }).click();
  await expect(page.getByRole('button', { name: 'Build kernel', exact: true })).toBeDisabled();
  await expect(page.getByRole('button', { name: 'Delete', exact: true })).toHaveCount(0);
  await page.reload();
  await expect(page.getByRole('cell', { name: 'Building kernel', exact: true })).toBeVisible();
  await page.getByRole('cell', { name: '7.2.13', exact: true }).click();
  const output = page.locator('#kernel-install-log');
  await expect(output).toBeHidden();
  await page.locator('#kernel-log-details > summary').click();
  await expect(output).toBeVisible();
  await output.press('End');
  await expect(output).toHaveText(log);
  assert.equal(fixture.requests.filter(r => r.method === 'POST' && r.path.endsWith('/install')).length, 0);
  await expect(page.locator('#content .wasm-details')).toHaveCount(0);
  assert.equal(await output.evaluate(el => getComputedStyle(el).overflowY), 'auto');
  assert.ok(await output.evaluate(el => el.clientHeight <= innerHeight * .35 && el.scrollHeight > el.clientHeight));
  const box = await output.boundingBox();
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  const bottom = await output.evaluate(el => el.scrollTop);
  await page.mouse.wheel(0, -300);
  await expect.poll(() => output.evaluate(el => el.scrollTop)).toBeLessThan(bottom);
  const reading = await output.evaluate(el => el.scrollTop);
  log += '\n[12:01:00] POLL_UPDATE';
  await expect(output).toContainText('POLL_UPDATE');
  assert.equal(await output.evaluate(el => el.scrollTop), reading);
  phase = 'succeeded';
  fixture.state.kernels[0].installed = true;
  await expect(page.getByRole('cell', { name: 'Installed', exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Delete', exact: true })).toBeEnabled();
});

test('kernel install recovers an already-running conflict and gates deletion by installed and in-use state', async () => {
  const kernel = { version: '7.2.12', architecture: 'aarch64', installed: false, inUse: false };
  fixture.state.kernels = [kernel];
  let status = 'idle';
  fixture.handlers.set('GET /api/kernels/7.2.12/install', ({ send }) => send(200, {
    version: kernel.version, status, log: status === 'idle' ? '' : 'Existing build continues\nCC init/main.c', startedAtMs: 1000,
  }));
  fixture.handlers.set('POST /api/kernels/7.2.12/install', ({ send }) => {
    status = 'running';
    send(409, { error: { code: 'kernel_install_in_progress', message: 'a kernel install is already running for this version' } });
  });
  await open('kernels');
  await page.getByRole('cell', { name: kernel.version, exact: true }).click();
  await expect(page.getByRole('button', { name: 'Build kernel', exact: true })).toBeEnabled();
  await expect(page.getByRole('button', { name: 'Delete', exact: true })).toHaveCount(0);
  await page.getByRole('button', { name: 'Build kernel', exact: true }).click();
  await expect(page.locator('#kernel-install-log')).toContainText('Existing build continues');
  await expect(page.getByRole('button', { name: 'Build kernel', exact: true })).toBeDisabled();
  await expect(page.getByRole('button', { name: 'Delete', exact: true })).toHaveCount(0);
  await expect(page.locator('#root')).not.toContainText('HTTP 409');
  assert.equal(fixture.requests.filter(r => r.method === 'POST' && r.path.endsWith('/install')).length, 1);
  kernel.installed = true; kernel.inUse = true; status = 'succeeded';
  await expect(page.getByRole('cell', { name: 'Installed', exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Build kernel', exact: true })).toBeEnabled();
  await expect(page.getByRole('button', { name: 'Delete', exact: true })).toHaveCount(0);
});

test('kernel queue shows measured download progress, queued builds and verified completion', async () => {
  fixture.state.kernels = ['7.2.13', '7.2.12'].map(version => ({ version, architecture: 'aarch64', installed: false, inUse: false }));
  const downloading = { version: '7.2.13', status: 'running', startedAtMs: 1, log: 'downloading and verifying source', downloadedBytes: 25 * 1024 * 1024, totalBytes: 100 * 1024 * 1024 };
  const waiting = { version: '7.2.12', status: 'running', startedAtMs: 2, log: 'downloading and verifying source\nwaiting for the host kernel builder' };
  for (const job of [downloading, waiting]) fixture.handlers.set(`GET /api/kernels/${job.version}/install`, ({ send }) => send(200, job));
  await open('kernels');
  const active = page.locator('.kernel-job[data-kernel-version="7.2.13"]');
  const queued = page.locator('.kernel-job[data-kernel-version="7.2.12"]');
  await expect(active).toContainText('25 MiB / 100 MiB · 25%');
  await expect(active.getByRole('progressbar', { name: 'Source download', exact: true })).toHaveAttribute('value', '25.00');
  await expect(queued).toContainText('Waiting for build');
  await expect(queued.getByRole('progressbar', { name: 'Installation stages', exact: true })).toHaveAttribute('aria-valuenow', '1');
  await expect(queued).not.toContainText('%');
  await page.getByRole('button', { name: '7.2.13', exact: true }).click();
  await expect(page.locator('#kernel-install-log')).toBeHidden();
  downloading.log += '\nwaiting for the host kernel builder\nbuilding Linux 7.2.13 for aarch64\nverifying built kernel';
  await expect(active).toContainText('Verifying kernel');
  await expect(active.getByRole('progressbar', { name: 'Installation stages', exact: true })).toHaveAttribute('aria-valuenow', '3');
  downloading.status = 'succeeded'; fixture.state.kernels[0].installed = true;
  await expect(active).toContainText('4 / 4 stages complete');
  await expect(active.getByRole('progressbar', { name: 'Installation stages', exact: true })).toHaveAttribute('aria-valuenow', '4');
  await expect(page.locator('#kernel-install-log')).toBeHidden();
});

test('Pipe credentials are sent to the API and never persisted locally', async () => {
  await open('pipe/auth');
  await page.getByRole('button', { name: 'Connect GitHub', exact: true }).click();
  await page.getByLabel('Token', { exact: true }).fill('test-only-token');
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.locator('#editor')).toHaveCount(0);
  assert.equal(mutation('/api/pipe/auth/github', 'PUT').body.token, 'test-only-token');
  assert.equal(await page.evaluate(() => JSON.stringify(localStorage).includes('test-only-token')), false);
  await expect(page.locator('#content')).not.toContainText('test-only-token');
});

test('account secrets require linked Auth and import only explicitly selected names', async () => {
  await open('secrets');
  await expect(page.getByRole('link', { name: 'Open Auth', exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Preview account secrets', exact: true })).toHaveCount(0);
  fixture.state.auth.push({ provider: 'firecrab', accountId: 'account-1', services: [] });
  fixture.handlers.set('POST /api/secrets/account/preview', ({ send }) => send(200, {
    id: 'account-plan', direction: 'import', expiresAt: Date.now() + 600000,
    changes: [{ name: 'NEW_KEY', change: 'add' }, { name: 'EXISTING_KEY', change: 'update' }, { name: 'SAME_KEY', change: 'unchanged' }],
  }));
  fixture.handlers.set('POST /api/secrets/plans/account-plan/apply', ({ body, send }) => send(200, { applied: body.names, failed: [] }));
  await page.reload();
  await page.getByRole('button', { name: 'Preview account secrets', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Import preview', exact: true })).toBeVisible();
  assert.deepEqual(mutation('/api/secrets/account/preview').body, {});
  await expect(page.getByRole('checkbox', { name: 'Apply NEW_KEY', exact: true })).toBeChecked();
  await expect(page.getByRole('checkbox', { name: 'Apply EXISTING_KEY', exact: true })).not.toBeChecked();
  await expect(page.getByRole('checkbox', { name: 'Apply SAME_KEY', exact: true })).toBeDisabled();
  assert.equal(mutation('/api/secrets/plans/account-plan/apply'), undefined);
  await page.getByRole('button', { name: 'Import selected (1)', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Import preview', exact: true })).toHaveCount(0);
  assert.deepEqual(mutation('/api/secrets/plans/account-plan/apply').body, { names: ['NEW_KEY'] });
});

test('routing preserves saved tunnel configuration', async () => {
  fixture.state.pipe.config.tunnel = { id: 'existing', credentialsFile: '/private/credentials' };
  await open('pipe/route');
  await page.getByRole('button', { name: 'Edit routes', exact: true }).click();
  await page.getByLabel('HOST | /PATH | ADDRESS:PORT (one per line)').fill('example.com | / | 10.42.0.2:80');
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.locator('#editor')).toHaveCount(0);
  assert.equal(mutation('/api/pipe', 'PUT').body.config.tunnel.id, 'existing');
  assert.deepEqual(mutation('/api/pipe', 'PUT').body.config.routes, [{ host: 'example.com', pathPrefix: '/', upstreams: ['10.42.0.2:80'] }]);
});

test('database table preview renders cells as text', async () => {
  fixture.state.databases.push({ id: 'db-1', name: 'local-db', provider: 'local', engine: 'postgres', host: 'localhost', port: 5432 });
  await open('pipe/database');
  await page.getByRole('button', { name: 'Browse local-db', exact: true }).click();
  await page.getByLabel('Table', {exact:true}).selectOption({label:'public.users'});
  await expect(page.getByRole('table',{name:'Data preview'})).toContainText('1');
  assert.deepEqual(mutation('/api/pipe/databases/db-1/preview').body, { schema: 'public', table: 'users', offset: 0, limit: 50 });
});

test('navigation cancels dirty forms and ignores late responses from the previous screen', async () => {
  await open('vms/new');
  await page.getByLabel('Name', { exact: true }).fill('unsaved');
  page.once('dialog', dialog => dialog.dismiss());
  await page.locator('nav a.sidebar-item[href="#/networks"]').click();
  await expect(page).toHaveURL(/#\/vms\/new$/);
  await expect(page.getByLabel('Name', { exact: true })).toHaveValue('unsaved');
  page.once('dialog', dialog => dialog.accept());
  await page.locator('nav a.sidebar-item[href="#/networks"]').click();
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('Networks');
  fixture.delayNext = { path: '/api/vms', ms: 500 };
  await page.locator('nav a.sidebar-item[href="#/vms"]').click();
  await page.locator('nav a.sidebar-item[href="#/services"]').click();
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('MicroService');
  await expect(page.locator('.service-table .cell-sub')).toHaveAttribute('title', 'ghcr.io/team/orders:1.0');
  await new Promise(resolve => setTimeout(resolve, 550));
  await expect(page.locator('.service-table .cell-sub')).toHaveAttribute('title', 'ghcr.io/team/orders:1.0');
});

test('untrusted API text cannot create DOM elements or execute handlers', async () => {
  fixture.state.vms[0].name = '<img src=x onerror="window.pwned=true">';
  await open();
  await expect(page.locator('.vm-list img')).toHaveCount(0);
  await expect(page.locator('.vm-list')).toContainText('<img src=x');
  assert.equal(await page.evaluate(() => window.pwned), undefined);
});

test('manager resource editor uses the controller ID header without borrowing failures', async () => {
  let saved;
  const status = { controllerId: 'controller-fixture', settings: { cpu: 2, memoryMib: 4096, diskGib: 64, networkMode: 'nat', networkInterface: null } };
  await page.route('http://127.0.0.1:5524/**', async route => {
    if (route.request().method() === 'PUT') saved = { body: route.request().postDataJSON(), controller: route.request().headers()['x-firecrab-controller-id'] };
    await route.fulfill({ json: status });
  });
  await open('host');
  await page.getByRole('button', { name: 'Edit resources', exact: true }).click();
  await page.getByLabel('CPU', { exact: true }).fill('6');
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.locator('#editor')).toHaveCount(0);
  assert.equal(saved.controller, 'controller-fixture'); assert.equal(saved.body.cpu, 6);
});

test('development resource bootstrap uses its own origin instead of the primary controller', async () => {
  const endpoint = '/__firecrab_resources/api/service/resources';
  const primaryRequests = [];
  let saved;
  page.on('request', request => { if (request.url().startsWith('http://127.0.0.1:5524/')) primaryRequests.push(request.url()); });
  await page.route(`${fixture.url}/`, async route => {
    const response = await route.fetch();
    const headers = { ...response.headers() };
    delete headers['content-encoding']; delete headers['content-length'];
    await route.fulfill({ response, headers, body: (await response.text()).replace('<head>', `<head><meta name="firecrab-resource-endpoint" content="${endpoint}">`) });
  });
  await page.route(`${fixture.url}${endpoint}`, async route => {
    if (route.request().method() === 'PUT') saved = {
      body: route.request().postDataJSON(), controller: route.request().headers()['x-firecrab-controller-id'],
    };
    await route.fulfill({ json: { controllerId: 'private-controller', settings: { cpu: 2, memoryMib: 4096, diskGib: 64, networkMode: 'nat', networkInterface: null } } });
  });
  await open('host');
  await page.getByRole('button', { name: 'Edit resources', exact: true }).click();
  await page.getByLabel('CPU', { exact: true }).fill('6');
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.locator('#editor')).toHaveCount(0);
  assert.equal(saved.controller, 'private-controller');
  assert.equal(saved.body.cpu, 6);
  assert.deepEqual(primaryRequests, [], 'a worktree resource edit must never reach the primary controller');
});

test('console binds Ghostty, ends close-code 4000 sessions, and releases the socket on navigation', async () => {
  let connections = 0;
  await page.routeWebSocket('**/ws/vms/**/console', socket => {
    connections++;
    socket.send('fixture console\r\n');
    setTimeout(() => socket.close({ code: 4000, reason: 'session_ended' }), 100);
  });
  await open(`console/${fixture.state.vms[0].id}`);
  await expect(page.locator('#terminal [data-terminal-engine=ghostty]')).toBeVisible();
  await expect(page.locator('#console-status')).toHaveText('Session ended');
  await new Promise(resolve => setTimeout(resolve, 1200));
  assert.equal(connections, 1);
  await page.getByRole('link', { name: '← MicroVM', exact: true }).click();
  await expect(page.locator('.vm-list tbody tr[data-id]:visible')).toHaveCount(12);
  await expect(page.locator('#terminal')).toHaveCount(0);
});

test('registry login retries keep one encoded host in the path and exclude it from the body', async () => {
  await open('pipe/registry');
  await page.getByRole('button', { name: 'Connect registry', exact: true }).click();
  await page.getByLabel('Registry host', { exact: true }).fill('ghcr.io');
  await page.getByLabel('Username', { exact: true }).fill('fixture-user');
  await page.getByLabel('Password / token', { exact: true }).fill('fixture-secret');
  fixture.failNext = { path: '/api/pipe/registries/ghcr.io', method: 'PUT' };
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.getByRole('alert')).toBeVisible();
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.locator('#editor')).toHaveCount(0);
  assert.deepEqual(mutation('/api/pipe/registries/ghcr.io', 'PUT').body, { username: 'fixture-user', secret: 'fixture-secret' });
});

test('initial API errors recover and periodic refresh preserves search focus and query', async () => {
  fixture.failNext = { path: '/api/vms', method: 'GET', status: 503, message: 'Temporarily unavailable' };
  await open();
  await expect(page.getByRole('alert')).toContainText('Temporarily unavailable');
  await page.waitForResponse(response => response.url().endsWith('/api/vms') && response.request().method() === 'GET');
  await expect(page.locator('.vm-list tbody tr[data-id]:visible')).toHaveCount(12);
  const search = page.getByRole('searchbox', { name: 'Filter MicroVMs' });
  await search.fill('vm-00009');
  const nextRefresh = page.waitForResponse(response => response.url().endsWith('/api/vms'));
  await nextRefresh;
  await expect(search).toBeFocused();
  await expect(search).toHaveValue('vm-00009');
  await expect(page.locator('.vm-list tbody tr[data-id]:visible')).toHaveCount(1);
});

test('desktop actions fit the table and the initial host response updates the sidebar', async () => {
  await open();
  await expect(page.getByRole('region', { name: 'Host status' })).toContainText('50%');
  await expect(page.locator('#host-load')).toHaveText('0.50');
  const row = page.locator('.vm-list tbody tr[data-id]:visible').first();
  await row.hover();
  for (const label of ['Stop', 'Actions']) {
    const box = await row.getByRole('button', { name: label, exact: true }).boundingBox();
    assert.ok(box.x >= 0 && box.x + box.width <= 1440, `${label} fits the viewport`);
  }
});

test('filters retain row identity, selection and focus across tabs and unchanged polling', async () => {
  await detail(0);
  await page.evaluate(() => { window.firstVmRow = document.querySelector('#vm-rows tr'); });
  const search = page.getByRole('searchbox', { name: 'Filter MicroVMs' });
  await search.fill('vm-00000');
  await page.getByRole('tab', { name: /^Stopped/ }).click();
  await expect(page.locator('#vm-empty')).toBeVisible();
  await page.getByRole('tab', { name: /^All/ }).click();
  await expect(page.locator('#vm-rows tr[aria-selected=true]')).toHaveCount(1);
  await search.focus();
  await page.waitForResponse(r => r.url().endsWith('/api/vms'));
  assert.equal(await page.evaluate(() => window.firstVmRow === document.querySelector('#vm-rows tr')), true);
  await expect(search).toBeFocused();
  await expect(page.locator('#detail-panel')).toContainText('vm-00000');
});

test('VM detail tabs expose timeline, reconciliation, masked environment and lazy logs', async () => {
  const vm = fixture.state.vms[1];
  vm.env = { APP_TOKEN: 'sensitive-test-value', 'ſecret': 'unicode-secret-value', HELLO: 'world' };
  vm.startupTimeline = [{ step: 'configuringNetwork', outcome: 'failed', startedAtMs: 100, endedAtMs: 112, detail: 'IPv4 lease 10.42.0.9 conflicts: File exists' }];
  vm.reconciliation = { outcome: 'networkFailed', checkedAtMs: 1000, detail: '<script>never</script>' };
  await detail(1);
  await expect(page.locator('.wasm-timeline')).toContainText('IP 10.42.0.9 is used');
  await expect(page.locator('#detail-panel')).toContainText('Network recovery failed');
  assert.equal(fixture.requests.some(r => r.path.endsWith('/log')), false);
  await page.getByRole('tab', { name: 'Environment', exact: true }).click();
  await expect(page.locator('#vm-tab-panel')).toContainText('••••••••');
  await expect(page.locator('#vm-tab-panel')).not.toContainText('sensitive-test-value');
  await expect(page.locator('#vm-tab-panel')).not.toContainText('unicode-secret-value');
  await page.getByRole('button', { name: 'Show values', exact: true }).click();
  await expect(page.locator('#vm-tab-panel')).toContainText('sensitive-test-value');
  await page.getByRole('tab', { name: 'Log', exact: true }).click();
  await expect(page.locator('#vm-tab-panel')).toContainText('Boot complete');
  assert.equal(fixture.requests.filter(r => r.path.endsWith('/log')).length, 1);
  const [download] = await Promise.all([page.waitForEvent('download'), page.getByRole('button', { name: 'Save log', exact: true }).click()]);
  const { readFile } = await import('node:fs/promises');
  const log = await readFile(await download.path(), 'utf8');
  assert.match(log, /startup timeline/); assert.match(log, /Boot complete/);
});

test('live egress edits preserve running VM resources and human-readable Shell selection', async () => {
  await detail(0);
  await page.getByLabel('More VM actions', { exact: true }).click();
  await page.getByRole('button', { name: 'Egress', exact: true }).click();
  await page.getByRole('radiogroup', { name: 'Internet access', exact: true }).getByRole('radio', { name: 'Isolated', exact: true }).check();
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.locator('#editor')).toHaveCount(0);
  assert.equal(mutation(`/api/vms/${fixture.state.vms[0].id}`, 'PUT').body.egressPolicy, 'isolated');
  await page.getByLabel('More VM actions', { exact: true }).click();
  await page.getByRole('button', { name: 'Shells', exact: true }).click();
  await expect(page.locator('#editor').getByLabel('Shells', { exact: true })).toContainText('bootstrap');
  await page.locator('#editor').getByLabel('Shells', { exact: true }).selectOption('shell-1');
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.locator('#editor')).toHaveCount(0);
  assert.deepEqual(mutation(`/api/vms/${fixture.state.vms[0].id}/shells`, 'PUT').body.shellIds, ['shell-1']);
});

test('field errors preserve input and identify the rejected field', async () => {
  await open('vms/new');
  await page.getByLabel('Name', { exact: true }).fill('field-error');
  fixture.failNext = { path: '/api/vms', method: 'POST', fields: { name: 'This name is reserved.' } };
  await page.getByRole('button', { name: 'Create MicroVM', exact: true }).click();
  await expect(page.getByLabel('Name', { exact: true })).toHaveAttribute('aria-invalid', 'true');
  await expect(page.locator('#error-name')).toHaveText('This name is reserved.');
  await expect(page.getByLabel('Name', { exact: true })).toHaveValue('field-error');
});

test('palette searches VMs and supports keyboard selection and empty results', async () => {
  await open();
  await page.keyboard.press('Control+k');
  await page.getByRole('combobox', { name: 'Search destinations' }).fill('VM-00009');
  await expect(page.locator('#palette-items').getByRole('option')).toHaveCount(1);
  await page.keyboard.press('Enter');
  await expect(page.locator('#detail-panel')).toContainText('vm-00009');
  await page.keyboard.press('Control+k');
  await page.getByRole('combobox', { name: 'Search destinations' }).fill('no-such-vm');
  await expect(page.locator('#palette-items')).toHaveText('No results');
  await page.keyboard.press('Escape');
  await expect(page.locator('#palette-query')).toHaveCount(0);
});

test('panel resize persists, tooltip is shared, and narrow screens start without an overlay', async () => {
  await detail(0);
  await page.getByRole('separator', { name: 'Resize detail panel' }).focus();
  await page.keyboard.press('ArrowLeft');
  assert.equal(await page.evaluate(() => localStorage.getItem('firecrab.detailPanelWidth')), '600');
  await page.locator('[data-vm-tooltip]').first().hover();
  await expect(page.getByRole('tooltip')).toContainText('The VM is running.');
  await expect(page.getByRole('tooltip')).toHaveCount(1);
  await page.setViewportSize({ width: 390, height: 844 });
  await page.reload();
  await expect(page.locator('#detail-panel')).toBeVisible(); // Explicit VM links reopen on reload.
  await page.goto(`${fixture.url}/#/vms`);
  await expect(page.locator('#content')).toHaveAttribute('data-loaded','true');
  await expect(page.locator('#detail-panel')).toBeHidden();
  await page.locator('.vm-row-name').first().click();
  await expect(page.locator('#detail-panel')).toBeVisible();
  // A wider fallback font must not force the state tabs past a phone viewport.
  await page.addStyleTag({ content: ':root { --font: monospace; }' });
  for (const width of [390, 320]) {
    await page.setViewportSize({ width, height: 844 });
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), `No horizontal page overflow at ${width}px`);
    await expect(page.getByRole('tab', { name: /^Error/ })).toBeInViewport();
  }
});

test('registry pages preserve opaque cursors, tags and selected reference', async () => {
  fixture.state.registries.push({ registry: 'ghcr.io', provider: 'ghcr', username: 'fixture' });
  fixture.handlers.set('GET /api/pipe/registries/ghcr.io/repositories', ({ url, send }) => send(200, url.searchParams.has('cursor') ? { repositories: ['apps/web','apps/api'], nextPage: null, nextCursor: null, listingSupported: true } : { repositories: ['apps/web'], nextPage: 2, nextCursor: 'opaque/+?=&', listingSupported: true }));
  fixture.handlers.set('GET /api/pipe/registry-tags', ({ url, send }) => send(200, url.searchParams.has('last') ? { tags: ['v2','v3'], nextLast: null } : { tags: ['v1','v2'], nextLast: 'v2/+?=' }));
  await open('pipe/registry');
  await expect(page.locator('#registry-tag')).toContainText('v1');
  await page.getByRole('button', { name: 'More repositories', exact: true }).click();
  await expect(page.locator('#registry-repository option')).toHaveCount(2);
  assert.equal(fixture.requests.findLast(r => r.path.endsWith('/repositories')).query.cursor, 'opaque/+?=&');
  await page.getByRole('button', { name: 'More tags', exact: true }).click();
  await expect(page.locator('#registry-tag option')).toHaveCount(3);
  assert.equal(fixture.requests.findLast(r => r.path.endsWith('/registry-tags')).query.last, 'v2/+?=');
  await page.locator('#registry-tag').selectOption('v3');
  await page.getByRole('button', { name: 'Pull', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Progress', exact: true })).toBeVisible();
  assert.deepEqual(mutation('/api/pipe/registry-transfers').body, { action: 'pull', reference: 'ghcr.io/apps/web:v3' });
});

test('registry switching ignores old tag results and retry handles failures', async () => {
  fixture.state.registries.push({ registry: 'ghcr.io' }, { registry: 'docker.io' });
  fixture.handlers.set('GET /api/pipe/registry-tags', async ({ url, send }) => {
    const old = url.searchParams.get('reference').startsWith('ghcr.io');
    if (old) await new Promise(resolve => setTimeout(resolve, 300));
    send(200, { tags: [old ? 'old-tag' : 'new-tag'], nextLast: null });
  });
  await open('pipe/registry');
  await page.getByRole('button', { name: 'Details', exact: true }).nth(1).click();
  await page.getByRole('button', { name: 'Repositories', exact: true }).click();
  await expect(page.locator('#registry-tag')).toContainText('new-tag');
  await new Promise(resolve => setTimeout(resolve, 350));
  await expect(page.locator('#registry-tag')).not.toContainText('old-tag');
  fixture.failNext = { path: '/api/pipe/registries/docker.io/repositories', method: 'GET' };
  await page.getByRole('button', { name: 'Repositories', exact: true }).click();
  await expect(page.locator('#workflow-root [role=alert]')).toBeVisible();
  await page.locator('#workflow-root').getByRole('button', { name: 'Retry', exact: true }).click();
  await expect(page.locator('#registry-tag')).toContainText('new-tag');
});

test('Cloudflare zones filter saved routes, preserve retry choices and clear on token change', async () => {
  fixture.state.pipe.config.routes = [{ host: 'app.example.com', pathPrefix: '/', upstreams: ['10.42.0.2:80'] }, { host: 'other.invalid', pathPrefix: '/', upstreams: ['10.42.0.2:80'] }];
  await open('pipe/named-tunnel');
  await page.getByRole('button', { name: 'Connect', exact: true }).click();
  await page.getByLabel('API token', { exact: true }).fill('test-token');
  await page.getByRole('button', { name: 'Load zones', exact: true }).click();
  await expect(page.getByLabel('Zone', { exact: true })).toContainText('example.com');
  await expect(page.getByLabel('Hostname', { exact: true })).toHaveValue('app.example.com');
  await expect(page.getByLabel('Hostname', { exact: true }).locator('option')).toHaveCount(1);
  fixture.failNext = { path: '/api/pipe/cloudflare/connect', method: 'POST' };
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.getByRole('alert')).toBeVisible();
  await expect(page.getByLabel('Hostname', { exact: true })).toHaveValue('app.example.com');
  await page.getByLabel('API token', { exact: true }).fill('changed-token');
  await expect(page.getByLabel('Zone', { exact: true }).locator('option')).toHaveCount(0);
  await page.getByRole('button', { name: 'Load zones', exact: true }).click();
  await expect(page.getByLabel('Hostname', { exact: true })).toHaveValue('app.example.com');
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.locator('#editor')).toHaveCount(0);
  assert.equal(mutation('/api/pipe/cloudflare/connect').body.apiToken, 'changed-token');
});

test('quick tunnel and fixed address edits retain identity and target contracts', async () => {
  const app = '10.42.0.2:80';
  fixture.state.vms[0].portForwards = [{ hostPort: 8080, guestPort: 80, protocol: 'tcp' }];
  fixture.state.pipe.quickTunnels = [{ id: 'quick-1', name: 'web', target: { type: 'apps', upstreams: [app] } }];
  fixture.state.pipe.firecrabTunnels = [{ subdomain: 'saved', target: { type: 'quick_tunnel', id: 'quick-1' } }];
  await open('pipe/quick-tunnel');
  await page.getByRole('button', { name: 'Edit', exact: true }).click();
  await expect(page.getByLabel('App addresses (one per line)', { exact: true })).toHaveValues([app]);
  await page.getByLabel('Name', { exact: true }).fill('renamed');
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.locator('#editor')).toHaveCount(0);
  assert.deepEqual(mutation('/api/pipe/quick-tunnels').body, { id: 'quick-1', name: 'renamed', target: { type: 'apps', upstreams: [app] } });
  await open('pipe/firecrab-tunnel');
  await page.getByRole('button', { name: 'Edit', exact: true }).click();
  await expect(page.getByLabel('Ownership token', { exact: true })).toHaveCount(0);
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.locator('#editor')).toHaveCount(0);
  assert.deepEqual(mutation('/api/pipe/firecrab-tunnels').body, { subdomain: 'saved', target: { type: 'apps', upstreams: [app] } });
});

test('database preview paginates both directions with delimiter-containing identifiers', async () => {
  fixture.state.databases.push({ id: 'db-1', name: 'db' });
  fixture.handlers.set('GET /api/pipe/databases/db-1/tables', ({ send }) => send(200, { tables: [{ schema: 'a|b', name: 'a|users' }] }));
  fixture.handlers.set('POST /api/pipe/databases/db-1/preview', ({ body, send }) => send(200, { columns: [{ name: 'name' }], rows: [['<img src=x>']], hasMore: body.offset === 0 }));
  await open('pipe/database');
  await page.getByRole('button', {name:'Browse db',exact:true}).click();
  await page.getByLabel('Table',{exact:true}).selectOption({label:'a|b.a|users'});
  await page.getByRole('button', { name:'Next rows', exact: true }).click();
  await page.getByRole('button', { name:'Previous rows', exact: true }).click();
  await expect(page.getByRole('table',{name:'Data preview'})).toContainText('<img src=x>');
  await expect(page.getByRole('table',{name:'Data preview'}).locator('img')).toHaveCount(0);
  assert.deepEqual(fixture.requests.filter(r => r.path.endsWith('/preview')).map(r => r.body.offset), [0,50,0]);
  assert.equal(mutation('/api/pipe/databases/db-1/preview').body.schema, 'a|b');
});

test('SSH loads private keys only on request and preserves unrelated forwarding rules', async () => {
  fixture.state.vms[0].portForwards = [{ hostPort: 8080, guestPort: 80, protocol: 'tcp' }, { hostPort: 5353, guestPort: 53, protocol: 'udp' }];
  fixture.state.vms[0].sshHostFingerprint = 'SHA256:fixture';
  await detail(0);
  await page.getByRole('button', { name: 'SSH connection', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'SSH connection', exact: true });
  await expect(dialog).toContainText('ssh-keyscan');
  assert.equal(fixture.requests.some(r => r.path.endsWith('/ssh-key')), false);
  await dialog.getByRole('button', { name: 'Show key', exact: true }).click();
  await expect(dialog.locator('#ssh-key')).toContainText('FIXTURE-ONLY');
  await dialog.getByRole('button', { name: 'Hide key', exact: true }).click();
  await expect(dialog.locator('#ssh-key')).not.toContainText('FIXTURE-ONLY');
  const [download] = await Promise.all([page.waitForEvent('download'), dialog.getByRole('button', { name: 'Download SSH key', exact: true }).click()]);
  assert.equal(download.suggestedFilename(), 'fixture-key.pem');
  await dialog.getByLabel('Host port', { exact: true }).fill('22222');
  await dialog.getByRole('button', { name: 'Create SSH port forward', exact: true }).click();
  await expect(dialog.getByRole('button', { name: 'Remove SSH port forward', exact: true })).toBeVisible();
  assert.equal(mutation(`/api/vms/${fixture.state.vms[0].id}/port-forwards`, 'PUT').body.portForwards.length, 3);
  await dialog.getByRole('button', { name: 'Remove SSH port forward', exact: true }).click();
  await expect(dialog.getByRole('button', { name: 'Create SSH port forward', exact: true })).toBeVisible();
  assert.equal(mutation(`/api/vms/${fixture.state.vms[0].id}/port-forwards`, 'PUT').body.portForwards.length, 2);
  await page.keyboard.press('Escape');
  await expect(dialog).toHaveCount(0);
});

test('console preferences persist without reconnecting and exports include live terminal output', async () => {
  let connections = 0; const inputs = [];
  await page.routeWebSocket('**/ws/vms/**/console', socket => {
    connections++; socket.send('live-terminal-marker\r\n');
    socket.onMessage(message => inputs.push(message));
  });
  await open(`console/${fixture.state.vms[0].id}`);
  await expect(page.locator('#console-status')).toHaveText('Connected');
  await page.getByRole('button', { name: 'Display settings', exact: true }).click();
  await page.getByLabel('Font size', { exact: true }).selectOption('18');
  await page.getByLabel('Color theme', { exact: true }).selectOption('light');
  assert.deepEqual(await page.evaluate(() => JSON.parse(localStorage.getItem('firecrab.console.prefs'))), { fontSize: 18, themeId: 'light', inspectOpen: true });
  assert.equal(connections, 1);
  assert.equal(await page.evaluate(() => window.firecrabTerminalKey(new KeyboardEvent('keydown', { key: 'Process', keyCode: 229 }))), false);
  await page.evaluate(() => document.dispatchEvent(new CustomEvent('firecrab-terminal-input', { detail: '\x1b[12;80R' })));
  await page.evaluate(() => document.dispatchEvent(new CustomEvent('firecrab-terminal-input', { detail: '한글\r' })));
  await expect.poll(() => inputs.length).toBe(2);
  assert.ok(Buffer.isBuffer(inputs[0])); assert.equal(inputs[0].toString('utf8'), '\x1b[12;80R');
  assert.equal(inputs[1].toString('utf8'), '한글\r');
  const [download] = await Promise.all([page.waitForEvent('download'), page.getByRole('button', { name: 'Save log', exact: true }).click()]);
  const { readFile } = await import('node:fs/promises');
  const contents = await readFile(await download.path(), 'utf8');
  assert.match(contents, /startup timeline/); assert.match(contents, /Boot complete/); assert.match(contents, /live-terminal-marker/);
  await page.getByRole('button', { name: 'Terminal only', exact: true }).click();
  await expect(page.locator('#console-header')).toBeHidden();
  await page.keyboard.press('Escape');
  await expect(page.locator('#console-header')).toBeVisible();
  await page.reload();
  await expect(page.locator('#console-status')).toHaveText('Connected');
  await page.getByRole('button', { name: 'Display settings', exact: true }).click();
  await expect(page.getByLabel('Font size', { exact: true })).toHaveValue('18');
});

test('console reconnects transient failures and opens a new ended session only on request', async () => {
  let connections = 0;
  await page.routeWebSocket('**/ws/vms/**/console', socket => {
    connections++;
    setTimeout(() => socket.close({ code: connections === 1 ? 1011 : 4000 }), 100);
  });
  await open(`console/${fixture.state.vms[0].id}`);
  await expect(page.locator('#console-status')).toHaveText('Session ended', { timeout: 8000 });
  assert.equal(connections, 2);
  await page.getByRole('button', { name: 'New session', exact: true }).click();
  await expect.poll(() => connections).toBe(3);
});

test('update dialog reviews notes, rejects stale PID progress, follows failures and retries', async () => {
  let run = 0; let progress = { phase: 'done', percent: 100, pid: 111, updatedAtMs: Date.now() - 10000 };
  fixture.handlers.set('GET /api/update', ({ send }) => send(200, { current: '0.3.1', latest: '0.4.0', updateAvailable: true, notes: '# Changes\n<script>never</script>', releaseUrl: 'javascript:alert(1)' }));
  fixture.handlers.set('POST /api/update', ({ send }) => { run++; send(202, { current: '0.3.1', pid: 221 + run }); });
  fixture.handlers.set('GET /api/update/progress', ({ send }) => send(200, progress));
  await open('host');
  await page.getByRole('button', { name: 'Check for updates', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'Updates', exact: true });
  await expect(dialog).toContainText('Changes');
  await expect(dialog.locator('script, a[href^="javascript:"]')).toHaveCount(0);
  assert.equal(mutation('/api/update'), undefined);
  await expect(dialog.getByRole('status')).toContainText('Completed');
  const stale = page.waitForResponse(response => response.url().endsWith('/api/update/progress'));
  await dialog.getByRole('button', { name: 'Update now', exact: true }).click();
  await stale;
  await expect(dialog.getByRole('status')).toContainText('Starting the updater…');
  assert.equal(run, 1);
  await page.keyboard.press('Escape');
  await expect(dialog).toBeVisible();
  progress = { phase: 'failed', percent: 25, error: 'Fixture verification failed', pid: 222, updatedAtMs: Date.now() };
  await expect(dialog.getByRole('alert')).toHaveText('Fixture verification failed', { timeout: 7000 });
  await dialog.getByRole('button', { name: 'Try again', exact: true }).click();
  await expect.poll(() => run).toBe(2);
  progress = { phase: 'done', percent: 100, pid: 223, updatedAtMs: Date.now() };
  await expect(dialog.getByRole('status')).toContainText('Completed', { timeout: 7000 });
  await dialog.getByRole('button', { name: 'Close', exact: true }).click();
  await expect(dialog).toHaveCount(0);
});

test('dirty Back and Forward cancellation preserves indexed browser history', async () => {
  await open();
  await page.locator('nav a.sidebar-item[href="#/networks"]').click();
  await page.getByRole('button', { name: 'New network', exact: true }).click();
  await page.getByLabel('Name', { exact: true }).fill('unsaved-network');
  const cancelled = page.waitForEvent('dialog').then(dialog => dialog.dismiss());
  await page.evaluate(() => history.back());
  await cancelled;
  await expect(page).toHaveURL(/#\/networks$/);
  await expect(page.getByLabel('Name', { exact: true })).toHaveValue('unsaved-network');
  page.once('dialog', dialog => dialog.accept());
  await page.evaluate(() => history.back());
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('MicroVM');
  await page.evaluate(() => history.forward());
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('Networks');
});

test('Korean locale translates forms, detail tabs and console settings', async () => {
  await page.addInitScript(() => localStorage.setItem('firecrab.locale', 'ko'));
  await open('vms/new');
  await expect(page.getByRole('heading', { level: 2 })).toHaveText('새 MicroVM');
  await expect(page.getByLabel('이름', { exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: 'MicroVM 만들기', exact: true })).toBeVisible();
  await expect(page.locator('.form-section h3')).toHaveText(['기본', '사양', '네트워크']);
  await page.getByRole('button', { name: '취소', exact: true }).click();
  await expect(page.locator('.vm-row-name')).toHaveCount(12);
  await page.locator('.vm-row-name').first().click();
  await expect(page.getByRole('tab', { name: '개요', exact: true })).toBeVisible();
  await page.goto(`${fixture.url}/#/console/${fixture.state.vms[0].id}`);
  await page.getByRole('button', { name: '표시 설정', exact: true }).click();
  await expect(page.getByLabel('글자 크기', { exact: true })).toBeVisible();
});

test('OCI inspection and database connection tests preserve the active draft', async () => {
  let releaseKernels;
  const kernels = new Promise(resolve => { releaseKernels = resolve; });
  fixture.handlers.set('GET /api/kernels', async ({ send }) => { await kernels; send(200, []); });
  await open('images');
  await page.getByRole('button', { name: 'Import OCI image', exact: true }).click();
  await page.getByLabel('OCI reference', { exact: true }).fill('ghcr.io/example/app:v1');
  await page.getByRole('button', { name: 'Inspect image', exact: true }).click();
  await expect(page.locator('#form-result')).toContainText('aarch64');
  const inventory = page.waitForResponse(response => response.url().endsWith('/api/kernels'));
  releaseKernels();
  await inventory;
  await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
  await expect(page.locator('#form-result')).toContainText('aarch64');
  await expect(page.getByLabel('OCI reference', { exact: true })).toHaveValue('ghcr.io/example/app:v1');
  await page.getByLabel('OCI reference', { exact: true }).fill('ghcr.io/example/app:v2');
  await expect(page.locator('#form-result')).toBeEmpty();
  page.once('dialog', dialog => dialog.accept());
  await page.getByRole('button', { name: 'Cancel', exact: true }).click();
  await open('pipe/database');
  await page.getByRole('button', { name: 'Add database', exact: true }).click();
  await page.getByLabel('Connection name', { exact: true }).fill('draft-db');
  await page.getByLabel('Database URI', { exact: true }).fill('postgres://fixture.invalid/test');
  await page.getByRole('button', { name: 'Test connection', exact: true }).click();
  await expect(page.locator('#db-notice')).toHaveText('Database connection succeeded.');
  await expect(page.getByLabel('Connection name', { exact: true })).toHaveValue('draft-db');
});


test('clipboard copies private keys and public addresses without storing keys', async () => {
  await context.grantPermissions(['clipboard-read','clipboard-write']);
  fixture.state.pipe.firecrabTunnels=[{subdomain:'fixture',publicUrl:'https://fixture.example.com/',target:{type:'apps',upstreams:['10.42.0.2:80']}}];
  await open('pipe/firecrab-tunnel');
  await page.getByRole('button',{name:'Copy address',exact:true}).click();
  assert.equal(await page.evaluate(()=>navigator.clipboard.readText()),'https://fixture.example.com/');
  await detail(0);
  await page.getByRole('button',{name:'SSH connection',exact:true}).click();
  await page.getByRole('button',{name:'Copy key',exact:true}).click();
  await expect.poll(()=>page.evaluate(()=>navigator.clipboard.readText())).toContain('FIXTURE-ONLY');
  assert.equal(await page.evaluate(()=>JSON.stringify(localStorage).includes('FIXTURE-ONLY')),false);
});

test('controller resource apply sends ownership header and follows operation completion', async () => {
  let applied=false,reads=0;
  await page.route('http://127.0.0.1:5524/**',async route=>{
    if(route.request().method()==='POST') {applied=true;assert.equal(route.request().headers()['x-firecrab-controller-id'],'owner');}
    else if(applied) reads++;
    await route.fulfill({json:{controllerId:'owner',settings:{cpu:2,memoryMib:4096,diskGib:64,networkMode:'nat'},operation:{state:applied && reads<1?'applying':'complete'}}});
  });
  await open('host');
  page.once('dialog',dialog=>dialog.accept());
  await page.getByRole('button',{name:'Apply resources',exact:true}).click();
  await expect.poll(()=>applied).toBe(true);
  await expect.poll(()=>reads,{timeout:7000}).toBeGreaterThan(0);
  await expect(page.getByRole('status')).toContainText('Resources applied. The management VM is ready.');
});
