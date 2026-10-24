import { expect, test, type Page } from "@playwright/test";
import type { AuthConnection, AuthLogin } from "../types/api/pipeAuth";

const github: AuthConnection = {
  provider: "github",
  account: "octocat",
  registry: "ghcr.io",
  region: null,
  services: ["registry"],
};
const aws: AuthConnection = {
  provider: "aws",
  account: "123456789012",
  registry: "123456789012.dkr.ecr.ap-northeast-2.amazonaws.com",
  region: "ap-northeast-2",
  services: ["registry"],
};
async function open(
  page: Page,
  options: {
    connections?: AuthConnection[];
    reject?: boolean;
    loadFailure?: boolean;
    locale?: "ko";
    overview?: boolean;
  } = {},
) {
  let connections = options.connections ?? [];
  let failLoad = options.loadFailure;
  const writes: AuthLogin[] = [];
  const deletes: string[] = [];
  const catalogs: string[] = [];
  const transfers: string[] = [];
  await page.addInitScript(
    (locale) => localStorage.setItem("firecrab.locale", locale),
    options.locale ?? "en",
  );
  await page.route(/^https?:\/\/[^/]+\/api\//, async (route) => {
    const url = new URL(route.request().url());
    const path = url.pathname;
    if (path === "/api/pipe/auth") {
      if (failLoad)
        return route.fulfill({
          status: 503,
          json: { error: { message: "Connections unavailable" } },
        });
      return route.fulfill({ json: connections });
    }
    if (path.startsWith("/api/pipe/auth/")) {
      const provider = path.split("/").at(-1)!;
      if (route.request().method() === "DELETE") {
        deletes.push(provider);
        connections = connections.filter(
          (connection) => connection.provider !== provider,
        );
        return route.fulfill({ json: { deleted: true } });
      }
      const login = route.request().postDataJSON() as AuthLogin;
      writes.push(login);
      if (options.reject)
        return route.fulfill({
          status: 400,
          json: {
            error: {
              message:
                "GitHub rejected the token. Check its validity and permissions.",
            },
          },
        });
      const connection = provider === "github" ? github : aws;
      connections = [
        ...connections.filter((row) => row.provider !== provider),
        connection,
      ];
      return route.fulfill({ json: connection });
    }
    if (path === "/api/pipe/registries")
      return route.fulfill({
        json: connections.map((connection) => ({
          registry: connection.registry,
          username: connection.provider === "aws" ? "AWS" : connection.account,
          provider: connection.provider === "aws" ? "ecr" : "ghcr",
          authProvider: connection.provider,
        })),
      });
    if (path.endsWith("/repositories")) {
      catalogs.push(url.search);
      const isAws = path.includes(aws.registry);
      return route.fulfill({
        json: isAws
          ? {
              repositories: [
                url.searchParams.has("cursor") ? "team/worker" : "team/app",
              ],
              nextPage: url.searchParams.has("cursor") ? null : 2,
              nextCursor: url.searchParams.has("cursor")
                ? null
                : "opaque+/=cursor",
              listingSupported: true,
            }
          : { repositories: [], nextPage: null, listingSupported: true },
      });
    }
    if (path === "/api/pipe/registry-tags")
      return route.fulfill({ json: { tags: ["v1"], nextLast: null } });
    if (path === "/api/pipe/registry-transfers") {
      transfers.push(route.request().postDataJSON().reference);
      return route.fulfill({
        status: 202,
        json: {
          alias: "a".repeat(64),
          status: "running",
          log: "registry transfer started",
        },
      });
    }
    if (path.startsWith("/api/pipe/registry-transfers/"))
      return route.fulfill({
        json: {
          alias: "a".repeat(64),
          status: "succeeded",
          log: "registry transfer succeeded",
        },
      });
    if (path === "/api/pipe/registry-images" || path === "/api/vms")
      return route.fulfill({ json: [] });
    if (path === "/api/pipe")
      return route.fulfill({
        json: {
          config: null,
          units: [],
          nginxAvailable: true,
          cloudflaredAvailable: true,
        },
      });
    return route.fulfill({
      status: 404,
      json: { error: { message: "Fixture route unavailable" } },
    });
  });
  await page.goto(options.overview ? "/#/pipe" : "/#/pipe/auth");
  return {
    writes,
    deletes,
    catalogs,
    transfers,
    recover: () => {
      failLoad = false;
    },
    get connections() {
      return connections;
    },
    panel: page.locator("main"),
  };
}

