import { defineConfig } from "@playwright/test";
import baseConfig from "./playwright.config";

// Service API responses are mocked; this suite only needs the dashboard server.
const servers = Array.isArray(baseConfig.webServer) ? baseConfig.webServer : [];
const baseURL = process.env.FIRECRAB_SERVICE_E2E_BASE_URL ?? "http://127.0.0.1:8095";
const address = new URL(baseURL);

export default defineConfig({
  ...baseConfig,
  testMatch: "micro-services.spec.ts",
  use: { ...baseConfig.use, baseURL },
  webServer: servers.filter((server) => server.command === "npm run dev")
    .map((server) => ({ ...server, command: `npm run dev -- --host ${address.hostname} --port ${address.port}`, url: baseURL, reuseExistingServer: false })),
});
