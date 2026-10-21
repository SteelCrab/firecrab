import { defineConfig } from "@playwright/test";
import baseConfig from "./playwright.config";

// PIPE responses are mocked. No Linux API or managed VM is required.
const servers = Array.isArray(baseConfig.webServer) ? baseConfig.webServer : [];
const baseURL = process.env.FIRECRAB_PIPE_E2E_BASE_URL ?? "http://127.0.0.1:8096";
const address = new URL(baseURL);

export default defineConfig({
  ...baseConfig,
  testMatch: /pipe(?:-databases)?\.spec\.ts/,
  timeout: 45_000,
  use: { ...baseConfig.use, baseURL },
  webServer: servers.filter(server => server.command === "npm run dev")
    .map(server => ({ ...server, command: `npm run dev -- --host ${address.hostname} --port ${address.port}`, url: baseURL, reuseExistingServer: false })),
});
