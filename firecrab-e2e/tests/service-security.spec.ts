import { expect, test } from "@playwright/test";
import { openDashboard } from "./support/mockApi";

test("Security shows deployment identity and never reports unavailable scans as clean", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await openDashboard(page, {
    hash: "#/security",
    routes: { "/api/service-security": {
      services: [{ id: "a", name: "orders-api", deploymentId: "deploy-1", digest: "sha256:123456", replicas: 2, sampledRecords: 0 }],
      cveScan: "unavailable", threatDetection: "limited", aiAnalysis: "unavailable"
    } }
  });
  const security = page.getByRole("navigation", { name: "Main navigation" }).getByRole("link", { name: "Security", exact: true });
  await expect(security).toHaveAttribute("aria-current", "page");
  await expect(security.locator("xpath=ancestor::div[contains(concat(' ', normalize-space(@class), ' '), ' sidebar-group ')][1]").locator(".sidebar-group-label")).toHaveText("AI");
  await expect(page.getByText("orders-api", { exact: true })).toBeVisible();
  await expect(page.getByText("deploy-1", { exact: true })).toBeVisible();
  await expect(page.getByText("CVE not scanned · no recent structured access logs", { exact: true })).toBeVisible();
  await expect(page.getByText(/CVE scanning is not available yet/)).toBeVisible();
  if (process.env.SECURITY_SCREENSHOT) await page.screenshot({ path: process.env.SECURITY_SCREENSHOT });
});


test("Security shows a limited log signal with evidence and guidance", async ({ page }) => {
  await openDashboard(page, { hash: "#/security", routes: { "/api/service-security": {
    services: [{ id: "a", name: "orders-api", deploymentId: "deploy-1", digest: null, replicas: 1, sampledRecords: 12,
      signals: [{ kind: "repeated_access_denials", severity: "medium", count: 12, source: "structured application access logs", recommendation: "Review access logs and authentication policy." }] }],
    cveScan: "unavailable", threatDetection: "limited", aiAnalysis: "unavailable"
  } } });
  await expect(page.getByText("CVE not scanned · Activity signals: 1")).toBeVisible();
  await expect(page.getByText("401/403 records: 12")).toContainText("Severity: medium");
  await expect(page.getByText(/Review access logs and authentication policy/)).toBeVisible();
});

test("Security sends an explicit AI request and shows guidance", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  let selection: unknown;
  await openDashboard(page, { hash: "#/security", routes: {
    "/api/service-security": {
      services: [{ id: "a", name: "orders-api", deploymentId: "deploy-1", digest: null, replicas: 1, sampledRecords: 12,
        signals: [{ kind: "repeated_access_denials", severity: "medium", count: 12, source: "structured application access logs", recommendation: "Review access logs." }] }],
      cveScan: "unavailable", threatDetection: "limited", aiAnalysis: "on_demand"
    },
    "/api/pipe/ai/connections": [{ id: "connection", name: "AI account" }],
    "/api/ai-sandbox/connections/connection/models": { models: [{ id: "verified-model", name: "Verified model", verified: true }] },
    "/api/service-security/a/analysis": async route => {
      selection = route.request().postDataJSON();
      await route.fulfill({ json: { serviceId: "a", deploymentId: "deploy-1", text: "Review the authentication policy." } });
    }
  } });
  await page.getByRole("button", { name: "Explain with AI" }).click();
  await expect(page.getByText("AI guidance: Review the authentication policy.")).toBeVisible();
  expect(selection).toEqual({ connectionId: "connection", model: "verified-model" });
  if (process.env.SECURITY_AI_SCREENSHOT) await page.screenshot({ path: process.env.SECURITY_AI_SCREENSHOT });
});
