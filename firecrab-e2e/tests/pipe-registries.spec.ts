import { expect, test, type Locator, type Page } from "@playwright/test";
import type { RegistryImage } from "../types/api/registries";

const HARBOR = "127.0.0.1:18080";
const ECR = "123456789012.dkr.ecr.ap-northeast-2.amazonaws.com";
const selection = (panel: Locator, name = "Selected repository") => panel.getByRole("group", { name, exact: true });
const lastPull = (panel: Locator) => panel.getByRole("group", { name: "Last pull", exact: true });
function localImage(reference: string, digestCharacter = "c"): RegistryImage {
  return { id: digestCharacter.repeat(64), reference, digest: `sha256:${digestCharacter.repeat(64)}`, architecture: "aarch64", sizeBytes: 1024, templateAlias: `test-sha256-${digestCharacter.repeat(12)}` };
}
async function open(page: Page, options: {
  saved?: "ghcr" | "harbor" | "dockerHub" | "both" | "ecr";
  ecrRegistry?: string;
  failPull?: boolean;
  pollErrorOnce?: boolean;
  running?: boolean;
  catalogError?: boolean;
  tagError?: boolean;
  slowTags?: boolean;
  empty?: boolean;
  locale?: "en" | "ko";
  images?: RegistryImage[];
  imagesError?: boolean;
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
    : options.saved === "ecr" ? [{ registry: options.ecrRegistry ?? "public.ecr.aws", username: "AWS", provider: "ecr" }]
    : options.saved === "harbor" ? [{ registry: HARBOR, username: "pipe-reader", provider: "harbor" }] : [];
  let polls = 0;
  let failTags = options.tagError;
  let failCatalog = options.catalogError;
  let images = options.images ?? [];
  let failImages = options.imagesError;
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
      if (registry === "public.ecr.aws" || registry.includes(".dkr.ecr.")) return route.fulfill({ json: { repositories: [], nextPage: null, listingSupported: false } });
      const repositories = options.empty ? [] : registry === HARBOR ? [`firecrab/${page === 1 ? "harbor-nginx" : "worker"}`]
        : [`${namespace}/${page === 1 ? "app" : "worker"}`];
      return route.fulfill({ json: { repositories, nextPage: page === 1 ? 2 : null, listingSupported: true } });
    }
    if (path.startsWith("/api/pipe/registries/")) {
      const registry = path.slice("/api/pipe/registries/".length);
      if (method === "DELETE") { connections = connections.filter(value => value.registry !== registry); return route.fulfill({ json: { deleted: true } }); }
      const body = route.request().postDataJSON(); logins.push({ registry, ...body });
      const saved = { registry, username: body.username, provider: body.provider ?? (registry === "docker.io" ? "dockerHub" : registry === "ghcr.io" ? "ghcr" : "ecr"), ...(body.accountName ? { accountName: body.accountName } : {}) };
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
      if (reference.startsWith(`${ECR}/ecr-nginx:`)) return route.fulfill({ json: { tags: ["latest", "1.30.6"], nextLast: null } });
      return route.fulfill({ json: url.searchParams.has("last") ? { tags: ["v2"], nextLast: null } : { tags: ["v1"], nextLast: "v1" } });
    }
    if (path === "/api/pipe/registry-transfers") {
      transfers.push(route.request().postDataJSON()); polls = 0;
      return route.fulfill({ status: 202, json: { alias: "b".repeat(64), status: "running", log: "registry transfer started" } });
    }
    if (path.startsWith("/api/pipe/registry-transfers/")) {
      polls++;
      if (options.pollErrorOnce && polls === 1) return route.fulfill({ status: 503, json: { error: { message: "Temporary registry outage" } } });
      const running = options.running || polls === 1;
      const reference = transfers.at(-1)?.reference;
      if (!running && !options.failPull && typeof reference === "string") {
        images = [...images.filter(image => image.reference !== reference), localImage(reference)];
      }
      return route.fulfill({ json: { alias: "b".repeat(64), status: running ? "running" : options.failPull ? "failed" : "succeeded", log: running ? "registry transfer started" : options.failPull ? "registry transfer failed: registry rejected the credentials" : "registry transfer succeeded" } });
    }
    if (path === "/api/pipe/registry-images") {
      localQueries.push(path);
      if (failImages) return route.fulfill({ status: 500, json: { error: { message: "Image status unavailable" } } });
      return route.fulfill({ json: images });
    }
    if (path.startsWith("/api/oci/")) localQueries.push(path);
    if (path === "/api/update") return route.fulfill({ json: { current: "0.3.1", latest: "0.3.1", updateAvailable: false } });
    return route.fulfill({ status: 404, json: { error: { message: "fixture route unavailable" } } });
  });
  await page.addInitScript(locale => localStorage.setItem("firecrab.locale", locale), options.locale ?? "en");
  await page.goto("/#/pipe/registry");
  const panel = page.getByRole("region", { name: "Pipe Registry", exact: true });
  await expect(panel.getByRole("button", { name: options.locale === "ko" ? "레지스트리 추가" : "Add registry" })).toBeEnabled();
  return { panel, logins, transfers, catalogs, tagQueries, localQueries, allowTags: () => { failTags = false; }, rejectTags: () => { failTags = true; }, allowCatalog: () => { failCatalog = false; }, allowImages: () => { failImages = false; } };
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
    const catalog = mock.panel.getByRole("table", { name: "Repositories", exact: true });
    await expect(catalog).toContainText("docker-user/app");
    await expect(catalog).not.toContainText("library/");
    await expect(catalog).not.toContainText("docker.io/");
    await expect(mock.panel.getByLabel("Source registry")).toHaveCount(0);
    await expect(mock.panel.getByRole("button", { name: "docker.io", exact: true })).toHaveAttribute("aria-pressed", "true");
    await expect(mock.panel.getByLabel("Tag", { exact: true })).toHaveValue("alpine");
    await expect(mock.panel.locator("input")).toHaveCount(0);
    await mock.panel.getByRole("button", { name: "More repositories", exact: true }).click();
    await expect(catalog).toContainText("docker-user/worker");
    await mock.panel.getByRole("button", { name: "Pull image", exact: true }).click();
    await expect(lastPull(mock.panel).getByText("Pulled", { exact: true })).toBeVisible();
    await expect(lastPull(mock.panel)).toContainText("docker.io/docker-user/app:alpine");
    const selected = selection(mock.panel);
    await expect(selected.getByText("Pulled", { exact: true })).toBeVisible();
    await expect(catalog.getByText("Pulled", { exact: true })).toBeVisible();
    await expect(mock.panel.getByLabel("Tag", { exact: true }).locator('option[value="alpine"]')).toHaveText("alpine");
    await expect(selected.getByText(/Pulled/)).toHaveCount(1);
    expect(mock.transfers).toEqual([{ action: "pull", reference: "docker.io/docker-user/app:alpine" }]);
    expect(mock.catalogs).toContainEqual({ registry: "docker.io", namespace: "", page: 2 });
    expect(mock.catalogs.every(query => query.registry === "docker.io" && query.namespace === "")).toBe(true);
    expect(mock.localQueries).toContain("/api/pipe/registry-images");
    expect(mock.localQueries.some(path => path.startsWith("/api/oci/"))).toBe(false);
    await page.reload();
    await expect(selected.getByText("Pulled", { exact: true })).toBeVisible();
    expect(mock.transfers).toHaveLength(1);
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
    await expect(mock.panel.getByRole("table", { name: "Repositories", exact: true })).toContainText("docker-user/app");
    const row = mock.panel.getByRole("row").filter({ has: page.getByRole("cell", { name: "docker.io", exact: true }) });
    await row.getByRole("button", { name: "docker.io actions" }).click();
    await page.getByRole("menuitem", { name: "Update token" }).click();
    await expect(mock.panel.getByRole("heading", { name: "Update token docker.io", exact: true })).toBeVisible();
    await expect(mock.panel.getByLabel("Provider")).toHaveCount(0);
    await expect(mock.panel.getByLabel("Personal access token (PAT)")).toBeFocused();
    await expect(mock.panel.getByLabel("Personal access token (PAT)")).toHaveValue("");
    // An open login form does not lock the repository browser.
    await expect(mock.panel.getByRole("button", { name: "Pull image", exact: true })).toBeEnabled();
    await mock.panel.getByLabel("Personal access token (PAT)").fill("rotated-token");
    await mock.panel.getByRole("button", { name: "Save login" }).click();
    await expect(mock.panel.getByText("Login saved.", { exact: true })).toBeVisible();
    expect(mock.logins[1].secret).toBe("rotated-token");
    await expect(mock.panel).not.toContainText("rotated-token");
    page.once("dialog", dialog => void dialog.dismiss());
    await row.getByRole("button", { name: "docker.io actions" }).click();
    await page.getByRole("menuitem", { name: "Delete", exact: true }).click();
    await expect(row).toHaveCount(1);
    let prompt = "";
    page.once("dialog", dialog => { prompt = dialog.message(); void dialog.accept(); });
    await row.getByRole("button", { name: "docker.io actions" }).click();
    await page.getByRole("menuitem", { name: "Delete", exact: true }).click();
    await expect(row).toHaveCount(0);
    expect(prompt).toBe("Delete the docker.io login? Pulled images are kept.");
    await expect(mock.panel.getByText("No registries.", { exact: true })).toBeVisible();
  });

  test("automatically loads a saved registry and the first repository's tags", async ({ page }) => {
    const mock = await open(page, { saved: "ghcr" });
    await expect(mock.panel.getByLabel("Tag", { exact: true })).toHaveValue("v1");
    expect(mock.tagQueries).toContain("ghcr.io/team/app:latest");
    await expect(mock.panel.getByRole("heading", { name: "Local OCI images" })).toHaveCount(0);
    await expect(mock.panel.getByRole("button", { name: "Import as M2Image" })).toHaveCount(0);
    await expect(mock.panel.locator("input")).toHaveCount(0);
  });

  test("automatically loads tags on repository selection and pulls a selected paged tag", async ({ page }) => {
    const mock = await open(page, { saved: "ghcr" });
    await mock.panel.getByRole("button", { name: "More repositories", exact: true }).click();
    const row = mock.panel.getByRole("table", { name: "Repositories", exact: true }).getByRole("row").filter({ hasText: "team/worker" });
    await row.getByRole("button", { name: "team/worker", exact: true }).click();
    await expect.poll(() => mock.tagQueries.includes("ghcr.io/team/worker:latest")).toBe(true);
    await mock.panel.getByRole("button", { name: "More tags" }).click();
    await mock.panel.getByLabel("Tag", { exact: true }).selectOption("v2");
    await mock.panel.getByRole("button", { name: "Pull image", exact: true }).click();
    await expect(lastPull(mock.panel).getByText("Pulled", { exact: true })).toBeVisible();
    expect(mock.transfers).toEqual([{ action: "pull", reference: "ghcr.io/team/worker:v2" }]);
  });

  test("connects Harbor, automatically lists its repository and tags, and pulls harbor-nginx", async ({ page }) => {
    const mock = await open(page);
    await mock.panel.getByRole("button", { name: "Add registry" }).click();
    await mock.panel.getByLabel("Provider").selectOption("harbor");
    await mock.panel.getByLabel("Registry hostname").fill(HARBOR);
    await mock.panel.getByLabel("Harbor username").fill("pipe-reader");
    await mock.panel.getByLabel("Harbor password or robot token").fill("harbor-secret");
    await expect(mock.panel.getByLabel("Harbor password or robot token")).toHaveAccessibleDescription("Needs list and pull access.");
    await mock.panel.getByRole("button", { name: "Save login" }).click();
    await expect(mock.panel.getByLabel("Tag", { exact: true })).toHaveValue("1.0.0");
    await expect(mock.panel.locator("input")).toHaveCount(0);
    expect(mock.logins[0]).toEqual({ registry: HARBOR, username: "pipe-reader", secret: "harbor-secret", provider: "harbor" });
    await mock.panel.screenshot({ path: "test-results/harbor-registry-desktop.png" });
    await mock.panel.getByRole("button", { name: "Pull image", exact: true }).click();
    await expect(lastPull(mock.panel).getByText("Pulled", { exact: true })).toBeVisible();
    expect(mock.transfers).toEqual([{ action: "pull", reference: `${HARBOR}/firecrab/harbor-nginx:1.0.0` }]);
  });

  test("keeps Harbor usable on Korean mobile screens without manual repository or tag fields", async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    const mock = await open(page, { saved: "harbor", locale: "ko" });
    await expect(mock.panel.getByLabel("태그", { exact: true })).toHaveValue("1.0.0");
    await expect(mock.panel.getByRole("button", { name: "이미지 가져오기", exact: true })).toBeEnabled();
    await expect(mock.panel.locator("input")).toHaveCount(0);
    await expect(mock.panel).not.toContainText("로컬 OCI 이미지");
    await mock.panel.screenshot({ path: "test-results/harbor-registry-mobile.png" });
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
  });

  test("discards a delayed tag lookup when the selected registry changes", async ({ page }) => {
    const mock = await open(page, { saved: "both", slowTags: true });
    await expect.poll(() => mock.tagQueries.includes("ghcr.io/team/app:latest")).toBe(true);
    await mock.panel.getByRole("button", { name: "docker.io", exact: true }).click();
    await expect(mock.panel.getByLabel("Tag", { exact: true })).toHaveValue("alpine");
    await page.waitForTimeout(450);
    await expect(mock.panel.getByLabel("Tag", { exact: true })).not.toContainText("v1");
    await mock.panel.getByRole("button", { name: "Pull image", exact: true }).click();
    await expect(lastPull(mock.panel).getByText("Pulled", { exact: true })).toBeVisible();
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
    await mock.panel.getByRole("button", { name: "Refresh", exact: true }).click();
    await expect(mock.panel.getByLabel("Tag", { exact: true })).toHaveValue("v1");
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
    await expect(mock.panel.getByText(/ECR login passwords cannot list all repositories/)).toBeVisible();
    await expect(mock.panel.getByText("No previously pulled repositories.", { exact: true })).toBeVisible();
    await expect(mock.panel.getByText("No repositories.", { exact: true })).toHaveCount(0);
    await expect(mock.panel.locator("input")).toHaveCount(0);
    await expect(mock.panel.getByRole("button", { name: "Pull image", exact: true })).toHaveCount(0);
  });

  test("shows the AWS account name while retaining AWS as the ECR authentication username", async ({ page }) => {
    const mock = await open(page, { saved: "ecr", ecrRegistry: ECR });
    const row = mock.panel.getByRole("row").filter({ has: page.getByRole("cell", { name: ECR, exact: true }) });
    await expect(row.getByRole("cell", { name: "123456789012", exact: true })).toBeVisible();
    await expect(row.getByRole("cell", { name: "AWS", exact: true })).toHaveCount(0);
    await row.getByRole("button", { name: `${ECR} actions`, exact: true }).click();
    await page.getByRole("menuitem", { name: "Update token", exact: true }).click();
    await mock.panel.getByLabel("AWS account name (optional)", { exact: true }).fill("production");
    await mock.panel.getByLabel("ECR login password", { exact: true }).fill("renewed-ecr-token");
    await mock.panel.getByRole("button", { name: "Save login", exact: true }).click();
    await expect(row.getByRole("cell", { name: "production", exact: true })).toHaveAttribute("title", "123456789012");
    expect(mock.logins).toEqual([{ registry: ECR, username: "AWS", secret: "renewed-ecr-token", accountName: "production" }]);
    await page.reload();
    await expect(row.getByRole("cell", { name: "production", exact: true })).toBeVisible();
    await row.getByRole("button", { name: `${ECR} actions`, exact: true }).click();
    await page.getByRole("menuitem", { name: "Update token", exact: true }).click();
    await expect(mock.panel.getByLabel("AWS account name (optional)", { exact: true })).toHaveValue("production");
    await expect(mock.panel.getByLabel("ECR login password", { exact: true })).toHaveValue("");
  });

  test("shows retained ECR repositories and fetches their remote tags for pulling", async ({ page }) => {
    const mock = await open(page, { saved: "ecr", ecrRegistry: ECR, images: [
      localImage(`${ECR}/ecr-nginx:1.30.6`),
      localImage(`${ECR}/ecr-nginx@sha256:${"a".repeat(64)}`, "a"),
      localImage(`${ECR}/team/worker:v1`, "b"),
      localImage("999999999999.dkr.ecr.ap-northeast-2.amazonaws.com/other:v1", "d"),
      localImage("ghcr.io/team/app:v1", "e"),
    ] });
    const catalog = mock.panel.getByRole("table", { name: "Repositories", exact: true });
    await expect(catalog.getByRole("button")).toHaveText(["ecr-nginx", "team/worker"]);
    await expect(catalog.getByRole("button", { name: "ecr-nginx", exact: true })).toHaveAttribute("aria-pressed", "true");
    const tag = mock.panel.getByLabel("Tag", { exact: true });
    await expect(tag.locator("option")).toHaveText(["latest", "1.30.6"]);
    await expect(selection(mock.panel).getByText("Not pulled", { exact: true })).toBeVisible();
    await tag.selectOption("1.30.6");
    await expect(selection(mock.panel).getByText("Pulled", { exact: true })).toBeVisible();
    await mock.panel.getByRole("button", { name: "Pull image", exact: true }).click();
    await expect(lastPull(mock.panel).getByText("Pulled", { exact: true })).toBeVisible();
    expect(mock.transfers).toEqual([{ action: "pull", reference: `${ECR}/ecr-nginx:1.30.6` }]);
    expect(mock.tagQueries).toContain(`${ECR}/ecr-nginx:latest`);
    await page.reload();
    await expect(catalog.getByRole("button", { name: "ecr-nginx", exact: true })).toBeVisible();
    await expect(tag.locator("option")).toHaveText(["latest", "1.30.6"]);
  });

  test("recovers ECR repositories after a local inventory lookup failure", async ({ page }) => {
    const mock = await open(page, { saved: "ecr", ecrRegistry: ECR, imagesError: true, images: [localImage(`${ECR}/ecr-nginx:1.30.6`)] });
    await expect(mock.panel.getByRole("alert")).toContainText("Local image status unavailable");
    await expect(mock.panel.getByText("No repositories.", { exact: true })).toHaveCount(0);
    await expect(mock.panel.getByText("No previously pulled repositories.", { exact: true })).toHaveCount(0);
    mock.allowImages();
    await mock.panel.getByRole("button", { name: "Refresh", exact: true }).click();
    await expect(mock.panel.getByRole("button", { name: "ecr-nginx", exact: true })).toBeVisible();
    await expect(mock.panel.getByLabel("Tag", { exact: true })).toHaveValue("latest");
    await expect(mock.panel.getByRole("alert")).toHaveCount(0);
  });

  test("retains the selection after a failed pull for retry", async ({ page }) => {
    const mock = await open(page, { saved: "harbor", failPull: true });
    const selected = selection(mock.panel);
    await expect(selected.getByText("Not pulled", { exact: true })).toBeVisible();
    await expect(mock.panel.getByRole("button", { name: "Pull image", exact: true })).toBeEnabled();
    await mock.panel.getByRole("button", { name: "Pull image", exact: true }).click();
    await expect(mock.panel.getByRole("alert")).toContainText("registry rejected the credentials");
    await expect(mock.panel.getByRole("button", { name: "Pull image", exact: true })).toBeEnabled();
    await expect(selected.getByText("Not pulled", { exact: true })).toBeVisible();
    await expect(mock.panel.getByRole("table", { name: "Repositories", exact: true }).getByText("Pulled", { exact: true })).toHaveCount(0);
  });

  test("keeps a newly pulled tag selected and restores its status after reload", async ({ page }) => {
    const mock = await open(page, { saved: "ghcr" });
    await mock.panel.getByRole("button", { name: "More tags", exact: true }).click();
    const tag = mock.panel.getByLabel("Tag", { exact: true });
    const selected = selection(mock.panel);
    await tag.selectOption("v2");
    await mock.panel.getByRole("button", { name: "Pull image", exact: true }).click();
    await expect(selected.getByText("Pulled", { exact: true })).toBeVisible();
    await expect(tag).toHaveValue("v2");
    await page.reload();
    await expect(tag).toHaveValue("v1");
    await expect(selected.getByText("Not pulled", { exact: true })).toBeVisible();
    await mock.panel.getByRole("button", { name: "More tags", exact: true }).click();
    await expect(tag.locator('option[value="v2"]')).toHaveText("v2");
    await tag.selectOption("v2");
    await expect(selected.getByText("Pulled", { exact: true })).toBeVisible();
    await expect(selected.getByText(/Pulled/)).toHaveCount(1);
    expect(mock.transfers).toEqual([{ action: "pull", reference: "ghcr.io/team/app:v2" }]);
  });

  test("does not copy pulled status to another tag, repository, or registry", async ({ page }) => {
    const mock = await open(page, { saved: "both", images: [localImage("ghcr.io/team/app:v1", "a")] });
    const selected = selection(mock.panel);
    await expect(selected.getByText("Pulled", { exact: true })).toBeVisible();
    await mock.panel.getByRole("button", { name: "More tags", exact: true }).click();
    await mock.panel.getByLabel("Tag", { exact: true }).selectOption("v2");
    await expect(selected.getByText("Not pulled", { exact: true })).toBeVisible();
    await mock.panel.getByRole("button", { name: "More repositories", exact: true }).click();
    await mock.panel.getByRole("button", { name: "team/worker", exact: true }).click();
    await expect(selected.getByText("Not pulled", { exact: true })).toBeVisible();
    await mock.panel.getByRole("button", { name: "docker.io", exact: true }).click();
    await expect(selected.getByText("Not pulled", { exact: true })).toBeVisible();
    expect(mock.transfers).toHaveLength(0);
  });

  test("shows unavailable status when local lookup fails and recovers on refresh", async ({ page }) => {
    const mock = await open(page, { saved: "harbor", images: [localImage(`${HARBOR}/firecrab/harbor-nginx:1.0.0`)], imagesError: true });
    const selected = selection(mock.panel);
    await expect(selected.getByText("Status unavailable", { exact: true })).toBeVisible();
    await expect(selected.getByText("Not pulled", { exact: true })).toHaveCount(0);
    await expect(mock.panel.getByRole("button", { name: "Pull image", exact: true })).toBeEnabled();
    mock.allowImages();
    await mock.panel.getByRole("button", { name: "Refresh", exact: true }).click();
    await expect(selected.getByText("Pulled", { exact: true })).toBeVisible();
    await expect(mock.panel.getByRole("alert")).toHaveCount(0);
  });

  test("uses row selection and keyboard-accessible menus for registry actions", async ({ page }) => {
    const mock = await open(page, { saved: "both" });
    await expect(mock.panel.getByLabel("Source registry")).toHaveCount(0);
    await expect(mock.panel.getByRole("button", { name: "Update token", exact: true })).toHaveCount(0);
    await expect(mock.panel.getByRole("button", { name: "Delete", exact: true })).toHaveCount(0);
    const trigger = mock.panel.getByRole("button", { name: "ghcr.io actions", exact: true });
    await trigger.focus();
    await page.keyboard.press("Enter");
    const menu = page.getByRole("menu", { name: "ghcr.io actions", exact: true });
    // Selection belongs to the row, so the menu holds only actions that change the login.
    await expect(menu.getByRole("menuitem")).toHaveText(["Update token", "Delete"]);
    await expect(menu.getByRole("menuitem", { name: "Update token", exact: true })).toBeFocused();
    await page.keyboard.press("ArrowDown");
    await expect(menu.getByRole("menuitem", { name: "Delete", exact: true })).toBeFocused();
    await page.keyboard.press("Escape");
    await expect(page.getByRole("menu")).toHaveCount(0);
    await expect(trigger).toBeFocused();
    await expect(mock.panel.getByRole("button", { name: "ghcr.io", exact: true })).toHaveAttribute("aria-pressed", "true");
    const docker = mock.panel.getByRole("button", { name: "docker.io", exact: true });
    await docker.focus();
    await page.keyboard.press("Enter");
    await expect(docker).toHaveAttribute("aria-pressed", "true");
    await expect(mock.panel.getByLabel("Tag", { exact: true })).toHaveValue("alpine");
    await mock.panel.getByRole("row").filter({ hasText: "GHCR" }).getByRole("cell", { name: "team", exact: true }).click();
    await expect(mock.panel.getByRole("button", { name: "ghcr.io", exact: true })).toHaveAttribute("aria-pressed", "true");
    await expect(mock.panel.getByLabel("Tag", { exact: true })).toHaveValue("v1");
  });

  test("keeps the registry action menu inside a narrow viewport", async ({ page }) => {
    await page.setViewportSize({ width: 320, height: 640 });
    const mock = await open(page, { saved: "harbor", locale: "ko" });
    await mock.panel.getByRole("button", { name: `${HARBOR} 작업`, exact: true }).click();
    const menu = page.getByRole("menu", { name: `${HARBOR} 작업`, exact: true });
    await expect(menu.getByRole("menuitem", { name: "토큰 갱신", exact: true })).toBeVisible();
    const bounds = await menu.boundingBox();
    expect(bounds).not.toBeNull();
    expect(bounds!.x).toBeGreaterThanOrEqual(0);
    expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(320);
    expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
  });

  test("blocks pulling after tag pagination fails and recovers after retry", async ({ page }) => {
    const mock = await open(page, { saved: "ghcr" });
    await expect(mock.panel.getByLabel("Tag", { exact: true })).toHaveValue("v1");
    mock.rejectTags();
    await mock.panel.getByRole("button", { name: "More tags", exact: true }).click();
    await expect(mock.panel.getByRole("alert")).toContainText("Repository lookup failed");
    await expect(mock.panel.getByRole("button", { name: "Pull image", exact: true })).toBeDisabled();
    expect(mock.transfers).toHaveLength(0);
    mock.allowTags();
    await mock.panel.getByRole("button", { name: "Retry tags", exact: true }).click();
    await expect(mock.panel.getByLabel("Tag", { exact: true })).toHaveValue("v1");
    await expect(mock.panel.getByRole("button", { name: "Pull image", exact: true })).toBeEnabled();
  });

  for (const rejectLogin of [false, true]) test(rejectLogin
    ? "preserves a rejected login after a transfer poll recovers"
    : "clears a transient transfer error after polling succeeds", async ({ page }) => {
    const mock = await open(page, { saved: "harbor", pollErrorOnce: true });
    await mock.panel.getByRole("button", { name: "Pull image", exact: true }).click();
    await expect(mock.panel.getByRole("alert")).toContainText("Temporary registry outage");
    if (rejectLogin) {
      await page.route("**/api/pipe/registries/ghcr.io", route => route.fulfill({ status: 401, json: { error: { message: "The registry token was rejected" } } }));
      await mock.panel.getByRole("button", { name: "Add registry", exact: true }).click();
      await mock.panel.getByLabel("GitHub username", { exact: true }).fill("team");
      await mock.panel.getByLabel("Personal access token (classic)", { exact: true }).fill("rejected-token");
      await mock.panel.getByRole("button", { name: "Save login", exact: true }).click();
      await expect(mock.panel.getByRole("alert")).toContainText("The registry token was rejected");
    }
    await expect(lastPull(mock.panel).getByText("Pulled", { exact: true })).toBeVisible();
    if (rejectLogin) await expect(mock.panel.getByRole("alert")).toContainText("The registry token was rejected");
    else await expect(mock.panel.getByRole("alert")).toHaveCount(0);
    expect(mock.transfers).toHaveLength(1);
  });

  test("restores a running pull after reload without starting a duplicate", async ({ page }) => {
    const mock = await open(page, { saved: "harbor", running: true });
    await expect(mock.panel.getByRole("button", { name: "Pull image", exact: true })).toBeEnabled();
    await mock.panel.getByRole("button", { name: "Pull image", exact: true }).click();
    await expect(lastPull(mock.panel).getByText("Pulling…", { exact: true })).toBeVisible();
    await expect(selection(mock.panel).getByText("Not pulled", { exact: true })).toBeVisible();
    await page.reload();
    await expect(lastPull(mock.panel).getByText("Pulling…", { exact: true })).toBeVisible();
    await expect(lastPull(mock.panel)).toContainText(`${HARBOR}/firecrab/harbor-nginx:1.0.0`);
    await expect(mock.panel.getByRole("button", { name: "Pull image", exact: true })).toBeDisabled();
    // A running pull blocks only another pull; the rest of the page stays usable.
    await expect(mock.panel.getByRole("button", { name: "Add registry", exact: true })).toBeEnabled();
    await expect(mock.panel.getByRole("button", { name: "Refresh", exact: true })).toBeEnabled();
    await expect(mock.panel.getByLabel("Tag", { exact: true })).toBeEnabled();
    expect(mock.transfers).toHaveLength(1);
  });
});
