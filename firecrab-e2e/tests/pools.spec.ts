import { expect, test, type Locator, type Page } from "@playwright/test";

import { ApiCleanup } from "../src/api.js";
import {
  FIXED_ALIAS,
  FIXED_REFERENCE,
  POOL_E2E_CIDR,
  POOL_E2E_NAME,
  READY_SENTINEL,
  SKIP_GUEST_BOOT,
} from "../src/constants.js";
import { startLocalOciRegistry, type LocalOciRegistry } from "../src/registry.js";

/**
 * Issue #291 browser E2E — warm MicroVM pools. Does not boot a guest.
 * QA row P6 (boot a warm member) lives in scripts/ci-qa-guest.sh.
 *
 *   FIRECRAB_E2E_SKIP_GUEST_BOOT=1 npm run test:pools --prefix firecrab-e2e
 *   npm run test:pools --prefix firecrab-e2e
 *
 * The form test does not need the net helper. Creating the network does
 * (same gate as the other specs' mutation half). minReady 0 so reconcile
 * never starts a member. Acquire's response is 409 pool_exhausted.
 * The dashboard poll clears the on-screen error, so the test does not
 * wait for that text.
 */
test.describe.configure({ mode: "serial" });

const api = new ApiCleanup();
const MIN_READY = "0";
const MAX_SIZE = "1";
const LEASE_TTL_SECONDS = "600";
const POOL_DELETE_TIMEOUT_MS = 60_000;

let registry: LocalOciRegistry | undefined;
/** Set only when this file imported the alias. Catalog images stay. */
let importedAlias: string | null = null;

async function openEnglish(page: Page, hash: string): Promise<void> {
  await page.addInitScript(() => {
    localStorage.setItem("firecrab.locale", "en");
  });
  await page.goto(hash);
}

function poolsPanel(page: Page): Locator {
  return page.locator("section.panel", {
    has: page.getByRole("heading", { name: "Pools", level: 2 }),
  });
}

