import { expect, test, type Page } from "@playwright/test";

import { createNetwork } from "./support/wasmDashboard";
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

test.beforeAll(async () => {
  await api.deleteNetworksByName(OWNED);
});

test.afterAll(async () => {
  await api.deleteNetworksByName(OWNED);
});

test("IPv6 defaults to IPv4-only and supports SLAAC or DHCPv6", async ({
  page,
}) => {
  await openEnglish(page, "/#/networks");
  await page.locator('.screen-header [data-action="create"][data-id="networks"]').click();
  const mode = page.getByRole("radiogroup", { name: "IPv6 address mode", exact: true });
  await expect(mode.getByRole("radio", { name: "Default", exact: true })).toBeChecked();
  await expect(page.locator("#field-ipv6Cidr")).toHaveValue("");
  await mode.getByRole("radio", { name: "SLAAC", exact: true }).check();
  await expect(mode.getByRole("radio", { name: "SLAAC", exact: true })).toBeChecked();
  await mode.getByRole("radio", { name: "DHCPv6", exact: true }).check();
  await expect(mode.getByRole("radio", { name: "DHCPv6", exact: true })).toBeChecked();
});

test("creates an IPv4-only network and an auto-ULA dual-stack network", async ({
  page,
}) => {
  test.skip(
    SKIP_GUEST_BOOT,
    "FIRECRAB_E2E_SKIP_GUEST_BOOT is set — form coverage already ran. Unset the flag (and run ./scripts/dev-net-helper.sh) to create networks.",
  );

  await openEnglish(page, "/#/networks");
  await createNetwork(page, IPV6_E2E_V4_NAME, IPV6_E2E_V4_CIDR);
  await createNetwork(page, IPV6_E2E_V6_NAME, IPV6_E2E_V6_CIDR, true);

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
