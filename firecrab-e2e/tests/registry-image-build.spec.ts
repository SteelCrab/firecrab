import { expect, test, type Page } from "@playwright/test";
import type { RegistryImage } from "../../firecrab-frontend/src/api/registries";

const DIGEST = `sha256:${"a".repeat(64)}`;
const SOURCE: RegistryImage = {
  id: "a".repeat(64), reference: "ghcr.io/team/app:latest", digest: DIGEST,
  architecture: "aarch64", sizeBytes: 1024,
  templateAlias: "ghcr.io-team-app-sha256-aaaaaaaaaaaa",
};

async function open(page: Page, options: { source?: RegistryImage; failBuild?: boolean; running?: boolean; failList?: boolean } = {}) {
  const source = options.source ?? SOURCE;
  const requests: { reference: string }[] = [];
  let installed = false;
  let started = false;
  let polls = 0;
  let failBuild = options.failBuild;
  let running = options.running;
  let failList = options.failList;
  const image = {
    alias: source.templateAlias, version: DIGEST, installed: true,
    kernelImage: "kernel", kernelSha256: "a".repeat(64), rootfsSha256: "b".repeat(64),
    minDiskGb: 1, rootfsSizeBytes: 64 * 1024 * 1024, description: "", hasGuestService: true,
  };
  await page.route(/^https?:\/\/[^/]+\/api\//, async route => {
    const path = new URL(route.request().url()).pathname;
    if (path === "/api/images") return route.fulfill({ json: installed ? [image] : [] });
    if (path === "/api/pipe/registry-images") {
      if (failList) return route.fulfill({ status: 500, json: { error: { message: "Pulled image list unavailable" } } });
      return route.fulfill({ json: [source] });
    }
    if (path === "/api/oci/import" && route.request().method() === "POST") {
      requests.push(route.request().postDataJSON());
      started = true;
      polls = 0;
      return route.fulfill({ status: 202, json: { alias: source.templateAlias, status: "running", log: "import started", startedAtMs: requests.length } });
    }
    if (path === `/api/oci/import/${source.templateAlias}`) {
      if (!started) return route.fulfill({ json: { alias: source.templateAlias, status: "idle", log: "" } });
      polls++;
      const active = running || polls === 1;
      if (!active && !failBuild) installed = true;
      return route.fulfill({ json: {
        alias: source.templateAlias, status: active ? "running" : failBuild ? "failed" : "succeeded",
        log: active ? "merging layers" : failBuild ? "import failed: ext4 build failed" : "import succeeded — template registered",
        startedAtMs: requests.length,
      } });
    }
    if (path === "/api/microregistry") return route.fulfill({ json: { images: [], catalogUrl: "https://registry.invalid/catalog.json", online: true } });
    if (["/api/vms", "/api/kernels", "/api/micro-networks", "/api/storage", "/api/shells"].includes(path)) return route.fulfill({ json: [] });
    if (path === "/api/update") return route.fulfill({ json: { current: "0.3.1", latest: "0.3.1", updateAvailable: false } });
    return route.fulfill({ status: 404, json: { error: { message: "fixture route unavailable" } } });
  });
  await page.addInitScript(() => localStorage.setItem("firecrab.locale", "en"));
  await page.goto("/#/images");
  const panel = page.locator("section.panel").filter({ has: page.getByRole("heading", { name: /^M2Image/ }) });
  const row = panel.locator("tr.pulled-image-row");
  return { panel, row, source, requests, allowBuild: () => { failBuild = false; running = false; }, allowList: () => { failList = false; } };
}

