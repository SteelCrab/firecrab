import { expect, test, type Page } from "@playwright/test";
import type { AuthConnection, AuthLogin } from "../types/api/pipeAuth";

const firecrab: AuthConnection = { provider: "firecrab", account: "owner@example.com", accountId: "11111111-1111-4111-8111-111111111111", expiresAt: 4102444800000, registry: null, region: null, services: [] };
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
  const tunnelSaves: unknown[] = [];
  const snapshot: import("../types/api/pipe").PipeSnapshot = {
    config: { listen: "127.0.0.1:8080", routes: [{ host: "app.example.com", pathPrefix: "/", upstreams: ["127.0.0.1:9000"] }], tunnel: null },
    units: [], nginxAvailable: true, cloudflaredAvailable: true, firecrabTunnels: [],
  };
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
                provider === "firecrab" ? "The Firecrab account integration token is invalid, expired, or revoked" : "GitHub rejected the token. Check its validity and permissions.",
            },
          },
        });
      const connection = provider === "github" ? github : provider === "firecrab" ? firecrab : aws;
      connections = [
        ...connections.filter((row) => row.provider !== provider),
        connection,
      ];
      return route.fulfill({ json: connection });
    }
    if (path === "/api/pipe/registries")
      return route.fulfill({
        json: connections.filter(connection => connection.registry !== null).map((connection) => ({
          registry: connection.registry,
          username: connection.provider === "aws" ? "AWS" : connection.account,
          provider: connection.provider === "aws" ? "ecr" : "ghcr",
          authProvider: connection.provider,
        })),
      });
    if (path.endsWith("/repositories")) {
      catalogs.push(url.search);
      const isAws = path.includes(aws.registry!);
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
    if (path === "/api/pipe") return route.fulfill({ json: snapshot });
    if (path === "/api/pipe/firecrab-tunnels") {
      tunnelSaves.push(route.request().postDataJSON());
      snapshot.firecrabTunnels = [{ subdomain: "console-app", publicUrl: "https://console-app.firecrab.dev", target: { type: "nginx", host: "app.example.com" }, connectorId: "11111111-1111-4111-8111-111111111111", connected: true, error: null }];
      return route.fulfill({ json: snapshot });
    }
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
    tunnelSaves,
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
  test("links a console account token, shows verified identity, and clears browser secrets", async ({ page }, info) => {
    const mock = await open(page, { connections: [github] });
    await page.getByRole("button", { name: "Connect Firecrab", exact: true }).click();
    const input = page.getByLabel("Account integration token", { exact: true });
    await expect(input).toHaveAttribute("type", "password");
    await expect(page.getByRole("link", { name: "console.firecrab.dev", exact: true })).toHaveAttribute("href", "https://console.firecrab.dev/#account");
    const token = "fca_" + "c".repeat(64);
    await input.fill(token);
    await page.getByRole("button", { name: "Save", exact: true }).click();
    await expect(page.getByRole("status")).toContainText("Connected to the Firecrab account");
    expect(mock.writes).toEqual([{ provider: "firecrab", token }]);
    const detail = page.locator("#detail-panel");
    await expect(detail.getByText(firecrab.account, { exact: true })).toBeVisible();
    await expect(detail.getByText(firecrab.accountId!, { exact: true })).toBeVisible();
    await expect(detail.getByText("Token expires", { exact: true })).toBeVisible();
    await expect(detail.getByRole("link", { name: "Firecrab Console", exact: true })).toHaveAttribute("href", "https://console.firecrab.dev/#account");
    await expect(page.locator('input[type="password"]')).toHaveCount(0);
    expect(await page.evaluate(() => JSON.stringify(localStorage) + JSON.stringify(sessionStorage))).not.toContain(token);
    await page.screenshot({ path: info.outputPath("pipe-auth-firecrab-account.png"), fullPage: true });
    page.once("dialog", dialog => { expect(dialog.message()).toContain("account integration token"); return dialog.accept(); });
    await detail.getByRole("button", { name: "Disconnect", exact: true }).click();
    await expect.poll(() => mock.deletes).toEqual(["firecrab"]);
    await expect(page.getByRole("cell", { name: /^octocat(\s|$)/ })).toBeVisible();
  });

  test("keeps Tunnel tokens separate from the linked account", async ({ page }) => {
    const mock = await open(page, { connections: [firecrab] });
    await page.getByRole("button", { name: "Connect Firecrab", exact: true }).click();
    await page.getByLabel("Account integration token", { exact: true }).fill("a".repeat(64));
    await page.getByRole("button", { name: "Save", exact: true }).click();
    expect(mock.writes).toHaveLength(0);
    page.once("dialog", dialog => dialog.accept());
    await page.getByRole("button", { name: "Cancel", exact: true }).click();
    await page.goto("/#/pipe/firecrab-tunnel");
    await page.getByRole("button", { name: "Connect", exact: true }).click();
    await expect(page.getByLabel("Account integration token", { exact: true })).toHaveCount(0);
    const token = "b".repeat(64);
    await page.getByLabel("Ownership token", { exact: true }).fill(token);
    await page.getByRole("radiogroup", { name: "Target", exact: true }).getByRole("radio", { name: "Named route", exact: true }).check();
    await page.getByLabel("Route host", { exact: true }).selectOption("app.example.com");
    await page.getByRole("button", { name: "Save", exact: true }).click();
    await expect(page.getByRole("link", { name: "https://console-app.firecrab.dev", exact: true })).toBeVisible();
    expect(mock.tunnelSaves).toEqual([{ token, target: { type: "nginx", host: "app.example.com" } }]);
  });

  test("preserves the linked Firecrab account when its replacement token is rejected", async ({ page }) => {
    const mock = await open(page, { connections: [firecrab], reject: true });
    await page.getByRole("button", { name: "Connect Firecrab", exact: true }).click();
    await page.getByLabel("Account integration token", { exact: true }).fill("fca_" + "a".repeat(64));
    await page.getByRole("button", { name: "Save", exact: true }).click();
    await expect(page.getByRole("alert")).toContainText("invalid, expired, or revoked");
    expect(mock.connections).toEqual([firecrab]);
    page.once("dialog", dialog => dialog.accept());
    await page.getByRole("button", { name: "Cancel", exact: true }).click();
    await expect(page.getByRole("cell", { name: firecrab.account, exact: true })).toBeVisible();
    await expect(page.locator('input[type="password"]')).toHaveCount(0);
  });

  test("shows the account token form on a narrow Korean screen", async ({ page }, info) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await open(page, { locale: "ko" });
    await page.getByRole("button", { name: "Firecrab 연결", exact: true }).click();
    await expect(page.getByLabel("계정 연동 토큰", { exact: true })).toBeVisible();
    await expect(page.getByRole("link", { name: "console.firecrab.dev", exact: true })).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
    await page.screenshot({ path: info.outputPath("pipe-auth-firecrab-mobile-ko.png"), fullPage: true });
  });

  test("discovers connected providers without loading credentials", async ({
    page,
  }) => {
    await open(page, { connections: [github, aws] });
    await expect(
      page.getByRole("cell", { name: /^octocat(\s|$)/ }),
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
      page.getByRole("cell", { name: /^octocat(\s|$)/ }),
    ).toBeVisible();
    expect(
      await page.evaluate(
        () => JSON.stringify(localStorage) + JSON.stringify(sessionStorage),
      ),
    ).not.toContain("private-token");
    await page.goto("/#/pipe/registry");
    await expect(
      page.getByRole("cell", { name: /^ghcr\.io(\s|$)/ }),
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
    await page.waitForResponse(
      (r) =>
        new URL(r.url()).pathname === "/api/pipe/auth" &&
        r.request().method() === "GET",
    );
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
