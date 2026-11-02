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
  await expect(page.getByText("CVE scan pending", { exact: true })).toBeVisible();
  await expect(page.getByText("No recent structured access logs", { exact: true })).toBeVisible();
  await expect(page.getByText("deploy-1", { exact: true })).not.toBeVisible();
  await page.getByText("Evidence and history", { exact: true }).click();
  await expect(page.getByText("deploy-1", { exact: true })).toBeVisible();
  await page.getByText("Coverage and limitations", { exact: true }).click();
  await expect(page.getByText(/CVE scans check supported image packages/)).toBeVisible();
  if (process.env.SECURITY_SCREENSHOT) await page.screenshot({ path: process.env.SECURITY_SCREENSHOT });
});


test("Security shows a limited log signal with evidence and guidance", async ({ page }) => {
  await openDashboard(page, { hash: "#/security", routes: { "/api/service-security": {
    services: [{ id: "a", name: "orders-api", deploymentId: "deploy-1", digest: null, replicas: 1, sampledRecords: 12,
      signals: [{ kind: "repeated_access_denials", severity: "medium", count: 12, source: "structured application access logs", recommendation: "Review access logs and authentication policy." }] }],
    cveScan: "unavailable", threatDetection: "limited", aiAnalysis: "unavailable"
  } } });
  await expect(page.getByText("Image digest unavailable", { exact: true })).toBeVisible();
  await expect(page.getByText("Activity detected", { exact: true })).toBeVisible();
  await expect(page.getByText("401/403 records: 12", { exact: true })).toBeVisible();
  await expect(page.getByText("Medium", { exact: true })).toBeVisible();
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
  await expect(page.getByRole("complementary", { name: "AI guidance" }).getByText("Review the authentication policy.")).toBeVisible();
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
  await expect(page.locator(".security-findings > li").filter({ has: cve })).toContainText("Update to 3.0.16-r0");
  await page.getByText("Evidence and history", { exact: true }).click();
  await expect(page.getByText(/Release: deploy-1/)).toBeVisible();
  await expect(page.getByText(/Replicas: replica-old/)).toBeVisible();
  await page.getByText("Coverage and limitations", { exact: true }).click();
  await expect(page.getByText(/Ingress logs are unavailable/)).toBeVisible();
  await expect(page.getByText(/The image OS is no longer supported/)).toBeVisible();
  await expect(page.getByText(/Previous release: deploy-1/)).toBeVisible();
  await expect(page.getByRole("link", { name:"CVE-2024-12345" })).toBeVisible();
  await expect(page.getByText(/Scanned replicas: replica-old/)).toBeVisible();
  if (process.env.SECURITY_CVE_SCREENSHOT) await page.screenshot({ path: process.env.SECURITY_CVE_SCREENSHOT, fullPage: true });
});

