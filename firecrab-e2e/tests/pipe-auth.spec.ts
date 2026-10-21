import { expect, test, type Page } from "@playwright/test";
import type { AuthConnection, AuthLogin } from "../../firecrab-frontend/src/api/pipeAuth";

const github: AuthConnection = { provider: "github", account: "octocat", registry: "ghcr.io", region: null, services: ["registry"] };
const aws: AuthConnection = { provider: "aws", account: "123456789012", registry: "123456789012.dkr.ecr.ap-northeast-2.amazonaws.com", region: "ap-northeast-2", services: ["registry"] };
async function open(page: Page, options: { connections?: AuthConnection[]; reject?: boolean; loadFailure?: boolean; locale?: "ko"; overview?: boolean } = {}) {
  let connections = options.connections ?? [];
  let failLoad = options.loadFailure;
  const writes: AuthLogin[] = [];
  const deletes: string[] = [];
  const catalogs: string[] = [];
  const transfers: string[] = [];
  await page.addInitScript(locale => localStorage.setItem("firecrab.locale", locale), options.locale ?? "en");
  await page.route(/^https?:\/\/[^/]+\/api\//, async route => {
    const url = new URL(route.request().url());
    const path = url.pathname;
    if (path === "/api/pipe/auth") {
      if (failLoad) return route.fulfill({ status: 503, json: { error: { message: "Connections unavailable" } } });
      return route.fulfill({ json: connections });
    }
    if (path.startsWith("/api/pipe/auth/")) {
      const provider = path.split("/").at(-1)!;
      if (route.request().method() === "DELETE") {
        deletes.push(provider);
        connections = connections.filter(connection => connection.provider !== provider);
        return route.fulfill({ json: { deleted: true } });
      }
      const login = route.request().postDataJSON() as AuthLogin;
      writes.push(login);
      if (options.reject) return route.fulfill({ status: 400, json: { error: { message: "GitHub rejected the token. Check its validity and permissions." } } });
      const connection = provider === "github" ? github : aws;
      connections = [...connections.filter(row => row.provider !== provider), connection];
      return route.fulfill({ json: connection });
    }
    if (path === "/api/pipe/registries") return route.fulfill({ json: connections.map(connection => ({ registry: connection.registry, username: connection.provider === "aws" ? "AWS" : connection.account, provider: connection.provider === "aws" ? "ecr" : "ghcr", authProvider: connection.provider })) });
    if (path.endsWith("/repositories")) {
      catalogs.push(url.search);
      const isAws = path.includes(aws.registry);
      return route.fulfill({ json: isAws ? { repositories: [url.searchParams.has("cursor") ? "team/worker" : "team/app"], nextPage: url.searchParams.has("cursor") ? null : 2, nextCursor: url.searchParams.has("cursor") ? null : "opaque+/=cursor", listingSupported: true } : { repositories: [], nextPage: null, listingSupported: true } });
    }
    if (path === "/api/pipe/registry-tags") return route.fulfill({ json: { tags: ["v1"], nextLast: null } });
    if (path === "/api/pipe/registry-transfers") {
      transfers.push(route.request().postDataJSON().reference);
      return route.fulfill({ status: 202, json: { alias: "a".repeat(64), status: "running", log: "registry transfer started" } });
    }
    if (path.startsWith("/api/pipe/registry-transfers/")) return route.fulfill({ json: { alias: "a".repeat(64), status: "succeeded", log: "registry transfer succeeded" } });
    if (path === "/api/pipe/registry-images" || path === "/api/vms") return route.fulfill({ json: [] });
    if (path === "/api/pipe") return route.fulfill({ json: { config: null, units: [], nginxAvailable: true, cloudflaredAvailable: true } });
    return route.fulfill({ status: 404, json: { error: { message: "Fixture route unavailable" } } });
  });
  await page.goto(options.overview ? "/#/pipe" : "/#/pipe/auth");
  return { writes, deletes, catalogs, transfers, recover: () => { failLoad = false; }, get connections() { return connections; }, panel: page.getByRole("region", { name: "Pipe Auth", exact: true }) };
}

