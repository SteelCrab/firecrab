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
  await expect(panel.locator("#mn-ipv6")).toHaveCount(0);
  await expect(panel.locator("#mn-ipv6-mode")).toHaveCount(0);

  await panel.locator("#mn-ipv6-enable").selectOption("on");
  await expect(panel.locator("#mn-ipv6")).toBeVisible();
  await expect(panel.locator("#mn-ipv6-mode")).toBeVisible();
  await expect(panel.locator("#mn-ipv6-mode")).toHaveValue("slaac");
});

async function submitCreate(page: Page, panel: ReturnType<typeof networkPanel>): Promise<void> {
  const pending = page.waitForResponse(
    (response) =>
      response.request().method() === "POST" && response.url().includes("/api/micro-networks"),
  );
  await panel.locator('button[type="submit"]').click();
  const response = await pending;
  if (!response.ok()) {
    throw new Error(`POST /api/micro-networks ${response.status()}: ${await response.text()}`);
  }
}

test("creates an IPv4-only network and an auto-ULA dual-stack network", async ({ page }) => {
  test.skip(
    SKIP_GUEST_BOOT,
    "FIRECRAB_E2E_SKIP_GUEST_BOOT is set — form coverage already ran. Unset the flag (and run ./scripts/dev-net-helper.sh) to create networks.",
  );

  await openEnglish(page, "/#/networks");
  const panel = networkPanel(page);
  const rows = panel.locator("table.vm-table tbody tr");

  await page.locator("#mn-name").fill(IPV6_E2E_V4_NAME);
  await page.locator("#mn-subnet").fill(IPV6_E2E_V4_CIDR);
  await expect(panel.locator("#mn-ipv6-enable")).toHaveValue("off");
  await submitCreate(page, panel);
  const v4row = rows.filter({ hasText: IPV6_E2E_V4_NAME });
  await expect(v4row).toBeVisible();
  await expect(v4row).toContainText("Off");

  await page.locator("#mn-name").fill(IPV6_E2E_V6_NAME);
  await page.locator("#mn-subnet").fill(IPV6_E2E_V6_CIDR);
  await panel.locator("#mn-ipv6-enable").selectOption("on");
  await submitCreate(page, panel);
  const v6row = rows.filter({ hasText: IPV6_E2E_V6_NAME });
  await expect(v6row).toBeVisible();
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
