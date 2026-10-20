import { expect, test, type Page } from "@playwright/test";

const ECR = "123456789012.dkr.ecr.ap-northeast-2.amazonaws.com";
const DIGEST = `sha256:${"a".repeat(64)}`;
const IMAGE = { id: "a".repeat(64), reference: "ghcr.io/team/app:v1", digest: DIGEST, architecture: "aarch64", sizeBytes: 1_234_567 };
async function open(page: Page, options: { local?: boolean; failPull?: boolean; running?: boolean } = {}) {
  const logins: { registry: string; username: string; secret: string }[] = [];
  const transfers: Record<string, unknown>[] = [];
  const imports: Record<string, unknown>[] = [];
  let connections: { registry: string; username: string; provider: string }[] = [];
  let images = options.local ? [IMAGE] : [];
  let polls = 0;
  let tagsPage = 0;
  await page.route(/^https?:\/\/[^/]+\/api\//, async route => {
    const url = new URL(route.request().url());
    const path = decodeURIComponent(url.pathname);
    const method = route.request().method();
    if (path === "/api/vms") return route.fulfill({ json: [] });
    if (path === "/api/pipe") return route.fulfill({ json: { config: null, units: [], nginxAvailable: true, cloudflaredAvailable: true } });
    if (path === "/api/pipe/registries") return route.fulfill({ json: connections });
    if (path.startsWith("/api/pipe/registries/")) {
      const registry = path.slice("/api/pipe/registries/".length);
      if (method === "DELETE") { connections = connections.filter(value => value.registry !== registry); return route.fulfill({ json: { deleted: true } }); }
      const body = route.request().postDataJSON(); logins.push({ registry, ...body });
      const saved = { registry, username: body.username, provider: registry === "docker.io" ? "dockerHub" : registry === "ghcr.io" ? "ghcr" : "ecr" };
      connections = [...connections.filter(value => value.registry !== registry), saved];
      return route.fulfill({ json: saved });
    }
    if (path === "/api/pipe/registry-images") return route.fulfill({ json: images });
    if (path === "/api/pipe/registry-tags") {
      tagsPage++;
      if (url.searchParams.get("reference") === "docker.io/nginx:latest") return route.fulfill({ json: { tags: ["alpine"], nextLast: null } });
      expect(url.searchParams.get("reference")).toBe("ghcr.io/team/app:latest");
      return route.fulfill({ json: url.searchParams.has("last") ? { tags: ["v2"], nextLast: null } : { tags: ["v1"], nextLast: "v1" } });
    }
    if (path === "/api/pipe/registry-transfers") {
      transfers.push(route.request().postDataJSON()); polls = 0;
      return route.fulfill({ status: 202, json: { alias: "b".repeat(64), status: "running", log: "registry transfer started" } });
    }
    if (path.startsWith("/api/pipe/registry-transfers/")) {
      polls++;
      const failed = options.failPull;
      const running = options.running || polls === 1;
      if (!running && !failed) images = [{ ...IMAGE, reference: transfers.at(-1)?.reference === "docker.io/nginx:alpine" ? "registry-1.docker.io/library/nginx:alpine" : IMAGE.reference }];
      return route.fulfill({ json: { alias: "b".repeat(64), status: running ? "running" : failed ? "failed" : "succeeded", log: running ? "registry transfer started" : failed ? "registry transfer failed: registry rejected the credentials" : "registry transfer succeeded" } });
    }
    if (path === "/api/oci/import") {
      imports.push(route.request().postDataJSON());
      return route.fulfill({ status: 202, json: { alias: "imported-app", status: "running", log: "import started" } });
    }
    if (path === "/api/oci/import/imported-app") return route.fulfill({ json: { alias: "imported-app", status: "succeeded", log: "import complete" } });
    if (path === "/api/update") return route.fulfill({ json: { current: "0.3.1", latest: "0.3.1", updateAvailable: false } });
    return route.fulfill({ status: 404, json: { error: { message: "fixture route unavailable" } } });
  });
  await page.addInitScript(() => localStorage.setItem("firecrab.locale", "en"));
  await page.goto("/#/pipe/registry");
  const panel = page.getByRole("region", { name: "Pipe Registry", exact: true });
  await expect(panel.getByRole("button", { name: "Add registry" })).toBeEnabled();
  return { panel, logins, transfers, imports, get tagsPage() { return tagsPage; } };
}