async function deleteOwnedPool(): Promise<void> {
  const deadline = Date.now() + POOL_DELETE_TIMEOUT_MS;
  while (Date.now() < deadline) {
    const owned = (await api.listPools()).filter((pool) => pool.name === POOL_E2E_NAME);
    if (owned.length === 0) return;
    for (const pool of owned) {
      if (!pool.deleting) await api.deletePool(pool.id);
    }
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  const left = (await api.listPools())
    .filter((pool) => pool.name === POOL_E2E_NAME)
    .map((pool) => pool.id);
  throw new Error(`pool ${POOL_E2E_NAME} still present after delete: ${left.join(", ")}`);
}

async function importFixture(page: Page, reference: string, alias: string): Promise<void> {
  await openEnglish(page, "/#/images");
  await expect(page.locator("#oci-reference")).toBeVisible();
  await expect(page.locator("#oci-import")).toBeDisabled();

  await page.locator("#oci-reference").fill(reference);
  await page.locator("#oci-inspect").click();

  const oci = page.locator("section.panel", { has: page.getByRole("heading", { name: "OCI" }) });
  await expect(oci.getByText("Compatible with this host.")).toBeVisible({ timeout: 30_000 });
  await expect(oci.locator("dd", { hasText: alias }).first()).toBeVisible();
  await expect(page.locator("#oci-import")).toBeEnabled();

  await page.locator("#oci-import").click();
  const status = oci.locator(".state-badge");
  await expect(status).toHaveText(/Imported|Import failed/, { timeout: 180_000 });
  if ((await status.textContent())?.includes("failed")) {
    const log = (await oci.locator(".image-install-log").textContent()) ?? "";
    throw new Error(`OCI import failed for ${reference}:\n${log}`);
  }

  await expect(page.locator("table.image-table")).toContainText(alias);
}

/** Fixture alias from the #90 registry. Import only when it is not installed. */
async function ensureFixtureAlias(page: Page): Promise<string> {
  const images = await api.listImages();
  if (images.some((image) => image.alias === FIXED_ALIAS && image.installed)) {
    return FIXED_ALIAS;
  }

  registry = await startLocalOciRegistry();
  expect(registry.announcement.reference).toBe(FIXED_REFERENCE);
  expect(registry.announcement.alias).toBe(FIXED_ALIAS);
  expect(registry.announcement.ready).toBe(READY_SENTINEL);
  importedAlias = registry.announcement.alias;
  await importFixture(page, registry.announcement.reference, importedAlias);
  return importedAlias;
}

async function submitCreate(page: Page, panel: Locator): Promise<void> {
  const pending = page.waitForResponse((response) => {
    if (response.request().method() !== "POST") return false;
    try {
      return new URL(response.url()).pathname.replace(/\/$/, "") === "/api/pools";
    } catch {
      return false;
    }
  });
  await panel.getByRole("button", { name: "Create pool" }).click();
  const response = await pending;
  const body = await response.text();
  if (!response.ok()) {
    throw new Error(`POST /api/pools ${response.status()}: ${body}`);
  }
  const created = JSON.parse(body) as { name?: string; minReady?: number; members?: unknown };
  expect(created.name).toBe(POOL_E2E_NAME);
  expect(created.minReady).toBe(0);
  if (!Array.isArray(created.members)) {
    throw new Error(`POST /api/pools members was not a list: ${body}`);
  }
  expect(created.members).toHaveLength(0);
}

test.beforeAll(async () => {
  await deleteOwnedPool();
  await api.deleteNetworksByName([POOL_E2E_NAME]);
});

test.afterAll(async () => {
  try {
    await deleteOwnedPool();
  } finally {
    try {
      await api.deleteNetworksByName([POOL_E2E_NAME]);
      if (importedAlias) await api.deleteImportedImage(importedAlias);
    } finally {
      await registry?.stop();
    }
  }
});

test("shows the Pools heading, empty copy, and create form without submitting", async ({
  page,
}) => {
  await openEnglish(page, "/#/pools");
  await expect(page).toHaveURL(/#\/pools$/);
  const panel = poolsPanel(page);
  await expect(panel.getByRole("heading", { name: "Pools", level: 2 })).toBeVisible();
  await expect(panel.locator("#pool-name")).toBeVisible();
  await expect(panel.locator("#pool-image")).toBeVisible();
  await expect(panel.locator("#pool-network")).toBeVisible();
  await expect(panel.locator("#pool-min")).toBeVisible();
  await expect(panel.locator("#pool-max")).toBeVisible();
  await expect(panel.locator("#pool-ttl")).toBeVisible();
  await expect(panel.getByRole("button", { name: "Create pool" })).toBeVisible();
  await expect(panel.getByText(/^Loading/)).toHaveCount(0);

  const pools = await api.listPools();
  if (pools.length === 0) {
    await expect(panel.getByText("No pools yet.", { exact: true })).toBeVisible();
  } else {
    await expect(panel.locator("table.vm-table")).toBeVisible();
  }
});

test("creates pool-e2e, reports no ready member on acquire, and deletes the row", async ({
  page,
}) => {
  test.skip(
    SKIP_GUEST_BOOT,
    "FIRECRAB_E2E_SKIP_GUEST_BOOT is set — the form test already ran. Unset the flag (and run ./scripts/dev-net-helper.sh) to create the pool. This spec does not boot a guest.",
  );

  const alias = await ensureFixtureAlias(page);
  const network = await api.createMicroNetwork(POOL_E2E_NAME, POOL_E2E_CIDR);
  expect(network.name).toBe(POOL_E2E_NAME);
  expect(network.subnetCidr).toBe(POOL_E2E_CIDR);

  await openEnglish(page, "/#/pools");
  const panel = poolsPanel(page);
  await expect(panel.locator(`#pool-image option[value="${alias}"]`)).toHaveCount(1, {
    timeout: 15_000,
  });
  await expect(panel.locator(`#pool-network option[value="${network.id}"]`)).toHaveCount(1);
  await panel.locator("#pool-name").fill(POOL_E2E_NAME);
  await panel.locator("#pool-image").selectOption(alias);
  await panel.locator("#pool-network").selectOption(network.id);
  await panel.locator("#pool-min").fill(MIN_READY);
  await panel.locator("#pool-max").fill(MAX_SIZE);
  await panel.locator("#pool-ttl").fill(LEASE_TTL_SECONDS);
  await expect(panel.locator("#pool-min")).toHaveValue(MIN_READY);
  await expect(panel.locator("#pool-max")).toHaveValue(MAX_SIZE);
  await expect(panel.locator("#pool-ttl")).toHaveValue(LEASE_TTL_SECONDS);
  await expect(panel.locator("#pool-image")).toHaveValue(alias);
  await expect(panel.locator("#pool-network")).toHaveValue(network.id);

  await submitCreate(page, panel);
  const row = panel.locator("table.vm-table tbody tr").filter({ hasText: POOL_E2E_NAME });
  await expect(row).toBeVisible();
  await expect(row).toContainText("0 / 0");
  await expect(row).toContainText("600s");
  const created = (await api.listPools()).find((pool) => pool.name === POOL_E2E_NAME);
  expect(created, `API missing ${POOL_E2E_NAME}`).toBeTruthy();
  expect(created?.minReady).toBe(0);
  expect(created?.members).toBe(0);

  const acquireResponse = page.waitForResponse(
    (response) =>
      response.request().method() === "POST" &&
      response.url().includes("/api/pools/") &&
      response.url().includes("/acquire"),
  );
  await row.getByRole("button", { name: "Acquire" }).click();
  const acquired = await acquireResponse;
  const acquireBody = await acquired.text();
  expect(acquired.status(), acquireBody).toBe(409);
  expect(acquireBody).toContain("pool_exhausted");
  expect((await api.listPools()).find((pool) => pool.name === POOL_E2E_NAME)?.members).toBe(0);

  page.once("dialog", async (dialog) => {
    expect(dialog.message()).toContain(POOL_E2E_NAME);
    await dialog.accept();
  });
  await row.getByRole("button", { name: "Delete" }).click();
  await expect(row).toHaveCount(0, { timeout: 30_000 });
});
