import { expect, test, type Page } from "@playwright/test";

import { createNetwork, importOci } from "./support/wasmDashboard";
import { ApiCleanup } from "../src/api.js";
import { openVmTab, vmPanel } from "./support/vmPanel";
import {
  FIXED_ALIAS,
  FIXED_REFERENCE,
  NETWORK_CIDR,
  NETWORK_NAME,
  NETWORK_READY,
  READY_SENTINEL,
  SKIP_GUEST_BOOT,
  VM_NAME,
} from "../src/constants.js";
import {
  startLocalOciRegistry,
  type LocalOciRegistry,
} from "../src/registry.js";

/**
 * Issue #90 browser E2E.
 *
 *   FIRECRAB_E2E_SKIP_GUEST_BOOT=1 npm test --prefix firecrab-e2e
 *   npm test --prefix firecrab-e2e
 *
 * The registry fixture is spawned here (python3 scripts/oci-e2e-registry.py,
 * FIRECRAB_OCI_E2E_PORT=15555). Cleanup always stops it and deletes any VM
 * or imported template this file created.
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
  const imported = registry?.announcement.alias ?? FIXED_ALIAS;
  await api.deleteOwnedVms(imported);
  await api.deleteImportedImage(imported);
  await api.deleteOwnedNetwork(createdNetworkId);
  createdNetworkId = null;
}

test.beforeAll(async () => {
  registry = await startLocalOciRegistry();
  expect(registry.announcement.reference).toBe(FIXED_REFERENCE);
  expect(registry.announcement.alias).toBe(FIXED_ALIAS);
  expect(registry.announcement.ready).toBe(READY_SENTINEL);
  await cleanupOwned();
});

test.afterAll(async () => {
  try {
    await cleanupOwned();
  } finally {
    await registry?.stop();
  }
});

test("inspects the local fixture and imports it as a registered image", async ({
  page,
}) => {
  await openEnglish(page, "/#/images");
  await importOci(page, reference(), alias());
});

test("creates a VM from the imported image and asserts the guest service started", async ({
  page,
}) => {
  test.skip(
    SKIP_GUEST_BOOT,
    "FIRECRAB_E2E_SKIP_GUEST_BOOT is set — inspect+import already covered. Unset the flag (and provide KVM + firecracker + ./scripts/dev-net-helper.sh) for the guest-boot half.",
  );

  await openEnglish(page, "/#/networks");
  await expect(page.locator("#content")).toHaveAttribute("data-loaded", "true");
  if ((await page.locator("table.network-table tbody tr").count()) === 0) {
    await createNetwork(page, NETWORK_NAME, NETWORK_CIDR);
    const networks = await api.listNetworks();
    createdNetworkId =
      networks.find((network) => network.name === NETWORK_NAME)?.id ?? null;
  }

  await openEnglish(page, "/#/vms");
  await page.locator("#vm-list-add").click();
  await expect(page).toHaveURL(/#\/vms\/new$/);
  await expect(page.locator("#field-template")).toBeEnabled({
    timeout: 15_000,
  });
  await expect(
    page.locator(`#field-template option[value="${alias()}"]`),
  ).toHaveCount(1, {
    timeout: 15_000,
  });
  await page.locator("#field-name").fill(VM_NAME);
  await page.locator("#field-template").selectOption(alias());
  await expect(page.locator("#field-microNetworkId")).not.toHaveValue("");
  await page.locator("#editor button[type=submit]").click();
  await expect(page).toHaveURL(/#\/vms(?:\?vm=.+)?$/);

  const row = page.locator("table.vm-list tbody tr", { hasText: VM_NAME });
  await expect(row).toBeVisible();
  // Row actions live behind the Actions toggle now.
  await row.getByRole("button", { name: /^Actions$|^작업$/ }).click();
  await page.getByRole("menuitem", { name: "start", exact: true }).click();
  await expect(row.locator(".vm-status-label")).toHaveAttribute(
    "data-state",
    /running|error/,
    { timeout: 240_000 },
  );
  if (
    (await row.locator(".vm-status-label").getAttribute("data-state")) !==
    "running"
  ) {
    await openVmTab(page, row, "Overview");
    const steps = (
      await vmPanel(page).locator(".vm-timeline-step").allTextContents()
    ).join("\n");
    await vmPanel(page).getByRole("tab", { name: /^Log/ }).click();
    await expect
      .poll(
        async () =>
          (await page.locator("pre.detail-log").textContent())?.trim() ?? "",
        {
          timeout: 10_000,
        },
      )
      .not.toBe("");
    const logText = (await page.locator("pre.detail-log").textContent()) ?? "";
    const banner =
      (await page
        .locator(".banner")
        .textContent()
        .catch(() => "")) ?? "";
    throw new Error(
      `VM ${VM_NAME} entered error instead of running.\nbanner: ${banner}\nsteps:\n${steps}\nlog:\n${logText}`,
    );
  }

  await openVmTab(page, row, "Log");
  const log = page.locator("pre.detail-log");
  await expect(log).toContainText(NETWORK_READY, { timeout: 30_000 });
  await expect(log).toContainText(sentinel(), { timeout: 60_000 });
});
