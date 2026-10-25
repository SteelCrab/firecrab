import { spawnSync } from 'node:child_process';
import { fileURLToPath, pathToFileURL } from 'node:url';

export function buildWasm() {
  const result = spawnSync('wasm-pack', ['build', '--target', 'web', '--release', '--locked', '--out-dir', 'pkg'], {
    cwd: fileURLToPath(new URL('..', import.meta.url)), stdio: 'inherit',
  });
  if (result.error) throw new Error('Install wasm-pack 0.14.0 and the wasm32-unknown-unknown Rust target before building the dashboard.', { cause: result.error });
  if (result.status !== 0) throw new Error(`WASM build failed (${result.status ?? result.signal})`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) buildWasm();
