import { defineConfig } from 'vite';
import { fileURLToPath } from 'node:url';
import { buildWasm } from './scripts/wasm.mjs';

export default defineConfig(({ command }) => {
  if (command === 'serve') buildWasm();
  return {
  plugins: [{
    name: 'firecrab-rust',
    configureServer(server) {
      const source = fileURLToPath(new URL('./src/', import.meta.url));
      const manifests = ['Cargo.toml', 'Cargo.lock'].map(name => fileURLToPath(new URL(name, import.meta.url)));
      server.watcher.add([source, ...manifests]);
      server.watcher.on('change', file => {
        if (!file.startsWith(source) && !manifests.includes(file)) return;
        try { buildWasm(); server.ws.send({ type: 'full-reload' }); }
        catch (error) { server.config.logger.error(error.message); }
      });
    },
  }],
  server: { port: 8080, host: true, strictPort: true, allowedHosts: true, proxy: {
    '/api': 'http://127.0.0.1:5523',
    '/ws': { target: 'ws://127.0.0.1:5523', ws: true },
  } },
  build: { target: 'es2022', assetsInlineLimit: 0 },
  };
});
