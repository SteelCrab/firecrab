import { expect, test } from "@playwright/test";
import { openDashboard } from "./support/mockApi";

test("Security shows deployment identity and never reports unavailable scans as clean", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await openDashboard(page, {
    hash: "#/security",
    routes: { "/api/service-security": {
      services: [{ id: "a", name: "orders-api", deploymentId: "deploy-1", digest: "sha256:123456", replicas: 2 }],
      cveScan: "unavailable", threatDetection: "unavailable", aiAnalysis: "unavailable"
    } }
  });
  await expect(page.getByRole("navigation", { name: "Main navigation" }).getByRole("link", { name: "Security", exact: true })).toHaveAttribute("aria-current", "page");
  await expect(page.getByText("orders-api", { exact: true })).toBeVisible();
  await expect(page.getByText("deploy-1", { exact: true })).toBeVisible();
  await expect(page.getByText("Not scanned", { exact: true })).toBeVisible();
  await expect(page.getByText(/CVE scanning, threat detection, and AI analysis are not available yet/)).toBeVisible();
  if (process.env.SECURITY_SCREENSHOT) await page.screenshot({ path: process.env.SECURITY_SCREENSHOT });
});
