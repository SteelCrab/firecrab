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
  await expect(page.getByRole("cell").filter({ hasText: "CVE scan pending · No recent structured access logs" })).toBeVisible();
  await expect(page.getByText(/CVE scans check supported packages/)).toBeVisible();
  if (process.env.SECURITY_SCREENSHOT) await page.screenshot({ path: process.env.SECURITY_SCREENSHOT });
});


test("Security shows a limited log signal with evidence and guidance", async ({ page }) => {
  await openDashboard(page, { hash: "#/security", routes: { "/api/service-security": {
    services: [{ id: "a", name: "orders-api", deploymentId: "deploy-1", digest: null, replicas: 1, sampledRecords: 12,
      signals: [{ kind: "repeated_access_denials", severity: "medium", count: 12, source: "structured application access logs", recommendation: "Review access logs and authentication policy." }] }],
    cveScan: "unavailable", threatDetection: "limited", aiAnalysis: "unavailable"
  } } });
  await expect(page.getByText("Image digest unavailable · Activity signals: 1")).toBeVisible();
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

test("Security shows CVE evidence, fixes, and retained release activity", async ({ page }) => {
  if (process.env.SECURITY_CVE_SCREENSHOT) await page.setViewportSize({ width: 1440, height: 1200 });
  await openDashboard(page, { hash: "#/security", routes: { "/api/service-security": {
    services: [{ id: "a", name: "orders-api", deploymentId: "deploy-2", digest: "sha256:abcd", replicas: 1, sampledRecords: 0, signals: [],
      scan: { deploymentId: "deploy-2", digest: "sha256:abcd", scannedAt: 1793310000, status: "complete", databaseUpdatedAt: "2026-10-30T00:00:00Z", limitations: ["os_end_of_support"], findings: [
        { cve: "CVE-2025-15467", package: "libcrypto3", installedVersion: "3.0.15-r0", fixedVersion: "3.0.16-r0", severity: "critical", source: "https://nvd.nist.gov/vuln/detail/CVE-2025-15467", advisorySource: "Alpine Secdb", target: "alpine 3.17" }
      ] }, previousScans: [{ deploymentId:"deploy-1", digest:"sha256:old", scannedAt:1793300000, status:"complete", replicaIds:["replica-old"], findings:[{cve:"CVE-2024-12345", package:"zlib", installedVersion:"1", fixedVersion:"2", severity:"high", advisorySource:"Vendor", target:"alpine"}] }], activityHistory: [{ deploymentId:"deploy-1", replicaIds:["replica-old"], bucketStart:1793310000, count:12, severity:"medium", source:"structured application logs with status codes", recommendation:"Review access logs." }]
    }], cveScan:"scheduled", threatDetection:"limited", aiAnalysis:"on_demand", ingressLogs:"unavailable"
  } } });
  const cve = page.getByRole("link", { name: "CVE-2025-15467" });
  await expect(cve).toHaveAttribute("href", "https://nvd.nist.gov/vuln/detail/CVE-2025-15467");
  await expect(page.getByText(/libcrypto3 3.0.15-r0/)).toContainText("Update to 3.0.16-r0");
  await expect(page.getByText(/Release: deploy-1/)).toContainText("replica-old");
  await expect(page.getByText(/Ingress logs are unavailable/)).toBeVisible();
  await expect(page.getByText(/The image OS is no longer supported/)).toBeVisible();
  await page.getByText(/Previous release: deploy-1/).click();
  await expect(page.getByRole("link", { name:"CVE-2024-12345" })).toBeVisible();
  await expect(page.getByText(/Scanned replicas: replica-old/)).toBeVisible();
  if (process.env.SECURITY_CVE_SCREENSHOT) await page.screenshot({ path: process.env.SECURITY_CVE_SCREENSHOT, fullPage: true });
});

test("Security reports a missing scanner and stale CVE data", async ({ page }) => {
  await openDashboard(page, { hash: "#/security", routes: { "/api/service-security": {
    services: [
      { id:"a", name:"api", deploymentId:"one", digest:"sha256:1", sampledRecords:0, signals:[], scan:{ status:"scanner_missing", findings:[] } },
      { id:"b", name:"worker", deploymentId:"two", digest:"sha256:2", sampledRecords:0, signals:[], scan:{ status:"stale", findings:[] } }
    ]
  } } });
  await expect(page.getByText(/CVE scanner is not installed/)).toBeVisible();
  await expect(page.getByText(/CVE data is stale/)).toBeVisible();
});

test("Security queues a manual CVE rescan", async ({ page }) => {
  let requested = false;
  await openDashboard(page, { hash: "#/security", routes: {
    "/api/service-security": { services: [{ id:"a", name:"api", deploymentId:"one", digest:`sha256:${"a".repeat(64)}`, sampledRecords:0, signals:[], scan:{ status:"scanner_missing", findings:[] } }] },
    "/api/service-security/a/scan": async route => { requested = route.request().method() === "POST"; await route.fulfill({ status:202, body:"" }); }
  } });
  await page.getByRole("button", { name:"Scan now" }).click();
  await expect.poll(() => requested).toBe(true);
});
