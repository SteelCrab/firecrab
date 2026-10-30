import { expect, type Page } from "@playwright/test";

/** The panel of a selected MicroService. */
export const servicePanel = (page: Page) =>
  page.getByRole("complementary", { name: "MicroService detail" });

/**
 * The header's "New MicroService" link to the page that creates one. An empty list shows the same
 * link again in its empty state, so the header is named by place.
 */
export const newServiceButton = (page: Page) =>
  page.locator(".screen-header").getByRole("link", { name: "New MicroService", exact: true });

/** Opens the panel head's More menu (Stop service, Edit configuration, Delete) unless it is open. */
export async function openMoreMenu(page: Page) {
  const panel = servicePanel(page);
  if ((await panel.locator("#vm-more-actions[open]").count()) === 0) {
    await panel.getByLabel("More", { exact: true }).click();
  }
}

/** Opens a service from its row, then starts its Deploy form from the panel head. */
export async function openDeployForm(page: Page, name = "orders") {
  await page.getByRole("button", { name: `Open ${name}`, exact: true }).click();
  await servicePanel(page).getByRole("button", { name: "Deploy", exact: true }).click();
}

export async function serviceAction(page: Page, name: string, action: "Edit" | "Delete") {
  const panel = servicePanel(page);
  // Overlay panels cover the list actions on narrower screens.
  if (await panel.isVisible()) {
    await panel.getByRole("button", { name: "Close detail", exact: true }).click();
  }
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
    await page.getByRole("button", { name: /^(Next|다음)$/ }).click();
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
  await serviceStep(page, 4);
  await page.locator("#editor button[type=submit]").click();
}
