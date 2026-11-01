import { test } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright-core";
import { expect } from "@playwright/test";
import { createFixtureServer, vm } from "./fixture-server.mjs";

async function setup(t, state = "running") {
  const fixture = await createFixtureServer(
    fileURLToPath(new URL("../dist", import.meta.url)),
    0,
  );
  t.after(() => fixture.close());
  const machine = vm(0, {
    id: "vm-state-console",
    name: "console-state",
    state,
  });
  fixture.state.vms = [machine];
  const control = {
    missing: false,
    unavailable: false,
    connections: 0,
    failedReads: 0,
    sockets: [],
  };
  fixture.handlers.set("GET /api/vms/vm-state-console", ({ send }) => {
    if (control.missing)
      return send(404, {
        error: {
          code: "not_found",
          message: "resource not found",
          requestId: "qa",
        },
      });
    if (control.unavailable) {
      control.failedReads++;
      return send(503, {
        error: {
          code: "unavailable",
          message: "temporarily unavailable",
          requestId: "qa",
        },
      });
    }
    return send(200, machine);
  });
  const browser = await chromium.launch({ headless: true });
  t.after(() => browser.close());
  const page = await browser.newPage();
  await page.addInitScript(() => localStorage.setItem("firecrab.locale", "en"));
  await page.routeWebSocket("**/ws/vms/vm-state-console/console", (socket) => {
    control.connections++;
    control.sockets.push(socket);
    socket.send(`SESSION_${control.connections}\r\nprompt# `);
  });
  const open = () => page.goto(`${fixture.url}/#/console/vm-state-console`);
  return { page, fixture, machine, control, open };
}

test("a missing VM does not open or repeatedly reconnect a console socket", async (t) => {
  const { page, control, open } = await setup(t);
  control.missing = true;
  await open();
  await expect(page.locator(".console-status")).toHaveText("VM not found");
  await expect(
    page.getByRole("button", { name: "Reconnect", exact: true }),
  ).toHaveCount(0);
  await page.waitForTimeout(3500);
  assert.equal(control.connections, 0);
});

test("stopped and paused VMs wait for running before attaching and keep terminal history", async (t) => {
  const { page, machine, control, open } = await setup(t, "stopped");
  await open();
  await expect(page.locator(".console-status")).toHaveText("VM not running");
  assert.equal(control.connections, 0);
  machine.state = "running";
  await expect(page.locator(".console-status")).toHaveText("Connected", {
    timeout: 6000,
  });
  await expect(page.locator(".xterm-rows")).toContainText("SESSION_1");
  const terminal = await page.locator(".xterm").elementHandle();
  machine.state = "paused";
  await expect(page.locator(".console-status")).toHaveText("VM not running", {
    timeout: 6000,
  });
  await page.waitForTimeout(3500);
  assert.equal(control.connections, 1);
  await expect(page.locator(".xterm-rows")).toContainText("SESSION_1");
  machine.state = "running";
  await expect(page.locator(".console-status")).toHaveText("Connected", {
    timeout: 6000,
  });
  await expect(page.locator(".xterm-rows")).toContainText("SESSION_2");
  assert.equal(
    await terminal.evaluate((el) => el === document.querySelector(".xterm")),
    true,
  );
  assert.equal(control.connections, 2);
});

test("a deleted VM disconnects once and a transient metadata failure keeps a running connection", async (t) => {
  const { page, control, open } = await setup(t);
  await open();
  await expect(page.locator(".console-status")).toHaveText("Connected");
  control.unavailable = true;
  await expect
    .poll(() => control.failedReads, { timeout: 6000 })
    .toBeGreaterThan(0);
  await expect(page.locator(".console-status")).toHaveText("Connected");
  assert.equal(control.connections, 1);
  control.unavailable = false;
  control.missing = true;
  await expect(page.locator(".console-status")).toHaveText("VM not found", {
    timeout: 6000,
  });
  await page.waitForTimeout(3500);
  assert.equal(control.connections, 1);
  await expect(page.locator(".xterm-rows")).toContainText("SESSION_1");
});

test("an older running response cannot reconnect a VM after a newer stopped response", async (t) => {
  const { page, machine, control, open } = await setup(t);
  await open();
  await expect(page.locator(".console-status")).toHaveText("Connected");
  let release,
    held = false;
  t.after(() => release?.());
  await page.route("**/api/vms/vm-state-console", async (route) => {
    if (!held) {
      held = true;
      const old = structuredClone(machine);
      await new Promise((resolve) => {
        release = () => {
          release = undefined;
          resolve();
        };
      });
      return route.fulfill({ json: old });
    }
    return route.fulfill({ json: machine });
  });
  await expect.poll(() => held, { timeout: 6000 }).toBe(true);
  machine.state = "stopped";
  await expect(page.locator(".console-status")).toHaveText("VM not running", {
    timeout: 6000,
  });
  release();
  await page.waitForTimeout(1200);
  await expect(page.locator(".console-status")).toHaveText("VM not running");
  assert.equal(control.connections, 1);
});
