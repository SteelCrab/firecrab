import { test } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";
import { expect } from "@playwright/test";
import { createFixtureServer } from "./fixture-server.mjs";

const agentId = "11111111-1111-4111-8111-111111111111";
const vmId = "22222222-2222-4222-8222-222222222222";
async function setup(
  t,
  { mismatch = false, invalid = false, shared = false } = {},
) {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  t.after(() => fixture.close());
  fixture.handlers.set("GET /api/host", ({ send }) =>
    send(200, {
      loadAverage1m: 1,
      memoryTotalMib: 4096,
      memoryAvailableMib: 2048,
      diskTotalGib: 64,
      diskAvailableGib: 32,
      uptimeSeconds: 60,
      platform: { os: "macos", controllerId: "native-controller" },
    }),
  );
  const browser = await chromium.launch({ headless: true });
  t.after(() => browser.close());
  const page = await browser.newPage();
  await page.addInitScript(() => localStorage.setItem("firecrab.locale", "en"));
  await page.route(`${fixture.url}/`, async (route) => {
    const response = await route.fetch();
    await route.fulfill({
      response,
      body: (await response.text()).replace(
        "<head>",
        '<head><meta name="firecrab-resource-endpoint" content="/__firecrab_resources/api/service/resources">',
      ),
    });
  });
  const calls = [];
  let installed = false,
    opens = 0;
  const projectPath = shared
    ? "/workspace/firecrab/project"
    : `/workspace/firecrab/${agentId}`;
  const destination = `http://127.0.0.1:43199/${agentId}/?folder=${encodeURIComponent(projectPath)}&tkn=${"a".repeat(64)}`;
  await page.route("http://127.0.0.1:43199/**", (route) =>
    route.fulfill({
      contentType: "text/html",
      body: "<h1>Private VM editor</h1>",
    }),
  );
  await page.route("**/__firecrab_resources/api/service/agents/**", (route) => {
    const request = route.request();
    calls.push({
      path: new URL(request.url()).pathname,
      method: request.method(),
      headers: request.headers(),
    });
    const state =
      request.method() === "POST"
        ? ++opens === 1
          ? "installing"
          : "ready"
        : installed
          ? "installed"
          : "installing";
    return route.fulfill({
      json: {
        vmId,
        agentId,
        projectPath,
        state,
        phase: "download",
        ...(state === "ready"
          ? {
              url: invalid
                ? destination.replace("127.0.0.1", "foreign.example")
                : destination,
            }
          : {}),
      },
    });
  });
  await page.goto(
    `${fixture.url}/#/ai-sandbox?editor=${agentId}&vm=${vmId}&controller=${mismatch ? "other-controller" : "native-controller"}`,
  );
  return { page, calls, destination, install: () => (installed = true) };
}
test("Web VS Code follows VM installation progress and opens the exact task through a private connection", async (t) => {
  const { page, calls, destination, install } = await setup(t);
  await expect(page.getByRole("status")).toContainText("Downloading editor");
  install();
  await expect(
    page.getByRole("heading", { name: "Private VM editor" }),
  ).toBeVisible({ timeout: 10000 });
  assert.equal(page.url(), destination);
  assert.equal(calls.filter((c) => c.method === "POST").length, 2);
  for (const call of calls) {
    assert.ok(call.path.endsWith(`/microvms/${vmId}/${agentId}/editor`));
    if (call.method === "POST")
      assert.equal(
        call.headers["x-firecrab-controller-id"],
        "native-controller",
      );
  }
});
test("A changed controller cannot start or connect a task editor", async (t) => {
  const { page, calls } = await setup(t, { mismatch: true });
  await expect(page.getByRole("alert")).toContainText("another host");
  assert.equal(calls.length, 0);
});
test("An editor response cannot redirect the browser outside the private VM connection", async (t) => {
  const { page, install } = await setup(t, { invalid: true });
  install();
  await expect(page.getByRole("alert")).toContainText(
    "private editor address",
    { timeout: 10000 },
  );
  assert.ok(page.url().includes("#/ai-sandbox?editor="));
  await expect(
    page.getByRole("button", { name: "Retry editor connection" }),
  ).toBeVisible();
});

test("Web VS Code opens the VM repository shared by all tasks", async (t) => {
  const { page, destination, install } = await setup(t, { shared: true });
  install();
  await expect(
    page.getByRole("heading", { name: "Private VM editor" }),
  ).toBeVisible({ timeout: 10000 });
  assert.equal(page.url(), destination);
  assert.equal(
    new URL(destination).searchParams.get("folder"),
    "/workspace/firecrab/project",
  );
});
