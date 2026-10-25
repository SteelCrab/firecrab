import { readdir, readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { brotliCompressSync, constants } from 'node:zlib';
import { gzipAsync } from '@gfx/zopfli';

// Store negotiated HTTP representations. Keep the original for older clients.
export async function compressAssets(directory) {
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) { await compressAssets(path); continue; }
    if (!/\.(?:wasm|js|css|html|svg)$/.test(entry.name)) continue;
    const original = await readFile(path);
    for (const [suffix, encoded] of [
      ['br', brotliCompressSync(original, { params: { [constants.BROTLI_PARAM_QUALITY]: 11 } })],
      ['gz', await gzipAsync(original, { numiterations: 15 })],
    ]) {
      if (encoded.length < original.length) await writeFile(`${path}.${suffix}`, encoded);
    }
  }
}
