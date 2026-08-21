import { expect, test, type Page } from "@playwright/test";

import { ApiCleanup } from "../src/api.js";
import {
  IPV6_E2E_V4_CIDR,
  IPV6_E2E_V4_NAME,
  IPV6_E2E_V6_CIDR,
  IPV6_E2E_V6_NAME,
  SKIP_GUEST_BOOT,
} from "../src/constants.js";

/**
 * Issue #146 browser E2E — MicroNetwork IPv6 as a create-time choice.
 *
 *   FIRECRAB_E2E_SKIP_GUEST_BOOT=1 npm run test:ipv6 --prefix firecrab-e2e
 *   npm run test:ipv6 --prefix firecrab-e2e
 *
 * The form test does not need the net helper. Creating a network does
 * (same as the OCI guest-boot half). Skip that test with the flag.
 */
test.describe.configure({ mode: "serial" });

const api = new ApiCleanup();
const OWNED = [IPV6_E2E_V4_NAME, IPV6_E2E_V6_NAME];

async function openEnglish(page: Page, hash: string): Promise<void> {
  await page.addInitScript(() => {
    localStorage.setItem("firecrab.locale", "en");
  });
  await page.goto(hash);
}

function networkPanel(page: Page) {
  return page.locator("section.panel", {
    has: page.getByRole("heading", { name: "MicroNetwork" }),
  });
}

test.beforeAll(async () => {
  await api.deleteNetworksByName(OWNED);
});

test.afterAll(async () => {
  await api.deleteNetworksByName(OWNED);
});

test("IPv6 create fields stay off until the select is enabled", async ({ page }) => {
  await openEnglish(page, "/#/networks");
  const panel = networkPanel(page);
  await expect(panel.locator("#mn-ipv6-enable")).toHaveValue("off");
  await expect(panel.locator("#mn-ipv6")).toBeDisabled();
  await expect(panel.locator("#mn-ipv6-mode")).toBeDisabled();

  await panel.locator("#mn-ipv6-enable").selectOption("on");
  await expect(panel.locator("#mn-ipv6")).toBeEnabled();
  await expect(panel.locator("#mn-ipv6-mode")).toBeEnabled();
  await expect(panel.locator("#mn-ipv6-mode")).toHaveValue("slaac");
});

test("creates an IPv4-only network and an auto-ULA dual-stack network", async ({ page }) => {
  test.skip(
    SKIP_GUEST_BOOT,
    "FIRECRAB_E2E_SKIP_GUEST_BOOT is set — form coverage already ran. Unset the flag (and run ./scripts/dev-net-helper.sh) to create networks.",
  );

  await openEnglish(page, "/#/networks");
  const panel = networkPanel(page);
  const rows = panel.locator("table.vm-table tbody tr");
  const fieldError = panel.locator(".field-error").filter({ hasText: /\S/ });

  await page.locator("#mn-name").fill(IPV6_E2E_V4_NAME);
  await page.locator("#mn-subnet").fill(IPV6_E2E_V4_CIDR);
  await expect(panel.locator("#mn-ipv6-enable")).toHaveValue("off");
  await panel.locator('button[type="submit"]').click();
  const v4row = rows.filter({ hasText: IPV6_E2E_V4_NAME });
  await expect(v4row.or(fieldError).first()).toBeVisible({ timeout: 30_000 });
  if ((await v4row.count()) === 0) {
    throw new Error(
      `failed to create ${IPV6_E2E_V4_NAME}: ${(await fieldError.allTextContents()).join("; ")}`,
    );
  }
  await expect(v4row).toContainText("Off");

  await page.locator("#mn-name").fill(IPV6_E2E_V6_NAME);
  await page.locator("#mn-subnet").fill(IPV6_E2E_V6_CIDR);
  await panel.locator("#mn-ipv6-enable").selectOption("on");
  await panel.locator('button[type="submit"]').click();
  const v6row = rows.filter({ hasText: IPV6_E2E_V6_NAME });
  await expect(v6row.or(fieldError).first()).toBeVisible({ timeout: 30_000 });
  if ((await v6row.count()) === 0) {
    throw new Error(
      `failed to create ${IPV6_E2E_V6_NAME}: ${(await fieldError.allTextContents()).join("; ")}`,
    );
  }
  await expect(v6row).toContainText("NAT66");

  const networks = await api.listNetworks();
  const v4 = networks.find((row) => row.name === IPV6_E2E_V4_NAME);
  const v6 = networks.find((row) => row.name === IPV6_E2E_V6_NAME);
  expect(v4, `API missing ${IPV6_E2E_V4_NAME}`).toBeTruthy();
  expect(v4?.ipv6Cidr ?? null).toBeNull();
  expect(v4?.ipv6AddressMode ?? null).toBeNull();
  expect(v6, `API missing ${IPV6_E2E_V6_NAME}`).toBeTruthy();
  expect(v6?.ipv6Cidr ?? "").toMatch(/^fd[0-9a-f:]+\/64$/i);
  expect(v6?.ipv6AddressMode).toBe("slaac");
  expect(v6?.ipv6Egress).toBe("nat66");
});
