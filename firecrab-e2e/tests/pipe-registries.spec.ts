import { expect, test, type Page } from "@playwright/test";

const HARBOR = "127.0.0.1:18080";
async function open(page: Page, options: {
  saved?: "ghcr" | "harbor" | "dockerHub" | "both" | "ecr";
  failPull?: boolean;
  running?: boolean;
  catalogError?: boolean;
  tagError?: boolean;
  slowTags?: boolean;
  empty?: boolean;
  locale?: "en" | "ko";
} = {}) {
  const logins: Record<string, unknown>[] = [];
  const transfers: Record<string, unknown>[] = [];
  const catalogs: { registry: string; namespace: string; page: number }[] = [];
  const tagQueries: string[] = [];
  const localQueries: string[] = [];
  const github = { registry: "ghcr.io", username: "team", provider: "ghcr" };
  const docker = { registry: "docker.io", username: "docker-user", provider: "dockerHub" };
  let connections = options.saved === "ghcr" ? [github]
    : options.saved === "dockerHub" ? [docker]
    : options.saved === "both" ? [github, docker]
    : options.saved === "ecr" ? [{ registry: "public.ecr.aws", username: "AWS", provider: "ecr" }]
    : options.saved === "harbor" ? [{ registry: HARBOR, username: "pipe-reader", provider: "harbor" }] : [];
  let polls = 0;
  let failTags = options.tagError;
  let failCatalog = options.catalogError;
  await page.route(/^https?:\/\/[^/]+\/api\//, async route => {
    const url = new URL(route.request().url());
    const path = decodeURIComponent(url.pathname);
    const method = route.request().method();
    if (path === "/api/vms") return route.fulfill({ json: [] });
    if (path === "/api/pipe") return route.fulfill({ json: { config: null, units: [], nginxAvailable: true, cloudflaredAvailable: true } });
    if (path === "/api/pipe/registries") return route.fulfill({ json: connections });
    if (path.endsWith("/repositories")) {
      const registry = path.split("/")[4];
      const namespace = connections.find(connection => connection.registry === registry)?.username ?? "";
      const page = Number(url.searchParams.get("page"));
      catalogs.push({ registry, namespace: url.searchParams.get("namespace") ?? "", page });
      if (failCatalog) {
        return route.fulfill({ status: 403, json: { error: { message: "Catalog access denied. Check token permissions." } } });
      }
      if (registry === "public.ecr.aws") return route.fulfill({ json: { repositories: [], nextPage: null, listingSupported: false } });
      const repositories = options.empty ? [] : registry === HARBOR ? [`firecrab/${page === 1 ? "harbor-nginx" : "worker"}`]
        : [`${namespace}/${page === 1 ? "app" : "worker"}`];
      return route.fulfill({ json: { repositories, nextPage: page === 1 ? 2 : null, listingSupported: true } });
    }
    if (path.startsWith("/api/pipe/registries/")) {
      const registry = path.slice("/api/pipe/registries/".length);
      if (method === "DELETE") { connections = connections.filter(value => value.registry !== registry); return route.fulfill({ json: { deleted: true } }); }
      const body = route.request().postDataJSON(); logins.push({ registry, ...body });
      const saved = { registry, username: body.username, provider: body.provider ?? (registry === "docker.io" ? "dockerHub" : registry === "ghcr.io" ? "ghcr" : "ecr") };
      connections = [...connections.filter(value => value.registry !== registry), saved];
      return route.fulfill({ json: saved });
    }
    if (path === "/api/pipe/registry-tags") {
      const reference = url.searchParams.get("reference") ?? "";
      tagQueries.push(reference);
      if (options.slowTags && reference.startsWith("ghcr.io")) await new Promise(resolve => setTimeout(resolve, 350));
      if (failTags) {
        return route.fulfill({ status: 404, json: { error: { message: "Repository lookup failed" } } });
      }
      if (reference.startsWith("docker.io/docker-user/")) return route.fulfill({ json: { tags: ["alpine"], nextLast: null } });
      if (reference.startsWith(HARBOR)) return route.fulfill({ json: { tags: ["1.0.0", "latest"], nextLast: null } });
      return route.fulfill({ json: url.searchParams.has("last") ? { tags: ["v2"], nextLast: null } : { tags: ["v1"], nextLast: "v1" } });
    }
    if (path === "/api/pipe/registry-transfers") {
      transfers.push(route.request().postDataJSON()); polls = 0;
      return route.fulfill({ status: 202, json: { alias: "b".repeat(64), status: "running", log: "registry transfer started" } });
    }
    if (path.startsWith("/api/pipe/registry-transfers/")) {
      polls++;
      const running = options.running || polls === 1;
      return route.fulfill({ json: { alias: "b".repeat(64), status: running ? "running" : options.failPull ? "failed" : "succeeded", log: running ? "registry transfer started" : options.failPull ? "registry transfer failed: registry rejected the credentials" : "registry transfer succeeded" } });
    }
    if (path === "/api/pipe/registry-images" || path.startsWith("/api/oci/")) localQueries.push(path);
    if (path === "/api/update") return route.fulfill({ json: { current: "0.3.1", latest: "0.3.1", updateAvailable: false } });
    return route.fulfill({ status: 404, json: { error: { message: "fixture route unavailable" } } });
  });
  await page.addInitScript(locale => localStorage.setItem("firecrab.locale", locale), options.locale ?? "en");
  await page.goto("/#/pipe/registry");
  const panel = page.getByRole("region", { name: "Pipe Registry", exact: true });
  await expect(panel.getByRole("button", { name: options.locale === "ko" ? "레지스트리 추가" : "Add registry" })).toBeEnabled();
  return { panel, logins, transfers, catalogs, tagQueries, localQueries, allowTags: () => { failTags = false; }, allowCatalog: () => { failCatalog = false; } };
}

