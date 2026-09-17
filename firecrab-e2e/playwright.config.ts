import path from "node:path";
import { fileURLToPath } from "node:url";
import { defineConfig, devices } from "@playwright/test";

const here = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(here, "..");
const frontend = path.join(repoRoot, "firecrab-frontend");
const skipGuestBoot =
  process.env.FIRECRAB_E2E_SKIP_GUEST_BOOT === "1" ||
  process.env.FIRECRAB_E2E_SKIP_GUEST_BOOT === "true" ||
  process.env.FIRECRAB_E2E_SKIP_GUEST_BOOT === "yes";
const reuseExistingServer =
  process.env.FIRECRAB_E2E_REUSE_SERVER === "1" ||
  process.env.FIRECRAB_E2E_REUSE_SERVER === "true";
const inCi = Boolean(process.env.CI) || Boolean(process.env.GITHUB_ACTIONS);
const baseURL = (process.env.FIRECRAB_E2E_BASE_URL ?? "http://localhost:8080").replace(
  /\/$/,
  "",
);

/**
 * Browser E2E for issue #90 — local OCI fixture only (no Docker Hub pull).
 *
 * Commands (from the repo root):
 *
 *   # inspect + import in Chromium (no guest boot):
 *   FIRECRAB_E2E_SKIP_GUEST_BOOT=1 npm test --prefix firecrab-e2e
 *
 *   # full path: inspect → import → create VM → FIRECRAB_NETWORK_READY →
 *   # FIRECRAB_OCI_E2E_READY on the console:
 *   npm test --prefix firecrab-e2e
 *
 * Playwright starts firecrab-api (unless :5523 already answers) and Vite
 * (unless :8080 already answers). Guest boot also needs KVM, firecracker on
 * PATH, and a *responsive* `./scripts/dev-net-helper.sh` (Unix socket
 * /run/firecrab/net-helper.sock). Skip guest boot only via
 * FIRECRAB_E2E_SKIP_GUEST_BOOT=1 — the suite does not infer /dev/kvm.
 *
 * The dashboard origin must be http://localhost:8080 (exact CORS allow-list).
 */
export default defineConfig({
  testDir: "tests",
  fullyParallel: false,
  workers: 1,
  retries: 0,
  forbidOnly: inCi,
  timeout: skipGuestBoot ? 240_000 : 420_000,
  expect: { timeout: 15_000 },
  reporter: [["list"]],
  use: {
    baseURL,
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
    video: "off",
    locale: "en-US",
  },
  projects: [
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"] },
    },
  ],
  webServer: [
    {
      command: "node firecrab-e2e/scripts/ensure-api.mjs",
      cwd: repoRoot,
      url: "http://127.0.0.1:5523/api/host",
      reuseExistingServer: reuseExistingServer || !inCi,
      timeout: 180_000,
    },
    {
      command: "npm run dev",
      cwd: frontend,
      url: "http://localhost:8080",
      reuseExistingServer: reuseExistingServer || !inCi,
      timeout: 120_000,
    },
  ],
});