test.describe("Pipe Auth @dashboard", () => {
  test("shows the provider connection count and opens the authentication card", async ({ page }) => {
    await open(page, { connections: [github, aws], overview: true });
    const card = page.getByRole("link", { name: "Pipe Auth", exact: true });
    await expect(card).toContainText("2 connected");
    await card.click();
    await expect(page).toHaveURL(/#\/pipe\/auth$/);
    await expect(page.getByRole("table", { name: "Provider connections" }).getByText("Connected", { exact: true })).toHaveCount(2);
  });

  test("verifies GitHub, clears the token and enables GHCR through Pipe Registry", async ({ page }) => {
    const mock = await open(page);
    await mock.panel.getByRole("button", { name: "Connect GitHub", exact: true }).click();
    const token = mock.panel.getByLabel("Personal access token (classic)");
    await expect(token).toHaveAttribute("type", "password");
    await token.fill("ghp-test-secret");
    await mock.panel.getByRole("button", { name: "Verify and connect" }).click();
    await expect(mock.panel.getByRole("status")).toContainText("Connected.");
    expect(mock.writes).toEqual([{ provider: "github", token: "ghp-test-secret" }]);
    await expect(mock.panel.locator('input[type="password"]')).toHaveCount(0);
    expect(await page.evaluate(() => JSON.stringify(localStorage) + JSON.stringify(sessionStorage))).not.toContain("ghp-test-secret");
    await mock.panel.getByRole("link", { name: "GHCR", exact: true }).click();
    const registry = page.getByRole("region", { name: "Pipe Registry", exact: true });
    await expect(registry.getByRole("button", { name: "ghcr.io", exact: true })).toBeVisible();
    await expect(registry.getByRole("link", { name: "Manage in Pipe Auth", exact: true })).toBeVisible();
    await expect(registry.getByRole("button", { name: "ghcr.io actions", exact: true })).toHaveCount(0);
    await registry.getByRole("link", { name: "Manage in Pipe Auth", exact: true }).click();
    await mock.panel.getByRole("button", { name: "GitHub actions", exact: true }).click();
    await page.getByRole("menuitem", { name: "Update credentials", exact: true }).click();
    await expect(mock.panel.getByLabel("Personal access token (classic)")).toHaveValue("");
  });

  test("connects AWS with optional session credentials and exposes the account registry", async ({ page }) => {
    const mock = await open(page);
    await mock.panel.getByRole("button", { name: "Connect AWS", exact: true }).click();
    await mock.panel.getByLabel("AWS access key ID", { exact: true }).fill("AKIAIOSFODNN7EXAMPLE");
    await mock.panel.getByLabel("AWS secret access key", { exact: true }).fill("aws-secret");
    await mock.panel.getByLabel("AWS session token (optional)", { exact: true }).fill("aws-session");
    await expect(mock.panel.getByLabel("AWS region", { exact: true })).toHaveValue("ap-northeast-2");
    await mock.panel.getByRole("button", { name: "Verify and connect" }).click();
    await expect(mock.panel.getByRole("status")).toContainText("Connected.");
    expect(mock.writes).toEqual([{ provider: "aws", accessKeyId: "AKIAIOSFODNN7EXAMPLE", secretAccessKey: "aws-secret", sessionToken: "aws-session", region: "ap-northeast-2" }]);
    await expect(mock.panel.getByRole("cell", { name: /123456789012/ })).toBeVisible();
    await expect(mock.panel.getByRole("link", { name: "Amazon ECR", exact: true })).toBeVisible();
    await expect(mock.panel.locator('input[type="password"]')).toHaveCount(0);
  });


  test("browses AWS ECR repositories, preserves opaque cursors and pulls a selected image", async ({ page }) => {
    const mock = await open(page, { connections: [aws] });
    await mock.panel.getByRole("link", { name: "Amazon ECR", exact: true }).click();
    const registry = page.getByRole("region", { name: "Pipe Registry", exact: true });
    await expect(registry.getByRole("button", { name: "team/app", exact: true })).toBeVisible();
    await registry.getByRole("button", { name: "More repositories", exact: true }).click();
    await registry.getByRole("button", { name: "team/worker", exact: true }).click();
    await expect(registry.getByLabel("Tag", { exact: true })).toHaveValue("v1");
    await registry.getByRole("button", { name: "Pull image", exact: true }).click();
    await expect(registry.getByRole("group", { name: "Last pull", exact: true })).toContainText("Pulled");
    expect(mock.transfers).toEqual([`${aws.registry}/team/worker:v1`]);
    const paged = mock.catalogs.filter(query => new URLSearchParams(query).has("cursor"));
    expect(paged).toHaveLength(1);
    expect(new URLSearchParams(paged[0]).get("cursor")).toBe("opaque+/=cursor");
  });

  test("preserves a connected account when replacement credentials are rejected", async ({ page }) => {
    const mock = await open(page, { connections: [github], reject: true });
    await mock.panel.getByRole("button", { name: "GitHub actions", exact: true }).click();
    await page.getByRole("menuitem", { name: "Update credentials", exact: true }).click();
    await mock.panel.getByLabel("Personal access token (classic)").fill("rejected-secret");
    await mock.panel.getByRole("button", { name: "Verify and connect" }).click();
    await expect(mock.panel.getByRole("alert")).toContainText("GitHub rejected the token");
    await expect(mock.panel.getByText("octocat", { exact: true })).toBeVisible();
    await expect(mock.panel.getByRole("status")).toHaveCount(0);
    await mock.panel.getByRole("button", { name: "Cancel", exact: true }).click();
    await expect(mock.panel.locator('input[type="password"]')).toHaveCount(0);
  });

  test("disconnects only the selected provider and updates the available registries", async ({ page }) => {
    const mock = await open(page, { connections: [github, aws] });
    page.once("dialog", dialog => dialog.accept());
    await mock.panel.getByRole("button", { name: "GitHub actions", exact: true }).click();
    await page.getByRole("menuitem", { name: "Disconnect", exact: true }).click();
    await expect(mock.panel.getByRole("button", { name: "Connect GitHub", exact: true })).toBeVisible();
    await expect(mock.panel.getByRole("link", { name: "Amazon ECR", exact: true })).toBeVisible();
    expect(mock.deletes).toEqual(["github"]);
    await mock.panel.getByRole("link", { name: "Amazon ECR", exact: true }).click();
    await expect(page.getByRole("button", { name: aws.registry, exact: true })).toBeVisible();
    await expect(page.getByRole("button", { name: "ghcr.io", exact: true })).toHaveCount(0);
  });

  test("retries failed connection discovery without showing false disconnected accounts", async ({ page }) => {
    const mock = await open(page, { connections: [github], loadFailure: true });
    await expect(mock.panel.getByRole("alert")).toContainText("Connections unavailable");
    await expect(mock.panel.getByRole("button", { name: "Connect GitHub", exact: true })).toHaveCount(0);
    mock.recover();
    await mock.panel.getByRole("button", { name: "Retry", exact: true }).click();
    await expect(mock.panel.getByText("octocat", { exact: true })).toBeVisible();
    await expect(mock.panel.getByRole("alert")).toHaveCount(0);
  });

  test("keeps Korean AWS forms usable in a narrow viewport", async ({ page }, info) => {
    await page.setViewportSize({ width: 390, height: 844 });
    const mock = await open(page, { connections: [github], locale: "ko" });
    await mock.panel.getByRole("button", { name: "AWS 연결", exact: true }).click();
    await expect(mock.panel.getByRole("button", { name: "인증 확인 후 연결", exact: true })).toBeVisible();
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    const providerName = await mock.panel.getByRole("cell", { name: "GitHub", exact: true }).boundingBox();
    expect(providerName!.height).toBeLessThan(40);
    await mock.panel.screenshot({ path: info.outputPath("pipe-auth-mobile-ko.png") });
  });
});
