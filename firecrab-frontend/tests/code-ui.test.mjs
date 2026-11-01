import { test } from "node:test";
import { tmpdir } from "node:os";
import { join } from "node:path";
const artifacts =
  process.env.FIRECRAB_CODE_ARTIFACT_DIR ?? join(tmpdir(), "firecrab-code-ui");
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { readFileSync, mkdirSync } from "node:fs";
import { chromium } from "playwright-core";
import { expect } from "@playwright/test";
import { createFixtureServer } from "./fixture-server.mjs";

const base = "/api/code/workspaces",
  account = "22222222-2222-4222-8222-222222222222",
  vm = "33333333-3333-4333-8333-333333333333",
  instance = "44444444-4444-4444-8444-444444444444",
  operation = "55555555-5555-4555-8555-555555555555";
const profiles = JSON.parse(
  readFileSync(
    new URL(
      "../../firecrab-api/src/code_agent_auth/adapters.json",
      import.meta.url,
    ),
    "utf8",
  ),
);
async function setup(locale = "en", width = 1280) {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage({ viewport: { width, height: 1000 } });
  const state = {
    record: null,
    runtime: null,
    editor: null,
    rows: [],
    writes: [],
    connected: true,
    provider: "openai",
    dropRun: false,
    dropVm: false,
    creations: 0,
    errors: [],
  };
  page.setDefaultTimeout(10000);
  page.on("dialog", (dialog) => {
    if (dialog.type() === "beforeunload") void dialog.accept();
    else {
      state.errors.push(dialog.message());
      void dialog.dismiss();
    }
  });
  page.on("pageerror", (e) => state.errors.push(e.message));
  await page.addInitScript(
    (locale) => localStorage.setItem("firecrab.locale", locale),
    locale,
  );
  await page.route(
    /\/api\/(code\/|pipe\/ai\/connections|pipe\/auth|images|micro-networks|storage\/roots)/,
    async (route) => {
      const req = route.request(),
        path = new URL(req.url()).pathname,
        send = (json, status = 200) => route.fulfill({ json, status });
      const body = req.method() === "GET" ? null : req.postDataJSON();
      if (body) state.writes.push({ path, body });
      if (path === "/api/code/agent-auth/adapters") return send(profiles);
      if (path === "/api/pipe/ai/connections")
        return send(
          state.connected
            ? [
                {
                  id: account,
                  name: "Development AUTH",
                  provider: state.provider,
                  authentication: "api_key",
                },
              ]
            : [],
        );
      if (path === "/api/pipe/auth")
        return send([{ provider: "github", account: "octocat" }]);
      if (path === "/api/code/github/repositories")
        return send({
          account: "octocat",
          repositories: [
            {
              owner: "team",
              repository: "web-app",
              defaultBranch: "main",
              language: "TypeScript",
              private: true,
            },
          ],
        });
      if (path === "/api/code/github/branches")
        return send({
          account: "octocat",
          owner: "team",
          repository: "web-app",
          branches: ["main", "feature/work"],
        });
      if (path === "/api/images")
        return send([{ alias: "ubuntu-26.04", installed: true, minDiskGb: 8 }]);
      if (path === "/api/micro-networks")
        return send([{ id: vm, name: "Development", internetEnabled: true }]);
      if (path === "/api/storage/roots")
        return send([{ id: "default", kind: "default" }]);
      const record = () =>
        state.record
          ? {
              ...state.record,
              connections: {
                github: "present_unverified",
                ai: !state.connected
                  ? "missing"
                  : state.provider === "openai"
                    ? "present_unverified"
                    : "incompatible",
              },
            }
          : null;
      if (path === base) return send(state.record ? [record()] : []);
      const saved = path.match(/^\/api\/code\/workspaces\/([0-9a-f-]+)$/);
      if (saved) {
        if (req.method() === "PUT") {
          assert.equal(body.expectedRevision, state.record?.revision ?? 0);
          state.record = {
            id: saved[1],
            revision: (state.record?.revision ?? 0) + 1,
            spec: body.spec,
          };
          return send(record());
        }
        return state.record
          ? send(record())
          : send({ error: { code: "not_found", message: "not found" } }, 404);
      }
      if (path.endsWith("/runtime/stop") && req.method() === "POST") {
        state.runtime = {
          ...state.runtime,
          action: "stop",
          phase: "stopped",
          vmState: "stopped",
          operationId: "66666666-6666-4666-8666-666666666666",
        };
        return send(state.runtime, 202);
      }
      if (path.endsWith("/runtime")) {
        if (req.method() === "POST") {
          if (state.dropVm) {
            state.dropVm = false;
            return route.abort("failed");
          }
          assert.equal(body.expectedRevision, state.record.revision);
          state.creations++;
          state.runtime = {
            workspaceId: state.record.id,
            configurationRevision: state.record.revision,
            vmId: vm,
            runtimeId: instance,
            operationId: operation,
            action: "start",
            phase: "queued",
            projectPath: "/home/fc-code/project",
            initialCommit: "a".repeat(40),
            vmState: "running",
            error: null,
            placement: {
              template: { name: body.template, version: "v1" },
              microNetworkId: body.microNetworkId,
              storageRoot: body.storageRoot,
            },
          };
        }
        return state.runtime
          ? send(state.runtime)
          : send({ error: { code: "not_found", message: "not found" } }, 404);
      }
      if (path.endsWith("/editor")) {
        if (req.method() === "POST") {
          assert.deepEqual(body, {
            expectedRevision: state.record.revision,
            vmId: vm,
            runtimeId: instance,
            operationId: operation,
          });
          state.editor = {
            workspaceId: state.record.id,
            revision: state.record.revision,
            vmId: vm,
            runtimeId: instance,
            operationId: operation,
            phase: "ready",
            version: "4.144.0",
          };
        }
        return send(state.editor);
      }
      if (path.endsWith("/agent-runs")) return send(state.rows);
      const run = path.match(/\/agent-runs\/([0-9a-f-]+)(\/cancel)?$/);
      if (run) {
        let row = state.rows.find((v) => v.id === run[1]);
        if (run[2]) {
          if (row) row.phase = "cancelled";
          return send({
            id: run[1],
            workspaceId: state.record.id,
            run: row ?? null,
          });
        }
        if (req.method() === "GET")
          return row ? send(row) : send({ error: { code: "not_found" } }, 404);
        assert.deepEqual(Object.keys(body).sort(), [
          "expectedRevision",
          "model",
          "operationId",
          "prompt",
          "runtimeId",
          "vmId",
        ]);
        assert.equal(body.vmId, vm);
        assert.equal(body.runtimeId, instance);
        assert.equal(body.expectedRevision, state.record.revision);
        const profile = profiles.find(
          (p) =>
            p.agent === state.record.spec.agent.agent &&
            p.provider === "openai" &&
            p.authentication === "api_key",
        );
        row = {
          id: run[1],
          workspaceId: state.record.id,
          revision: body.expectedRevision,
          connectionId: account,
          agent: state.record.spec.agent.agent,
          adapterId: profile.id,
          cliVersion: profile.cliVersion,
          model: body.model,
          modelsUsed: [],
          vmId: vm,
          runtimeId: instance,
          operationId: operation,
          phase: "queued",
          output: "",
          truncated: false,
          providerRequests: 0,
          error: null,
        };
        state.rows.unshift(row);
        if (state.dropRun) {
          state.dropRun = false;
          return route.abort("failed");
        }
        return send(row, 202);
      }
      return send({ error: { code: "not_found" } }, 404);
    },
  );
  return {
    fixture,
    browser,
    page,
    state,
    close: async () => {
      await browser.close();
      await fixture.close();
    },
  };
}
async function create(s, ko = false) {
  const { page, state } = s;
  await page.goto(`${s.fixture.url}/#/code/new`);
  await page
    .getByRole("combobox", {
      name: ko ? "GitHub 저장소" : "GitHub repository",
      exact: true,
    })
    .selectOption("team/web-app");
  await expect(page.locator("#code-setup select:visible")).toHaveCount(1);
  await page
    .getByRole("button", {
      name: ko ? "워크스페이스 생성" : "Create workspace",
      exact: true,
    })
    .click();
  await expect(page.locator("#code-editor h2")).toHaveText("web-app");
  assert.deepEqual(state.record.spec.agent, {
    agent: "codex",
    connectionId: account,
  });
}
for (const [locale, width] of [
  ["en", 1280],
  ["en", 390],
  ["ko", 1280],
  ["ko", 390],
])
  test(`CODE integrated repository → runtime → editor → assigned AUTH, ${locale} ${width}px`, async () => {
    const s = await setup(locale, width),
      { page, state } = s,
      ko = locale === "ko";
    try {
      await create(s, ko);
      await page
        .getByRole("button", {
          name: ko ? "워크스페이스 시작" : "Start workspace",
          exact: true,
        })
        .click();
      await expect.poll(() => state.runtime?.phase).toBe("queued");
      state.runtime.phase = "ready";
      await page
        .getByRole("button", {
          name: ko ? "편집 도구 준비" : "Prepare editor tools",
          exact: true,
        })
        .click();
      await expect(
        page.getByRole("link", {
          name: ko ? "Web VS Code 열기" : "Open Web VS Code",
          exact: true,
        }),
      ).toHaveAttribute("href", `${base}/${state.record.id}/editor/open`);
      await expect(
        page.getByRole("link", {
          name: ko ? "터미널 열기" : "Open terminal",
          exact: true,
        }),
      ).toHaveAttribute("href", `${base}/${state.record.id}/terminal/open`);
      const prompt = page.getByLabel(ko ? "작업 지시" : "Task", {
          exact: true,
        }),
        run = page.getByRole("button", {
          name: ko ? "에이전트 실행" : "Run agent",
          exact: true,
        });
      await prompt.fill("Update README.md in this checkout.");
      state.dropRun = true;
      await run.click();
      await expect.poll(() => state.rows.length).toBe(1);
      await page.reload();
      await expect(page.locator("#code-agent-execution")).toContainText(
        ko ? "대기 중" : "Queued",
      );
      assert.equal(
        state.writes.filter((w) => /\/agent-runs\/[0-9a-f-]+$/.test(w.path))
          .length,
        1,
      );
      state.rows[0].phase = "succeeded";
      state.rows[0].output = "Updated README.md";
      await expect(
        page.getByLabel(ko ? "에이전트 출력" : "Agent output", { exact: true }),
      ).toHaveText("Updated README.md");
      state.provider = "anthropic";
      await prompt.fill("Must remain blocked");
      await expect(run).toBeDisabled();
      state.connected = false;
      await expect(page.locator("#code-agent-execution")).not.toContainText(
        "Development AUTH",
      );
      await expect(run).toBeDisabled();
      state.connected = true;
      state.provider = "openai";
      await expect(run).toBeEnabled();
      assert.equal(state.creations, 1);
      assert.equal(state.rows[0].vmId, state.runtime.vmId);
      assert.equal(
        state.rows[0].connectionId,
        state.record.spec.agent.connectionId,
      );
      assert.equal(
        await page.evaluate(
          () => document.documentElement.scrollWidth <= innerWidth,
        ),
        true,
      );
      assert.deepEqual(state.errors, []);
      if (locale === "en") {
        mkdirSync(artifacts, { recursive: true });
        await page.screenshot({
          path: join(artifacts, `code-integrated-en-${width}.png`),
          fullPage: true,
        });
      }
    } finally {
      await s.close();
    }
  });
