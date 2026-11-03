import { test } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";
import { expect } from "@playwright/test";
import { createFixtureServer, vm } from "./fixture-server.mjs";

const RESOURCE_META =
  '<meta name="firecrab-resource-endpoint" content="/__firecrab_resources/api/service/resources">';

const agent = (id, name, kind, state, overrides = {}) => ({
  id,
  name,
  role: "Implementation",
  kind,
  cwd: "/workspace/firecrab/project",
  model: "",
  workspaceId: `w-${id}`,
  paneId: `p-${id}`,
  closed: false,
  launchError: null,
  state,
  ...overrides,
});

const linked = vm(0, { name: "my-workspace", state: "running" });
const preparing = vm(4, { name: "docs-sandbox", state: "running" });
const plain = vm(8, { name: "nginx", state: "running" });
const stopped = vm(1, { name: "old-sandbox", state: "stopped" });
const builder = vm(12, {
  name: "builder",
  state: "running",
  purpose: "builder",
});

function statuses() {
  return {
    [linked.id]: {
      controllerId: "native-controller",
      project: {
        vmId: linked.id,
        repository: "https://github.com/steelcrab/web-console.git",
        branch: "main",
        path: "/workspace/firecrab/project",
        state: "ready",
      },
      agents: [
        agent("claude-1", "Fix the sign-in page", "claude", "working"),
        agent("codex-1", "Add payment tests", "codex", "blocked"),
        agent("login-1", "Sign in", "claude", "done", {
          role: "authentication",
        }),
        agent("old-1", "Old task", "codex", "closed", { closed: true }),
      ],
    },
    [preparing.id]: {
      controllerId: "native-controller",
      project: {
        vmId: preparing.id,
        repository: "https://github.com/steelcrab/docs-site.git",
        branch: "main",
        path: "/workspace/firecrab/project",
        state: "preparing",
      },
      agents: [],
    },
    [plain.id]: {
      controllerId: "native-controller",
      project: null,
      agents: [],
    },
  };
}

async function setup(
  t,
  {
    os = "macos",
    meta = true,
    vms = [linked, preparing, plain, stopped, builder],
    status = statuses(),
    locale = "en",
    width = 1440,
    hash = "#/code",
  } = {},
) {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  let browser;
  t.after(async () => {
    await browser?.close();
    await fixture.close();
  });
  fixture.state.vms = structuredClone(vms);
  fixture.handlers.set("GET /api/host", ({ send }) =>
    send(200, {
      loadAverage1m: 1,
      memoryTotalMib: 4096,
      memoryAvailableMib: 2048,
      diskTotalGib: 64,
      diskAvailableGib: 32,
      uptimeSeconds: 60,
      platform: {
        os,
        name: { macos: "macOS", windows: "Windows 11", linux: "Ubuntu 24.04" }[
          os
        ],
        architecture: "arm64",
        virtualization: os === "linux" ? null : "managed",
        controllerId: "native-controller",
      },
    }),
  );
  browser = await chromium.launch({ headless: true });
  const page = await browser.newPage({ viewport: { width, height: 900 } });
  await page.addInitScript(
    (value) => localStorage.setItem("firecrab.locale", value),
    locale,
  );
  if (meta)
    await page.route(`${fixture.url}/`, async (route) => {
      const response = await route.fetch();
      await route.fulfill({
        response,
        body: (await response.text()).replace(
          "<head>",
          `<head>${RESOURCE_META}`,
        ),
      });
    });
  const calls = [];
  await page.route("**/__firecrab_resources/api/service/agents/**", (route) => {
    const path = new URL(route.request().url()).pathname;
    const vmId = path.split("/agents/microvms/")[1]?.split("/")[0];
    calls.push({ method: route.request().method(), path, vmId });
    const value = status[vmId];
    return value
      ? route.fulfill({ json: value })
      : route.fulfill({
          status: 400,
          json: { error: "MicroVM SSH is not ready" },
        });
  });
  await page.goto(`${fixture.url}/${hash}`);
  return { page, calls };
}

