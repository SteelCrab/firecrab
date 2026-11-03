import { test } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { chromium } from "playwright-core";
import { expect } from "@playwright/test";
import { createFixtureServer, vm } from "./fixture-server.mjs";

const session = (id, name, state, kind = "codex") => ({
  id,
  name,
  role: name === "Builder" ? "Implementation" : "Review",
  kind,
  cwd: `/workspace/project/${name.toLowerCase()}`,
  model: "",
  workspaceId: `w-${id}`,
  paneId: `p-${id}`,
  closed: state === "closed",
  launchError: null,
  state,
});
async function setup(
  t,
  {
    hash = "#/ai-sandbox",
    width = 1440,
    locale = "en",
    empty = false,
    mismatch = false,
    select = true,
    selectCli = true,
    ready = true,
    authenticated = true,
    project = true,
    github,
    view = "terminals",
  } = {},
) {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    1,
  );
  let browser;
  t.after(async () => {
    await browser?.close();
    await fixture.close();
  });
  fixture.handlers.set("GET /api/host", ({ send }) =>
    send(200, {
      loadAverage1m: 1,
      memoryTotalMib: 4096,
      memoryAvailableMib: 2048,
      diskTotalGib: 64,
      diskAvailableGib: 32,
      uptimeSeconds: 60,
      platform: {
        os: "macos",
        name: "macOS",
        architecture: "arm64",
        virtualization: "Virtualization.framework",
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
  const state = {
    agents: empty
      ? []
      : [
          session("builder", "Builder", "working"),
          session("reviewer", "Reviewer", "blocked", "claude"),
        ],
    calls: [],
    fail: false,
    ready,
    authenticated,
    preparing: false,
    github: github || {
      state: authenticated
        ? "connected"
        : ready
          ? "disconnected"
          : "unavailable",
    },
    project: project
      ? {
          vmId: "vm-one",
          repository: "https://github.com/example/project.git",
          branch: "main",
          path: "/workspace/firecrab/project",
          state: "ready",
        }
      : null,
  };
  state.streams = [];
  state.keys = [];
  await page.routeWebSocket("**/agents/microvms/*/*/terminal?*", (socket) => {
    const url = new URL(socket.url());
    const parts = url.pathname.split("/");
    const id = parts.at(-2);
    const agent = state.agents.find((a) => a.id === id);
    const text =
      agent?.role === "authentication"
        ? "Open the sign-in link and enter the code. No task has started."
        : id === "reviewer"
          ? "Reviewing src/server.ts\r\nApproval required: inspect the changes and reply."
          : "$ codex\r\nImplementing the health endpoint\r\nBuild completed";
    let seq = 0;
    const stream = {
      id,
      url,
      socket,
      frame(text, full = false) {
        socket.send(
          JSON.stringify({
            type: "terminal.frame",
            seq: ++seq,
            width: Number(url.searchParams.get("cols")),
            height: Number(url.searchParams.get("rows")),
            encoding: "ansi",
            full,
            bytes: Buffer.from(text).toString("base64"),
          }),
        );
      },
    };
    state.streams.push(stream);
    stream.frame("\x1b[2J\x1b[H" + text, true);
    socket.onMessage((message) => {
      const value = JSON.parse(String(message));
      state.keys.push({ id, ...value });
    });
  });
  await page.route(
    "**/__firecrab_resources/api/service/agents**",
    async (route) => {
      const request = route.request();
      const endpoint = new URL(request.url()).pathname;
      const scope = endpoint.split("/agents/microvms/")[1] || "";
      const vmId = scope.split("/")[0];
      const rest = scope.split("/").slice(1).join("/");
      const path = rest ? `/${rest}` : "";
      const body = request.method() === "POST" ? request.postDataJSON() : null;
      state.calls.push({
        endpoint: new URL(request.url()).pathname,
        path,
        method: request.method(),
        body,
        headers: request.headers(),
      });
      if (path === "/prepare" && request.method() === "POST") {
        state.preparing = true;
        return route.fulfill({
          json: { state: "installing", cli: body.cli, step: 1 },
        });
      }
      if (path === "/login" && request.method() === "POST") {
        const agent = {
          ...session(
            body.id,
            body.cli === "github" ? "GitHub sign in" : "Codex sign in",
            "blocked",
            body.cli,
          ),
          role: "authentication",
          cwd: "/root",
        };
        state.agents.push(agent);
        return route.fulfill({ json: agent });
      }
      if (path === "/github" && request.method() === "POST") {
        state.github = {
          state: "pending",
          userCode: "AB12-CD34",
          verificationUri: "https://github.com/login/device",
        };
        return route.fulfill({ json: state.github });
      }
      if (path === "/github/repositories")
        return route.fulfill({
          json: {
            repositories: [
              {
                name: "example/private-project",
                private: true,
                archived: false,
              },
            ],
          },
        });
      if (path === "/project" && request.method() === "POST") {
        state.project = {
          vmId,
          repository: body.repository
            ? `https://github.com/${body.repository}.git`
            : null,
          branch: body.branch,
          path: "/workspace/firecrab/project",
          state: "ready",
        };
        return route.fulfill({ json: state.project });
      }
      if (path === "/tasks" && request.method() === "POST") {
        const kind = body.cli || "codex";
        const agent = {
          ...session(
            body.id,
            body.input.trim().split("\n")[0].slice(0, 30),
            "idle",
            kind,
          ),
          cwd: "/workspace/firecrab/project",
          model: body.model,
        };
        state.agents.push(agent);
        return route.fulfill({ json: agent });
      }
      if (!path) {
        if (request.method() === "POST")
          return route.fulfill({
            status: 405,
            json: { error: "Start a task instead" },
          });
        if (state.fail)
          return route.fulfill({
            status: 503,
            json: { error: "Native runtime unavailable" },
          });
        return route.fulfill({
          json: {
            available: state.ready,
            running: !empty,
            controllerId: mismatch ? "other-controller" : "native-controller",
            vmId,
            github: state.github,
            project: state.project,
            githubConnected: state.github.state === "connected",
            githubAvailable: state.ready,
            preparation: state.preparing
              ? {
                  state: "installing",
                  cli: "codex",
                  step: 2,
                  log: "Installing Herdr",
                }
              : null,
            reason: null,
            drivers: [
              {
                id: "codex",
                name: "Codex",
                available: state.ready,
                authenticated: state.authenticated,
              },
              {
                id: "claude",
                name: "Claude Code",
                available: state.ready,
                authenticated: state.authenticated,
              },
              { id: "gemini", name: "Gemini CLI", available: false },
            ],
            agents: state.agents,
          },
        });
      }
      const [id, op] = path.slice(1).split("/"),
        agent = state.agents.find((a) => a.id === id);
      if (!agent)
        return route.fulfill({
          status: 404,
          json: { error: "Missing fixture agent" },
        });
      if (op === "automation") {
        state.automation ||= {
          vmId,
          agentId: id,
          config: {
            testCommand: "npm ci && npm test",
            buildCommand: "npm run build",
            startCommand: "npm run dev -- --host 0.0.0.0 --port 3000",
            port: 3000,
            security: true,
          },
          repository: "https://github.com/example/project",
          branch: "firecrab/task-test",
          head: "abcdef1",
          workflowConfigured: false,
          scannerAvailable: false,
          run: null,
        };
        if (request.method() === "POST") {
          if (body.action === "configure_ci")
            state.automation.workflowConfigured = true;
          if (body.action === "run")
            state.automation.run = {
              id: body.id,
              state: "running",
              phase: "tests",
              exitCode: null,
              appReady: false,
              config: body.config,
              log: "Running tests",
              findings: null,
            };
          if (body.action === "start_preview")
            state.automation.preview = {
              running: true,
              publicUrl: "https://qa-preview.trycloudflare.com",
              error: null,
            };
          if (body.action === "stop_preview") state.automation.preview = null;
          if (body.action === "check_preview")
            return route.fulfill({
              json: {
                url: "https://qa-preview.trycloudflare.com",
                status: 200,
                ok: true,
                latencyMs: 42,
              },
            });
          if (body.action === "github")
            return route.fulfill({ json: { runs: [] } });
        }
        return route.fulfill({ json: state.automation });
      }
      if (op === "resume") {
        agent.hasPendingTask = false;
        return route.fulfill({ json: agent });
      }
      if (op === "close") {
        agent.closed = true;
        agent.state = "closed";
        return route.fulfill({ json: agent });
      }
      return route.fulfill({
        json: {
          agent,
          text:
            agent.role === "authentication"
              ? "Open the sign-in link and enter the code. No task has started."
              : agent.name === "Reviewer"
                ? "Reviewing src/server.ts\nApproval required: inspect the changes and reply."
                : "$ codex\nImplementing the health endpoint\n✓ Build completed\nRunning focused tests…",
        },
      });
    },
  );
  await page.goto(`${fixture.url}/${hash}`);
  await expect(
    page.getByRole("heading", {
      name: "AI Sandbox",
      exact: true,
    }),
  ).toBeVisible();
  if (select) {
    await page
      .getByRole("combobox", { name: "MicroVM", exact: true })
      .selectOption(fixture.state.vms[0].id);
    if (selectCli)
      await page.getByRole("radio", { name: "Codex", exact: true }).check();
  }
  if (select && selectCli && ready && authenticated && project) {
    await page
      .locator(".am-quick-sidebar")
      .getByRole("button", {
        name: locale === "ko" ? "AI 작업" : "AI tasks",
        exact: true,
      })
      .click();
    if (view === "terminals")
      await page
        .getByRole("button", {
          name: locale === "ko" ? "터미널" : "Terminals",
          exact: true,
        })
        .click();
  }
  return { fixture, page, state };
}

for (const [width, locale] of [
  [1440, "en"],
  [390, "ko"],
  [1440, "ko"],
])
  test(`CLI agent sessions stay separate at ${width}px in ${locale}`, async (t) => {
    const { fixture, page, state } = await setup(t, { width, locale });
    const builder = page.getByRole("region", { name: "Builder", exact: true }),
      reviewer = page.getByRole("region", { name: "Reviewer", exact: true });
    await expect(builder).toBeVisible();
    await expect(reviewer).toBeVisible();
    const editor = builder.getByRole("link", {
      name: "Web VS Code",
      exact: true,
    });
    const editorUrl = new URL(await editor.getAttribute("href"));
    const editorQuery = new URLSearchParams(editorUrl.hash.split("?")[1]);
    assert.equal(editorQuery.get("vm"), fixture.state.vms[0].id);
    assert.equal(editorQuery.get("editor"), "builder");
    assert.equal(editorQuery.get("controller"), "native-controller");
    await expect(editor).toHaveAttribute("target", "_blank");
    await expect(
      builder.locator('.am-pane-heading [data-agent-kind="codex"] img'),
    ).toBeVisible();
    await expect(
      reviewer.locator('.am-pane-heading [data-agent-kind="claude"] img'),
    ).toBeVisible();
    const builderChoice = page
      .locator(".am-agent")
      .filter({ hasText: "Builder" });
    await expect(builderChoice).toHaveAttribute("aria-pressed", "true");
    await builderChoice.click();
    await expect(builderChoice).toHaveAttribute("aria-pressed", "false");
    await expect(builder).toHaveCount(0);
    await expect(reviewer.getByRole("log")).toBeVisible();
    await builderChoice.click();
    await expect(builder).toBeVisible();
    await expect(reviewer.getByRole("log")).toContainText("Approval required");
    const builderInput = builder.getByRole("textbox"),
      reviewerInput = reviewer.getByRole("textbox");
    await expect(builderInput).toBeEnabled();
    await expect(reviewerInput).toBeEnabled();
    await builderInput.pressSequentially("Implement a health endpoint");
    await reviewerInput.pressSequentially("Review authentication");
    await builderInput.press("Enter");
    const input = (id) =>
      state.keys
        .filter((k) => k.id === id && k.type === "terminal.input")
        .map((k) => Buffer.from(k.bytes, "base64").toString())
        .join("");
    await expect
      .poll(() => input("builder"))
      .toBe("Implement a health endpoint\r");
    await expect.poll(() => input("reviewer")).toBe("Review authentication");
    assert.equal(state.calls.filter((c) => c.method === "POST").length, 0);
    await expect(page.locator(".am-input")).toHaveCount(0);
    const attached = state.streams.find((v) => v.id === "builder");
    assert.equal(
      attached.url.searchParams.get("controller"),
      "native-controller",
    );
    assert.ok(
      attached.url.pathname.includes(
        `/microvms/${fixture.state.vms[0].id}/builder/terminal`,
      ),
    );
    await expect(
      reviewer.getByRole("button", { name: "Enter", exact: true }),
    ).toHaveCount(0);
    await reviewer.locator(".am-more > summary").click();
    await reviewer
      .getByRole("button", {
        name: locale === "ko" ? "작업 중단" : "Interrupt task",
        exact: true,
      })
      .click();
    assert.deepEqual(
      state.calls.filter((c) => c.method === "POST").at(-1).body,
      { key: "ctrl+c" },
    );
    const before = state.calls.filter((c) => c.method === "POST").length;
    await page.reload();
    await expect(page.getByRole("log").first()).toBeVisible();
    assert.equal(state.calls.filter((c) => c.method === "POST").length, before);
    assert.equal(
      fixture.requests.filter(
        (r) => r.path.startsWith("/api/vms") && r.method !== "GET",
      ).length,
      0,
    );
    assert(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    );
    await page.screenshot({
      path: join(tmpdir(), `firecrab-agents-${locale}-${width}.png`),
      fullPage: false,
    });
  });

test("work calls an agent only when a task is submitted", async (t) => {
  const { page, state } = await setup(t, { empty: true });
  await page.setViewportSize({ width: 1280, height: 720 });
  const entryHeading = await page
    .getByRole("heading", { name: "What shall we work on?", exact: true })
    .boundingBox();
  const frameHeading = await page.locator(".screen-header").boundingBox();
  assert(
    entryHeading.y >= frameHeading.y + frameHeading.height,
    "Task heading must remain reachable in a short viewport",
  );
  await expect(
    page.getByRole("button", { name: "New agent", exact: true }),
  ).toHaveCount(0);
  assert.equal(state.calls.filter((c) => c.method === "POST").length, 0);
  await expect(
    page.getByRole("button", { name: "New task", exact: true }),
  ).toHaveCount(0);
  const dialog = page.locator(".am-inline-task");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(dialog.getByLabel("Name", { exact: true })).toHaveCount(0);
  await expect(
    dialog.getByRole("button", { name: "Start task", exact: true }),
  ).toBeDisabled();
  assert.equal(state.calls.filter((c) => c.method === "POST").length, 0);
  await dialog
    .locator("summary")
    .filter({ hasText: /^Model$/ })
    .click();
  await expect(dialog.getByLabel("Model", { exact: true })).toBeEnabled();
  await dialog.getByLabel("Model", { exact: true }).fill("temporary-model");
  await page
    .locator(".am-quick-sidebar")
    .getByRole("button", { name: "Connect accounts", exact: true })
    .click();
  await page.getByRole("radio", { name: "Claude Code", exact: true }).check();
  await expect(dialog.getByLabel("Model", { exact: true })).toHaveValue("");
  await page.getByRole("radio", { name: "Codex", exact: true }).check();
  await page
    .locator(".am-quick-sidebar")
    .getByRole("button", { name: "AI tasks", exact: true })
    .click();
  await page.screenshot({
    path: join(tmpdir(), "firecrab-microvm-task-form-20261031.png"),
    fullPage: false,
  });
  await dialog
    .getByLabel("Task", { exact: true })
    .fill("Review authentication changes");

  await dialog.getByRole("button", { name: "Start task", exact: true }).click();
  await expect(dialog).toHaveCount(0);
  await expect(
    page.getByRole("region", {
      name: "Review authentication changes",
      exact: true,
    }),
  ).toBeVisible();
  const call = state.calls.find((c) => c.method === "POST");
  assert.equal(call.path, "/tasks");
  assert.deepEqual(
    { ...call.body, id: "generated" },
    {
      id: "generated",
      input: "Review authentication changes",
      cli: "codex",
      model: "",
    },
  );
});

test("ending a session requires confirmation and keeps its retained output", async (t) => {
  const { page, state } = await setup(t);
  const panel = page.getByRole("region", { name: "Builder", exact: true });
  await expect(panel.getByRole("log")).toContainText("health endpoint");
  page.once("dialog", (d) => d.dismiss());
  await panel.locator(".am-more > summary").click();
  await panel.getByRole("button", { name: "End session", exact: true }).click();
  assert.equal(
    state.calls.filter((c) => c.path === "/builder/close").length,
    0,
  );
  page.once("dialog", (d) => d.accept());
  await panel.locator(".am-more > summary").click();
  await panel.getByRole("button", { name: "End session", exact: true }).click();
  await expect(panel.getByRole("textbox")).toBeDisabled();
  await expect(panel.getByRole("log")).toContainText("health endpoint");
  assert.equal(
    state.calls.filter((c) => c.path === "/builder/close").length,
    1,
  );
});

test("a different native controller cannot receive agent commands", async (t) => {
  const { page, state } = await setup(t, { mismatch: true });
  await expect(page.locator(".am-worker-count")).toContainText(
    "Checking connection",
  );
  await expect(page.getByRole("alert")).toContainText("does not match");
  await expect(
    page.getByRole("button", { name: "New task", exact: true }),
  ).toBeDisabled();
  const panel = page.getByRole("region", { name: "Builder", exact: true });
  await expect(panel.getByRole("textbox")).toBeDisabled();
  assert.equal(state.streams.length, 0);
  assert.equal(state.calls.filter((c) => c.method === "POST").length, 0);
});

test("agent terminals forward raw keys, Unicode, paste, resize and scroll to their own PTY", async (t) => {
  const { page, state } = await setup(t);
  const pane = page.getByRole("region", { name: "Builder", exact: true });
  const input = pane.getByRole("textbox", { name: "Builder terminal input" });
  await expect(input).toBeEnabled();
  await expect(pane.getByRole("log")).toContainText("health endpoint");
  const keys = () =>
    Buffer.concat(
      state.keys
        .filter((v) => v.id === "builder" && v.type === "terminal.input")
        .map((v) => Buffer.from(v.bytes, "base64")),
    ).toString();
  await input.focus();
  await page.keyboard.insertText("한글");
  for (const key of ["ArrowUp", "Tab", "Control+c", "Escape"])
    await input.press(key);
  await expect.poll(keys).toBe("한글\x1b[A\t\x03\x1b");
  state.keys.length = 0;
  await input.evaluate((node) => {
    const clipboardData = new DataTransfer();
    clipboardData.setData("text/plain", "first\nsecond");
    node.dispatchEvent(
      new ClipboardEvent("paste", {
        clipboardData,
        bubbles: true,
        cancelable: true,
      }),
    );
  });
  await expect.poll(keys).toBe("\x1b[200~first\rsecond\x1b[201~");
  state.keys.length = 0;
  const pasted = "한글".repeat(6000) + "\nend";
  await input.evaluate((node, text) => {
    const clipboardData = new DataTransfer();
    clipboardData.setData("text/plain", text);
    node.dispatchEvent(
      new ClipboardEvent("paste", {
        clipboardData,
        bubbles: true,
        cancelable: true,
      }),
    );
  }, pasted);
  await expect
    .poll(keys)
    .toBe("\x1b[200~" + pasted.replace("\n", "\r") + "\x1b[201~");
  assert.equal(
    state.keys.filter((v) => v.type === "terminal.input").length,
    1,
    "A long bracketed paste must reach the CLI as one paste",
  );
  assert.equal(
    state.keys.filter((v) => v.id === "reviewer" && v.type === "terminal.input")
      .length,
    0,
  );
  await page.setViewportSize({ width: 1100, height: 740 });
  await expect
    .poll(
      () =>
        state.keys.filter(
          (v) => v.id === "builder" && v.type === "terminal.resize",
        ).length,
    )
    .toBeGreaterThan(0);
  const terminal = pane.locator(".am-terminal");
  await terminal.hover();
  await page.mouse.wheel(0, -120);
  await expect
    .poll(() =>
      state.keys.some(
        (v) =>
          v.id === "builder" &&
          v.type === "terminal.scroll" &&
          v.direction === "up",
      ),
    )
    .toBe(true);
});

test("terminal reconnect preserves the agent session without replaying input", async (t) => {
  const { page, state } = await setup(t);
  const pane = page.getByRole("region", { name: "Builder", exact: true });
  const input = pane.getByRole("textbox", { name: "Builder terminal input" });
  await expect(input).toBeEnabled();
  await input.pressSequentially("unfinished");
  await expect
    .poll(() => state.keys.filter((v) => v.type === "terminal.input").length)
    .toBe(10);
  const original = await input.elementHandle();
  const before = state.streams.length;
  state.fail = true;
  await page
    .getByRole("button", { name: "Check environment", exact: true })
    .click();
  await expect(input).toBeDisabled();
  state.fail = false;
  await page
    .getByRole("button", { name: "Check environment", exact: true })
    .click();
  await expect(input).toBeEnabled();
  assert.equal(state.streams.length, before);
  assert.equal(await original.evaluate((node) => node.isConnected), true);
  state.streams
    .find((v) => v.id === "builder")
    .socket.close({ code: 1011, reason: "QA disconnect" });
  await expect(input).toBeDisabled();
  await pane.getByRole("button", { name: "Reconnect terminal" }).click();
  await expect(input).toBeEnabled();
  assert.equal(
    state.keys.filter((v) => v.type === "terminal.input").length,
    10,
  );
  assert.equal(state.calls.filter((v) => v.method === "POST").length, 0);
  await expect(pane.getByRole("log")).toContainText("health endpoint");
});

test("VM and CLI selection are explicit and opening a VM creation form starts nothing", async (t) => {
  const { fixture, page, state } = await setup(t, {
    empty: true,
    select: false,
  });
  await expect(
    page.getByRole("button", { name: "Start task", exact: true }),
  ).toHaveCount(0);
  assert.equal(state.calls.length, 0);
  await page.getByRole("button", { name: "New MicroVM", exact: true }).click();
  const form = page.getByRole("region", {
    name: "Workspace setup",
    exact: true,
  });
  await expect(form.getByLabel("Workspace name", { exact: true })).toHaveValue(
    "my-workspace",
  );
  await expect(form).toContainText("Ubuntu 26.04 LTS");
  await expect(form).toContainText("2 vCPU");
  await expect(form).toContainText("4 GiB");
  await expect(form).toContainText("32 GiB");
  await page.screenshot({
    path: join(tmpdir(), "firecrab-quick-start-1440.png"),
  });
  await page.setViewportSize({ width: 390, height: 844 });
  assert.equal(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
    true,
  );
  await page.screenshot({
    path: join(tmpdir(), "firecrab-quick-start-390.png"),
  });
  await page
    .getByRole("combobox", { name: "MicroVM", exact: true })
    .selectOption(fixture.state.vms[0].id);
  await expect(
    page.getByRole("button", { name: "Start task", exact: true }),
  ).toHaveCount(0);
  await expect(
    page.getByRole("radio", { name: "Select automatically", exact: true }),
  ).toHaveCount(0);
  assert.equal(state.calls.filter((c) => c.method === "POST").length, 0);
});

test("preparation and VM account connection do not submit a model task", async (t) => {
  const { page, state } = await setup(t, {
    empty: true,
    ready: false,
    authenticated: false,
  });
  await page
    .getByRole("button", { name: "Prepare CLI environment", exact: true })
    .click();
  await expect(page.getByRole("status")).toContainText("Preparing tools");
  assert.equal(
    state.calls.filter((c) => c.method === "POST" && c.path === "/tasks")
      .length,
    0,
  );
  assert.deepEqual(state.calls.find((c) => c.path === "/prepare").body, {
    cli: "codex",
  });
  state.preparing = false;
  state.ready = true;
  await page
    .getByRole("button", { name: "Check environment", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Connect CLI account", exact: true })
    .click();
  const login = page.getByRole("region", {
    name: "Codex sign in",
    exact: true,
  });
  await expect(login.getByRole("log")).toContainText("No task has started");
  await expect(login.getByRole("link", { name: "Web VS Code" })).toHaveCount(0);
  assert.equal(
    state.calls.filter((c) => c.method === "POST" && c.path === "/tasks")
      .length,
    0,
  );
  state.authenticated = true;
  await page
    .getByRole("button", { name: "Check environment", exact: true })
    .click();
  await expect(
    page.getByRole("button", { name: "New task", exact: true }),
  ).toBeEnabled();
  assert(state.calls.every((c) => c.endpoint.includes("/microvms/")));
});

test("quick start prepares the default VM and resumes the same job after reload", async (t) => {
  const { fixture, page, state } = await setup(t, {
    empty: true,
    select: false,
  });
  const id = "59ded74d-c9a0-4ec6-a30a-71bb091a6eb3";
  let job;
  fixture.handlers.set("POST /api/workspace-setups", ({ send, body }) => {
    job = { ...body, vmId: id, state: "running", phase: "image", error: null };
    send(202, job);
    fixture.handlers.set(`GET /api/workspace-setups/${body.id}`, ({ send }) =>
      send(200, job),
    );
  });
  await page
    .getByRole("button", { name: "Create workspace", exact: true })
    .click();
  await expect(
    page.getByRole("region", { name: "Workspace setup" }),
  ).toContainText("Working…");
  await page.reload();
  await expect(
    page.getByRole("region", { name: "Workspace setup" }),
  ).toContainText("Working…");
  assert.equal(
    fixture.requests.filter(
      (r) => r.method === "POST" && r.path === "/api/workspace-setups",
    ).length,
    1,
  );
  assert.deepEqual(
    {
      cpu: job.cpu,
      ram: job.ram,
      diskGb: job.diskGb,
      microNetworkId: job.microNetworkId,
      storageRoot: job.storageRoot,
    },
    { cpu: 2, ram: 4096, diskGb: 32, microNetworkId: null, storageRoot: null },
  );
  const row = vm(9, { id, name: job.name, state: "running" });
  fixture.state.vms.push(row);
  fixture.handlers.set(`GET /api/vms/${id}`, ({ send }) => send(200, row));
  job = { ...job, state: "ready", phase: "complete" };
  await expect(
    page.getByRole("combobox", { name: "MicroVM", exact: true }),
  ).toHaveValue(id, { timeout: 15000 });
  await expect(
    page.getByRole("heading", { name: "Choose your AI and repository" }),
  ).toBeVisible();
  assert.equal(
    state.calls.filter((c) => c.method === "POST" && c.path === "/tasks")
      .length,
    0,
  );
  assert.equal(
    await page.evaluate(() =>
      localStorage.getItem("firecrab.workspaceSetup.native-controller"),
    ),
    null,
  );
});

test("a task waiting on CLI setup is sent only after the user resumes it", async (t) => {
  const { page, state } = await setup(t);
  state.agents[0].hasPendingTask = true;
  state.agents[0].state = "idle";
  const pane = page.getByRole("region", { name: "Builder", exact: true });
  const send = pane.getByRole("button", {
    name: "Send waiting task",
    exact: true,
  });
  await expect(send).toBeEnabled();
  assert.equal(state.calls.filter((c) => c.path.endsWith("/resume")).length, 0);
  await send.click();
  await expect(send).toHaveCount(0);
  await page.reload();
  await expect(pane).toBeVisible();
  assert.equal(
    state.calls.filter((c) => c.method === "POST" && c.path.endsWith("/resume"))
      .length,
    1,
  );
});

test("verification stays scoped to its VM and does not publish until the app is ready", async (t) => {
  const { page, state, fixture } = await setup(t);
  const panel = page.getByRole("region", { name: "Reviewer", exact: true });
  await panel.locator(".am-more > summary").click();
  await panel
    .getByRole("button", { name: "Automation and preview", exact: true })
    .click();
  const modal = page.getByRole("dialog", {
    name: "Automation and preview",
    exact: true,
  });
  await expect(
    modal.getByRole("button", { name: "Create test URL", exact: true }),
  ).toBeDisabled();
  assert.equal(state.calls.filter((c) => c.method === "POST").length, 0);
  await modal
    .getByRole("button", { name: "Create workflow", exact: true })
    .click();
  await expect(modal.getByRole("status")).toContainText("firecrab-checks.yml");
  await expect(
    modal.getByRole("button", { name: "Create workflow", exact: true }),
  ).toBeDisabled();
  await modal
    .getByRole("button", { name: "Run verification", exact: true })
    .click();
  await expect(modal.getByRole("status")).toContainText("Running");
  const call = state.calls.find((c) => c.body?.action === "run");
  assert.equal(call.path, "/reviewer/automation");
  assert.equal(call.body.config.security, true);
  assert.equal(call.headers["x-firecrab-controller-id"], "native-controller");
  assert.equal(
    fixture.requests.some(
      (r) => r.method === "POST" && r.path === "/api/pipe/quick-tunnels",
    ),
    false,
  );
  await page.screenshot({
    path: join(tmpdir(), "firecrab-automation-en-1440.png"),
  });
  await modal.getByRole("button", { name: "Close", exact: true }).click();
});

test("preview targets only the selected VM port and records an explicit HTTPS check", async (t) => {
  const { page, state, fixture } = await setup(t);
  const panel = page.getByRole("region", { name: "Reviewer", exact: true });
  await panel.locator(".am-more > summary").click();
  await panel
    .getByRole("button", { name: "Automation and preview", exact: true })
    .click();
  const modal = page.getByRole("dialog", {
    name: "Automation and preview",
    exact: true,
  });
  await expect(
    modal.getByRole("button", { name: "Run verification", exact: true }),
  ).toBeEnabled();
  state.automation.run = {
    id: "verified",
    state: "passed",
    phase: "complete",
    exitCode: 0,
    appReady: true,
    config: state.automation.config,
    log: "Verified",
    findings: { vulnerabilities: 0, secrets: 0, misconfigurations: 0 },
  };
  await modal
    .getByRole("button", { name: "Refresh verification", exact: true })
    .click();
  await expect(
    modal.getByRole("button", { name: "Create test URL", exact: true }),
  ).toBeEnabled();
  await modal
    .getByRole("button", { name: "Create test URL", exact: true })
    .click();
  await expect(
    modal.getByRole("link", { name: "https://qa-preview.trycloudflare.com" }),
  ).toBeVisible();
  const publish = state.calls.find((c) => c.body?.action === "start_preview");
  assert.equal(publish.path, "/reviewer/automation");
  assert.equal(publish.body.target, undefined);
  assert.equal(
    fixture.requests.some(
      (r) => r.method === "POST" && r.path.startsWith("/api/pipe/"),
    ),
    false,
  );
  await modal.getByRole("button", { name: "Check HTTPS", exact: true }).click();
  await expect(
    modal.getByRole("status").filter({ hasText: "HTTPS 200" }),
  ).toBeVisible();
  const call = state.calls.find((c) => c.body?.action === "check_preview");
  assert.equal(call.body.tunnelId, undefined);
  assert.equal(call.body.url, undefined);
  await page.screenshot({
    path: join(tmpdir(), "firecrab-automation-preview-en-1440.png"),
  });
});

test("automation settings fit a Korean mobile viewport", async (t) => {
  const { page } = await setup(t, { locale: "ko", width: 390 });
  const panel = page.getByRole("region", { name: "Reviewer", exact: true });
  await panel.locator(".am-more > summary").click();
  await panel
    .getByRole("button", { name: "자동화·미리보기", exact: true })
    .click();
  const modal = page.getByRole("dialog", {
    name: "자동화·미리보기",
    exact: true,
  });
  await expect(
    modal.getByRole("button", { name: "검증 실행", exact: true }),
  ).toBeVisible();
  await modal.getByText("실행 설정", { exact: true }).click();
  await expect(modal.getByLabel("테스트 명령", { exact: true })).toBeVisible();
  assert.equal(
    await modal.evaluate((el) => el.scrollWidth <= el.clientWidth),
    true,
  );
  await page.screenshot({
    path: join(tmpdir(), "firecrab-automation-ko-390.png"),
  });
});

test("a temporary host status failure keeps the selected VM and task draft mounted", async (t) => {
  const { page, fixture, state } = await setup(t, { empty: true });
  const selectedVm = fixture.state.vms[0].id;
  const task = page.getByRole("textbox", { name: "Task", exact: true });
  await task.fill("Keep this draft while status is unavailable");
  const originalEnvironment = await page
    .locator(".am-environment")
    .elementHandle();
  const originalTask = await task.elementHandle();
  const healthy = fixture.handlers.get("GET /api/host");
  const unavailable = page.waitForResponse(
    (response) =>
      new URL(response.url()).pathname === "/api/host" &&
      response.status() === 503,
  );
  fixture.handlers.set("GET /api/host", ({ send }) =>
    send(503, {
      error: {
        code: "temporarily_unavailable",
        message: "Temporary status failure",
      },
    }),
  );
  await unavailable;
  await expect(
    page.getByRole("link", { name: "Host status unavailable", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("combobox", { name: "MicroVM", exact: true }),
  ).toHaveValue(selectedVm);
  await expect(task).toHaveValue("Keep this draft while status is unavailable");
  assert.equal(
    await originalEnvironment.evaluate((node) => node.isConnected),
    true,
  );
  assert.equal(await originalTask.evaluate((node) => node.isConnected), true);
  await expect(
    page.getByRole("button", { name: "Start task", exact: true }),
  ).toBeDisabled();
  fixture.handlers.set("GET /api/host", healthy);
  await expect(
    page.getByRole("button", { name: "Start task", exact: true }),
  ).toBeEnabled({ timeout: 12000 });
  await expect(task).toHaveValue("Keep this draft while status is unavailable");
  assert.equal(await originalTask.evaluate((node) => node.isConnected), true);
  assert.equal(
    state.calls.some((call) => call.method === "POST"),
    false,
  );
});

test("a delayed older status cannot replace the current task form", async (t) => {
  const { page, fixture, state } = await setup(t, { empty: true });
  const task = page.getByRole("textbox", { name: "Task", exact: true });
  await task.fill("Keep the current authenticated workspace");
  const originalTask = await task.elementHandle();
  const id = fixture.state.vms[0].id;
  let releaseOld, acknowledgeOld;
  const held = new Promise((resolve) => {
    releaseOld = resolve;
  });
  const observed = new Promise((resolve) => {
    acknowledgeOld = resolve;
  });
  let requests = 0;
  await page.route(
    `**/__firecrab_resources/api/service/agents/microvms/${id}`,
    async (route) => {
      const old = requests++ === 0;
      const snapshot = {
        available: true,
        project: state.project,
        github: state.github,
        running: true,
        controllerId: "native-controller",
        vmId: id,
        drivers: [
          { id: "codex", name: "Codex", available: true, authenticated: !old },
        ],
        agents: [],
      };
      if (old) {
        acknowledgeOld();
        await held;
      }
      await route.fulfill({ json: snapshot });
    },
  );
  const refresh = page.getByRole("button", {
    name: "Check environment",
    exact: true,
  });
  await refresh.click();
  await observed;
  const current = page.waitForResponse(
    async (response) =>
      response.url().endsWith(`/microvms/${id}`) &&
      (await response.json()).drivers[0].authenticated,
  );
  await refresh.click();
  await current;
  const late = page.waitForResponse(
    async (response) =>
      response.url().endsWith(`/microvms/${id}`) &&
      !(await response.json()).drivers[0].authenticated,
  );
  releaseOld();
  await late;
  await page.evaluate(
    () =>
      new Promise((resolve) =>
        requestAnimationFrame(() => requestAnimationFrame(resolve)),
      ),
  );
  assert.equal(await originalTask.evaluate((node) => node.isConnected), true);
  await expect(task).toHaveValue("Keep the current authenticated workspace");
  await expect(
    page.getByRole("button", { name: "Start task", exact: true }),
  ).toBeEnabled();
});

for (const hash of [
  "#/ai-sandbox",
  "#/ai-sandbox?view=models&project=legacy-project",
])
  test(`Sandbox uses MicroVM tasks without a separate project workflow at ${hash}`, async (t) => {
    const { page, fixture, state } = await setup(t, { hash, empty: true });
    await expect(
      page.getByRole("heading", {
        name: "What shall we work on?",
        exact: true,
      }),
    ).toBeVisible();
    await expect(
      page.getByRole("combobox", { name: "MicroVM", exact: true }),
    ).toHaveValue(fixture.state.vms[0].id);
    for (const name of [
      "Model tasks",
      "New project",
      "Project settings",
      "Workspace options",
    ])
      await expect(page.getByRole("button", { name, exact: true })).toHaveCount(
        0,
      );
    await page
      .getByRole("textbox", { name: "Task", exact: true })
      .fill("Inspect this repository");
    await expect(
      page.getByRole("button", { name: "Start task", exact: true }),
    ).toBeEnabled();
    assert.equal(state.calls.filter((c) => c.method === "POST").length, 0);
    assert.equal(
      fixture.requests.filter(
        (r) =>
          r.path.startsWith("/api/ai-sandbox") ||
          r.path.startsWith("/api/pipe/ai"),
      ).length,
      0,
    );
  });

test("GitHub AUTH shows a browser device code without creating a coding agent and survives reload", async (t) => {
  const { page, state } = await setup(t, {
    empty: true,
    authenticated: false,
    project: false,
  });
  const auth = page.locator(".am-github-auth");
  await page
    .getByRole("button", { name: "Connect with GitHub AUTH", exact: true })
    .click();
  await expect(page.getByText("AB12-CD34", { exact: true })).toBeVisible();
  const link = page.getByRole("link", { name: "Authorize on GitHub" });
  await expect(link).toHaveAttribute("href", "https://github.com/login/device");
  assert.equal(state.agents.length, 0);
  assert.equal(state.calls.filter((c) => c.method === "POST").length, 1);
  await page.reload();
  await expect(page.getByText("AB12-CD34", { exact: true })).toBeVisible();
  assert.equal(
    state.calls.filter((c) => c.path === "/github" && c.method === "POST")
      .length,
    1,
  );
  state.github = { state: "connected" };
  await expect(
    page.getByText("Connected in this VM", { exact: true }),
  ).toBeVisible({ timeout: 10000 });
  await expect(
    page.getByRole("combobox", { name: "Your GitHub repositories" }),
  ).toBeVisible();
  assert.equal(state.agents.length, 0);
  assert.equal(await auth.count(), 0);
});

test("GitHub authentication tools can be prepared before selecting any coding CLI", async (t) => {
  const { page, state } = await setup(t, {
    empty: true,
    ready: false,
    selectCli: false,
    authenticated: false,
    project: false,
  });
  await expect(
    page.getByRole("radio", { name: "Codex", exact: true }),
  ).not.toBeChecked();
  await page
    .getByRole("button", { name: "Prepare GitHub AUTH", exact: true })
    .click();
  assert.deepEqual(state.calls.find((c) => c.path === "/prepare").body, {
    cli: "github",
  });
  assert.equal(state.agents.length, 0);
});

test("One VM repository is selected after AUTH and reused by new tasks", async (t) => {
  const { page, state } = await setup(t, { empty: true, project: false });
  await page
    .locator(".am-quick-sidebar")
    .getByRole("button", { name: "AI tasks", exact: true })
    .click();
  await page
    .getByLabel("Task", { exact: true })
    .fill("Inspect the connected repository");
  await expect(
    page.getByRole("button", { name: "Start task", exact: true }),
  ).toBeDisabled();
  await page
    .locator(".am-quick-sidebar")
    .getByRole("button", { name: "Connect accounts", exact: true })
    .click();
  await page
    .getByRole("combobox", { name: "Your GitHub repositories" })
    .selectOption("example/private-project");
  await page
    .getByRole("button", { name: "Connect repository", exact: true })
    .click();
  await expect(
    page.getByText("example/private-project", { exact: true }),
  ).toBeVisible();
  await page
    .getByRole("button", { name: "Start an AI task", exact: true })
    .click();
  await page.getByRole("button", { name: "Start task", exact: true }).click();
  const task = state.calls.find((c) => c.path === "/tasks");
  assert.equal(task.body.repository, undefined);
  assert.equal(task.body.branch, undefined);
  assert.equal(state.agents[0].cwd, "/workspace/firecrab/project");
  await page.reload();
  await expect(
    page.getByText("example/private-project", { exact: true }),
  ).toBeVisible();
  assert.equal(
    state.calls.filter((c) => c.path === "/project" && c.method === "POST")
      .length,
    1,
  );
});

test("A different controller cannot start GitHub AUTH or bind a repository", async (t) => {
  const { page, state } = await setup(t, {
    empty: true,
    mismatch: true,
    authenticated: false,
    project: false,
  });
  await expect(
    page.getByRole("button", { name: "Connect with GitHub AUTH", exact: true }),
  ).toBeDisabled();
  await page.locator(".am-repository-alternative > summary").click();
  await expect(
    page.getByRole("button", { name: "Connect repository", exact: true }),
  ).toBeDisabled();
  assert.equal(state.calls.filter((c) => c.method === "POST").length, 0);
});

for (const width of [1440, 390]) {
  test(`GitHub setup stays compact and usable at ${width}px`, async (t) => {
    const { page, state } = await setup(t, {
      empty: true,
      project: false,
      authenticated: false,
      width,
      locale: "ko",
      github: {
        state: "pending",
        userCode: "AB12-CD34",
        verificationUri: "https://github.com/login/device",
      },
    });
    await expect(page.getByText("AB12-CD34", { exact: true })).toBeVisible();
    const panel = page.locator(".am-vm-repository");
    const box = await panel.boundingBox();
    assert(box.width <= 682);
    await page.screenshot({
      path: join(tmpdir(), `firecrab-auth-design-pending-${width}.png`),
      fullPage: false,
    });
    assert(
      box.height < 430,
      JSON.stringify(
        await panel.evaluate((el) => ({
          height: el.getBoundingClientRect().height,
          children: [...el.children].map((n) => ({
            class: n.className,
            height: n.getBoundingClientRect().height,
            padding: getComputedStyle(n).padding,
            display: getComputedStyle(n).display,
          })),
        })),
      ),
    );
    assert(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    );
    await expect(
      page.getByRole("link", { name: "GitHub에서 인증", exact: true }),
    ).toHaveAttribute("href", "https://github.com/login/device");
    await expect(page.getByLabel("저장소 주소", { exact: true })).toBeHidden();
    await page.locator(".am-repository-alternative > summary").click();
    await expect(page.getByLabel("저장소 주소", { exact: true })).toBeVisible();
    assert.equal(state.calls.filter((c) => c.method === "POST").length, 0);
    await page.locator(".am-repository-alternative > summary").click();
    await page.screenshot({
      path: join(tmpdir(), `firecrab-auth-design-pending-${width}.png`),
      fullPage: false,
    });
  });
}

test("Connected repository collapses to a source row and keeps terminal room", async (t) => {
  const { page } = await setup(t, { locale: "ko" });
  const panel = page.locator(".am-vm-repository");
  await expect(panel).toHaveClass(/is-ready/);
  assert((await panel.boundingBox()).height < 70);
  assert(
    (
      await page
        .getByRole("region", { name: "Builder", exact: true })
        .boundingBox()
    ).height > 380,
  );
  await expect(page.locator(".am-github-auth")).toHaveCount(0);
  await page.screenshot({
    path: join(tmpdir(), "firecrab-auth-design-connected.png"),
    fullPage: false,
  });
});

test("A failed VM connection keeps the source row and hides technical errors until opened", async (t) => {
  const { page, state } = await setup(t, { empty: true });
  await page
    .getByRole("textbox", { name: "Task", exact: true })
    .fill("Keep this task draft");
  state.fail = true;
  await page
    .getByRole("button", { name: "Check environment", exact: true })
    .click();
  const failure = page.locator(".am-connection-error");
  await expect(failure).toContainText("Could not connect to this MicroVM");
  await expect(
    page.getByText("VM connection unavailable", { exact: true }),
  ).toBeVisible();
  await expect(page.locator(".am-vm-repository")).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Start task", exact: true }),
  ).toBeDisabled();
  await expect(failure.locator("code")).toBeHidden();
  await failure.locator("summary").click();
  await expect(failure.locator("code")).toContainText(
    "Native runtime unavailable",
  );
  await expect(
    page.getByRole("textbox", { name: "Task", exact: true }),
  ).toHaveValue("Keep this task draft");
});

for (const [width, locale] of [
  [1440, "en"],
  [390, "ko"],
]) {
  test(`the task board follows parallel CLI work at ${width}px in ${locale}`, async (t) => {
    const { page, state } = await setup(t, { width, locale, view: "board" });
    const board = page.getByRole("region", {
      name: locale === "ko" ? "작업 보드" : "Task board",
      exact: true,
    });
    const working = board.getByRole("region", {
      name: locale === "ko" ? "실행 중" : "Working",
      exact: true,
    });
    const attention = board.getByRole("region", {
      name: locale === "ko" ? "입력·확인 필요" : "Needs attention",
      exact: true,
    });
    const done = board.getByRole("region", {
      name: locale === "ko" ? "완료" : "Done",
      exact: true,
    });
    await expect(board).toBeVisible();
    await expect(
      page.getByRole("button", {
        name: locale === "ko" ? "보드" : "Board",
        exact: true,
      }),
    ).toHaveAttribute("aria-pressed", "true");
    await expect(working.locator('[data-task-id="builder"]')).toBeVisible();
    await expect(attention.locator('[data-task-id="reviewer"]')).toBeVisible();
    await expect(
      working.locator('[data-agent-kind="codex"] img'),
    ).toBeVisible();
    await expect(
      attention.locator('[data-agent-kind="claude"] img'),
    ).toBeVisible();
    state.agents.find((agent) => agent.id === "reviewer").state = "working";
    await expect(working.locator('[data-task-id="reviewer"]')).toBeVisible();
    await expect(page.locator(".am-worker-count")).toContainText(
      locale === "ko" ? "에이전트 2개 실행 중" : "2 agents working",
    );
    state.agents.find((agent) => agent.id === "builder").state = "done";
    await expect(done.locator('[data-task-id="builder"]')).toBeVisible();
    await expect(working.locator('[data-task-id="builder"]')).toHaveCount(0);
    assert.equal(
      state.calls.filter((call) => call.method === "POST").length,
      0,
    );
    assert(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    );
    await page.screenshot({
      path: join(tmpdir(), `firecrab-agent-board-${locale}-${width}.png`),
      fullPage: false,
    });
    await working.locator('[data-task-id="reviewer"]').click();
    const pane = page.getByRole("region", { name: "Reviewer", exact: true });
    await expect(pane.getByRole("log")).toBeVisible();
    const streams = state.streams.length;
    await page
      .getByRole("button", {
        name: locale === "ko" ? "보드" : "Board",
        exact: true,
      })
      .click();
    await expect(board).toBeVisible();
    await page
      .getByRole("button", {
        name: locale === "ko" ? "터미널" : "Terminals",
        exact: true,
      })
      .click();
    await expect(pane.getByRole("log")).toBeVisible();
    assert.equal(
      state.streams.length,
      streams,
      "Changing views must keep the existing PTY connection",
    );
    await page.reload();
    await expect(pane.getByRole("log")).toBeVisible();
    assert.equal(
      state.calls.filter((call) => call.method === "POST").length,
      0,
    );
  });
}

test("the board starts one CLI only after submitting a task", async (t) => {
  const { page, state } = await setup(t, { empty: true, view: "board" });
  const board = page.getByRole("region", { name: "Task board", exact: true });
  await expect(board).toBeVisible();
  await expect(board.locator("[data-task-id]")).toHaveCount(0);
  assert.equal(state.calls.filter((call) => call.method === "POST").length, 0);
  const form = page.locator(".am-inline-task");
  await form
    .getByLabel("Task", { exact: true })
    .fill("Build the account settings");
  await form.getByRole("button", { name: "Start task", exact: true }).click();
  await expect(
    board.getByRole("button", { name: /Build the account settings/ }),
  ).toBeVisible();
  assert.equal(state.calls.filter((call) => call.method === "POST").length, 1);
  assert.equal(
    state.calls.find((call) => call.method === "POST").path,
    "/tasks",
  );
  await page.reload();
  await expect(
    board.getByRole("button", { name: /Build the account settings/ }),
  ).toBeVisible();
  assert.equal(state.calls.filter((call) => call.method === "POST").length, 1);
});

test("the board distinguishes undelivered work from completion and account sessions", async (t) => {
  const { page, state } = await setup(t, { view: "board" });
  state.agents = [
    {
      ...session("waiting", "Prepare the feature", "done"),
      hasPendingTask: true,
    },
    {
      ...session("uncertain", "Check delivery", "working"),
      taskDeliveryUncertain: true,
    },
    { ...session("login", "Codex sign in", "working"), role: "authentication" },
  ];
  const board = page.getByRole("region", { name: "Task board", exact: true });
  await expect(
    board
      .getByRole("region", { name: "Waiting", exact: true })
      .locator('[data-task-id="waiting"]'),
  ).toContainText("Waiting for CLI setup");
  await expect(
    board
      .getByRole("region", { name: "Needs attention", exact: true })
      .locator('[data-task-id="uncertain"]'),
  ).toContainText("Check task delivery");
  await expect(
    board
      .getByRole("region", { name: "Done", exact: true })
      .locator("[data-task-id]"),
  ).toHaveCount(0);
  await expect(board.locator('[data-task-id="login"]')).toHaveCount(0);
  await expect(page.locator(".am-worker-count")).toContainText(
    "0 agents working",
  );
  await expect(page.locator(".am-auth-sessions")).toContainText(
    "Codex sign in",
  );
  assert.equal(state.calls.filter((call) => call.method === "POST").length, 0);
});

test("a board task can call Claude without changing the VM's selected Codex environment", async (t) => {
  const { page, state } = await setup(t, { view: "board" });
  await page.getByRole("button", { name: "New task", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "New task", exact: true });
  await dialog
    .getByRole("radio", { name: "Claude Code for this task", exact: true })
    .check();
  await dialog
    .getByRole("textbox", { name: "Task", exact: true })
    .fill("Review the API changes");
  await dialog.getByRole("button", { name: "Start task", exact: true }).click();
  const task = page
    .getByRole("region", { name: "Task board", exact: true })
    .getByRole("button", { name: /Review the API changes/ });
  await expect(task.locator('[data-agent-kind="claude"]')).toBeVisible();
  await page
    .locator(".am-quick-sidebar")
    .getByRole("button", { name: "Connect accounts", exact: true })
    .click();
  await expect(
    page.getByRole("radio", { name: "Codex", exact: true }),
  ).toBeChecked();
  const calls = state.calls.filter((call) => call.method === "POST");
  assert.equal(calls.length, 1);
  assert.equal(calls[0].path, "/tasks");
  assert.equal(calls[0].body.cli, "claude");
});

test("a disconnected board marks its last observation and recovers without starting work", async (t) => {
  const { page, state } = await setup(t, { view: "board" });
  const card = page
    .getByRole("region", { name: "Task board", exact: true })
    .locator('[data-task-id="builder"]');
  await expect(card).toContainText("Working");
  state.fail = true;
  await expect(page.locator(".am-connection-error")).toBeVisible();
  await expect(card).toContainText("Last known state");
  await expect(page.locator(".am-worker-count")).toContainText(
    "Checking connection",
  );
  state.fail = false;
  await expect(card).toContainText("Working");
  await expect(page.locator(".am-connection-error")).toHaveCount(0);
  assert.equal(state.calls.filter((call) => call.method === "POST").length, 0);
});

test("failed preparation retries keep the operation ID and install only the selected CLI", async (t) => {
  const { page, fixture, state } = await setup(t, {
    empty: true,
    select: false,
    ready: false,
  });
  const id = "a3524395-661c-4bee-aefc-bc733592f15d";
  let posted = [];
  fixture.handlers.set("POST /api/workspace-setups", ({ body, send }) => {
    posted.push(body);
    send(202, {
      ...body,
      vmId: id,
      state: posted.length === 1 ? "failed" : "ready",
      phase: posted.length === 1 ? "image" : "complete",
      error: posted.length === 1 ? "Download unavailable" : null,
      log: posted.length === 1 ? "Ubuntu OCI import failed: manifest unavailable" : "",
    });
  });
  const row = vm(10, { id, state: "running", name: "my-workspace" });
  fixture.state.vms.push(row);
  fixture.handlers.set(`GET /api/vms/${id}`, ({ send }) => send(200, row));
  await page
    .getByRole("button", { name: "Create workspace", exact: true })
    .click();
  await expect(page.getByRole("alert")).toContainText("Download unavailable");
  await page.getByText("Installation log", { exact: true }).click();
  await expect(page.locator(".awc-form pre")).toContainText(
    "Ubuntu OCI import failed: manifest unavailable",
  );
  await page
    .getByRole("button", { name: "Continue preparation", exact: true })
    .click();
  await expect
    .poll(
      () =>
        state.calls.filter((c) => c.method === "POST" && c.path === "/prepare")
          .length,
    )
    .toBe(1);
  assert.deepEqual(posted[0], posted[1]);
  state.ready = true;
  state.preparing = false;
  await expect(
    page.getByRole("heading", { name: "Choose your AI and repository" }),
  ).toBeVisible({ timeout: 15000 });
  assert.equal(
    state.calls.filter((c) => c.method === "POST" && c.path === "/prepare")
      .length,
    1,
  );
  assert.equal(
    state.calls.filter((c) => c.method === "POST" && c.path === "/tasks")
      .length,
    0,
  );
});

test("a prepared VM with another controller cannot receive automatic CLI installation", async (t) => {
  const { page, fixture, state } = await setup(t, {
    empty: true,
    select: false,
    mismatch: true,
    ready: false,
  });
  const id = "76f11710-583c-42b1-8eaf-ff5a186921ee";
  const row = vm(11, { id, state: "running" });
  fixture.state.vms.push(row);
  fixture.handlers.set(`GET /api/vms/${id}`, ({ send }) => send(200, row));
  fixture.handlers.set("POST /api/workspace-setups", ({ body, send }) =>
    send(202, {
      ...body,
      vmId: id,
      state: "ready",
      phase: "complete",
      error: null,
    }),
  );
  await page
    .getByRole("button", { name: "Create workspace", exact: true })
    .click();
  await expect(page.getByRole("alert")).toContainText("host");
  assert.equal(state.calls.filter((c) => c.method === "POST").length, 0);
  await expect(
    page.getByRole("button", {
      name: "Open the prepared MicroVM",
      exact: true,
    }),
  ).toBeVisible();
});

test(
  "quick start verifies the app, proves its fixed HTTPS address and returns to real AI work",
  { timeout: 45000 },
  async (t) => {
    const { page, state, fixture } = await setup(t);
    const sidebar = page.locator(".am-quick-sidebar");
    await sidebar.getByRole("button", { name: "Run app", exact: true }).click();
    const run = page.getByRole("button", {
      name: "Verify and run",
      exact: true,
    });
    await expect(run).toBeDisabled();
    state.agents.forEach((agent) => (agent.state = "done"));
    await expect(run).toBeEnabled({ timeout: 15000 });
    await run.click();
    const submitted = state.calls.find(
      (c) =>
        c.method === "POST" &&
        c.path.endsWith("/automation") &&
        c.body.action === "run",
    );
    assert.equal(
      submitted.endpoint.includes(`/microvms/${fixture.state.vms[0].id}/`),
      true,
    );
    assert.equal(
      state.calls.some((c) => c.body?.action === "start_preview"),
      false,
    );
    state.automation.run = {
      ...state.automation.run,
      state: "passed",
      phase: "complete",
      appReady: true,
      appRunning: true,
      exitCode: 0,
      log: "Real fixture command completed",
    };
    await page
      .getByRole("button", { name: "Refresh app status", exact: true })
      .click();
    await page
      .getByRole("button", { name: "Connect app address", exact: true })
      .click();
    let proofStatus = 503;
    let savedTarget;
    const publicUrl = "https://workspace-demo.firecrab.dev";
    await page.route("https://workspace-demo.firecrab.dev/**", (route) =>
      route.fulfill({
        contentType: "text/html",
        body: "<!doctype html><title>App preview fixture</title><p>Fixture app</p>",
      }),
    );
    fixture.handlers.set("GET /api/pipe", ({ send }) =>
      send(200, fixture.state.pipe),
    );
    fixture.handlers.set(
      "POST /api/pipe/firecrab-tunnels",
      ({ body, send }) => {
        savedTarget = body.target;
        fixture.state.pipe.firecrabTunnels = [
          {
            subdomain: "workspace-demo",
            publicUrl,
            target: body.target,
            connectorId: "guest-preview",
            connected: true,
            error: null,
          },
        ];
        send(200, fixture.state.pipe);
      },
    );
    fixture.handlers.set(
      "POST /api/pipe/firecrab-tunnels/workspace-demo/check",
      ({ send }) =>
        send(200, {
          url: publicUrl,
          status: proofStatus,
          ok: proofStatus === 200,
          latencyMs: 18,
        }),
    );
    const token = "a".repeat(64);
    await page
      .getByLabel("Firecrab connection token", { exact: true })
      .fill(token);
    await page
      .getByRole("button", { name: "Connect Firecrab address", exact: true })
      .click();
    await expect(page.getByRole("alert")).toContainText("503");
    assert.deepEqual(savedTarget, {
      type: "preview",
      vm_id: fixture.state.vms[0].id,
      url: "https://qa-preview.trycloudflare.com",
    });

    await expect(
      page.getByTitle("Running app preview", { exact: true }),
    ).toHaveCount(0);
    assert.equal(
      (await page.evaluate(() => JSON.stringify(localStorage))).includes(token),
      false,
    );
    proofStatus = 200;
    await page
      .getByRole("button", { name: "Check HTTPS", exact: true })
      .click();
    await expect(page.getByRole("status")).toContainText("HTTPS 200");
    await page
      .getByRole("button", { name: "Open app preview", exact: true })
      .click();
    await expect(
      page.getByTitle("Running app preview", { exact: true }),
    ).toHaveAttribute("src", publicUrl);
    await page.screenshot({
      path: join(tmpdir(), "firecrab-workspace-preview-1440.png"),
    });
    const previous = state.calls.filter(
      (c) => c.path === "/tasks" && c.method === "POST",
    ).length;
    await page
      .getByRole("button", { name: "Ask AI for another change", exact: true })
      .click();
    const dialog = page.getByRole("dialog", { name: "New task", exact: true });
    await expect(dialog).toBeVisible();
    assert.equal(
      state.calls.filter((c) => c.path === "/tasks" && c.method === "POST")
        .length,
      previous,
    );
    await dialog
      .getByRole("textbox", { name: "Task", exact: true })
      .fill("Add a delete button");
    await dialog
      .getByRole("button", { name: "Start task", exact: true })
      .click();
    await expect
      .poll(
        () =>
          state.calls.filter((c) => c.path === "/tasks" && c.method === "POST")
            .length,
      )
      .toBe(previous + 1);
  },
);
