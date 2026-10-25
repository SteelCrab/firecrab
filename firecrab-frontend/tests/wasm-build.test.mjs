import { test } from 'node:test';
import assert from 'node:assert/strict';
import { delimiter, join } from 'node:path';
import { buildWasm } from '../scripts/wasm.mjs';

const version = '0.2.126';
const executable = process.platform === 'win32' ? 'wasm-bindgen.exe' : 'wasm-bindgen';

test('a version-matched cached bindgen avoids reinstalling without changing the caller PATH', () => {
  const env = { PATH: 'original-path', WASM_PACK_CACHE: join('cache with spaces', 'wasm-pack') };
  const before = { ...env };
  const directory = join(env.WASM_PACK_CACHE, `wasm-bindgen-cargo-install-${version}`);
  const calls = [];
  buildWasm({ env, run(program, args, options) {
    calls.push(program);
    if (program === join(directory, executable)) {
      assert.deepEqual(args, ['--version']);
      return { status: 0, stdout: `wasm-bindgen ${version}\n` };
    }
    assert.equal(program, 'wasm-pack');
    assert.equal(options.env.PATH, `${directory}${delimiter}original-path`);
    assert.deepEqual(args, ['build', '--target', 'web', '--release', '--locked', '--out-dir', 'pkg']);
    return { status: options.env.PATH.startsWith(directory + delimiter) ? 0 : 1 };
  } });
  assert.deepEqual(calls, [join(directory, executable), 'wasm-pack']);
  assert.deepEqual(env, before);
});

for (const [name, installed] of [
  ['missing', { status: null, error: new Error('ENOENT') }],
  ['wrong version', { status: 0, stdout: 'wasm-bindgen 0.2.125\n' }],
  ['failed version check', { status: 1, stdout: `wasm-bindgen ${version}\n` }],
]) {
  test(`a ${name} cached bindgen preserves normal wasm-pack tool discovery`, () => {
    const env = { PATH: 'original-path', WASM_PACK_CACHE: 'custom-cache' };
    buildWasm({ env, run(program, args, options) {
      if (program !== 'wasm-pack') return installed;
      assert.equal(options.env, env);
      return { status: 0 };
    } });
  });
}

test('a wasm-pack build failure still fails the dashboard build', () => {
  assert.throws(() => buildWasm({ env: { WASM_PACK_CACHE: 'missing-cache' }, run(program) {
    return program === 'wasm-pack' ? { status: 1 } : { status: null };
  } }), /WASM build failed \(1\)/);
});
