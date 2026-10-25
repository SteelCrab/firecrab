import { defineConfig } from 'vite';
import { fileURLToPath } from 'node:url';
import { ghosttyOptimizations } from './build/ghosttyOptimizations.js';
import { buildWasm } from './scripts/wasm.mjs';

export default defineConfig(({ command }) => {
  if (command === 'serve') buildWasm();
  return {
  plugins: [ghosttyOptimizations(), {
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
  // Vite's default host checks keep the forwarded authority trustworthy.
  server: { port: 8080, host: true, strictPort: true, proxy: {
    // Preserve the dashboard authority for the API's same-origin checks on any dev port.
    '/api': { target: 'http://127.0.0.1:5523', changeOrigin: false },
    '/ws': { target: 'ws://127.0.0.1:5523', ws: true },
  } },
  build: { target: 'es2022', assetsInlineLimit: 0 },
  };
});