test.describe("Pulled registry images in M2Image @dashboard", () => {
  for (const { source, name, registry } of [{ source: SOURCE, name: "team/app:latest", registry: "GHCR" }, { source: {
    ...SOURCE, reference: "127.0.0.1:18080/project/app:latest",
    templateAlias: "127.0.0.1-18080-project-app-sha256-aaaaaaaaaaaa",
  }, name: "project/app:latest", registry: "Harbor" }, { source: {
    ...SOURCE, reference: `ghcr.io/team/app@${DIGEST}`,
  }, name: "team/app@sha256:aaaaaaaaaaaa", registry: "GHCR" }, { source: {
    ...SOURCE, reference: "registry-1.docker.io/team/app:latest",
    templateAlias: "team-app-sha256-aaaaaaaaaaaa",
  }, name: "team/app:latest", registry: "Docker Hub" }, { source: {
    ...SOURCE, reference: "123456789012.dkr.ecr.ap-northeast-2.amazonaws.com/team/app:latest",
    templateAlias: "123456789012.dkr.ecr.ap-northeast-2.amazonaws.com-team-app-sha256-aaaaaaaaaaaa",
  }, name: "team/app:latest", registry: "ECR" }]) {
    test(`builds the downloaded digest of ${source.reference} and makes it selectable for VM creation`, async ({ page }) => {
      const mock = await open(page, { source });
      const host = source.reference.split("/")[0];
      await expect(mock.panel.getByRole("columnheader", { name: "Registry", exact: true })).toBeVisible();
      await expect(mock.row.getByRole("cell", { name: registry, exact: true })).toBeVisible();
      await expect(mock.row.getByRole("cell", { name: registry, exact: true })).toHaveAttribute("title", host);
      await expect(mock.row.locator("td:first-child")).toHaveText(name);
      await expect(mock.row.getByText("Build required", { exact: true })).toBeVisible();
      await mock.row.getByRole("button", { name: "⋯", exact: true }).click();
      await mock.row.getByRole("button", { name: "Build M2Image", exact: true }).click();
      await expect(mock.row.getByText("Building…", { exact: true })).toBeVisible();
      const installed = mock.panel.getByRole("row").filter({ has: page.getByText("Installed", { exact: true }) });
      await expect(installed.getByText("Installed", { exact: true })).toBeVisible();
      await expect(installed.getByRole("cell", { name: registry, exact: true })).toBeVisible();
      await expect(installed.getByRole("cell", { name: registry, exact: true })).toHaveAttribute("title", host);
      await expect(installed.getByRole("cell", { name, exact: true })).toHaveAttribute("title", source.templateAlias);
      await expect(mock.row).toHaveCount(0);
      const repository = source.reference.split("@")[0].replace(/:[^/]+$/, "");
      expect(mock.requests).toEqual([{ reference: `${repository}@${DIGEST}` }]);
      await page.reload();
      await expect(installed.getByText("Installed", { exact: true })).toBeVisible();
      await expect(installed.getByRole("cell", { name: registry, exact: true })).toBeVisible();
      await expect(installed.getByRole("cell", { name, exact: true })).toBeVisible();
      await expect(mock.row).toHaveCount(0);
      await page.goto("/#/vms/new");
      await expect(page.locator(`#vm-image option[value="${source.templateAlias}"]`)).toHaveCount(1);
    });
  }

  test("keeps failed builds visible with their log and allows retry", async ({ page }) => {
    const mock = await open(page, { failBuild: true });
    await mock.row.getByRole("button", { name: "⋯", exact: true }).click();
    await mock.row.getByRole("button", { name: "Build M2Image", exact: true }).click();
    await expect(mock.row.getByText("Build failed", { exact: true })).toBeVisible();
    await expect(mock.panel.getByRole("alert")).toContainText("ext4 build failed");
    mock.allowBuild();
    await mock.row.getByRole("button", { name: "⋯", exact: true }).click();
    await mock.row.getByRole("button", { name: "Build M2Image", exact: true }).click();
    await expect(mock.panel.locator("table.image-table").getByText("Installed", { exact: true })).toBeVisible();
    expect(mock.requests).toHaveLength(2);
  });

  test("resumes a running build after reload without starting another import", async ({ page }) => {
    const mock = await open(page, { running: true });
    await mock.row.getByRole("button", { name: "⋯", exact: true }).click();
    await mock.row.getByRole("button", { name: "Build M2Image", exact: true }).click();
    await expect(mock.row.getByText("Building…", { exact: true })).toBeVisible();
    await page.reload();
    await expect(mock.row.getByText("Building…", { exact: true })).toBeVisible();
    mock.allowBuild();
    await expect(mock.panel.locator("table.image-table").getByText("Installed", { exact: true })).toBeVisible();
    expect(mock.requests).toHaveLength(1);
  });

  test("shows a pulled-image lookup error and loads sources after refresh", async ({ page }) => {
    const mock = await open(page, { failList: true });
    await expect(mock.panel.getByRole("alert")).toContainText("Pulled image list unavailable");
    mock.allowList();
    await mock.panel.getByRole("button", { name: "Refresh", exact: true }).click();
    await expect(mock.row.getByText("Build required", { exact: true })).toBeVisible();
    await expect(mock.panel.getByRole("alert")).toHaveCount(0);
  });

  test("keeps source and built-image rows usable on a narrow screen", async ({ page }) => {
    await page.setViewportSize({ width: 320, height: 640 });
    const mock = await open(page);
    await expect(mock.row.getByText("Build required", { exact: true })).toBeVisible();
    await expect(mock.row.locator('[data-label="Registry"]')).toHaveText("GHCR");
    expect(await mock.panel.evaluate(panel => panel.scrollWidth > panel.clientWidth)).toBe(false);
    await mock.row.getByRole("button", { name: "⋯", exact: true }).click();
    await mock.row.getByRole("button", { name: "Build M2Image", exact: true }).click();
    await expect(mock.panel.locator("table.image-table").getByText("Installed", { exact: true })).toBeVisible();
    await expect(mock.panel.locator('table.image-table [data-label="Registry"]')).toHaveText("GHCR");
    expect(await mock.panel.evaluate(panel => panel.scrollWidth > panel.clientWidth)).toBe(false);
  });
});
