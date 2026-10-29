import { expect, test, type Locator, type Page } from "@playwright/test";
import type { RegistryImage } from "../types/api/registries";

const HARBOR = "127.0.0.1:18080";
const ECR = "123456789012.dkr.ecr.ap-northeast-2.amazonaws.com";
const selection = (panel: Locator, name = "Selected repository") =>
  panel.getByRole("group", { name, exact: true });
const lastPull = (panel: Locator) =>
  panel.getByRole("group", { name: "Last pull", exact: true });
function localImage(reference: string, digestCharacter = "c"): RegistryImage {
  return {
    id: digestCharacter.repeat(64),
    reference,
    digest: `sha256:${digestCharacter.repeat(64)}`,
    architecture: "aarch64",
    sizeBytes: 1024,
    templateAlias: `test-sha256-${digestCharacter.repeat(12)}`,
  };
}
async function open(
  page: Page,
  options: {
    saved?: "ghcr" | "harbor" | "dockerHub" | "both" | "ecr" | "twoEcr";
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
  } = {},
) {
  const logins: Record<string, unknown>[] = [];
  const transfers: Record<string, unknown>[] = [];
  const catalogs: { registry: string; namespace: string; page: number }[] = [];
  const tagQueries: string[] = [];
  const localQueries: string[] = [];
  const github = { registry: "ghcr.io", username: "team", provider: "ghcr" };
  const docker = {
    registry: "docker.io",
    username: "docker-user",
    provider: "dockerHub",
  };
  let connections =
    options.saved === "ghcr"
      ? [github]
      : options.saved === "dockerHub"
        ? [docker]
        : options.saved === "both"
          ? [github, docker]
          : options.saved === "ecr"
            ? [
                {
                  registry: options.ecrRegistry ?? "public.ecr.aws",
                  username: "AWS",
                  provider: "ecr",
                },
              ]
            : options.saved === "harbor"
              ? [
                  {
                    registry: HARBOR,
                    username: "pipe-reader",
                    provider: "harbor",
                  },
                ]
              : [];
  if (options.saved === "twoEcr") connections = [
    { registry: "public.ecr.aws", username: "AWS", provider: "ecr" },
    { registry: ECR, username: "AWS", provider: "ecr" },
  ];
  let polls = 0;
  let failTags = options.tagError;
  let failCatalog = options.catalogError;
  let images = options.images ?? [];
  let failImages = options.imagesError;
  await page.route(/^https?:\/\/[^/]+\/api\//, async (route) => {
    const url = new URL(route.request().url());
    const path = decodeURIComponent(url.pathname);
    const method = route.request().method();
    if (path === "/api/vms") return route.fulfill({ json: [] });
    if (path === "/api/pipe")
      return route.fulfill({
        json: {
          config: null,
          units: [],
          nginxAvailable: true,
          cloudflaredAvailable: true,
        },
      });
    if (path === "/api/pipe/registries")
      return route.fulfill({ json: connections });
    if (path.endsWith("/repositories")) {
      const registry = path.split("/")[4];
      const namespace =
        connections.find((connection) => connection.registry === registry)
          ?.username ?? "";
      const page = Number(url.searchParams.get("page"));
      catalogs.push({
        registry,
        namespace: url.searchParams.get("namespace") ?? "",
        page,
      });
      if (failCatalog) {
        return route.fulfill({
          status: 403,
          json: {
            error: {
              message: "Catalog access denied. Check token permissions.",
            },
          },
        });
      }
      if (registry === "public.ecr.aws" || registry.includes(".dkr.ecr."))
        return route.fulfill({
          json: { repositories: [], nextPage: null, listingSupported: false },
        });
      const repositories = options.empty
        ? []
        : registry === HARBOR
          ? [`firecrab/${page === 1 ? "harbor-nginx" : "worker"}`]
          : [`${namespace}/${page === 1 ? "app" : "worker"}`];
      return route.fulfill({
        json: {
          repositories,
          nextPage: page === 1 ? 2 : null,
          listingSupported: true,
        },
      });
    }
    if (path.startsWith("/api/pipe/registries/")) {
      const registry = path.slice("/api/pipe/registries/".length);
      if (method === "DELETE") {
        connections = connections.filter(
          (value) => value.registry !== registry,
        );
        return route.fulfill({ json: { deleted: true } });
      }
      const body = route.request().postDataJSON();
      logins.push({ registry, ...body });
      const saved = {
        registry,
        username: body.username,
        provider:
          body.provider ??
          (registry === "docker.io"
            ? "dockerHub"
            : registry === "ghcr.io"
              ? "ghcr"
              : "ecr"),
        ...(body.accountName ? { accountName: body.accountName } : {}),
      };
      connections = [
        ...connections.filter((value) => value.registry !== registry),
        saved,
      ];
      return route.fulfill({ json: saved });
    }
    if (path === "/api/pipe/registry-tags") {
      const reference = url.searchParams.get("reference") ?? "";
      tagQueries.push(reference);
      if (options.slowTags && reference.startsWith("ghcr.io"))
        await new Promise((resolve) => setTimeout(resolve, 350));
      if (failTags) {
        return route.fulfill({
          status: 404,
          json: { error: { message: "Repository lookup failed" } },
        });
      }
      if (reference.startsWith("docker.io/docker-user/"))
        return route.fulfill({ json: { tags: ["alpine"], nextLast: null } });
      if (reference.startsWith(HARBOR))
        return route.fulfill({
          json: { tags: ["1.0.0", "latest"], nextLast: null },
        });
      if (reference.startsWith(`${ECR}/ecr-nginx:`))
        return route.fulfill({
          json: { tags: ["latest", "1.30.6"], nextLast: null },
        });
      return route.fulfill({
        json: url.searchParams.has("last")
          ? { tags: ["v2"], nextLast: null }
          : { tags: ["v1"], nextLast: "v1" },
      });
    }
    if (path === "/api/pipe/registry-transfers") {
      transfers.push(route.request().postDataJSON());
      polls = 0;
      return route.fulfill({
        status: 202,
        json: {
          alias: "b".repeat(64),
          status: "running",
          log: "registry transfer started",
        },
      });
    }
    if (path.startsWith("/api/pipe/registry-transfers/")) {
      polls++;
      if (options.pollErrorOnce && polls === 1)
        return route.fulfill({
          status: 503,
          json: { error: { message: "Temporary registry outage" } },
        });
      const running = options.running || polls === 1;
      const reference = transfers.at(-1)?.reference;
      if (!running && !options.failPull && typeof reference === "string") {
        images = [
          ...images.filter((image) => image.reference !== reference),
          localImage(reference),
        ];
      }
      return route.fulfill({
        json: {
          alias: "b".repeat(64),
          status: running
            ? "running"
            : options.failPull
              ? "failed"
              : "succeeded",
          log: running
            ? "registry transfer started"
            : options.failPull
              ? "registry transfer failed: registry rejected the credentials"
              : "registry transfer succeeded",
        },
      });
    }
    if (path === "/api/pipe/registry-images") {
      localQueries.push(path);
      if (failImages)
        return route.fulfill({
          status: 500,
          json: { error: { message: "Image status unavailable" } },
        });
      return route.fulfill({ json: images });
    }
    if (path.startsWith("/api/oci/")) localQueries.push(path);
    if (path === "/api/update")
      return route.fulfill({
        json: { current: "0.3.1", latest: "0.3.1", updateAvailable: false },
      });
    return route.fulfill({
      status: 404,
      json: { error: { message: "fixture route unavailable" } },
    });
  });
  await page.addInitScript(
    (locale) => localStorage.setItem("firecrab.locale", locale),
    options.locale ?? "en",
  );
  await page.goto("/#/pipe/registry");
  const panel = page.locator("main");
  return {
    panel,
    logins,
    transfers,
    catalogs,
    tagQueries,
    localQueries,
    allowTags: () => {
      failTags = false;
    },
    rejectTags: () => {
      failTags = true;
    },
    allowCatalog: () => {
      failCatalog = false;
    },
    allowImages: () => {
      failImages = false;
    },
  };
}

