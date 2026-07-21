import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// https://vite.dev/config/
export default defineConfig({
  plugins: [react()],
  server: {
    port: 8080,
    // firecrab-api's default FIRECRAB_ALLOWED_ORIGINS is an exact-string
    // match on http://localhost:8080; silently hopping to 8081 on a port
    // clash would turn into a confusing 403 instead of a clear bind error.
    strictPort: true,
    proxy: {
      '/api': {
        target: 'http://127.0.0.1:3000',
        changeOrigin: true,
      },
      // Kept as its own path prefix (not nested under /api) because Vite's
      // proxy, like trunk's, is HTTP-or-WebSocket per prefix rather than
      // per-request — see the matching comment in firecrab-api/src/server.rs.
      '/ws': {
        target: 'ws://127.0.0.1:3000',
        ws: true,
      },
    },
  },
})
