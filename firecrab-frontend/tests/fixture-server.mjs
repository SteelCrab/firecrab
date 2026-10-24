import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { resolve, sep, extname } from 'node:path';
import { gzipSync } from 'node:zlib';

export function vm(index = 0, overrides = {}) {
  const name = `vm-${String(index).padStart(5, '0')}`;
  const state = ['running', 'stopped', 'error', 'created'][index % 4];
  return {
    id: `00000000-0000-4000-8000-${String(index).padStart(12, '0')}`,
    name, state, template: 'ubuntu-24.04', templateVersion: '2026.09.1',
    cpu: 2, ram: 1024, diskGb: 8, startupStep: null, startupTimeline: [],
    reconciliation: null, egressPolicy: 'internet', ipv4: state === 'running' ? `10.42.${Math.floor(index / 250)}.${index % 250 + 2}` : null,
    ipv6: null, mac: null, hostname: name, microNetworkId: 'network-1', storageRoot: '/storage',
    cpuUsagePercent: state === 'running' ? 12.5 : null,
    memoryUsedMib: state === 'running' ? 256 : null, memoryTotalMib: 1024,
    memoryUsedPercent: state === 'running' ? 25 : null,
    usageHistory: state === 'running' ? Array.from({ length: 12 }, (_, n) => ({ timestampMs: n * 3000, cpuUsagePercent: 10 + n % 5, memoryUsedPercent: 25 })) : [],
    shellRefs: [], portForwards: [], env: {}, sshHostFingerprint: null,
    ...overrides,
  };
}
const network = { id: 'network-1', name: 'default', subnetCidr: '10.42.0.0/16', gateway: '10.42.0.1', internetEnabled: true, uplink: null, ipv6Cidr: null, ipv6Gateway: null, ipv6AddressMode: null, ipv6Egress: null };
const image = { alias: 'ubuntu-24.04', version: '2026.09.1', installed: true, minDiskGb: 2, sizeBytes: 1048576, kernelVersion: '6.12.1', architecture: 'aarch64' };
const shell = { id: 'shell-1', name: 'bootstrap', description: 'Bootstrap', latestVersion: 1, createdAtMs: 0, updatedAtMs: 0 };
const service = { id: 'service-1', name: 'web', image: 'ghcr.io/team/orders:1.0', runtime: 'vm', port: 80, healthPath: '/', domain: null, ram: 512, cpu: 1, replicas: 1, startupTimeout: 120, drainTimeout: 30, env: {}, secrets: {} };
const storage = { id: 'storage-1', name: 'default', path: '/storage', availableGib: 100, totalGib: 200, kind: 'directory' };
const host = { uptimeSeconds: 3600, loadAverage1m: 0.5, memoryTotalMib: 16384, memoryAvailableMib: 8192, diskTotalGib: 200, diskAvailableGib: 100,
  platform: { os: 'linux', name: 'Debian', architecture: 'aarch64', virtualization: 'kvm' } };

