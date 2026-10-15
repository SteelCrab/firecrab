import { defineConfig } from "@playwright/test";
import baseConfig from "./playwright.config";

// Both Host panel tests intercept every API request. Only Vite is needed.
const servers = Array.isArray(baseConfig.webServer) ? baseConfig.webServer : [];
const frontend = servers.filter((server) => server.command === "npm run dev");
if (frontend.length !== 1) {
  throw new Error("The Host panel suite requires one frontend web server");
}

export default defineConfig({
  ...baseConfig,
  testMatch: "host-firecrab.spec.ts",
  webServer: frontend.map((server) => ({ ...server, reuseExistingServer: false })),
});
