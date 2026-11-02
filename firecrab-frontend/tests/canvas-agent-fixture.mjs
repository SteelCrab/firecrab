// Synthetic controller data for UI contracts. No provider, editor, or VM is executed.
import { randomUUID } from 'node:crypto';
import { installCanvasHistoryFixture } from './canvas-fixture.mjs';

export function installCanvasAgentFixture(fixture, canvas = installCanvasHistoryFixture(fixture)) {
  const accounts = [
    { id: randomUUID(), name: 'UI fixture · no model execution', provider: 'openai', authentication: 'chatgpt' },
    { id: randomUUID(), name: 'UI fixture · no model execution', provider: 'anthropic', authentication: 'api_key' },
  ];
  const tasks = new Map(), snapshots = new Map(), admissions = [];
  let failSave = false, loseAdmission = false, hideTasks = false, verified = true;
  const inherited = fixture.matchHandler;
  const route = (method, path, handler) => fixture.handlers.set(`${method} ${path}`, handler);
  const missing = (send) => send(404, { error: { code: 'not_found', message: 'Missing fixture record' } });
  route('GET', '/api/ai-sandbox/projects', ({ send }) => send(200, [{
    id: canvas.project, spec: { name: 'Folder agent UI · test data', sourcePath: null, instructions: '' },
  }]));
  route('GET', '/api/ai-sandbox/connections', ({ send }) => send(200, accounts));
  for (const account of accounts) {
    const id = account.provider === 'anthropic' ? 'qa-claude' : 'qa-chatgpt';
    route('GET', `/api/ai-sandbox/connections/${account.id}/models`, ({ send }) => send(200, {
      models: [{ id, name: id, verified, toolsVerified: verified }],
    }));
    route('POST', `/api/ai-sandbox/connections/${account.id}/verify`, ({ send }) => {
      verified = true; return send(200, {});
    });
  }
  route('GET', `/api/ai-sandbox/projects/${canvas.project}/tasks`, ({ url, send }) => {
    const directory = url.searchParams.get('projectDirectory');
    const label = url.searchParams.get('assignedAgent');
    let rows = hideTasks ? [] : [...tasks.values()].filter(task =>
      task.request.projectDirectory === directory && task.request.coordination.assignedAgent === label);
    const before = url.searchParams.get('before');
    if (before) rows = rows.slice(0, rows.findIndex(row => row.id === before));
    return send(200, rows.slice(-50));
  });
  fixture.matchHandler = ({ method, path }) => {
    if (path.startsWith(`${canvas.base}/views/`) && method === 'PUT') {
      return (input) => failSave ? input.send(409, { error: { message: 'Fixture private save rejected' } })
        : inherited({ method, path })(input);
    }
    if (path.startsWith(`${canvas.base}/instructions/snapshots/`)) return ({ body, send }) => {
      const id = path.split('/').at(-1);
      if (method === 'PUT' && !snapshots.has(id)) snapshots.set(id, {
        id, directory: body.directory,
        files: [{ path: 'AGENTS.md', version: 7 }, ...(body.directory ? [{ path: `${body.directory}/AGENTS.md`, version: 9 }] : [])],
      });
      return snapshots.has(id) ? send(200, snapshots.get(id)) : missing(send);
    };
    if (method === 'PUT' && path.startsWith(`/api/ai-sandbox/projects/${canvas.project}/tasks/`)) return ({ body, send }) => {
      const id = path.split('/').at(-1);
      admissions.push({ id, body: structuredClone(body) });
      if (!tasks.has(id)) tasks.set(id, {
        id, projectId: canvas.project, request: structuredClone(body), selection: body.selection,
        state: 'result', createdAt: Date.now(), result: 'Fixture result. No model or VM executed.',
        folderMutations: [], events: [],
      });
      if (loseAdmission) {
        loseAdmission = false;
        hideTasks = true;
        return send(503, { error: { message: 'Fixture acknowledgement lost' } });
      }
      return send(200, tasks.get(id));
    };
    if (path.startsWith('/api/ai-sandbox/tasks/')) return ({ send }) => {
      const [, , , , id, action] = path.split('/');
      const task = hideTasks ? null : tasks.get(id);
      if (!task) return missing(send);
      if (method === 'POST' && action === 'cancel') task.state = 'cancelled';
      return send(200, task);
    };
    return inherited?.({ method, path });
  };
  return {
    ...canvas, accounts, tasks, snapshots, admissions,
    rejectPrivateSave(value = true) { failSave = value; },
    loseNextAdmission() { loseAdmission = true; },
    revealTasks() { hideTasks = false; },
    requireModelVerification() { verified = false; },
  };
}
