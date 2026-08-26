import { expect, test, type Page } from "@playwright/test";

import { ApiCleanup } from "../src/api.js";
import {
  DHCP_FIXED_ALIAS,
  DHCP_FIXED_REFERENCE,
  DHCP_HOST_PORT,
  DHCP_NETWORK_CIDR,
  DHCP_NETWORK_NAME,
  DHCP_REGISTRY_PORT,
  DHCP_VM_NAME,
  NETWORK_FAILED,
  NETWORK_READY,
  SKIP_GUEST_BOOT,
} from "../src/constants.js";
import { startLocalOciRegistry, type LocalOciRegistry } from "../src/registry.js";

/**
 * OCI guest DHCP + port-forward boot — the nginx-stable dashboard path
 * (busybox `udhcpc`, `FIRECRAB_NETWORK_READY`) without Docker Hub.
 *
 *   FIRECRAB_E2E_SKIP_GUEST_BOOT=1 npm run test:dhcp --prefix firecrab-e2e
 *   npm run test:dhcp --prefix firecrab-e2e
 *
 * Guest boot needs a helper the API can connect to
 * (`./scripts/dev-net-helper.sh`, socket `/run/firecrab/net-helper.sock`).
 * An orphan dnsmasq holding `:67` reproduces `FIRECRAB_NETWORK_FAILED
 * no-ipv4-address` — this spec fails loudly in that state.
 */
test.describe.configure({ mode: "serial" });

let registry: LocalOciRegistry;
let createdNetworkId: string | null = null;
const api = new ApiCleanup();

function reference(): string {
  return registry.announcement.reference;
}

function alias(): string {
  return registry.announcement.alias;
}

function sentinel(): string {
  return registry.announcement.ready;
}

async function openEnglish(page: Page, hash: string): Promise<void> {
  await page.addInitScript(() => {
    localStorage.setItem("firecrab.locale", "en");
  });
  await page.goto(hash);
}

async function cleanupOwned(): Promise<void> {
  const imported = registry?.announcement.alias ?? DHCP_FIXED_ALIAS;
  await api.deleteOwnedVms(imported, DHCP_VM_NAME);
  await api.deleteImportedImage(imported);
  await api.deleteOwnedNetwork(createdNetworkId, DHCP_NETWORK_NAME);
  createdNetworkId = null;
}

test.beforeAll(async () => {
  registry = await startLocalOciRegistry(DHCP_REGISTRY_PORT);
  expect(registry.announcement.reference).toBe(DHCP_FIXED_REFERENCE);
  expect(registry.announcement.alias).toBe(DHCP_FIXED_ALIAS);
  await cleanupOwned();
});

test.afterAll(async () => {
  try {
    await cleanupOwned();
  } finally {
    await registry?.stop();
  }
});

test("inspects the DHCP-boot fixture and imports it as a registered image", async ({ page }) => {
  await openEnglish(page, "/#/images");
  await expect(page.locator("#oci-reference")).toBeVisible();
  await expect(page.locator("#oci-import")).toBeDisabled();

  await page.locator("#oci-reference").fill(reference());
  await page.locator("#oci-inspect").click();

  const oci = page.locator("section.panel", { has: page.getByRole("heading", { name: "OCI" }) });
  await expect(oci.getByText("Compatible with this host.")).toBeVisible({ timeout: 30_000 });
  await expect(oci.locator("dd", { hasText: alias() }).first()).toBeVisible();
  await expect(page.locator("#oci-import")).toBeEnabled();

  await page.locator("#oci-import").click();
  const status = oci.locator(".state-badge");
  await expect(status).toHaveText(/Imported|Import failed/, { timeout: 180_000 });
  if ((await status.textContent())?.includes("failed")) {
    const log = (await oci.locator(".image-install-log").textContent()) ?? "";
    throw new Error(`OCI import failed for ${reference()}:\n${log}`);
  }

  await expect(page.locator("table.image-table")).toContainText(alias());
});

