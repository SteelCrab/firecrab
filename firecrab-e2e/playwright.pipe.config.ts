import { defineConfig } from "@playwright/test";
import baseConfig from "./playwright.config";

// Pipe responses are mocked. No Linux API or managed VM is required.
// Keep this server separate from the running app.
const servers = Array.isArray(baseConfig.webServer) ? baseConfig.webServer : [];
const baseURL = process.env.FIRECRAB_PIPE_E2E_BASE_URL ?? "http://127.0.0.1:8096";
const address = new URL(baseURL);

export default defineConfig({
  ...baseConfig,
  testMatch: ["pipe.spec.ts", "pipe-databases.spec.ts", "pipe-registries.spec.ts", "registry-image-build.spec.ts"],
  timeout: 45_000,
  use: { ...baseConfig.use, baseURL },
  webServer: servers.filter(server => server.command === "npm run dev")
    .map(server => ({ ...server, command: `npm run dev -- --host ${address.hostname} --port ${address.port}`, url: baseURL, reuseExistingServer: false })),
});
