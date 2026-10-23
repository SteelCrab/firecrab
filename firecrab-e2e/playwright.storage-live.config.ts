import { defineConfig, devices } from "@playwright/test";

const baseURL = process.env.FIRECRAB_STORAGE_QA_URL;
if (!baseURL || !["127.0.0.1", "localhost"].includes(new URL(baseURL).hostname)) {
  throw new Error("Run through the disposable local S3 QA runner");
}
export default defineConfig({
  testDir: "tests", testMatch: ["pipe-storage-live.spec.ts", "pipe-storage-account-live.spec.ts"], workers: 1, retries: 0, timeout: 90_000,
  reporter: [["list"]], outputDir: process.env.FIRECRAB_STORAGE_QA_ACCOUNT_CREDENTIALS ? "../target/qa/storage-account-browser" : "../target/qa/storage-live-browser",
  // The account test submits real signing material. Never retain HTTP traces.
  use: { ...devices["Desktop Chrome"], baseURL, trace: "off", screenshot: "off" },
});