test.describe("Pipe registry automatic discovery @dashboard", () => {
  test("does not query image lists without a saved registry", async ({ page }) => {
    const mock = await open(page);
    await expect(mock.panel.getByText("No registries.", { exact: true })).toBeVisible();
    await expect(mock.panel.getByLabel("Source registry")).toHaveCount(0);
    await expect(mock.panel.locator("input")).toHaveCount(0);
    expect(mock.catalogs).toHaveLength(0);
    expect(mock.tagQueries).toHaveLength(0);
  });

  test("lists the saved Docker Hub account and pulls its selected image", async ({ page }) => {
    const mock = await open(page, { saved: "dockerHub" });
    const catalog = mock.panel.getByRole("table", { name: "Registry images", exact: true });
    await expect(catalog).toContainText("docker.io/docker-user/app");
    await expect(catalog).not.toContainText("library/");
    await expect(mock.panel.getByLabel("Source registry")).toHaveText("Docker Hub (docker.io) · docker-user");
    await expect(mock.panel.getByRole("button", { name: "alpine", exact: true })).toHaveAttribute("aria-pressed", "true");
    await expect(mock.panel.locator("input")).toHaveCount(0);
    await mock.panel.getByRole("button", { name: "More images", exact: true }).click();
    await expect(catalog).toContainText("docker.io/docker-user/worker");
    await mock.panel.getByRole("button", { name: "Pull image", exact: true }).click();
    await expect(mock.panel.getByText("Image pulled.", { exact: true })).toBeVisible();
    expect(mock.transfers).toEqual([{ action: "pull", reference: "docker.io/docker-user/app:alpine" }]);
    expect(mock.catalogs).toContainEqual({ registry: "docker.io", namespace: "", page: 2 });
    expect(mock.catalogs.every(query => query.registry === "docker.io" && query.namespace === "")).toBe(true);
    expect(mock.localQueries).toHaveLength(0);
  });

  test("does not replace an empty Docker Hub account with official images", async ({ page }) => {
    const mock = await open(page, { saved: "dockerHub", empty: true });
    await expect(mock.panel.getByText("No repositories.", { exact: true })).toBeVisible();
    await expect(mock.panel.getByRole("button", { name: "Pull image", exact: true })).toHaveCount(0);
    expect(mock.catalogs.length).toBeGreaterThan(0);
    expect(mock.catalogs.every(query => query.registry === "docker.io" && query.namespace === "" && query.page === 1)).toBe(true);
    expect(mock.tagQueries).toHaveLength(0);
  });

  test("saves and rotates a Docker Hub login and refreshes the automatic list", async ({ page }) => {
    const mock = await open(page);
    await mock.panel.getByRole("button", { name: "Add registry" }).click();
    await mock.panel.getByLabel("Provider").selectOption("dockerHub");
    await mock.panel.getByLabel("Docker Hub username").fill("docker-user");
    await mock.panel.getByLabel("Personal access token (PAT)").fill("private-token");
    await mock.panel.getByRole("button", { name: "Save login" }).click();
    await expect(mock.panel.getByRole("table", { name: "Registry images", exact: true })).toContainText("docker.io/docker-user/app");
    const row = mock.panel.getByRole("row").filter({ has: page.getByRole("cell", { name: "docker.io", exact: true }) });
    await row.getByRole("button", { name: "Update token" }).click();
    await expect(mock.panel.getByLabel("Personal access token (PAT)")).toHaveValue("");
    await mock.panel.getByLabel("Personal access token (PAT)").fill("rotated-token");
    await mock.panel.getByRole("button", { name: "Save login" }).click();
    await expect(mock.panel.getByRole("button", { name: "Pull image", exact: true })).toBeEnabled();
    expect(mock.logins[1].secret).toBe("rotated-token");
    await expect(mock.panel).not.toContainText("rotated-token");
    await row.getByRole("button", { name: "Remove" }).click();
    await expect(row).toHaveCount(0);
  });

  test("automatically loads a saved registry and the first repository's tags", async ({ page }) => {
    const mock = await open(page, { saved: "ghcr" });
    await expect(mock.panel.getByRole("button", { name: "v1", exact: true })).toHaveAttribute("aria-pressed", "true");
    expect(mock.tagQueries).toContain("ghcr.io/team/app:latest");
    await expect(mock.panel.getByRole("heading", { name: "Local OCI images" })).toHaveCount(0);
    await expect(mock.panel.getByRole("button", { name: "Import as M2Image" })).toHaveCount(0);
    await expect(mock.panel.locator("input")).toHaveCount(0);
  });

  test("automatically loads tags on repository selection and pulls a selected paged tag", async ({ page }) => {
    const mock = await open(page, { saved: "ghcr" });
    await mock.panel.getByRole("button", { name: "More images", exact: true }).click();
    const row = mock.panel.getByRole("table", { name: "Registry images", exact: true }).getByRole("row").filter({ hasText: "ghcr.io/team/worker" });
    await row.getByRole("button", { name: "Select", exact: true }).click();
    await expect.poll(() => mock.tagQueries.includes("ghcr.io/team/worker:latest")).toBe(true);
    await mock.panel.getByRole("button", { name: "More tags" }).click();
    await mock.panel.getByRole("button", { name: "v2", exact: true }).click();
    await mock.panel.getByRole("button", { name: "Pull image", exact: true }).click();
    await expect(mock.panel.getByText("Image pulled.")).toBeVisible();
    expect(mock.transfers).toEqual([{ action: "pull", reference: "ghcr.io/team/worker:v2" }]);
  });

  test("connects Harbor, automatically lists its repository and tags, and pulls harbor-nginx", async ({ page }) => {
    const mock = await open(page);
    await mock.panel.getByRole("button", { name: "Add registry" }).click();
    await mock.panel.getByLabel("Provider").selectOption("harbor");
    await mock.panel.getByLabel("Registry hostname").fill(HARBOR);
    await mock.panel.getByLabel("Harbor username").fill("pipe-reader");
    await mock.panel.getByLabel("Harbor password or robot token").fill("harbor-secret");
    await mock.panel.getByRole("button", { name: "Save login" }).click();
    await expect(mock.panel.getByRole("button", { name: "1.0.0", exact: true })).toHaveAttribute("aria-pressed", "true");
    await expect(mock.panel.locator("input")).toHaveCount(0);
    expect(mock.logins[0]).toEqual({ registry: HARBOR, username: "pipe-reader", secret: "harbor-secret", provider: "harbor" });
    await mock.panel.screenshot({ path: "test-results/harbor-registry-desktop.png" });
    await mock.panel.getByRole("button", { name: "Pull image", exact: true }).click();
    await expect(mock.panel.getByText("Image pulled.")).toBeVisible();
    expect(mock.transfers).toEqual([{ action: "pull", reference: `${HARBOR}/firecrab/harbor-nginx:1.0.0` }]);
  });

  test("keeps Harbor usable on Korean mobile screens without manual repository or tag fields", async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    const mock = await open(page, { saved: "harbor", locale: "ko" });
    await expect(mock.panel.getByRole("button", { name: "1.0.0", exact: true })).toHaveAttribute("aria-pressed", "true");
    await expect(mock.panel.getByRole("button", { name: "이미지 가져오기", exact: true })).toBeEnabled();
    await expect(mock.panel.locator("input")).toHaveCount(0);
    await expect(mock.panel).not.toContainText("로컬 OCI 이미지");
    await mock.panel.screenshot({ path: "test-results/harbor-registry-mobile.png" });
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
  });

  test("discards a delayed tag lookup when the selected registry changes", async ({ page }) => {
    const mock = await open(page, { saved: "both", slowTags: true });
    await expect.poll(() => mock.tagQueries.includes("ghcr.io/team/app:latest")).toBe(true);
    await mock.panel.getByLabel("Source registry").selectOption("docker.io");
    await expect(mock.panel.getByRole("button", { name: "alpine", exact: true })).toBeVisible();
    await page.waitForTimeout(450);
    await expect(mock.panel.getByRole("button", { name: "v1", exact: true })).toHaveCount(0);
    await mock.panel.getByRole("button", { name: "Pull image", exact: true }).click();
    await expect(mock.panel.getByText("Image pulled.")).toBeVisible();
    expect(mock.transfers[0].reference).toBe("docker.io/docker-user/app:alpine");
  });

  test("shows lookup failures, disables pulling, and recovers with a retry", async ({ page }) => {
    const mock = await open(page, { saved: "ghcr", tagError: true });
    await expect(mock.panel.getByRole("alert")).toContainText("Repository lookup failed");
    await expect(mock.panel.getByRole("button", { name: "Pull image", exact: true })).toBeDisabled();
    mock.allowTags();
    await mock.panel.getByRole("button", { name: "Retry tags" }).click();
    await expect(mock.panel.getByRole("button", { name: "Pull image", exact: true })).toBeEnabled();
    expect(mock.transfers).toHaveLength(0);
  });

  test("shows catalog failures and refreshes without asking for repository input", async ({ page }) => {
    const mock = await open(page, { saved: "ghcr", catalogError: true });
    await expect(mock.panel.getByRole("alert")).toContainText("Catalog access denied");
    mock.allowCatalog();
    await mock.panel.getByRole("button", { name: "Refresh repositories" }).click();
    await expect(mock.panel.getByRole("button", { name: "v1", exact: true })).toBeVisible();
    await expect(mock.panel.locator("input")).toHaveCount(0);
  });

  test("shows an empty catalog without offering an unqueried pull", async ({ page }) => {
    const mock = await open(page, { saved: "ghcr", empty: true });
    await expect(mock.panel.getByText("No repositories.", { exact: true })).toBeVisible();
    await expect(mock.panel.getByRole("button", { name: "Pull image", exact: true })).toHaveCount(0);
    expect(mock.tagQueries).toHaveLength(0);
  });

  test("shows the ECR automatic discovery limitation without restoring manual inputs", async ({ page }) => {
    const mock = await open(page, { saved: "ecr" });
    await expect(mock.panel.getByText("Repository lookup requires AWS IAM.", { exact: true })).toBeVisible();
    await expect(mock.panel.locator("input")).toHaveCount(0);
    await expect(mock.panel.getByRole("button", { name: "Pull image", exact: true })).toHaveCount(0);
  });

  test("retains the selection after a failed pull for retry", async ({ page }) => {
    const mock = await open(page, { saved: "harbor", failPull: true });
    await expect(mock.panel.getByRole("button", { name: "Pull image", exact: true })).toBeEnabled();
    await mock.panel.getByRole("button", { name: "Pull image", exact: true }).click();
    await expect(mock.panel.getByRole("alert")).toContainText("registry rejected the credentials");
    await expect(mock.panel.getByRole("button", { name: "Pull image", exact: true })).toBeEnabled();
  });

  test("restores a running pull after reload without starting a duplicate", async ({ page }) => {
    const mock = await open(page, { saved: "harbor", running: true });
    await expect(mock.panel.getByRole("button", { name: "Pull image", exact: true })).toBeEnabled();
    await mock.panel.getByRole("button", { name: "Pull image", exact: true }).click();
    await expect(mock.panel.getByText("Pulling…")).toBeVisible();
    await page.reload();
    await expect(mock.panel.getByText("Pulling…")).toBeVisible();
    await expect(mock.panel.getByRole("button", { name: "Pull image", exact: true })).toBeDisabled();
    expect(mock.transfers).toHaveLength(1);
  });
});
