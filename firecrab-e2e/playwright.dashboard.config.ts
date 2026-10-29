import { defineConfig } from "@playwright/test";
import mocked from "./playwright.mocked.config";

// All fixture suites run against the WASM dev server without a Linux API.
export default defineConfig({
  ...mocked,
  testMatch: /(stacks|ai-sandbox|block-storage|command-palette|dashboard-frame|vm-list|vm-detail|screen-details|ssh-console|console-session|wasm-terminal|host-firecrab|manager-resources|update-dialog|vm-reconciliation|micro-services|pipe-ai|pipe-auth|pipe|pipe-storage|pipe-databases|pipe-registries|registry-image-build|pools-dashboard|schedules|security-groups|security-group-connections|secrets)\.spec\.ts/,
});
