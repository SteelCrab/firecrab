import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { buildWasm } from './wasm.mjs';
import { compressAssets } from './compress.mjs';
buildWasm();
const result = spawnSync(process.execPath, ['node_modules/vite/bin/vite.js', 'build'], {
  cwd: fileURLToPath(new URL('..', import.meta.url)), stdio: 'inherit',
});
if (result.error) throw result.error;
if (result.status !== 0) process.exit(result.status ?? 1);
await compressAssets(fileURLToPath(new URL('../dist/', import.meta.url)));
