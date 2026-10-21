import path from "node:path";
import { fileURLToPath } from "node:url";
import { defineConfig, devices } from "@playwright/test";

const here = path.dirname(fileURLToPath(import.meta.url));
const baseURL = "http://127.0.0.1:8096";

export default defineConfig({
  testDir: "tests", testMatch: "secrets.spec.ts", workers: 1, retries: 0,
  forbidOnly: !!process.env.CI,
  timeout: 30_000, expect: { timeout: 10_000 },
  outputDir: "test-results/secrets-ui/artifacts",
  reporter: [
    ["list"],
    ["json", { outputFile: "test-results/secrets-ui/results.json" }],
    ["junit", { outputFile: "test-results/secrets-ui/junit.xml" }],
  ],
  use: {
    baseURL, ...devices["Desktop Chrome"], screenshot: "only-on-failure", trace: "retain-on-failure",
    launchOptions: process.env.FIRECRAB_SECRETS_E2E_CHROMIUM
      ? { executablePath: process.env.FIRECRAB_SECRETS_E2E_CHROMIUM }
      : {},
  },
  webServer: {
    command: "npm run dev -- --host 127.0.0.1 --port 8096",
    cwd: path.resolve(here, "../firecrab-frontend"), url: baseURL, reuseExistingServer: false,
  },
});