test("Security reports a missing scanner and stale CVE data", async ({ page }) => {
  await openDashboard(page, { hash: "#/security", routes: { "/api/service-security": {
    services: [
      { id:"a", name:"api", deploymentId:"one", digest:"sha256:1", sampledRecords:0, signals:[], logGap:true, scan:{ status:"scanner_missing", findings:[] } },
      { id:"b", name:"worker", deploymentId:"two", digest:"sha256:2", sampledRecords:0, signals:[], scan:{ status:"stale", findings:[] } }
    ]
  } } });
  await expect(page.getByText(/CVE scanner is not installed/)).toBeVisible();
  await expect(page.getByText(/CVE data is stale/)).toBeVisible();
  await expect(page.getByText(/Some application logs may be missing/)).toBeVisible();
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


test("Security identifies workloads outside its coverage", async ({ page }) => {
  await openDashboard(page, { hash: "#/security", routes: { "/api/service-security": {
    services: [], unsupportedWorkloads: [{ id:"cluster-id", name:"cluster-service", reason:"cluster_managed" }, { id:"db-id", name:"orders-db", reason:"database_connection_unlinked" }]
  } } });
  await page.getByText("Coverage and limitations", { exact: true }).click();
  await expect(page.getByText(/Standalone MicroVMs are not checked/)).toBeVisible();
  await expect(page.getByText(/Cluster-managed services are not checked/)).toBeVisible();
  await expect(page.getByText(/Database connections need linked images and logs/)).toBeVisible();
  await expect(page.getByRole("heading", { name:"Items not checked" })).toBeVisible();
  await expect(page.getByText("cluster-service · Cluster-managed service")).toBeVisible();
  await expect(page.getByText("orders-db · Database connection: image and logs not linked")).toBeVisible();
});


test("Security explains limited ingress coverage and detected loss", async ({ page }) => {
  if (process.env.SECURITY_INGRESS_SCREENSHOT) await page.setViewportSize({ width: 1440, height: 1000 });
  await openDashboard(page, { hash: "#/security", routes: { "/api/service-security": {
    services: [{ id:"a", name:"api", deploymentId:"one", digest:null, sampledRecords:0, signals:[], activityHistory:[{ deploymentId:"one", replicaIds:[], bucketStart:1793310000, count:12, severity:"medium", source:"ingress HTTP status codes", recommendation:"Review ingress and authentication configuration." }] }], unsupportedWorkloads:[{ id:"db-id", name:"orders-db", reason:"database_connection_unlinked" }], ingressLogs:"limited"
  } } });
  await page.getByText("Coverage and limitations", { exact: true }).click();
  await expect(page.getByText(/Uses structured logs and ingress 401\/403 counts/)).toBeVisible();
  await expect(page.getByText(/ingress HTTP status codes/)).toBeVisible();
  await page.getByText("Evidence and history", { exact: true }).click();
  await expect(page.getByText(/Replicas: Unavailable/)).toBeVisible();
  if (process.env.SECURITY_INGRESS_SCREENSHOT) await page.screenshot({ path: process.env.SECURITY_INGRESS_SCREENSHOT, fullPage: true });
});

test("Security reports known ingress event loss", async ({ page }) => {
  await openDashboard(page, { hash: "#/security", routes: { "/api/service-security": {
    services: [], ingressLogs:"degraded"
  } } });
  await page.getByText("Coverage and limitations", { exact: true }).click();
  await expect(page.getByText(/Some ingress security events were lost/)).toBeVisible();
});


test("Security links database connections to an operator-selected service", async ({ page }) => {
  let linked = false;
  const writes: unknown[] = [];
  await openDashboard(page, { hash:"#/security", routes: {
    "/api/service-security": async route => route.fulfill({ json: {
      services:[{ id:"a", name:"postgres", deploymentId:"one", digest:null, sampledRecords:0, signals:[], activityHistory:[{ deploymentId:"one", replicaIds:["replica"], bucketStart:1793310000, count:12, severity:"medium", source:"PostgreSQL JSON authentication logs", recommendation:"Review authentication policy." }] }],
      databases:[{ id:"db-id", name:"orders-db", serviceId:linked ? "a" : null, serviceName:linked ? "postgres" : null }],
      unsupportedWorkloads:linked ? [] : [{ id:"db-id", name:"orders-db", reason:"database_connection_unlinked" }]
    }}),
    "/api/service-security/databases/db-id/link": async route => {
      writes.push({ method:route.request().method(), body:route.request().postData() ? route.request().postDataJSON() : null });
      linked = route.request().method() === "PUT";
      await route.fulfill({ status:204, body:"" });
    }
  }});
  await page.getByText("Database connections (1)", { exact: true }).click();
  await expect(page.getByText(/Firecrab does not verify this link/)).toBeVisible();
  await expect(page.getByText(/Authentication failures: 12/)).toBeVisible();
  await page.getByLabel("orders-db").selectOption("a");
  await page.getByRole("button", { name:"Link", exact:true }).click();
  await expect(page.getByRole("button", { name:"Unlink" })).toBeVisible();
  expect(writes[0]).toEqual({ method:"PUT", body:{ serviceId:"a" } });
  if (process.env.SECURITY_DATABASE_SCREENSHOT) await page.screenshot({ path:process.env.SECURITY_DATABASE_SCREENSHOT, fullPage:true });
  await page.getByRole("button", { name:"Unlink" }).click();
  await expect(page.getByText("Not linked", { exact:true })).toBeVisible();
  expect(writes[1]).toEqual({ method:"DELETE", body:null });
});


test("Security keeps findings compact and preserves disclosure and model choices", async ({ page }) => {
  await page.setViewportSize({ width: 1440, height: 1000 });
  let request: unknown;
  const digest = `sha256:${"a".repeat(64)}`;
  await openDashboard(page, { hash: "#/security", routes: {
    "/api/service-security": {
      services: [
        { id: "api", name: "inference-api", deploymentId: "release-orders-current", digest, sampledRecords: 24,
          scan: { status: "complete", findings: [{ cve: "CVE-2025-15467", package: "libcrypto3", installedVersion: "3.0.15-r0", fixedVersion: "3.0.16-r0", severity: "critical", advisorySource: "Alpine Secdb", target: "alpine 3.17" }] } },
        { id: "db", name: "api-gateway", deploymentId: "release-db-current", digest: null, sampledRecords: 12,
          activityHistory: [{ deploymentId: "release-db-current", replicaIds: ["replica-db"], count: 12, severity: "medium", source: "ingress HTTP status codes", recommendation: "Review the authentication policy for repeated access denials." }] },
        { id: "worker", name: "image-worker", deploymentId: "release-worker-current", digest, sampledRecords: 0, scan: { status: "scanner_missing", findings: [] } },
      ], unsupportedWorkloads: [{ id: "external-db", name: "analytics-db", reason: "database_connection_unlinked" }], ingressLogs: "limited",
    },
    "/api/pipe/ai/connections": [{ id: "connection", name: "AI account" }],
    "/api/ai-sandbox/connections/connection/models": { models: [{ id: "model-one", name: "Model one", verified: true }, { id: "model-two", name: "Model two", verified: true }] },
    "/api/service-security/api/analysis": async route => {
      request = route.request().postDataJSON();
      await route.fulfill({ json: { serviceId: "api", deploymentId: "release-orders-current", text: "Update libcrypto3 to the fixed version.\nRedeploy the service.\nRescan the new image." } });
    },
  }});
  await expect(page.locator(".security-overview")).toContainText("Services to review2");
  await expect(page.locator(".security-overview")).toContainText("Images scanned1 / 3");
  await expect(page.locator(".security-overview")).toContainText("Outside coverage1");
  await expect(page.getByRole("article", { name: "inference-api" }).getByText(digest, { exact: true })).not.toBeVisible();
  await page.getByText("AI guidance settings", { exact: true }).click();
  await page.getByLabel("AI model", { exact: true }).selectOption({ label: "AI account · Model two" });
  const orders = page.getByRole("article", { name: "inference-api" });
  await page.getByText("AI guidance settings", { exact: true }).click();
  await orders.getByText("Evidence and history", { exact: true }).click();
  await orders.getByRole("button", { name: "Explain with AI" }).click();
  await expect(orders.getByRole("complementary", { name: "AI guidance" })).toContainText("Update libcrypto3");
  const guidanceLines = await orders.getByRole("complementary", { name: "AI guidance" }).locator(".security-copy-line").allTextContents();
  expect(guidanceLines).toEqual(["Update libcrypto3 to the fixed version.", "Redeploy the service.", "Rescan the new image."]);
  expect(guidanceLines.every(line => line.trim().split(/\s+/).length <= 8)).toBe(true);
  expect(request).toEqual({ connectionId: "connection", model: "model-two" });
  await expect(orders.locator(".security-evidence")).toHaveAttribute("open", "");
  await page.getByText("AI guidance settings", { exact: true }).click();
  await expect(page.getByLabel("AI model", { exact: true })).toHaveValue(JSON.stringify({ connectionId: "connection", model: "model-two" }));
  await page.getByText("AI guidance settings", { exact: true }).click();
  await orders.getByText("Evidence and history", { exact: true }).click();
  if (process.env.SECURITY_OVERVIEW_SCREENSHOT) await page.screenshot({ path: process.env.SECURITY_OVERVIEW_SCREENSHOT, fullPage: true });
  if (process.env.SECURITY_KOREAN_SCREENSHOT) {
    await page.getByLabel("Language", { exact: true }).selectOption("ko");
    await expect(page.getByRole("heading", { name: "서버리스 보안" })).toBeVisible();
    await page.screenshot({ path: process.env.SECURITY_KOREAN_SCREENSHOT, fullPage: true });
  }
  await page.setViewportSize({ width: 390, height: 844 });
  expect(await page.locator(".security-page").evaluate(el => el.scrollWidth <= el.clientWidth)).toBe(true);
  await expect(orders.locator("[data-action=security-scan]")).toBeVisible();
  if (process.env.SECURITY_MOBILE_SCREENSHOT) await page.screenshot({ path: process.env.SECURITY_MOBILE_SCREENSHOT, fullPage: true });
});


test("Security prioritizes severe CVEs without hiding remaining findings", async ({ page }) => {
  await openDashboard(page, { hash: "#/security", routes: { "/api/service-security": {
    services: [{ id: "a", name: "api", deploymentId: "one", digest: "sha256:abc", scan: { status: "complete", findings: [
      { cve: "CVE-2024-10001", severity: "low", package: "low-package" },
      { cve: "CVE-2024-10002", severity: "critical", package: "critical-package" },
      { cve: "CVE-2024-10003", severity: "high", package: "high-package" },
      { cve: "CVE-2024-10004", severity: "medium", package: "medium-package" },
    ] }}],
  }}});
  const links = page.locator(".security-findings a:visible");
  await expect(links).toHaveText(["CVE-2024-10002", "CVE-2024-10003", "CVE-2024-10004"]);
  await expect(page.getByRole("link", { name: "CVE-2024-10001" })).not.toBeVisible();
  await page.getByText("More CVE findings (1)", { exact: true }).focus();
  await page.keyboard.press("Enter");
  await expect(page.getByRole("link", { name: "CVE-2024-10001" })).toBeVisible();
});
