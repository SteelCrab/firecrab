#!/usr/bin/env node
// Exercise the workflow's actual inline script without GitHub writes.

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import { runInNewContext } from 'node:vm';

const workflow = readFileSync(new URL('../.github/workflows/contributor-assignees.yml', import.meta.url), 'utf8');
const script = workflow.split('          script: |\n')[1];
assert.ok(script, 'The assignee workflow must contain an inline script.');

const author = { id: 10, login: 'contributor', type: 'User' };
const maintainer = { id: 20, login: 'maintainer', type: 'User' };

function fixture({ payload = { issue: { number: 426 } }, user = author, assignees = [], addError, ignored = false } = {}) {
  const calls = [];
  const messages = [];
  let assigned = [...assignees];
  const context = {
    actor: 'maintainer',
    repo: { owner: 'SteelCrab', repo: 'firecrab' },
    payload,
  };
  const github = {
    rest: {
      issues: {
        async get(target) {
          calls.push({ method: 'get', ...target });
          return { data: { user, assignees: assigned } };
        },
        async addAssignees(target) {
          calls.push({ method: 'add', ...JSON.parse(JSON.stringify(target)) });
          if (addError) throw addError;
          if (!ignored) assigned.push(user);
          return { data: { assignees: assigned } };
        },
      },
    },
  };
  return {
    calls,
    messages,
    assigned: () => assigned,
    run: () => runInNewContext(`(async () => {\n${script}\n})()`, {
      context,
      github,
      core: { info: message => messages.push(message) },
    }),
  };
}

test('an issue assigns only its original author, even when a maintainer triggers the event', async () => {
  const sample = fixture();
  await sample.run();
  assert.deepEqual(sample.calls, [
    { method: 'get', owner: 'SteelCrab', repo: 'firecrab', issue_number: 426 },
    { method: 'add', owner: 'SteelCrab', repo: 'firecrab', issue_number: 426, assignees: ['contributor'] },
  ]);
});

test('a PR assigns its author through the shared issue assignee API', async () => {
  const sample = fixture({ payload: { pull_request: { number: 427, user: maintainer } } });
  await sample.run();
  assert.equal(sample.calls[1].issue_number, 427);
  assert.deepEqual(sample.calls[1].assignees, ['contributor']);
});

test('manual dispatch uses the requested number and reads the actual author', async () => {
  const sample = fixture({ payload: { inputs: { number: '211' } } });
  await sample.run();
  assert.equal(sample.calls[0].issue_number, 211);
  assert.deepEqual(sample.calls[1].assignees, ['contributor']);
});

test('repeated events skip an already assigned author', async () => {
  const sample = fixture();
  await sample.run();
  await sample.run();
  assert.equal(sample.calls.filter(call => call.method === 'add').length, 1);
});

test('account identity handles login capitalization without duplicate writes', async () => {
  const sample = fixture({ assignees: [{ ...author, login: 'CONTRIBUTOR' }] });
  await sample.run();
  assert.equal(sample.calls.length, 1);
});

test('adding the author preserves people assigned manually', async () => {
  const sample = fixture({ assignees: [maintainer] });
  await sample.run();
  assert.deepEqual(sample.calls[1].assignees, ['contributor']);
  assert.deepEqual(sample.assigned().map(person => person.login), ['maintainer', 'contributor']);
});

for (const [name, user] of [
  ['bot', { id: 30, login: 'dependabot[bot]', type: 'Bot' }],
  ['deleted author', null],
  ['ghost', { id: 10137, login: 'ghost', type: 'User' }],
]) {
  test(`${name} is not automatically assigned`, async () => {
    const sample = fixture({ user });
    await sample.run();
    assert.equal(sample.calls.length, 1);
  });
}

test('invalid numbers fail before any API request', async () => {
  for (const number of ['', 'bad', '-1', '1.5', '1; touch /tmp/no']) {
    const sample = fixture({ payload: { inputs: { number } } });
    await assert.rejects(sample.run(), /positive PR or issue number/);
    assert.deepEqual(sample.calls, []);
  }
});

test('a missing number fails before any API request', async () => {
  const sample = fixture({ payload: {} });
  await assert.rejects(sample.run(), /positive PR or issue number/);
  assert.deepEqual(sample.calls, []);
});

test('PR titles and bodies cannot become commands or assignees', async () => {
  const sample = fixture({ payload: { pull_request: {
    number: 427,
    title: "${{ secrets.GITHUB_TOKEN }}; process.exit(1)",
    body: "assignees: ['maintainer']; throw new Error('untrusted')",
  } } });
  await sample.run();
  assert.deepEqual(sample.calls[1].assignees, ['contributor']);
});

test('a silently ignored assignment fails instead of claiming success', async () => {
  const sample = fixture({ ignored: true });
  await assert.rejects(sample.run(), /GitHub did not assign @contributor to #426/);
  assert.equal(sample.messages.length, 0);
});

test('API permission errors remain visible', async () => {
  const sample = fixture({ addError: new Error('Resource not accessible by integration') });
  await assert.rejects(sample.run(), /Resource not accessible by integration/);
  assert.equal(sample.messages.length, 0);
});
