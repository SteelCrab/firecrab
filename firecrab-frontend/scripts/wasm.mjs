import { spawnSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { delimiter, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const root = fileURLToPath(new URL('..', import.meta.url));

function bindgenEnvironment(env, run) {
  // Cargo writes this package name/version pair into the generated lockfile.
  const lock = readFileSync(new URL('../Cargo.lock', import.meta.url), 'utf8');
  const version = lock.match(/^name = "wasm-bindgen"\r?\nversion = "([0-9.]+)"$/m)?.[1];
  if (!version) throw new Error('The dashboard Cargo.lock must contain a locked wasm-bindgen version.');
  let cache = env.WASM_PACK_CACHE;
  if (cache === undefined) {
    const base = process.platform === 'darwin' ? join(homedir(), 'Library', 'Caches')
      : process.platform === 'win32' ? (env.LOCALAPPDATA || join(homedir(), 'AppData', 'Local'))
      : (env.XDG_CACHE_HOME || join(homedir(), '.cache'));
    cache = join(base, '.wasm-pack');
  }
  const directory = join(cache, `wasm-bindgen-cargo-install-${version}`);
  const binary = join(directory, process.platform === 'win32' ? 'wasm-bindgen.exe' : 'wasm-bindgen');
  const installed = run(binary, ['--version'], { cwd: root, env, encoding: 'utf8', timeout: 5000 });
  if (installed.status !== 0 || installed.stdout?.trim() !== `wasm-bindgen ${version}`) return env;
  // wasm-pack verifies this version too. Pass the cache only to this child;
  // its installer need not run again when a matching tool is already present.
  const key = process.platform === 'win32'
    ? (Object.keys(env).find(key => key.toUpperCase() === 'PATH') || 'PATH') : 'PATH';
  return { ...env, [key]: `${directory}${delimiter}${env[key] || ''}` };
}

export function buildWasm({ env = process.env, run = spawnSync } = {}) {
  const result = run('wasm-pack', ['build', '--target', 'web', '--release', '--locked', '--out-dir', 'pkg'], {
    cwd: root, stdio: 'inherit', env: bindgenEnvironment(env, run),
  });
  if (result.error) throw new Error('Install wasm-pack 0.14.0 and the wasm32-unknown-unknown Rust target before building the dashboard.', { cause: result.error });
  if (result.status !== 0) throw new Error(`WASM build failed (${result.status ?? result.signal})`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) buildWasm();
