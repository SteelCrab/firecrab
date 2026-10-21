import { expect, test, type Page } from "@playwright/test";

import { ApiCleanup } from "../src/api.js";
import {
  NETWORK_READY,
  REGISTER_FIXED_ALIAS,
  REGISTER_FIXED_REFERENCE,
  REGISTER_NETWORK_CIDR,
  REGISTER_NETWORK_NAME,
  REGISTER_REGISTRY_PORT,
  REGISTER_VM_NAME,
  REGISTER_VERSION,
  SKIP_GUEST_BOOT,
} from "../src/constants.js";
import { startLocalOciRegistry, type LocalOciRegistry } from "../src/registry.js";

/**
 * Issue #108 browser E2E — register an already-installed image.
 *
 *   FIRECRAB_E2E_SKIP_GUEST_BOOT=1 npm run test:register --prefix firecrab-e2e
 *   npm run test:register --prefix firecrab-e2e
 *
 * The registry fixture is spawned here (python3 scripts/oci-e2e-registry.py,
 * FIRECRAB_OCI_REGISTER_E2E_PORT=15556). Cleanup always stops it and deletes
 * any VM, imported template, staged package, or catalog row this file created.
 *
 * Reinstall consumes the registered local package after deleting the import,
 * then boots the restored image. FIRECRAB_E2E_SKIP_GUEST_BOOT disables that
 * scenario. Failed register jobs require host-artifact manipulation and have
 * API unit coverage; that browser scenario is explicitly skipped.
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

function conflictCode(json: unknown): string | undefined {
  if (!json || typeof json !== "object") return undefined;
  const error = (json as { error?: { code?: unknown } }).error;
  return error && typeof error.code === "string" ? error.code : undefined;
}

async function cleanupOwned(): Promise<void> {
  const imported = registry?.announcement.alias ?? REGISTER_FIXED_ALIAS;
  await api.deleteOwnedVms(imported, REGISTER_VM_NAME);
  await api.deleteStagedPackage(imported);
  await api.deleteImportedImage(imported);
  await api.deleteStagedPackage(imported);
  await api.deleteLocalCatalogRow(imported);
  await api.deleteOwnedNetwork(createdNetworkId, REGISTER_NETWORK_NAME);
  createdNetworkId = null;
}

test.beforeAll(async () => {
  registry = await startLocalOciRegistry(REGISTER_REGISTRY_PORT);
  expect(registry.announcement.reference).toBe(REGISTER_FIXED_REFERENCE);
  expect(registry.announcement.alias).toBe(REGISTER_FIXED_ALIAS);
  await cleanupOwned();
  const leftover = (await api.listMicroregistry()).some((row) => row.alias === alias());
  if (leftover) {
    throw new Error(
      `local catalog still has ${alias()} after deleting this suite's registration`,
    );
  }
});

test.afterAll(async () => {
  try {
    await cleanupOwned();
  } finally {
    await registry?.stop();
  }
});

test("inspects the local fixture and imports it as an installed image", async ({ page }) => {
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
  await expect(page.locator("table.image-table")).toContainText(alias());
});

test("registers the installed image and refuses a second register with 409", async ({ page }) => {
  await openEnglish(page, "/#/images");
  const select = page.locator("#microregistry-register-alias");
  await expect(select).toBeEnabled({ timeout: 15_000 });
  await expect(page.locator(`#microregistry-register-alias option[value="${alias()}"]`)).toHaveCount(
    1,
    { timeout: 15_000 },
  );

  await select.selectOption(alias());
  await page.locator("#microregistry-register-version").fill(REGISTER_VERSION);
  const submit = page.locator("#microregistry-register-submit");
  await expect(submit).toBeEnabled();

  const posted = page.waitForResponse((response) => {
    const url = new URL(response.url());
    return response.request().method() === "POST" && url.pathname === "/api/microregistry/register";
  });
  await submit.click();
  const accepted = await posted;
  if (accepted.status() === 409) {
    throw new Error(
      `register 409 for ${alias()} on the first submit — leftover local catalog row`,
    );
  }
  expect(accepted.status()).toBe(202);

  // Check the server's terminal result as well as the rendered catalog row.
  await expect
    .poll(async () => (await api.getRegister(alias()))?.status ?? "", { timeout: 180_000 })
    .toMatch(/^(succeeded|failed)$/);
  const finished = await api.getRegister(alias());
  if (finished?.status === "failed") {
    throw new Error(`register failed for ${alias()}:\n${finished.log}`);
  }
  expect(finished?.status).toBe("succeeded");

  await page.locator("button.microregistry-refresh").click();
  const table = page.locator("table.microregistry-table");
  const rows = table.locator("tbody tr", { hasText: alias() });
  await expect(rows).toHaveCount(1);
  await expect(rows).toContainText(REGISTER_VERSION);

  const listed = (await api.listMicroregistry()).filter((row) => row.alias === alias());
  expect(listed).toEqual([{ alias: alias(), version: REGISTER_VERSION }]);

  // Check the server's collision guard even if the form prevents resubmission.
  const second = await api.startRegister(alias(), REGISTER_VERSION);
  expect(second.status).toBe(409);
  expect(conflictCode(second.json)).toBe("alias_collision");
  expect((await api.listMicroregistry()).filter((row) => row.alias === alias())).toHaveLength(1);
  await page.locator("button.microregistry-refresh").click();
  await expect(rows).toHaveCount(1);
});

test("a failed register job leaves no current catalog row", async () => {
  // Not gated by FIRECRAB_E2E_SKIP_GUEST_BOOT — that flag only skips guest boot.
  // Kernel rejection and failed-job cleanup have API unit coverage. The
  // dashboard cannot cause a post-accept failure without tampering with a
  // host artifact, so this browser scenario stays explicitly skipped.
  test.skip(
    true,
    "No dashboard or HTTP action triggers a post-accept register failure; kernel rejection and cleanup are covered by API unit tests.",
  );
});

test("deletes the template, reinstalls from the local row, and boots to FIRECRAB_NETWORK_READY", async ({
  page,
}) => {
  test.skip(
    SKIP_GUEST_BOOT,
    "FIRECRAB_E2E_SKIP_GUEST_BOOT is set — register, local row, and 409 already ran. Unset the flag (and provide KVM + firecracker + ./scripts/dev-net-helper.sh) for delete → reinstall → guest-boot.",
  );

  await openEnglish(page, "/#/images");
  const imageRow = page.locator("table.image-table tbody tr", { hasText: alias() });
  await expect(imageRow).toBeVisible();
  await imageRow.locator("button.options-menu-trigger").click();
  page.once("dialog", (dialog) => dialog.accept());
  await imageRow.getByRole("button", { name: "Delete" }).click();
  await expect(imageRow).toHaveCount(0, { timeout: 15_000 });
  await expect
    .poll(async () => (await api.listImages()).some((image) => image.alias === alias() && image.installed))
    .toBe(false);

  await page.locator("button.microregistry-refresh").click();
  const catalogRow = page.locator("table.microregistry-table tbody tr", { hasText: alias() });
  await expect(catalogRow).toHaveCount(1);
  await expect(catalogRow).toContainText(REGISTER_VERSION);

  const entry = await api.catalogEntry(alias());
  const action = catalogRow.getByRole("button", { name: /^(Download|Install)$/ });
  await expect(action).toBeVisible();
  if (!entry?.downloadable || (await action.isDisabled())) {
    throw new Error(
      `cannot reinstall ${alias()} from the local catalog row after template delete (downloadable=${String(entry?.downloadable)}, packageStaged=${String(entry?.packageStaged)}).`,
    );
  }

  const installRequest = page.waitForResponse((response) => {
    const url = new URL(response.url());
    return response.request().method() === "POST" &&
      url.pathname === `/api/images/${alias()}/install`;
  });
  await action.click();
  const installed = await installRequest;
  if (!installed.ok()) {
    throw new Error(`local catalog reinstall HTTP ${installed.status()}: ${await installed.text()}`);
  }
  await expect(catalogRow.locator(".state-badge")).toHaveText(/Installed|Download failed|Unsupported/, {
    timeout: 180_000,
  });
  const badge = (await catalogRow.locator(".state-badge").textContent()) ?? "";
  if (!/Installed/.test(badge)) {
    const banner = (await page.locator(".field-error").allTextContents()).join("; ");
    throw new Error(
      `reinstall from the local row did not install ${alias()}: status=${badge.trim()} ${banner}`,
    );
  }
  await expect
    .poll(async () => (await api.listImages()).some((image) => image.alias === alias() && image.installed), {
      timeout: 30_000,
    })
    .toBe(true);

  await openEnglish(page, "/#/networks");
  const networkPanel = page.locator("section.panel", {
    has: page.getByRole("heading", { name: "MicroNetwork" }),
  });
  const networkRows = networkPanel.locator("table.vm-table tbody tr");
  if ((await networkRows.count()) === 0) {
    await page.locator("#mn-name").fill(REGISTER_NETWORK_NAME);
    await page.locator("#mn-subnet").fill(REGISTER_NETWORK_CIDR);
    await networkPanel.locator('button[type="submit"]').click();
    const created = networkRows.filter({ hasText: REGISTER_NETWORK_NAME });
    const fieldError = networkPanel.locator(".field-error").filter({ hasText: /\S/ });
    await expect(created.or(fieldError).first()).toBeVisible({ timeout: 30_000 });
    if ((await created.count()) === 0) {
      throw new Error(
        `failed to create MicroNetwork ${REGISTER_NETWORK_NAME}: ${(await fieldError.allTextContents()).join("; ")}`,
      );
    }
    const networks = await api.listNetworks();
    createdNetworkId = networks.find((network) => network.name === REGISTER_NETWORK_NAME)?.id ?? null;
  }

  await openEnglish(page, "/#/vms");
  await page.locator("#vm-list-add").click();
  await expect(page).toHaveURL(/#\/vms\/new$/);
  await expect(page.locator("#vm-image")).toBeEnabled({ timeout: 15_000 });
  await expect(page.locator(`#vm-image option[value="${alias()}"]`)).toHaveCount(1, {
    timeout: 15_000,
  });
  await page.locator("#vm-name").fill(REGISTER_VM_NAME);
  await page.locator("#vm-image").selectOption(alias());
  await expect(page.locator("#vm-micro-network")).not.toHaveValue("");
  await page.locator("#vm-create-submit").click();
  await expect(page).toHaveURL(/#\/vms$/);
  await expect(page.getByText(`Created: ${REGISTER_VM_NAME}`)).toBeVisible({ timeout: 30_000 });

  const row = page.locator("table.vm-table tbody tr", { hasText: REGISTER_VM_NAME });
  await expect(row).toBeVisible();
  // Row actions live behind the Actions toggle now.
  await row.getByRole("button", { name: /^Actions$|^작업$/ }).click();
  await row.getByRole("menuitem", { name: "start", exact: true }).click();
  await expect(row.locator(".vm-status-label")).toHaveAttribute("data-state", /running|error/, { timeout: 240_000 });
  if ((await row.locator(".vm-status-label").getAttribute("data-state")) !== "running") {
    await row.locator("button.link-button").click();
    await expect(page.locator(".vm-detail-heading")).toBeVisible();
    await expect
      .poll(async () => (await page.locator("pre.detail-log").textContent())?.trim() ?? "", {
        timeout: 10_000,
      })
      .not.toBe("");
    const logText = (await page.locator("pre.detail-log").textContent()) ?? "";
    const banner = (await page.locator(".banner").textContent().catch(() => "")) ?? "";
    const steps = (await page.locator(".pipeline-step").allTextContents()).join("\n");
    throw new Error(
      `VM ${REGISTER_VM_NAME} entered error instead of running.\nbanner: ${banner}\nsteps:\n${steps}\nlog:\n${logText}`,
    );
  }

  await row.locator("button.link-button").click();
  const log = page.locator("pre.detail-log");
  await expect(log).toContainText(NETWORK_READY, { timeout: 30_000 });
  await expect(log).toContainText(sentinel(), { timeout: 60_000 });
});