test("CODE unknown provisioning survives reload and requires an exact explicit retry", async () => {
  const s = await setup(),
    { page, state } = s;
  try {
    await create(s);
    state.dropVm = true;
    await page
      .getByRole("button", { name: "Start workspace", exact: true })
      .click();
    await expect(
      page.getByRole("button", { name: "Check operation", exact: true }),
    ).toBeVisible();
    await page.reload();
    await page
      .getByRole("button", { name: "Check operation", exact: true })
      .click();
    await page
      .getByRole("button", { name: "Retry same request", exact: true })
      .click();
    await expect.poll(() => state.creations).toBe(1);
    const attempts = state.writes.filter((w) => w.path.endsWith("/runtime"));
    assert.equal(attempts.length, 2);
    assert.deepEqual(attempts[0], attempts[1]);
    assert.deepEqual(state.errors, []);
  } finally {
    await s.close();
  }
});
test("CODE session storage failure prevents agent dispatch", async () => {
  const s = await setup(),
    { page, state } = s;
  try {
    await create(s);
    await page
      .getByRole("button", { name: "Start workspace", exact: true })
      .click();
    await expect.poll(() => state.runtime?.phase).toBe("queued");
    state.runtime.phase = "ready";
    await page.getByLabel("Task", { exact: true }).fill("Update README");
    const run = page.getByRole("button", { name: "Run agent", exact: true });
    await expect(run).toBeEnabled();
    await page.evaluate(() => {
      Storage.prototype.setItem = () => {
        throw new Error("blocked");
      };
    });
    await run.click();
    await expect(page.getByRole("alert")).toContainText("session storage");
    assert.equal(state.rows.length, 0);
    assert.deepEqual(state.errors, []);
  } finally {
    await s.close();
  }
});

