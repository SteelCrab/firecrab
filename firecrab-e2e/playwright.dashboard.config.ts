import { defineConfig } from "@playwright/test";
import mocked from "./playwright.mocked.config";

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
  testMatch:
    /(stacks|ai-sandbox|block-storage|command-palette|dashboard-frame|vm-list|vm-detail|screen-details|ssh-console|console-session|host-firecrab|manager-resources|update-dialog|vm-reconciliation|micro-services|pipe-ai|pipe-auth|pipe|pipe-storage|pipe-databases|pipe-registries|registry-image-build|pools-dashboard|schedules|security-groups|security-group-connections|secrets)\.spec\.ts/,
});
