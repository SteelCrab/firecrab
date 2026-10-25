import { defineConfig } from "@playwright/test";
import base from "./playwright.pipe.config";
const port = Number(process.env.FIRECRAB_SANDBOX_E2E_PORT ?? 8097);
if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error("Invalid Sandbox E2E port");
const url = `http://127.0.0.1:${port}`;
export default defineConfig({ ...base, testMatch: ["ai-sandbox.spec.ts"], use: { ...base.use, baseURL: url }, webServer: [{ command: `npm run dev -- --host 127.0.0.1 --port ${port}`, cwd: "../firecrab-frontend", url, reuseExistingServer: false, timeout: 60_000 }] });
