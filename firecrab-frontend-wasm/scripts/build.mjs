import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
const cwd = fileURLToPath(new URL('..', import.meta.url));
for (const [command, args] of [
  ['wasm-pack', ['build', '--target', 'web', '--release', '--locked', '--out-dir', 'pkg']],
  [process.execPath, ['node_modules/vite/bin/vite.js', 'build']],
]) {
  const result = spawnSync(command, args, { cwd, stdio: 'inherit' });
  if (result.error) throw result.error;
  if (result.status !== 0) process.exit(result.status ?? 1);
}