test("starts the imported guest with a port forward and gets a DHCP lease", async ({ page }) => {
  test.skip(
    SKIP_GUEST_BOOT,
    "FIRECRAB_E2E_SKIP_GUEST_BOOT is set — import already covered. Unset the flag (and run ./scripts/dev-net-helper.sh) to boot through DHCP.",
  );

  await openEnglish(page, "/#/networks");
  const networkPanel = page.locator("section.panel", {
    has: page.getByRole("heading", { name: "MicroNetwork" }),
  });
  const pendingNetwork = page.waitForResponse(
    (response) =>
      response.request().method() === "POST" && response.url().includes("/api/micro-networks"),
  );
  await page.locator("#mn-name").fill(DHCP_NETWORK_NAME);
  await page.locator("#mn-subnet").fill(DHCP_NETWORK_CIDR);
  await networkPanel.locator('button[type="submit"]').click();
  const networkResponse = await pendingNetwork;
  if (!networkResponse.ok()) {
    throw new Error(
      `POST /api/micro-networks ${networkResponse.status()}: ${await networkResponse.text()}`,
    );
  }
  const created = networkPanel
    .locator("table.vm-table tbody tr")
    .filter({ hasText: DHCP_NETWORK_NAME });
  await expect(created).toBeVisible({ timeout: 30_000 });
  const networks = await api.listNetworks();
  createdNetworkId = networks.find((network) => network.name === DHCP_NETWORK_NAME)?.id ?? null;
  expect(createdNetworkId, `API missing ${DHCP_NETWORK_NAME}`).toBeTruthy();

  await openEnglish(page, "/#/vms");
  await page.locator("#vm-list-add").click();
  await expect(page).toHaveURL(/#\/vms\/new$/);
  await expect(page.locator("#vm-image")).toBeEnabled({ timeout: 15_000 });
  await expect(page.locator(`#vm-image option[value="${alias()}"]`)).toHaveCount(1, {
    timeout: 15_000,
  });
  await page.locator("#vm-name").fill(DHCP_VM_NAME);
  await page.locator("#vm-image").selectOption(alias());
  await page
    .locator("#vm-micro-network")
    .selectOption({ label: `${DHCP_NETWORK_NAME} (${DHCP_NETWORK_CIDR})` });
  await page.getByRole("button", { name: /Add Port Forward Rule/ }).click();
  const hostPort = page.locator(".port-forwards-list input[placeholder='8080']");
  await expect(hostPort).toHaveValue("8080");
  await hostPort.fill(String(DHCP_HOST_PORT));

  await page.locator("#vm-create-submit").click();
  await expect(page).toHaveURL(/#\/vms$/);
  await expect(page.getByText(`Created: ${DHCP_VM_NAME}`)).toBeVisible({ timeout: 30_000 });

  const row = page.locator("table.vm-table tbody tr", { hasText: DHCP_VM_NAME });
  await expect(row).toBeVisible();
  await row.getByRole("button", { name: "start" }).click();
  await expect(row.locator(".state-badge")).toHaveText(/running|error/, { timeout: 240_000 });
  if ((await row.locator(".state-badge").textContent()) !== "running") {
    await row.locator("button.link-button").click();
    await expect(page.locator(".console-title")).toBeVisible();
    const logText = (await page.locator("pre.detail-log").textContent()) ?? "";
    const banner = (await page.locator(".banner").textContent().catch(() => "")) ?? "";
    throw new Error(
      `VM ${DHCP_VM_NAME} entered error instead of running.\nbanner: ${banner}\nlog:\n${logText}`,
    );
  }

  await row.locator("button.link-button").click();
  const log = page.locator("pre.detail-log");
  await expect(log).toContainText(NETWORK_READY, { timeout: 30_000 });
  await expect(log).not.toContainText(NETWORK_FAILED);
  await expect(log).toContainText(sentinel(), { timeout: 60_000 });
  await expect(page.locator("dt").filter({ hasText: /^ip$/ }).locator("+ dd")).toContainText(
    /^172\.30\.94\./,
  );
  await expect(page.locator("dt", { hasText: /^ports$/ })).toBeVisible();
  await expect(page.getByText(`80:${DHCP_HOST_PORT}/tcp`)).toBeVisible();

  const vms = await api.listVms();
  const vm = vms.find((row) => row.name === DHCP_VM_NAME);
  expect(vm, `API missing ${DHCP_VM_NAME}`).toBeTruthy();
  const detail = await api.getVm(vm!.id);
  expect(detail?.state).toBe("running");
  expect(detail?.ipv4 ?? "").toMatch(/^172\.30\.94\./);
  expect(detail?.portForwards).toEqual(
    expect.arrayContaining([
      expect.objectContaining({
        guestPort: 80,
        hostPort: DHCP_HOST_PORT,
        protocol: "tcp",
      }),
    ]),
  );
});
