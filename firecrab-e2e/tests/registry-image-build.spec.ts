import { expect, test, type Page } from "@playwright/test";
import type { RegistryImage } from "../types/api/registries";

const DIGEST = `sha256:${"a".repeat(64)}`;
const SOURCE: RegistryImage = {
  id: "a".repeat(64),
  reference: "ghcr.io/team/app:latest",
  digest: DIGEST,
  architecture: "aarch64",
  sizeBytes: 1024,
  templateAlias: "ghcr.io-team-app-sha256-aaaaaaaaaaaa",
};

async function open(
  page: Page,
  options: {
    source?: RegistryImage;
    failBuild?: boolean;
    running?: boolean;
    failList?: boolean;
  } = {},
) {
  const source = options.source ?? SOURCE;
  const requests: { reference: string }[] = [];
  let installed = false;
  let started = false;
  let polls = 0;
  let failBuild = options.failBuild;
  let running = options.running;
  let failList = options.failList;
  const image = {
    alias: source.templateAlias,
    version: DIGEST,
    installed: true,
    kernelImage: "kernel",
    kernelSha256: "a".repeat(64),
    rootfsSha256: "b".repeat(64),
    minDiskGb: 1,
    rootfsSizeBytes: 64 * 1024 * 1024,
    description: "",
    hasGuestService: true,
  };
  await page.route(/^https?:\/\/[^/]+\/api\//, async (route) => {
    const path = new URL(route.request().url()).pathname;
    if (path === "/api/images")
      return route.fulfill({ json: installed ? [image] : [] });
    if (path === "/api/pipe/registry-images") {
      if (failList)
        return route.fulfill({
          status: 500,
          json: { error: { message: "Pulled image list unavailable" } },
        });
      return route.fulfill({ json: [source] });
    }
    if (path === "/api/oci/import" && route.request().method() === "POST") {
      requests.push(route.request().postDataJSON());
      started = true;
      polls = 0;
      return route.fulfill({
        status: 202,
        json: {
          alias: source.templateAlias,
          status: "running",
          log: "import started",
          startedAtMs: requests.length,
        },
      });
    }
    if (path === `/api/oci/import/${source.templateAlias}`) {
      if (!started)
        return route.fulfill({
          json: { alias: source.templateAlias, status: "idle", log: "" },
        });
      polls++;
      const active = running || polls === 1;
      if (!active && !failBuild) installed = true;
      return route.fulfill({
        json: {
          alias: source.templateAlias,
          status: active ? "running" : failBuild ? "failed" : "succeeded",
          log: active
            ? "merging layers"
            : failBuild
              ? "import failed: ext4 build failed"
              : "import succeeded — template registered",
          startedAtMs: requests.length,
        },
      });
    }
    if (path === "/api/microregistry")
      return route.fulfill({
        json: {
          images: [],
          catalogUrl: "https://registry.invalid/catalog.json",
          online: true,
        },
      });
    if (
      [
        "/api/vms",
        "/api/kernels",
        "/api/micro-networks",
        "/api/storage",
        "/api/shells",
        "/api/microblockstorage",
      ].includes(path)
    )
      return route.fulfill({ json: [] });
    if (path === "/api/update")
      return route.fulfill({
        json: { current: "0.3.1", latest: "0.3.1", updateAvailable: false },
      });
    return route.fulfill({
      status: 404,
      json: { error: { message: "fixture route unavailable" } },
    });
  });
  await page.addInitScript(() => localStorage.setItem("firecrab.locale", "en"));
  await page.goto("/#/images");
  const panel = page.locator("main");
  const row = panel.locator(".pulled-image-row");
  return {
    panel,
    row,
    source,
    requests,
    allowBuild: () => {
      failBuild = false;
      running = false;
    },
    allowList: () => {
      failList = false;
    },
  };
}

test.describe("Pulled registry images in M2Image @dashboard", () => {
  for (const reference of [
    "ghcr.io/team/app:latest",
    "127.0.0.1:18080/project/app:latest",
    `ghcr.io/team/app@${DIGEST}`,
    "registry-1.docker.io/team/app:latest",
    "123456789012.dkr.ecr.ap-northeast-2.amazonaws.com/team/app:latest",
  ]) {
    test(`builds the downloaded digest of ${reference}`, async ({ page }) => {
      const m = await open(page, { source: { ...SOURCE, reference } });
      await expect(m.row).toContainText(reference);
      await m.row
        .getByRole("button", { name: "Build M2Image", exact: true })
        .click();
      await expect(m.row).toContainText("Building…");
      await expect(
        page.getByRole("cell", { name: SOURCE.templateAlias, exact: true }),
      ).toBeVisible();
      const repository = reference.split("@")[0].replace(/:[^/]+$/, "");
      expect(m.requests).toEqual([{ reference: `${repository}@${DIGEST}` }]);
      await expect(m.row).toHaveCount(0);
      await page.goto("/#/vms/new");
      await expect(
        page.locator(`#field-template option[value="${SOURCE.templateAlias}"]`),
      ).toHaveCount(1);
    });
  }
  test("keeps failed builds visible with their log and retries", async ({
    page,
  }) => {
    const m = await open(page, { failBuild: true });
    await m.row
      .getByRole("button", { name: "Build M2Image", exact: true })
      .click();
    await expect(m.row).toContainText("Build failed");
    await expect(m.panel).toContainText("ext4 build failed");
    m.allowBuild();
    await m.row
      .getByRole("button", { name: "Build M2Image", exact: true })
      .click();
    await expect(
      page.getByRole("cell", { name: SOURCE.templateAlias, exact: true }),
    ).toBeVisible();
    expect(m.requests).toHaveLength(2);
  });
  test("resumes a running build after reload without another import", async ({
    page,
  }) => {
    const m = await open(page, { running: true });
    await m.row
      .getByRole("button", { name: "Build M2Image", exact: true })
      .click();
    await expect(m.row).toContainText("Building…");
    await page.reload();
    await expect(m.row).toContainText("Building…");
    m.allowBuild();
    await expect(
      page.getByRole("cell", { name: SOURCE.templateAlias, exact: true }),
    ).toBeVisible();
    expect(m.requests).toHaveLength(1);
  });
  test("reports an unavailable inventory and recovers on refresh", async ({
    page,
  }) => {
    const m = await open(page, { failList: true });
    await expect(page.getByRole("alert")).toContainText(
      "Pulled image list unavailable",
    );
    m.allowList();
    await page.getByRole("button", { name: "Refresh", exact: true }).click();
    await expect(m.row).toContainText("Build required");
    await expect(page.getByRole("alert")).toHaveCount(0);
  });
  test("keeps source builds usable on narrow screens", async ({ page }) => {
    await page.setViewportSize({ width: 320, height: 640 });
    const m = await open(page);
    await expect(m.row).toContainText("Build required");
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    ).toBe(true);
    await m.row
      .getByRole("button", { name: "Build M2Image", exact: true })
      .click();
    await expect(
      page.getByRole("cell", { name: SOURCE.templateAlias, exact: true }),
    ).toBeVisible();
  });
});
