import type { Locator, Page } from "@playwright/test";

/** The MicroVM detail panel, in either locale. */
export const vmPanel = (page: Page): Locator => page.locator(".detail-panel");

/** Selects `row`'s VM and shows one tab of its detail panel. */
export async function openVmTab(page: Page, row: Locator, tab: "Overview" | "Network" | "Environment" | "Log"): Promise<void> {
  await row.locator(".vm-row-name").click();
  await vmPanel(page).getByRole("tab", { name: new RegExp(`^${tab}`) }).click();
}