// Assert API behavior and credential lifetime independently of the previous React layout.
test.describe("Pipe Auth @dashboard", () => {
  test("discovers connected providers without loading credentials", async ({
    page,
  }) => {
    await open(page, { connections: [github, aws] });
    await expect(
      page.getByRole("cell", { name: "octocat", exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("cell", { name: aws.registry!, exact: true }),
    ).toBeVisible();
    await expect(page.locator("input[type=password]")).toHaveCount(0);
  });
  test("verifies GitHub, clears its token, and exposes GHCR", async ({
    page,
  }) => {
    const mock = await open(page);
    await page
      .getByRole("button", { name: "Connect GitHub", exact: true })
      .click();
    await page.getByLabel("Token", { exact: true }).fill("private-token");
    await page.getByRole("button", { name: "Save", exact: true }).click();
    await expect(page.locator("#editor")).toHaveCount(0);
    expect(mock.writes).toEqual([
      { provider: "github", token: "private-token" },
    ]);
    await expect(
      page.getByRole("cell", { name: "octocat", exact: true }),
    ).toBeVisible();
    expect(
      await page.evaluate(
        () => JSON.stringify(localStorage) + JSON.stringify(sessionStorage),
      ),
    ).not.toContain("private-token");
    await page.goto("/#/pipe/registry");
    await expect(
      page.getByRole("cell", { name: "ghcr.io", exact: true }),
    ).toBeVisible();
  });
  test("connects AWS with session credentials and preserves its region", async ({
    page,
  }) => {
    const mock = await open(page);
    await page
      .getByRole("button", { name: "Connect AWS", exact: true })
      .click();
    await page.getByLabel("Access key ID", { exact: true }).fill("FIXTURE_KEY");
    await page
      .getByLabel("Secret access key", { exact: true })
      .fill("private-key");
    await page.getByLabel("Session token", { exact: true }).fill("session");
    await page.getByRole("button", { name: "Save", exact: true }).click();
    await expect(page.locator("#editor")).toHaveCount(0);
    expect(mock.writes).toEqual([
      {
        provider: "aws",
        accessKeyId: "FIXTURE_KEY",
        secretAccessKey: "private-key",
        sessionToken: "session",
        region: "ap-northeast-2",
      },
    ]);
  });
  test("preserves a connected account and replacement input after rejection", async ({
    page,
  }) => {
    const mock = await open(page, { connections: [github], reject: true });
    await page
      .getByRole("button", { name: "Connect GitHub", exact: true })
      .click();
    await page.getByLabel("Token", { exact: true }).fill("rejected-token");
    await page.getByRole("button", { name: "Save", exact: true }).click();
    await expect(page.getByRole("alert")).toContainText("GitHub rejected");
    await expect(page.getByLabel("Token", { exact: true })).toHaveValue(
      "rejected-token",
    );
    expect(mock.connections).toEqual([github]);
  });
  test("disconnects only the selected provider", async ({ page }) => {
    const mock = await open(page, { connections: [github, aws] });
    await page
      .getByRole("row")
      .filter({ hasText: "octocat" })
      .getByRole("button", { name: "Details", exact: true })
      .click();
    page.once("dialog", (dialog) => dialog.accept());
    await page.getByRole("button", { name: "Disconnect", exact: true }).click();
    await expect.poll(() => mock.deletes).toEqual(["github"]);
    await expect(
      page.getByRole("cell", { name: aws.account!, exact: true }),
    ).toBeVisible();
  });
  test("retries failed discovery and preserves a dirty credential draft", async ({
    page,
  }) => {
    const mock = await open(page, { loadFailure: true });
    await expect(page.getByRole("alert")).toContainText(
      "Connections unavailable",
    );
    mock.recover();
    await page.getByRole("button", { name: "Refresh", exact: true }).click();
    await expect(page.getByRole("alert")).toHaveCount(0);
    await page
      .getByRole("button", { name: "Connect GitHub", exact: true })
      .click();
    await page.getByLabel("Token", { exact: true }).fill("draft");
    page.once("dialog", (d) => d.dismiss());
    await page.getByRole("link", { name: "MicroVM", exact: true }).click();
    await expect(page.getByLabel("Token", { exact: true })).toHaveValue(
      "draft",
    );
  });
  test("preserves the opaque ECR cursor and pulls the selected repository", async ({
    page,
  }) => {
    const mock = await open(page, { connections: [aws] });
    await page.goto("/#/pipe/registry");
    await expect(page.locator("#registry-repository")).toHaveValue("team/app");
    await page
      .getByRole("button", { name: "More repositories", exact: true })
      .click();
    await expect(page.locator("#registry-repository option")).toHaveCount(2);
    expect(
      mock.catalogs.some(
        (q) => new URLSearchParams(q).get("cursor") === "opaque+/=cursor",
      ),
    ).toBe(true);
    await page.locator("#registry-repository").selectOption("team/worker");
    await expect(page.locator("#registry-tag")).toHaveValue("v1");
    await page.getByRole("button", { name: "Pull", exact: true }).click();
    await expect
      .poll(() => mock.transfers)
      .toEqual([`${aws.registry}/team/worker:v1`]);
  });
  test("keeps Korean credentials usable on mobile", async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await open(page, { locale: "ko" });
    await page.locator("[data-action=create][data-id=aws]").click();
    await page.locator("#field-accessKeyId").fill("FIXTURE");
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    ).toBe(true);
  });
});
