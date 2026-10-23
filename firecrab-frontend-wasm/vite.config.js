import { defineConfig } from 'vite';
import { fileURLToPath } from 'node:url';
export default defineConfig({
  publicDir: fileURLToPath(new URL('../firecrab-frontend/public', import.meta.url)),
  server: { proxy: {
    '/api': 'http://127.0.0.1:5523',
    '/ws': { target: 'ws://127.0.0.1:5523', ws: true },
  } },
  build: { target: 'es2022', assetsInlineLimit: 0 },
});
