import { defineConfig } from "@playwright/test";
import mocked from "./playwright.mocked.config";
import { dashboardSpecs } from "./src/dashboardSpecs";

// All fixture suites run against the React dev server without a Linux API.
export default defineConfig({
  ...mocked,
  // Exercise the release bundle; source edits must not hot-reload tests mid-flow.
  webServer: (Array.isArray(mocked.webServer) ? mocked.webServer : []).map(
    (server) => ({
      ...server,
      command: "node ../firecrab-e2e/scripts/serve-dashboard.mjs",
    }),
  ),
  testMatch: dashboardSpecs,
});
