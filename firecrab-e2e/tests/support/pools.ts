import { expect, type Locator, type Page } from "@playwright/test";

export async function openPoolCreate(page: Page): Promise<Locator> {
  await expect(page.locator(".pools-screen")).toBeVisible();
  // An empty list replaces New pool with the form after its first response.
  await expect(page.getByText("Loading pools…", { exact: true })).toHaveCount(0);
  const form = page.getByRole("region", { name: "Create a pool", exact: true });
  if (!(await form.isVisible()))
    await page.getByRole("button", { name: "New pool", exact: true }).click();
  await expect(form).toBeVisible();
  return form;
}
