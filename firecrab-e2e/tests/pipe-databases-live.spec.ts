import { expect, test } from "@playwright/test";

// This file intentionally has no @dashboard tag. The fixture suite never runs it.
test("connects, reads and removes a real database through the dashboard", async ({ page }, testInfo) => {
  const raw = process.env.FIRECRAB_PIPE_LIVE_DATABASE;
  if (!raw) throw new Error("FIRECRAB_PIPE_LIVE_DATABASE is required; use the disposable QA runner");
  const database = JSON.parse(raw) as { uri: string; schema: string; table: string };
  const name = `Browser QA ${testInfo.project.name}`;
  // Update checks are unrelated to DB access and would contact GitHub.
  await page.route("**/api/update", route => route.fulfill({ json: { current: "0.3.1", latest: "0.3.1", updateAvailable: false } }));
  await page.addInitScript(() => localStorage.setItem("firecrab.locale", "en"));
  await page.goto("/#/pipe/database");
  const panel = page.getByRole("region", { name: "Databases", exact: true });
  await panel.getByRole("button", { name: "Add database", exact: true }).click();
  await panel.getByLabel("Connection name", { exact: true }).fill(name);
  await panel.getByLabel("Database URI", { exact: true }).fill(database.uri);
  await panel.getByLabel("TLS", { exact: true }).selectOption("disable");
  await panel.getByRole("button", { name: "Test connection", exact: true }).click();
  await expect(panel.getByRole("status")).toHaveText("Database connection succeeded.");
  const response = page.waitForResponse(value => new URL(value.url()).pathname === "/api/pipe/databases" && value.request().method() === "POST");
  await panel.getByRole("button", { name: "Save connection", exact: true }).click();
  const created = await response;
  expect(created.status()).toBe(201);
  const summary = await created.json() as { id: string };
  try {
    await page.reload();
    await panel.getByRole("button", { name: `Browse ${name}`, exact: true }).click();
    await panel.getByLabel("Table", { exact: true }).selectOption({ label: `${database.schema}.${database.table}` });
    await expect(panel.getByRole("cell", { name: "café", exact: true })).toBeVisible();
    await expect(panel.getByRole("cell", { name: "<script>text</script>", exact: true })).toBeVisible();
    await expect(panel.locator(".pipe-database-preview tbody tr")).toHaveCount(3);
    const longValue = panel.locator(".pipe-database-cell").filter({ hasText: /^x{2048}$/ });
    await expect(longValue).toHaveCount(1);
    expect(await longValue.evaluate(element => element.clientHeight)).toBeLessThanOrEqual(160);
    expect(await longValue.evaluate(element => element.scrollHeight > element.clientHeight)).toBe(true);
    await expect(panel.locator(".pipe-database-preview script")).toHaveCount(0);
    await expect(panel.getByRole("button", { name: "Next rows", exact: true })).toBeDisabled();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    await panel.screenshot({ path: testInfo.outputPath("real-database-preview.png") });
    await panel.getByRole("button", { name: `Remove connection ${name}`, exact: true }).click();
    await expect(panel.getByRole("cell", { name, exact: true })).toHaveCount(0);
    await expect(panel.getByRole("status")).toHaveText("Connection removed. The source database is unchanged.");
    const removed = await page.request.get(`/api/pipe/databases/${summary.id}/tables`);
    expect(removed.status()).toBe(404);
  } finally {
    // Also remove the test connection after an assertion failure.
    await page.request.delete(`/api/pipe/databases/${summary.id}`);
  }
});
