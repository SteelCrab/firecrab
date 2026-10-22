import { defineConfig, devices } from "@playwright/test";

const baseURL = process.env.FIRECRAB_PIPE_LIVE_URL;
if (!baseURL) throw new Error("FIRECRAB_PIPE_LIVE_URL must identify an isolated QA API");
const address = new URL(baseURL);
if (address.protocol !== "http:" || !["localhost", "127.0.0.1", "[::1]"].includes(address.hostname) || address.username || address.password || address.pathname !== "/") {
  throw new Error("Live database E2E requires a loopback HTTP QA API");
}
const engine = process.env.FIRECRAB_PIPE_LIVE_ENGINE;
if (engine !== "postgres" && engine !== "mysql") throw new Error("FIRECRAB_PIPE_LIVE_ENGINE must be postgres or mysql");

export default defineConfig({
  testDir: "tests",
  testMatch: "pipe-databases-live.spec.ts",
  workers: 1,
  retries: 0,
  timeout: 60_000,
  expect: { timeout: 10_000 },
  outputDir: `test-results/pipe-live-${engine}`,
  reporter: [["list"], ["json", { outputFile: `test-results/pipe-live-${engine}.json` }]],
  use: { baseURL, trace: "retain-on-failure", screenshot: "only-on-failure", locale: "en-US" },
  projects: [
    { name: "desktop", use: { ...devices["Desktop Chrome"] } },
    { name: "mobile", use: { ...devices["Pixel 7"] } },
  ],
});
