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
  await expect(page.getByRole("region", { name: "Set up a Stack", exact: true })).toHaveCount(0);
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
  await expect(page.getByRole("region", { name: "Set up a Stack", exact: true })).toHaveCount(0);
});

for (const locale of ["en", "ko"] as const) {
  test(`shows four setup steps for an empty Stack list in ${locale}`, async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 1100 });
    await openDashboard(page, {
      hash: "#/stacks",
      locale,
      routes: { "/api/stacks": [] },
    });
    const guide = page.getByRole("region", {
      name: locale === "ko" ? "Stack 구성 방법" : "Set up a Stack",
      exact: true,
    });
    await expect(guide).toBeVisible();
    await expect(guide.locator("ol > li")).toHaveCount(4);
    await expect(guide.locator("ol > li").first()).toContainText("stack.yaml");
    await expect(guide.locator("ol > li").nth(1)).toContainText("firecrab stack validate --file stack.yaml");
    await expect(guide.locator("ol > li").nth(1)).toContainText("firecrab stack register --name demo");
    await expect(guide.locator("ol > li").nth(2)).toContainText("firecrab stack plan --file stack.yaml --json > plan.json");
    await expect(guide.locator("ol > li").nth(2)).toContainText("firecrab stack apply --file stack.yaml --expected-plan plan.json");
    await expect(guide.locator("ol > li").last()).toContainText(locale === "ko" ? "새로고침" : "Refresh");
    await expect(guide.getByRole("link")).toHaveAttribute("href", /guides\/stacks\.md#schema$/);
    await page.screenshot({ path: `test-results/stack-setup-${locale}.png`, fullPage: true });
    await page.setViewportSize({ width: 390, height: 844 });
    await expect(guide).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    await page.screenshot({ path: `test-results/stack-setup-${locale}-mobile.png`, fullPage: true });
  });
}