test.describe("Pipe registries @dashboard", () => {
  test("saves and rotates a Docker Hub login, lists official image tags, pulls, and removes the login", async ({ page }) => {
    const mock = await open(page);
    const panel = mock.panel;
    await panel.getByRole("button", { name: "Add registry" }).click();
    await panel.getByLabel("Provider").selectOption("dockerHub");
    await panel.getByLabel("Docker Hub username").fill("docker-user");
    await panel.getByLabel("Personal access token (PAT)").fill("dckr_pat_secret");
    await expect(panel.getByText(/This login is shared/)).toBeVisible();
    await panel.getByRole("button", { name: "Save login" }).click();
    await expect(panel.getByText("Registry login saved.")).toBeVisible();
    expect(mock.logins[0]).toEqual({ registry: "docker.io", username: "docker-user", secret: "dckr_pat_secret" });
    await expect(panel).not.toContainText("dckr_pat_secret");
    const row = panel.getByRole("row").filter({ has: page.getByRole("cell", { name: "docker.io", exact: true }) });
    await row.getByRole("button", { name: "Update token" }).click();
    await expect(panel.getByLabel("Provider")).toHaveValue("dockerHub");
    await expect(panel.getByLabel("Docker Hub username")).toHaveValue("docker-user");
    await expect(panel.getByLabel("Personal access token (PAT)")).toHaveValue("");
    await panel.getByLabel("Personal access token (PAT)").fill("rotated-token");
    await panel.getByRole("button", { name: "Save login" }).click();
    await expect(panel.getByRole("form", { name: "Registry login" })).toHaveCount(0);
    expect(mock.logins[1].secret).toBe("rotated-token");
    await panel.getByLabel("Source registry").selectOption("docker.io");
    await panel.getByLabel("Repository", { exact: true }).fill("nginx");
    await panel.getByRole("button", { name: "List tags" }).click();
    await expect(panel.getByLabel("Tag or digest")).toHaveValue("alpine");
    await panel.getByRole("button", { name: "Pull image", exact: true }).click();
    await expect(panel.getByText("Image pull completed.")).toBeVisible();
    expect(mock.transfers).toEqual([{ action: "pull", reference: "docker.io/nginx:alpine" }]);
    await expect(panel.getByRole("cell", { name: /registry-1\.docker\.io\/library\/nginx:alpine/ })).toBeVisible();
    await panel.screenshot({ path: "test-results/docker-hub-desktop.png" });
    await row.getByRole("button", { name: "Remove" }).click();
    await expect(panel.getByText("Registry login removed.")).toBeVisible();
    await expect(row).toHaveCount(0);
  });

  test("offers public Docker Hub pulls without a login on mobile", async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    const mock = await open(page);
    await mock.panel.getByLabel("Source registry").selectOption("docker.io");
    await mock.panel.getByLabel("Repository", { exact: true }).fill("nginx");
    await mock.panel.getByLabel("Tag or digest").fill("alpine");
    await mock.panel.getByRole("button", { name: "Pull image", exact: true }).click();
    await expect(mock.panel.getByText("Image pull completed.")).toBeVisible();
    expect(mock.logins).toHaveLength(0);
    await mock.panel.screenshot({ path: "test-results/docker-hub-mobile.png" });
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
  });

  test("saves GHCR and ECR logins, browses tags, pulls, and removes a login", async ({ page }) => {
    const mock = await open(page);
    const panel = mock.panel;
    await panel.getByRole("button", { name: "Add registry" }).click();
    await panel.getByLabel("GitHub username").fill("octocat");
    await panel.getByLabel("Personal access token (classic)").fill("ghp-test-secret");
    await panel.getByRole("button", { name: "Save login" }).click();
    await expect(panel.getByText("Registry login saved.")).toBeVisible();
    expect(mock.logins[0]).toEqual({ registry: "ghcr.io", username: "octocat", secret: "ghp-test-secret" });
    await expect(panel).not.toContainText("ghp-test-secret");
    await panel.getByRole("button", { name: "Add registry" }).click();
    await panel.getByLabel("Provider").selectOption("ecr");
    await panel.getByLabel("Registry hostname").fill(ECR);
    await panel.getByLabel("ECR login password").fill("ecr-test-secret");
    await expect(panel.getByText(/Tokens expire after 12 hours/)).toBeVisible();
    await panel.getByRole("button", { name: "Save login" }).click();
    await expect(panel.getByRole("cell", { name: ECR, exact: true })).toBeVisible();
    expect(mock.logins[1].username).toBe("AWS");
    await panel.getByLabel("Source registry").selectOption("ghcr.io");
    await panel.getByLabel("Repository", { exact: true }).fill("team/app");
    await panel.getByRole("button", { name: "List tags" }).click();
    await expect(panel.getByLabel("Tag or digest")).toHaveValue("v1");
    await panel.getByRole("button", { name: "More tags" }).click();
    await expect.poll(() => mock.tagsPage).toBe(2);
    await expect(panel.locator('#registry-tags option[value="v2"]')).toHaveCount(1);
    await panel.getByRole("button", { name: "Pull image", exact: true }).click();
    await expect(panel.getByText("Image pull completed.")).toBeVisible();
    expect(mock.transfers).toEqual([{ action: "pull", reference: "ghcr.io/team/app:v1" }]);
    await expect(panel.getByRole("cell", { name: /ghcr\.io\/team\/app:v1/ })).toBeVisible();
    await expect(panel.getByRole("button", { name: /push/i })).toHaveCount(0);
    await expect(panel.getByLabel("Destination image reference")).toHaveCount(0);
    await expect(panel).not.toContainText(/push/i);
    await panel.screenshot({ path: "test-results/pipe-registries-desktop.png" });
    const ghcrRow = panel.getByRole("row").filter({ has: page.getByRole("cell", { name: "ghcr.io", exact: true }) });
    await ghcrRow.getByRole("button", { name: "Remove" }).click();
    await expect(panel.getByText("Registry login removed.")).toBeVisible();
    await expect(ghcrRow).toHaveCount(0);
  });

  test("imports the pinned local digest and keeps source tags from changing the image", async ({ page }) => {
    const mock = await open(page, { local: true });
    await mock.panel.getByRole("button", { name: "Import as M2Image" }).click();
    await expect(mock.panel.getByText("M2Image import completed.")).toBeVisible();
    expect(mock.imports).toEqual([{ reference: `ghcr.io/team/app@${DIGEST}` }]);
  });

  test("shows pull failures and supports retry on a narrow screen", async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    const mock = await open(page, { local: true, failPull: true });
    await mock.panel.getByLabel("Repository", { exact: true }).fill("team/app");
    await mock.panel.getByRole("button", { name: "Pull image", exact: true }).click();
    await expect(mock.panel.getByRole("alert")).toContainText("registry rejected the credentials");
    await expect(mock.panel.getByRole("button", { name: "Pull image", exact: true })).toBeEnabled();
    await mock.panel.screenshot({ path: "test-results/pipe-registries-mobile.png" });
    expect(await mock.panel.locator(".registry-reference").first().evaluate(element => element.getBoundingClientRect().width)).toBeGreaterThan(200);
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth > innerWidth);
    expect(overflow).toBe(false);
  });

  test("restores a running transfer after reload without starting a duplicate", async ({ page }) => {
    const mock = await open(page, { running: true });
    await mock.panel.getByLabel("Repository", { exact: true }).fill("team/app");
    await mock.panel.getByRole("button", { name: "Pull image", exact: true }).click();
    await expect(mock.panel.getByText("Image operation in progress…")).toBeVisible();
    await page.reload();
    await expect(mock.panel.getByText("Image operation in progress…")).toBeVisible();
    await expect(mock.panel.getByRole("button", { name: "Pull image", exact: true })).toBeDisabled();
    expect(mock.transfers).toHaveLength(1);
  });
});
