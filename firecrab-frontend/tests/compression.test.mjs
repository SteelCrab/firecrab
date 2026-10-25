import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readdir, readFile } from 'node:fs/promises';
import { brotliDecompressSync, gunzipSync } from 'node:zlib';

test('release representations decode to the original JavaScript, CSS and both WASM modules', async () => {
  const assets = new URL('../dist/assets/', import.meta.url);
  const names = (await readdir(assets)).filter(name => /\.(?:wasm|js|css)$/.test(name));
  assert.equal(names.filter(name => name.endsWith('.wasm')).length, 2);
  for (const name of names) {
    const original = await readFile(new URL(name, assets));
    if (original.length < 100) continue;
    for (const [suffix, decode] of [['br', brotliDecompressSync], ['gz', gunzipSync]]) {
      const encoded = await readFile(new URL(`${name}.${suffix}`, assets));
      assert.ok(encoded.length < original.length, `${name}.${suffix} reduces transfer`);
      assert.deepEqual(decode(encoded), original, `${name}.${suffix} preserves bytes`);
    }
  }
});