async function chooseRegistry(page: Page, host: string) {
  await page
    .getByRole("row")
    .filter({ has: page.getByRole("cell", { name: host, exact: true }) })
    .getByRole("button", { name: "Details", exact: true })
    .click();
  await page.getByRole("button", { name: "Repositories", exact: true }).click();
}
test.describe("Pipe registry automatic discovery @dashboard", () => {
  test("disconnects the selected host when multiple connections use ECR", async ({ page }) => {
    await open(page, { saved: "twoEcr" });
    const row = page.locator("#content").getByRole("row").filter({ has: page.getByRole("cell", { name: ECR, exact: true }) });
    await row.getByRole("button", { name: "Details", exact: true }).click();
    const detail = page.locator("#detail-panel");
    await expect(detail.getByRole("heading", { name: ECR, exact: true })).toBeVisible();
    const removed = page.waitForRequest(request => request.method() === "DELETE" && decodeURIComponent(new URL(request.url()).pathname) === `/api/pipe/registries/${ECR}`);
    page.once("dialog", dialog => dialog.accept());
    await detail.getByRole("button", { name: "Disconnect", exact: true }).click();
    await removed;
    await expect(row).toHaveCount(0);
    await expect(page.locator("#content").getByRole("cell", { name: /^public\.ecr\.aws(\s|$)/ })).toBeVisible();
  });
  test("does not query catalogs or tags without a saved login", async ({
    page,
  }) => {
    const m = await open(page);
    await expect(page.locator("#content")).toHaveAttribute(
      "data-loaded",
      "true",
    );
    expect(m.catalogs).toEqual([]);
    expect(m.tagQueries).toEqual([]);
  });
  for (const [saved, repository, tag] of [
    ["ghcr", "team/app", "v1"],
    ["dockerHub", "docker-user/app", "alpine"],
    ["harbor", "firecrab/harbor-nginx", "1.0.0"],
  ] as const) {
    test(`discovers ${saved}, paginates repositories and pulls a selected tag`, async ({
      page,
    }) => {
      const m = await open(page, { saved });
      await expect(page.locator("#registry-repository")).toHaveValue(
        repository,
      );
      await expect(page.locator("#registry-tag")).toHaveValue(tag);
      await page
        .getByRole("button", { name: "More repositories", exact: true })
        .click();
      await expect(page.locator("#registry-repository option")).toHaveCount(2);
      await page.getByRole("button", { name: "Pull", exact: true }).click();
      await expect.poll(() => m.transfers.length).toBe(1);
      expect(m.transfers[0]).toEqual({
        action: "pull",
        reference: `${saved === "ghcr" ? "ghcr.io" : saved === "dockerHub" ? "docker.io" : HARBOR}/${repository}:${tag}`,
      });
    });
  }
  test("paginates tags without replacing the selected repository", async ({
    page,
  }) => {
    await open(page, { saved: "ghcr" });
    await expect(page.locator("#registry-tag")).toHaveValue("v1");
    await page.getByRole("button", { name: "More tags", exact: true }).click();
    await expect(page.locator("#registry-tag option")).toHaveCount(2);
    await page.locator("#registry-tag").selectOption("v2");
    await expect(page.locator("#registry-repository")).toHaveValue("team/app");
  });
  test("keeps an empty Docker Hub catalog empty", async ({ page }) => {
    const m = await open(page, { saved: "dockerHub", empty: true });
    await expect(page.locator("#workflow-root")).toContainText(
      "No repositories found.",
    );
    expect(m.tagQueries).toEqual([]);
    await expect(
      page.getByRole("button", { name: "Pull", exact: true }),
    ).toHaveCount(0);
  });
  test("shows catalog and tag failures and retries each lookup", async ({
    page,
  }) => {
    const m = await open(page, {
      saved: "ghcr",
      catalogError: true,
      tagError: true,
    });
    await expect(page.locator("#workflow-root [role=alert]")).toContainText(
      "Catalog access denied",
    );
    m.allowCatalog();
    await page
      .locator("#workflow-root")
      .getByRole("button", { name: "Retry", exact: true })
      .click();
    await expect(page.locator("#workflow-root [role=alert]")).toContainText(
      "Repository lookup failed",
    );
    await expect(
      page.getByRole("button", { name: "Pull", exact: true }),
    ).toBeDisabled();
    m.allowTags();
    await page
      .locator("#workflow-root")
      .getByRole("button", { name: "Retry", exact: true })
      .click();
    await expect(page.locator("#registry-tag")).toHaveValue("v1");
  });
  test("rejects delayed tag responses from the previously selected registry", async ({
    page,
  }) => {
    await open(page, { saved: "both", slowTags: true });
    await chooseRegistry(page, "docker.io");
    await expect(page.locator("#registry-tag")).toHaveValue("alpine");
    await page.waitForTimeout(400);
    await expect(page.locator("#registry-tag option")).toHaveText(["alpine"]);
  });
  test("shows the ECR discovery limitation without inventing repositories", async ({
    page,
  }) => {
    await open(page, { saved: "ecr" });
    await expect(page.locator("#workflow-root [role=alert]")).toContainText(
      "not supported",
    );
    await expect(
      page.getByRole("button", { name: "Pull", exact: true }),
    ).toHaveCount(0);
  });
  test("recovers ECR repositories from inventory and queries live tags", async ({
    page,
  }) => {
    const m = await open(page, {
      saved: "ecr",
      ecrRegistry: ECR,
      images: [localImage(`${ECR}/ecr-nginx:latest`)],
    });
    await expect(page.locator("#registry-repository")).toHaveValue("ecr-nginx");
    await expect(page.locator("#registry-tag option")).toHaveCount(2);
    await page.locator("#registry-tag").selectOption("1.30.6");
    await page.getByRole("button", { name: "Pull", exact: true }).click();
    await expect.poll(() => m.transfers.length).toBe(1);
    expect(m.transfers[0].reference).toBe(`${ECR}/ecr-nginx:1.30.6`);
  });
  test("saves and rotates credentials without persisting their value", async ({
    page,
  }) => {
    const m = await open(page);
    await page
      .getByRole("button", { name: "Connect registry", exact: true })
      .click();
    await page.getByLabel("Registry host", { exact: true }).fill("docker.io");
    await page.getByLabel("Username", { exact: true }).fill("docker-user");
    await page
      .getByLabel("Password / token", { exact: true })
      .fill("first-private-token");
    await page.getByRole("button", { name: "Save", exact: true }).click();
    await expect(page.locator("#editor")).toHaveCount(0);
    await chooseRegistry(page, "docker.io");
    await page
      .getByRole("button", { name: "Update token", exact: true })
      .click();
    await expect(
      page.getByLabel("Password / token", { exact: true }),
    ).toHaveValue("");
    await page
      .getByLabel("Password / token", { exact: true })
      .fill("rotated-private-token");
    await page.getByRole("button", { name: "Save", exact: true }).click();
    await expect.poll(() => m.logins.length).toBe(2);
    expect(
      await page.evaluate(
        () => JSON.stringify(localStorage) + JSON.stringify(sessionStorage),
      ),
    ).not.toContain("private-token");
  });
  test("keeps failed transfers visible and preserves selection for retry", async ({
    page,
  }) => {
    const m = await open(page, { saved: "ghcr", failPull: true });
    await expect(page.locator("#registry-tag")).toHaveValue("v1");
    await page.getByRole("button", { name: "Pull", exact: true }).click();
    await expect(page.locator("#content")).toContainText(
      "registry transfer failed",
    );
    await expect(page.locator("#registry-tag")).toHaveValue("v1");
    await page.getByRole("button", { name: "Pull", exact: true }).click();
    await expect.poll(() => m.transfers.length).toBe(2);
  });
  test("restores a running transfer after reload without posting twice", async ({
    page,
  }) => {
    const m = await open(page, { saved: "ghcr", running: true });
    await expect(page.locator("#registry-tag")).toHaveValue("v1");
    await page.getByRole("button", { name: "Pull", exact: true }).click();
    await expect(
      page.getByRole("heading", { name: "Progress", exact: true }),
    ).toBeVisible();
    await page.reload();
    await expect(
      page.getByRole("heading", { name: "Progress", exact: true }),
    ).toBeVisible();
    expect(m.transfers).toHaveLength(1);
  });
  test("reports unavailable local status and fits Korean mobile layouts", async ({
    page,
  }) => {
    const m = await open(page, {
      saved: "harbor",
      imagesError: true,
      locale: "ko",
    });
    await expect(page.locator("#workflow-root")).toContainText(
      "상태 확인 불가",
    );
    m.allowImages();
    await page.waitForResponse(
      (r) =>
        new URL(r.url()).pathname === "/api/pipe/registries" &&
        r.request().method() === "GET",
    );
    await page.setViewportSize({ width: 390, height: 844 });
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    ).toBe(true);
  });
});
