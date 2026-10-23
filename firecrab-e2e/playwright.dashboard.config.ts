import { defineConfig } from "@playwright/test";
import mocked from "./playwright.mocked.config";

// All fixture suites run against the WASM dev server without a Linux API.
export default defineConfig({
  ...mocked,
  testMatch: /(command-palette|dashboard-frame|vm-list|vm-detail|screen-details|ssh-console|console-session|host-firecrab|manager-resources|update-dialog|vm-reconciliation|micro-services|pipe-auth|pipe|pipe-databases|pipe-registries|registry-image-build)\.spec\.ts/,
});