test("CODE lists AI Sandbox projects with one VS Code link per agent", async (t) => {
  const { page, calls } = await setup(t);
  await expect(
    page.getByRole("heading", { name: "VS Code", exact: true }),
  ).toBeVisible();
  const card = page.getByRole("article", { name: "steelcrab/web-console" });
  await expect(card).toBeVisible();
  await expect(card).toContainText("main");
  await expect(card).toContainText("my-workspace");

  const links = card.getByRole("link", { name: "Open VS Code" });
  await expect(links).toHaveCount(2);
  const first = card.getByRole("listitem", { name: "Fix the sign-in page" });
  await expect(first).toContainText("Claude Code");
  await expect(first).toContainText("Working");
  const href = new URL(
    await first
      .getByRole("link", { name: "Open VS Code" })
      .getAttribute("href"),
  );
  const query = new URLSearchParams(href.hash.split("?")[1]);
  assert.equal(href.hash.split("?")[0], "#/ai-sandbox");
  assert.equal(query.get("editor"), "claude-1");
  assert.equal(query.get("vm"), linked.id);
  assert.equal(query.get("controller"), "native-controller");
  await expect(
    first.getByRole("link", { name: "Open VS Code" }),
  ).toHaveAttribute("target", "_blank");
  await expect(card).not.toContainText("Sign in");
  await expect(card).not.toContainText("Old task");

  // A project that is still preparing shows no editor link.
  const docs = page.getByRole("article", { name: "steelcrab/docs-site" });
  await expect(docs).toContainText("Preparing");
  await expect(docs.getByRole("link", { name: "Open VS Code" })).toHaveCount(0);

  // A VM without a linked project is not listed.
  await expect(page.getByRole("article")).toHaveCount(2);
  await expect(
    page.getByText("1 stopped MicroVM was not checked."),
  ).toBeVisible();

  // Only running regular MicroVMs are asked, and only with reads.
  assert.deepEqual(
    [...new Set(calls.map((c) => c.vmId))].sort(),
    [linked.id, preparing.id, plain.id].sort(),
  );
  assert.ok(calls.every((c) => c.method === "GET"));
});

test("CODE opens the selected MicroVM in AI Sandbox", async (t) => {
  const { page } = await setup(t);
  const card = page.getByRole("article", { name: "steelcrab/web-console" });
  await card.getByRole("button", { name: "Open in AI Sandbox" }).click();
  await expect(page).toHaveURL(/#\/ai-sandbox$/);
  assert.equal(
    await page.evaluate(() =>
      localStorage.getItem("firecrab.agentVm.native-controller"),
    ),
    linked.id,
  );
});

test("CODE search filters by repository, branch and MicroVM", async (t) => {
  const { page } = await setup(t);
  const search = page.getByRole("searchbox", { name: "Search projects" });
  await search.fill("docs");
  await expect(page.getByRole("article")).toHaveCount(1);
  await expect(
    page.getByRole("article", { name: "steelcrab/docs-site" }),
  ).toBeVisible();
  await search.fill("my-workspace");
  await expect(
    page.getByRole("article", { name: "steelcrab/web-console" }),
  ).toBeVisible();
  await expect(page.getByRole("article")).toHaveCount(1);
});

test("CODE explains how to link a project when none is linked", async (t) => {
  const { page } = await setup(t, { vms: [plain], locale: "ko" });
  await expect(
    page.getByText("프로젝트가 연동된 AI Sandbox가 없습니다"),
  ).toBeVisible();
  await expect(
    page.getByRole("link", { name: "AI Sandbox 열기" }).last(),
  ).toHaveAttribute("href", "#/ai-sandbox");
});

for (const [os, commands] of [
  ["macos", ["firecrab service status", "firecrab service start"]],
  ["windows", ["firecrab service status", "firecrab service start"]],
])
  test(`CODE gives ${os} main server commands without a host bridge`, async (t) => {
    const { page, calls } = await setup(t, { os, meta: false });
    const notice = page.getByRole("region", { name: "VS Code connection" });
    await expect(notice).toContainText("http://127.0.0.1:5523/");
    for (const command of commands) await expect(notice).toContainText(command);
    await expect(notice).toContainText(
      os === "windows" ? "PowerShell" : "Terminal",
    );
    assert.equal(calls.length, 0);
  });

test("CODE says Linux hosts cannot open AI Sandbox VS Code yet", async (t) => {
  const { page, calls } = await setup(t, { os: "linux" });
  const notice = page.getByRole("region", { name: "VS Code connection" });
  await expect(notice).toContainText("Linux");
  await expect(notice).toContainText("macOS");
  await expect(notice).toContainText("Windows");
  await expect(notice).toContainText(
    "systemctl status firecrab-helper firecrab-api",
  );
  assert.equal(calls.length, 0);
});

test("CODE fits a phone screen", async (t) => {
  const { page } = await setup(t, { width: 390, locale: "ko" });
  await expect(
    page.getByRole("article", { name: "steelcrab/web-console" }),
  ).toBeVisible();
  const overflow = await page.evaluate(
    () => document.documentElement.scrollWidth - window.innerWidth,
  );
  assert.ok(overflow <= 0, `page scrolls sideways by ${overflow}px`);
});

test("old CODE workspace links open the VS Code list", async (t) => {
  const { page } = await setup(t, {
    hash: "#/code/11111111-1111-4111-8111-111111111111/editor",
  });
  await expect(
    page.getByRole("heading", { name: "VS Code", exact: true }),
  ).toBeVisible();
  await expect(page).toHaveURL(/#\/code$/);
});
