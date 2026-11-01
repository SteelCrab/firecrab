import { expect, type Page } from "@playwright/test";

export async function importOci(page: Page, reference: string, alias: string) {
  await page
    .getByRole("button", { name: "Import OCI image", exact: true })
    .click();
  await page.locator("#oci-reference").fill(reference);
  await page.getByRole("button", { name: "Inspect", exact: true }).click();
  await expect(page.locator(".subpanel > dl.detail-fields")).toContainText(
    alias,
    {
      timeout: 30_000,
    },
  );
  const accepted = page.waitForResponse(
    (response) =>
      response.request().method() === "POST" &&
      new URL(response.url()).pathname === "/api/oci/import",
  );
  await page.locator("#oci-import").click();
  const response = await accepted;
  expect(response.ok(), await response.text()).toBe(true);
  await expect
    .poll(
      async () => {
        const result = await page.request.get(
          `/api/oci/import/${encodeURIComponent(alias)}`,
        );
        expect(result.ok(), await result.text()).toBe(true);
        const job = await result.json();
        if (job.status === "failed")
          throw new Error(`OCI import failed for ${reference}:\n${job.log}`);
        return job.status;
      },
      { timeout: 180_000 },
    )
    .toBe("succeeded");
  await expect(page.locator("table.image-table")).toContainText(alias, {
    timeout: 30_000,
  });
}

export async function createNetwork(
  page: Page,
  name: string,
  cidr: string,
  dualStack = false,
) {
  await page.getByRole("button", { name: "New network", exact: true }).click();
  const form = page.locator("#editor");
  await form.getByLabel("Name", { exact: true }).fill(name);
  await form.getByLabel("IPv4 subnet (CIDR)", { exact: true }).fill(cidr);
  const mode = form.getByRole("radiogroup", {
    name: "IPv6 address mode",
    exact: true,
  });
  await expect(
    mode.getByRole("radio", { name: "Default", exact: true }),
  ).toBeChecked();
  if (dualStack)
    await mode.getByRole("radio", { name: "SLAAC", exact: true }).check();
  const pending = page.waitForResponse(
    (response) =>
      response.request().method() === "POST" &&
      new URL(response.url()).pathname === "/api/micro-networks",
  );
  await form.getByRole("button", { name: "Save", exact: true }).click();
  const response = await pending;
  expect(response.ok(), await response.text()).toBe(true);
  await expect(
    page
      .getByRole("row")
      .filter({ has: page.getByRole("cell", { name, exact: true }) }),
  ).toBeVisible({ timeout: 30_000 });
}