export async function createFixtureServer(root, initialRows = 100) {
  root = resolve(root);
  const files = new Map();
  const fixture = {
    requests: [], failNext: null, delayNext: null, handlers: new Map(), state: {},
    reset(rows = initialRows) {
      this.requests = [];
      this.handlers.clear();
      this.failNext = null;
      this.delayNext = null;
      this.state = { vms: Array.from({ length: rows }, (_, i) => vm(i)), services: [structuredClone(service)], networks: [structuredClone(network)], storages: [structuredClone(storage)], images: [structuredClone(image)], shells: [structuredClone(shell)],
        kernels: [{ version: '6.12.1', installed: true, architecture: 'aarch64' }],
        auth: [{provider:"github",account:"team",registry:"ghcr.io",region:null,services:["registry"]}], aiConnections: [], registries: [], databases: [], pools: [], schedules: [], securityGroups: [], secrets: [], connections: [], storageConnections: [], pipe: { config: { listen: '127.0.0.1:8080', routes: [], tunnel: null }, units: [], firecrabTunnels: [], quickTunnels: [], nginxAvailable: true, cloudflaredAvailable: true } };
    },
  };
  fixture.reset();
  const server = createServer(async (req, res) => {
    try {
      const url = new URL(req.url, 'http://fixture');
      const path = decodeURIComponent(url.pathname);
      const method = req.method;
      res.setHeader('Cache-Control', 'no-store');
      const send = (status, value, type = 'application/json') => {
        if (status === 204) { res.writeHead(status); res.end(); return; }
        const body = Buffer.isBuffer(value) ? value : Buffer.from(type === 'application/json' ? JSON.stringify(value) : value);
        const compressed = req.headers['accept-encoding']?.includes('gzip') ? gzipSync(body) : body;
        if (compressed !== body) res.setHeader('Content-Encoding', 'gzip');
        res.writeHead(status, { 'Content-Type': type, 'Content-Length': compressed.length }); res.end(compressed);
      };
      if (!path.startsWith('/api/')) {
        const filename = resolve(root, `.${path === '/' ? '/index.html' : path}`);
        if (!filename.startsWith(root + sep)) return send(403, 'Forbidden', 'text/plain');
        let contents = files.get(filename);
        if (!contents) { try { contents = await readFile(filename); files.set(filename, contents); } catch { return send(404, 'Missing file', 'text/plain'); } }
        for (const [encoding, suffix] of [['br', 'br'], ['gzip', 'gz']]) {
          if (!req.headers['accept-encoding']?.split(',').some(value => value.trim().split(';')[0] === encoding && !/;\s*q=0(?:\.0*)?$/.test(value.trim()))) continue;
          const encodedPath = `${filename}.${suffix}`;
          if (!files.has(encodedPath)) { try { files.set(encodedPath, await readFile(encodedPath)); } catch { files.set(encodedPath, null); } }
          const encoded = files.get(encodedPath);
          if (!encoded) continue;
          const types = { '.wasm': 'application/wasm', '.js': 'text/javascript', '.css': 'text/css', '.html': 'text/html' };
          res.writeHead(200, { 'Content-Type': types[extname(filename)] ?? 'application/octet-stream', 'Content-Encoding': encoding, 'Content-Length': encoded.length, 'Vary': 'accept-encoding' });
          res.end(encoded); return;
        }
        const types = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.wasm': 'application/wasm', '.png': 'image/png', '.svg': 'image/svg+xml', '.ico': 'image/x-icon', '.woff2': 'font/woff2' };
        return send(200, contents, types[extname(filename)] ?? 'application/octet-stream');
      }
      let raw = '';
      for await (const chunk of req) { raw += chunk; if (raw.length > 2_000_000) return send(413, { error: { message: 'Request too large' } }); }
      const body = raw ? JSON.parse(raw) : null;
      fixture.requests.push({ path, method, body, query: Object.fromEntries(url.searchParams) });
      const handler = fixture.handlers.get(`${method} ${path}`);
      if (handler) return await handler({ req, res, url, body, send, fixture });
      if (fixture.delayNext?.path === path) { const delay = fixture.delayNext; fixture.delayNext = null; await new Promise(resolve => setTimeout(resolve, delay.ms)); }
      if (fixture.failNext?.path === path && (!fixture.failNext.method || fixture.failNext.method === method)) { const failure = fixture.failNext; fixture.failNext = null; return send(failure.status ?? 422, { error: { message: failure.message ?? 'Fixture rejected the request', fields: failure.fields } }); }
      const state = fixture.state;
      const entries = { '/api/pipe/ai/connections':'aiConnections', '/api/pipe/storage':'storageConnections','/api/pools':'pools','/api/schedules':'schedules','/api/security-groups':'securityGroups','/api/secrets/connections':'connections','/api/secrets':'secrets', '/api/vms': 'vms', '/api/micro-services': 'services', '/api/micro-networks': 'networks', '/api/micro-storages': 'storages', '/api/images': 'images', '/api/kernels': 'kernels', '/api/shells': 'shells', '/api/pipe/auth': 'auth', '/api/pipe/registries': 'registries', '/api/pipe/databases': 'databases' };
      const resourceBase = Object.keys(entries).sort((a, b) => b.length - a.length).find(base => path === base || path.startsWith(base + '/'));
      const simple = {
        '/api/host': host,
        '/api/info': { version: '0.3.1', build: 'benchmark-fixture', architecture: 'aarch64', paths: {}, services: [] },
        '/api/update': { current: '0.3.1', latest: '0.3.1', updateAvailable: false },
        '/api/update/progress': { phase: 'idle', log: '' },
        '/api/storage': [storage], '/api/storage/devices': [], '/api/network': {},
        '/api/microregistry': { source: 'fixture', images: [{ ...image, downloadable: true, packageStaged: true }] },
        '/api/pipe': state.pipe,
        '/api/pipe/registry-images': [],
        '/api/pipe/registry-tags': { tags: ['1.0','2.0','stable-alpine'], nextLast: null },
        '/api/oci/inspect': { reference: url.searchParams.get('reference'), architecture: 'aarch64', compatible: true },
      };
      if (method === 'GET' && path in simple) return send(200, simple[path]);
      if (method === 'GET' && resourceBase === path) return send(200, state[entries[resourceBase]]);
      if (path.endsWith('/deployment/check')) return send(200, null);
      if (path.endsWith('/repositories')) return send(200, {repositories:['team/orders','team/nginx'],listingSupported:true,nextPage:null});
      if (path === '/api/oci/import' || path === '/api/pipe/registry-transfers') return send(202, { alias: 'nginx-1.27', status: 'succeeded', log: 'Fixture import complete' });
      if (path.startsWith('/api/oci/import/') || path.startsWith('/api/pipe/registry-transfers/')) return send(200, { alias: 'nginx-1.27', status: 'succeeded', log: 'Fixture import complete' });
      if (path === '/api/microregistry/register') return send(202, { jobId: 'register-1', status: 'succeeded', log: 'Registered' });
      if (path.startsWith('/api/microregistry/jobs/')) return send(200, { jobId: 'register-1', status: 'succeeded', log: 'Registered' });
      if (path.startsWith('/api/pipe/cloudflare/zones')) return send(200, [{ id: 'zone-1', name: 'example.com' }]);
      if (['/api/pipe/cloudflare/connect', '/api/pipe/cloudflare/disconnect', '/api/pipe/disable', '/api/pipe/service', '/api/pipe/firecrab-tunnels', '/api/pipe/quick-tunnels'].some(base => path === base || path.startsWith(base + '/'))) return send(200, state.pipe);
      if (path === '/api/pipe' && method === 'PUT') { state.pipe.config = body.config; return send(200, state.pipe); }
      if (resourceBase) {
        const key = entries[resourceBase];
        const tail = path.slice(resourceBase.length + 1);
        const [id, action, revision] = tail.split('/');
        const index = state[key].findIndex(item => [item.id, item.alias, item.version, item.provider, item.registry].includes(id));
        const item = state[key][index];
        if (method === 'POST' && path === resourceBase) {
          if (key === 'vms') {
            for (const field of ['cpu', 'ram', 'diskGb']) if (!Number.isInteger(body[field]) || body[field] < 1) return send(422, { error: { message: `${field} must be a positive number` } });
          }
          const next = { ...body, id: `created-${state[key].length}`, ...(key === 'vms' ? { state: 'created', usageHistory: [] } : {}) };
          state[key].push(next); return send(201, next);
        }
        if (key === 'registries' && action === 'repositories') return send(200, { repositories: ['apps/web'], nextPage: null, listingSupported: true });
        if (key === 'databases' && action === 'tables') return send(200, { tables: [{ schema: 'public', name: 'users' }], truncated: false });
        if (key === 'databases' && action === 'preview') return send(200, { columns: [{ name: 'id', dataType: 'int' }], rows: [['1']], hasMore: false, offset: body.offset });
        if (key === 'databases' && id === 'test') return send(200, { connected: true });
        if (key === 'registries' && method === 'PUT' && Object.keys(body).some(key => !['username','secret','provider','accountName'].includes(key))) return send(422, { error: { message: 'Unknown registry login field' } });
        if (['auth', 'registries'].includes(key) && method === 'PUT') { const next = key === 'auth' ? { provider: id, account: 'fixture-user', registry: 'ghcr.io', region: null, services: ['registry'] } : { registry: id, username: body.username, provider: 'ghcr' }; state[key].push(next); return send(200, next); }
        if (!item) return send(404, { error: { message: 'Resource not found' } });
        if (key === 'vms' && ['start', 'stop'].includes(action)) { item.state = action === 'start' ? 'running' : 'stopped'; return send(200, item); }
        if (key === 'vms' && action === 'log') return send(200, { consoleLog: 'Boot complete\nfixture login:', truncated: false });
        if (key === 'vms' && action === 'ssh-key') { res.setHeader('Content-Disposition', 'attachment; filename="fixture-key.pem"'); return send(200, '-----BEGIN OPENSSH PRIVATE KEY-----\nFIXTURE-ONLY\n-----END OPENSSH PRIVATE KEY-----', 'text/plain'); }
        if (key === 'vms' && action === 'ssh-host-key') return send(200, { fingerprint: 'SHA256:fixture', publicKey: 'ssh-ed25519 FIXTURE' });
        if (key === 'services' && action === 'deployment' && method === 'POST') {
          if (!Number.isInteger(body.hostPort) || Object.keys(body).some(key => !['hostPort', 'microNetworkId'].includes(key))) return send(422, { error: { message: 'Expected hostPort and optional microNetworkId' } });
          return send(200, { phase: 'running', instances: [], request: body });
        }
        if (key === 'services' && ['deployment', 'stop'].includes(action)) return send(200, { phase: action === 'stop' ? 'stopped' : 'running', instances: [], request: { hostPort: 30100 } });
        if (key === 'shells' && action === 'revisions') return send(200, { shellId: id, revisionId: revision ?? 'revision-2', version: 2, content: body?.content ?? '#!/bin/sh\necho fixture', createdAtMs: 0 });
        if (['images', 'kernels'].includes(key) && ['install', 'package'].includes(action)) return send(200, { alias: id, status: 'succeeded', log: 'Installed' });
        if (method === 'GET') return send(200, key === 'shells' ? { ...item, revisions: [{ id: 'revision-1', version: 1 }], latestContent: '#!/bin/sh\necho fixture' } : item);
        if (method === 'DELETE') { state[key].splice(index, 1); return send(204, null); }
        if (['PUT', 'PATCH'].includes(method)) { Object.assign(item, body); return send(200, item); }
      }
      return send(404, { error: { message: `No fixture: ${method} ${path}` } });
    } catch (error) { res.writeHead(500, { 'Content-Type': 'application/json' }); res.end(JSON.stringify({ error: { message: error.message } })); }
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  fixture.url = `http://127.0.0.1:${server.address().port}`;
  fixture.close = () => new Promise(resolve => { server.closeAllConnections(); server.close(resolve); });
  return fixture;
}
