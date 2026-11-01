import { expect, type Page } from "@playwright/test";

export async function importOci(page: Page, reference: string, alias: string) {
  await page
    .getByRole("button", { name: "Import OCI image", exact: true })
    .click();
  await page.locator("#oci-reference").fill(reference);
  await page
    .getByRole("button", { name: "Inspect", exact: true })
    .click();
  await expect(page.locator(".subpanel")).toContainText(alias, {
    timeout: 30_000,
  });
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
  await page.locator("#mn-name").fill(name);
  await page.locator("#mn-subnet").fill(cidr);
  await expect(page.locator("#mn-ipv6-enable")).toHaveValue("off");
  if (dualStack) await page.locator("#mn-ipv6-enable").selectOption("on");
  const pending = page.waitForResponse(
    (response) =>
      response.request().method() === "POST" &&
      new URL(response.url()).pathname === "/api/micro-networks",
  );
  await page.locator("form.create-grid button[type=submit]").click();
  const response = await pending;
  expect(response.ok(), await response.text()).toBe(true);
  await expect(
    page.locator("table.vm-table tbody tr").filter({ hasText: name }),
  ).toBeVisible({ timeout: 30_000 });
}