test("CODE can stop a retained VM after setup failure", async () => {
  const s = await setup(),
    { page, state } = s;
  try {
    await create(s);
    await page
      .getByRole("button", { name: "Start workspace", exact: true })
      .click();
    await expect.poll(() => state.runtime?.phase).toBe("queued");
    state.runtime.phase = "failed";
    state.runtime.error = "sync_failed";
    await expect(
      page.getByRole("button", { name: "Start workspace", exact: true }),
    ).toBeDisabled();
    await page.locator("#code-workspace-settings > summary").click();
    await page
      .getByRole("button", { name: "Stop MicroVM", exact: true })
      .click();
    await expect.poll(() => state.runtime.phase).toBe("stopped");
    assert.equal(state.creations, 1);
    assert.deepEqual(state.writes.at(-1).body, { expectedRevision: 1 });
    assert.deepEqual(state.errors, []);
  } finally {
    await s.close();
  }
});

test("CODE earlier runs stay available and output remains text", async () => {
  const s = await setup(),
    { page, state } = s;
  try {
    await create(s);
    const profile = profiles.find(
      (p) => p.agent === "codex" && p.authentication === "api_key",
    );
    const row = {
      workspaceId: state.record.id,
      revision: 1,
      connectionId: account,
      agent: "codex",
      adapterId: profile.id,
      cliVersion: profile.cliVersion,
      model: "",
      modelsUsed: [],
      vmId: vm,
      runtimeId: instance,
      operationId: operation,
      phase: "succeeded",
      output: "Latest result",
      truncated: false,
      providerRequests: 1,
      error: null,
    };
    state.rows = [
      { ...row, id: "77777777-7777-4777-8777-777777777777" },
      {
        ...row,
        id: "88888888-8888-4888-8888-888888888888",
        output: "<img src=x onerror=alert(1)>",
      },
    ];
    await page.getByText("Earlier runs", { exact: true }).click();
    await expect(
      page.getByLabel("Agent output", { exact: true }).nth(1),
    ).toHaveText(state.rows[1].output);
    await expect(page.locator("#code-agent-execution img")).toHaveCount(0);
    assert.deepEqual(state.errors, []);
  } finally {
    await s.close();
  }
});

