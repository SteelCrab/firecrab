import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { buildWasm } from './wasm.mjs';
const cwd = fileURLToPath(new URL('..', import.meta.url));
buildWasm();
for (const [command, args] of [
  [process.execPath, ['node_modules/vite/bin/vite.js', 'build']],
]) {
  const result = spawnSync(command, args, { cwd, stdio: 'inherit' });
  if (result.error) throw result.error;
  if (result.status !== 0) process.exit(result.status ?? 1);
}
