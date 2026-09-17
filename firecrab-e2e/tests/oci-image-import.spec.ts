import { expect, test, type Page } from "@playwright/test";

import { ApiCleanup } from "../src/api.js";
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
import { startLocalOciRegistry, type LocalOciRegistry } from "../src/registry.js";

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

test("inspects the local fixture and imports it as a registered image", async ({ page }) => {
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

  await expect(oci.getByText("Registered image")).toBeVisible();
  await expect(oci.getByText(new RegExp(`${alias()}.*Installed`))).toBeVisible();
  await expect(page.locator("table.image-table")).toContainText(alias());
  await expect(page.locator("table.image-table").getByText("Installed").first()).toBeVisible();
});

test("creates a VM from the imported image and asserts the guest service started", async ({
  page,
}) => {
  test.skip(
    SKIP_GUEST_BOOT,
    "FIRECRAB_E2E_SKIP_GUEST_BOOT is set — inspect+import already covered. Unset the flag (and provide KVM + firecracker + ./scripts/dev-net-helper.sh) for the guest-boot half.",
  );

  await openEnglish(page, "/#/networks");
  const networkPanel = page.locator("section.panel", {
    has: page.getByRole("heading", { name: "MicroNetwork" }),
  });
  const networkRows = networkPanel.locator("table.vm-table tbody tr");
  // Prefer an already-provisioned network. Creating one needs the helper and
  // a free CIDR; a developer host usually already has `default`.
  if ((await networkRows.count()) === 0) {
    await page.locator("#mn-name").fill(NETWORK_NAME);
    await page.locator("#mn-subnet").fill(NETWORK_CIDR);
    await networkPanel.locator('button[type="submit"]').click();
    const created = networkRows.filter({ hasText: NETWORK_NAME });
    const fieldError = networkPanel.locator(".field-error").filter({ hasText: /\S/ });
    await expect(created.or(fieldError).first()).toBeVisible({ timeout: 30_000 });
    if ((await created.count()) === 0) {
      throw new Error(
        `failed to create MicroNetwork ${NETWORK_NAME}: ${(await fieldError.allTextContents()).join("; ")}`,
      );
    }
    const networks = await api.listNetworks();
    createdNetworkId = networks.find((network) => network.name === NETWORK_NAME)?.id ?? null;
  }

  await openEnglish(page, "/#/vms");
  await page.locator("#vm-list-add").click();
  await expect(page).toHaveURL(/#\/vms\/new$/);
  await expect(page.locator("#vm-image")).toBeEnabled({ timeout: 15_000 });
  await expect(page.locator(`#vm-image option[value="${alias()}"]`)).toHaveCount(1, {
    timeout: 15_000,
  });
  await page.locator("#vm-name").fill(VM_NAME);
  await page.locator("#vm-image").selectOption(alias());
  await expect(page.locator("#vm-micro-network")).not.toHaveValue("");
  await page.locator("#vm-create-submit").click();
  await expect(page).toHaveURL(/#\/vms$/);
  await expect(page.getByText(`Created: ${VM_NAME}`)).toBeVisible({ timeout: 30_000 });

  const row = page.locator("table.vm-table tbody tr", { hasText: VM_NAME });
  await expect(row).toBeVisible();
  // Row actions live behind the Actions toggle now.
  await row.getByRole("button", { name: /^Actions$|^작업$/ }).click();
  await row.getByRole("button", { name: "start" }).click();
  await expect(row.locator(".state-badge")).toHaveText(/running|error/, { timeout: 240_000 });
  if ((await row.locator(".state-badge").textContent()) !== "running") {
    await row.locator("button.link-button").click();
    await expect(page.locator(".console-title")).toBeVisible();
    await expect
      .poll(async () => (await page.locator("pre.detail-log").textContent())?.trim() ?? "", {
        timeout: 10_000,
      })
      .not.toBe("");
    const logText = (await page.locator("pre.detail-log").textContent()) ?? "";
    const banner = (await page.locator(".banner").textContent().catch(() => "")) ?? "";
    const steps = (await page.locator(".pipeline-step").allTextContents()).join("\n");
    throw new Error(
      `VM ${VM_NAME} entered error instead of running.\nbanner: ${banner}\nsteps:\n${steps}\nlog:\n${logText}`,
    );
  }

  await row.locator("button.link-button").click();
  const log = page.locator("pre.detail-log");
  await expect(log).toContainText(NETWORK_READY, { timeout: 30_000 });
  await expect(log).toContainText(sentinel(), { timeout: 60_000 });
});
