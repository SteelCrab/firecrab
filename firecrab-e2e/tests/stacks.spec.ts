import { expect, test } from "@playwright/test";
import { openDashboard } from "./support/mockApi";

test("shows Git status, owned operation details and recovery guidance", async ({ page }) => {
  await openDashboard(page, {
    hash: "#/stacks",
    routes: {
      "/api/stacks": [{ id: "s1", name: "demo", bindings: [] }],
      "/api/stacks/demo/source": {
        generation: 3,
        source: { repository: "https://github.com/example/infra", ref: "heads/main", syncMode: "automatic", paused: false },
        latest: { state: "recovery_required", error: "apply_interrupted" },
        desiredRevision: { commit: "desired" }, attemptedRevision: { commit: "attempted" },
        successfulRevision: { commit: "applied" }, recoveryOperationId: "op-123",
        lastApply: { state: "interrupted", changes: [{ kind: "service", key: "web", resourceId: "svc-1", state: "failed" }] },
      },
    },
  });
  await expect(page.getByRole("heading", { name: "demo" })).toBeVisible();
  await expect(page.getByText("Recovery required", { exact: true })).toBeVisible();
  await expect(page.getByText("desired", { exact: true })).toBeVisible();
  await expect(page.getByText("attempted", { exact: true })).toBeVisible();
  await expect(page.getByText("applied", { exact: true })).toBeVisible();
  await expect(page.getByText("service · web · svc-1 · failed")).toBeVisible();
  await expect(page.getByText(/--generation 3 --recover-operation-id op-123/)).toBeVisible();
  await expect(page.getByText(/Guest disks and application data are not restored/)).toBeVisible();
  await expect(page.getByRole("button", { name: "Refresh", exact: true })).toBeVisible();
  await page.screenshot({ path: "test-results/stack-git-status.png", fullPage: true });
  await page.setViewportSize({ width: 375, height: 812 });
  await expect(page.getByRole("heading", { name: "demo" })).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
});

test("distinguishes an unavailable Git source from an unconfigured source", async ({ page }) => {
  await openDashboard(page, {
    hash: "#/stacks",
    routes: {
      "/api/stacks": [{ id: "s1", name: "unconfigured" }, { id: "s2", name: "unavailable" }],
      "/api/stacks/unavailable/source": route => route.fulfill({ status: 500, json: { error: { message: "unavailable" } } }),
    },
  });
  await expect(page.getByText("Git source is not configured.")).toBeVisible();
  await expect(page.getByRole("alert").filter({ hasText: "Source unavailable" })).toBeVisible();
});

test("does not present a failed stack list as an empty result", async ({ page }) => {
  await openDashboard(page, {
    hash: "#/stacks",
    routes: { "/api/stacks": route => route.fulfill({ status: 500, json: { error: { message: "unavailable" } } }) },
  });
  await expect(page.getByText(/HTTP 500: unavailable/)).toBeVisible();
  await expect(page.getByText("No registered stacks.")).toHaveCount(0);
});


test("shows Terraform ownership without a native stack and keeps mobile layout within the screen", async ({ page }) => {
  await openDashboard(page, {hash:"#/stacks",routes:{
    "/api/stacks":[],
    "/api/iac/owners":[{resource:{kind:"network",id:"11111111-1111-4111-8111-111111111111"},generation:3,
      owner:{manager:"terraform",id:"22222222-2222-4222-8222-222222222222",key:"firecrab_network.application"}}],
  }});
  await expect(page.getByRole("heading",{name:"Terraform ownership",exact:true})).toBeVisible();
  await expect(page.getByText("firecrab_network.application",{exact:true})).toBeVisible();
  await expect(page.getByText("Ownership generation",{exact:true})).toBeVisible();
  await page.setViewportSize({width:375,height:812});
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
});

test("reports ownership lookup errors without hiding native stacks", async ({ page }) => {
  await openDashboard(page,{hash:"#/stacks",routes:{
    "/api/stacks":[{id:"s1",name:"demo"}],
    "/api/iac/owners":route=>route.fulfill({status:500,json:{error:{message:"ownership unavailable"}}}),
  }});
  await expect(page.getByRole("heading",{name:"demo",exact:true})).toBeVisible();
  await expect(page.getByRole("alert").filter({hasText:"Ownership unavailable"})).toBeVisible();
});
