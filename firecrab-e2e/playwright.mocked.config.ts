import { defineConfig } from "@playwright/test";
import baseConfig from "./playwright.config";

// Every suite here answers /api in the browser. No Linux API or managed VM is required.
// The console suite's booted-guest test needs both, so it stays out of this run.
process.env.FIRECRAB_E2E_SKIP_GUEST_BOOT ??= "1";
const servers = Array.isArray(baseConfig.webServer) ? baseConfig.webServer : [];
const baseURL = process.env.FIRECRAB_MOCKED_E2E_BASE_URL ?? "http://127.0.0.1:8097";
const address = new URL(baseURL);

export default defineConfig({
  ...baseConfig,
  testMatch: /(block-storage|command-palette|dashboard-frame|vm-list|vm-detail|screen-details|ssh-console|console-session|wasm-terminal|host-firecrab|manager-resources|update-dialog|vm-reconciliation|pipe-storage|micro-services|queues|pools-dashboard|schedules|security-groups|security-group-connections)\.spec\.ts/,
  projects: process.env.FIRECRAB_MOCKED_E2E_CHANNEL === "chrome"
    ? [{ name: "chrome", use: { browserName: "chromium", channel: "chrome" } }]
    : process.env.FIRECRAB_MOCKED_E2E_CROSS_BROWSER === "1"
    ? (["chromium", "firefox", "webkit"] as const).map(browserName => ({ name: browserName, use: { browserName } }))
    : baseConfig.projects,
  timeout: 45_000,
  use: { ...baseConfig.use, baseURL },
  webServer: servers.filter(server => server.command === "npm run dev")
    .map(server => ({ ...server, command: process.env.FIRECRAB_MOCKED_E2E_PRODUCTION === "1"
      ? `npm run build && npm run preview -- --host ${address.hostname} --port ${address.port} --strictPort`
      : `npm run dev -- --host ${address.hostname} --port ${address.port}`, url: baseURL, reuseExistingServer: false })),
});