test("CODE terminal bundle connects with binary input and bounded resize messages", async () => {
  const s = await setup(),
    { page } = s;
  const messages = [];
  try {
    await page.routeWebSocket("**/_fc/terminal/socket", (socket) => {
      socket.onMessage((data) => messages.push(data));
      socket.send(Buffer.from("Workspace terminal ready\r\n"));
    });
    await page.goto(`${s.fixture.url}/code-terminal.html`);
    await expect(page.locator("#terminal-status")).toHaveText(
      "/home/fc-code/project",
    );
    await page.locator(".xterm-helper-textarea").pressSequentially("pwd");
    await expect
      .poll(() =>
        messages
          .map((v) => (typeof v === "string" ? v : Buffer.from(v).toString()))
          .join(""),
      )
      .toContain("pwd");
    const sizes = messages
      .filter((v) => typeof v === "string")
      .map((v) => JSON.parse(v));
    assert.ok(sizes.length > 0);
    assert.ok(
      sizes.every(
        (v) => v.rows >= 2 && v.rows <= 1000 && v.cols >= 2 && v.cols <= 1000,
      ),
    );
    await page.getByRole("button", { name: "Disconnect", exact: true }).click();
    await expect(page.locator("#terminal-status")).toContainText(
      "Disconnected",
    );
  } finally {
    await s.close();
  }
});
