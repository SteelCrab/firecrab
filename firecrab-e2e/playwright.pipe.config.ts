import { defineConfig } from "@playwright/test";
import baseConfig from "./playwright.config";

// Pipe tests use API fixtures. Keep this server separate from the running app.
const servers = Array.isArray(baseConfig.webServer) ? baseConfig.webServer : [];
const baseURL = "http://127.0.0.1:8096";
export default defineConfig({
  ...baseConfig,
  testMatch: ["pipe.spec.ts", "pipe-registries.spec.ts"],
  use: { ...baseConfig.use, baseURL },
  webServer: servers.filter(server => server.command === "npm run dev")
    .map(server => ({ ...server, command: "npm run dev -- --host 127.0.0.1 --port 8096", url: baseURL, reuseExistingServer: false })),
});
