import { expect, type Page } from "@playwright/test";

export async function serviceAction(page: Page, name: string, action: "Edit" | "Delete") {
  await page.getByLabel(`Actions for ${name}`, { exact: true }).click();
  await page.getByRole("button", { name: `${action} ${name}`, exact: true }).click();
}

export async function serviceStep(page: Page, target: number) {
  let current = Number(
    await page.locator("#service-steps").getAttribute("data-step"),
  );
  if (current > target) {
    await page
      .locator(`[data-action="service-step"][data-id="${target}"]`)
      .click();
    current = target;
  }
  while (current < target) {
    await page.getByRole("button", { name: /^(Continue|계속)$/ }).click();
    current++;
    await expect(page.locator("#service-steps")).toHaveAttribute(
      "data-step",
      String(current),
    );
  }
}
export async function selectServiceApp(
  page: Page,
  reference = "ghcr.io/team/orders:1.0",
) {
  await serviceStep(page, 0);
  const [registry, ...path] = reference.split("/");
  const rest = path.join("/");
  const split = rest.lastIndexOf(":");
  await page.locator("#service-repository").selectOption(rest.slice(0, split));
  await page.locator("#service-tag").selectOption(rest.slice(split + 1));
}
export async function saveService(page: Page) {
  await serviceStep(page, 3);
  await page.locator("#editor button[type=submit]").click();
}
