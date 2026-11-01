// Stateful controller fixture for UI contracts; no VM or real project files are used.
import { createHash } from 'node:crypto';

export function installCanvasHistoryFixture(fixture) {
  const project = '55000000-0000-4000-8000-000000000001';
  const base = `/api/ai-sandbox/projects/${project}/workspace`;
  const versions = [], requests = [];
  const views = new Map(), receipts = new Map();
  let writes = 0, loseAck = false, hideReceipt = false;

  function route(path, handler, method = 'GET') {
    fixture.handlers.set(`${method} ${path}`, handler);
  }
  function addFile(content, restoredFrom = null) {
    const file = {
      version: {
        id: versions.length + 1, path: 'api/auth.txt',
        sha256: createHash('sha256').update(content).digest('hex'),
        byteLength: Buffer.byteLength(content), author: { kind: 'local' },
        restoredFrom, createdAt: versions.length + 1,
      },
      contentBase64: Buffer.from(content).toString('base64'),
    };
    versions.push(file);
    return file;
  }
  addFile('saved before\n');
  addFile('saved current\n');

  route('/api/ai-sandbox/projects', ({ send }) => send(200, [
    { id: project, spec: { name: 'History UI test fixture', sourcePath: null, instructions: '' } },
  ]));
  route(base, ({ send }) => send(200, { projectId: project, status: 'ready' }));
  route(`${base}/files/tree`, ({ send }) => send(200, [{ name: 'api', directory: true }]));
  route(`${base}/terminals`, ({ send }) => send(200, []));
  route(`${base}/views`, ({ send }) => send(200, [...views.entries()].map(([id, row]) =>
    ({ id, revision: row.revision, updatedAt: 0 }))));
  fixture.matchHandler = ({ method, path }) => {
    if (!['GET', 'PUT'].includes(method) || !path.startsWith(`${base}/views/`)) return null;
    return ({ req, url, body, send }) => {
      const id = url.pathname.split('/').at(-1);
      if (req.method === 'GET') return send(views.has(id) ? 200 : 404,
        views.get(id) ?? { error: { message: 'Missing private view' } });
      const current = views.get(id)?.revision ?? 0;
      if (body.expectedRevision !== current) return send(409, { error: { message: 'Private view changed' } });
      const saved = { id, revision: current + 1, document: body.document };
      views.set(id, saved);
      return send(200, { revision: saved.revision });
    };
  };
  route(`${base}/files/history`, ({ url, send }) => {
    const before = url.searchParams.get('before');
    return send(200, versions.toReversed().map(file => file.version)
      .filter(version => before === null || version.id < Number(before)).slice(0, 100));
  });
  route(`${base}/files`, ({ send }) => send(200, versions.at(-1)));
  route(`${base}/files/diff`, ({ url, send }) => send(200, {
    before: versions[Number(url.searchParams.get('from')) - 1],
    after: versions[Number(url.searchParams.get('to')) - 1],
  }));
  route(`${base}/file-operations`, ({ send }) => send(200,
    hideReceipt ? [] : [...receipts.values()].map(receipt => receipt.op)));
  route(`${base}/files/restore`, ({ body, send }) => {
    requests.push(structuredClone(body));
    if (receipts.has(body.operationId)) {
      hideReceipt = false;
      const receipt = receipts.get(body.operationId);
      return send(receipt.status, receipt.file ?? { error: { message: 'Saved file changed; compare again' } });
    }
    const op = {
      id: body.operationId, path: 'api/auth.txt', expectedVersion: body.expectedVersion,
      version: null, status: 'failed',
    };
    if (body.expectedVersion !== versions.at(-1).version.id) {
      receipts.set(body.operationId, { status: 409, op });
      return send(409, { error: { message: 'Saved file changed; compare again' } });
    }
    const source = versions.find(file => file.version.id === body.version);
    if (!source) return send(404, { error: { message: 'Unknown version' } });
    writes++;
    const file = addFile(Buffer.from(source.contentBase64, 'base64').toString(), body.version);
    Object.assign(op, { status: 'applied', version: file.version.id });
    receipts.set(body.operationId, { status: 200, file, op });
    if (loseAck) {
      loseAck = false;
      hideReceipt = true;
      return send(503, { error: { message: 'Restore acknowledgement unavailable' } });
    }
    return send(200, file);
  }, 'POST');
  return {
    project, base,
    get restoreCount() { return writes; },
    get restoreRequests() { return requests; },
    get savedFiles() { return versions; },
    externalWrite(content = 'concurrent saved content\n') { return addFile(content); },
    loseNextAcknowledgement() { loseAck = true; },
  };
}
