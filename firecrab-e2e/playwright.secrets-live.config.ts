import { defineConfig, devices } from "@playwright/test";

export default defineConfig({
  testDir: "secrets",
  testMatch: "live.spec.ts",
  workers: 1,
  retries: 0,
  forbidOnly: !!process.env.CI,
  timeout: 60_000,
  expect: { timeout: 10_000 },
  outputDir: "test-results/secrets-live/artifacts",
  reporter: [
    ["list"],
    ["json", { outputFile: "test-results/secrets-live/results.json" }],
    ["junit", { outputFile: "test-results/secrets-live/junit.xml" }],
  ],
  use: {
    ...devices["Desktop Chrome"],
    launchOptions: process.env.FIRECRAB_SECRETS_E2E_CHROMIUM
      ? { executablePath: process.env.FIRECRAB_SECRETS_E2E_CHROMIUM }
      : {},
    screenshot: "only-on-failure",
    trace: "retain-on-failure",
  },
});
