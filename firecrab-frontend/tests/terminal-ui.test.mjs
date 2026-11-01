import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { chromium, firefox, webkit } from "playwright-core";
import { expect } from "@playwright/test";
import { createFixtureServer, vm } from "./fixture-server.mjs";
import { consoleResizeFixture } from "./console-resize-fixture.mjs";

test(
  "JavaScript terminal preserves split UTF-8 and forwards keyboard input",
  { timeout: 30000 },
  async () => {
    const browserType = { chromium, firefox, webkit }[
      process.env.FIRECRAB_TERMINAL_BROWSER || "chromium"
    ];
    const browser = await browserType.launch({
        headless: true,
        timeout: 15000,
      }),
      page = await browser.newPage();
    const fixture = await createFixtureServer(
      fileURLToPath(new URL("../dist", import.meta.url)),
      0,
    );
    const machine = vm(1, { id: "vm-terminal", state: "running" });
    fixture.state.vms = [machine];
    fixture.handlers.set("GET /api/vms/vm-terminal", ({ send }) =>
      send(200, machine),
    );
    fixture.handlers.set("GET /api/vms/vm-terminal/log", ({ send }) =>
      send(200, { consoleLog: "server log" }),
    );
    const received = [];
    await page.routeWebSocket("**/ws/vms/vm-terminal/console", (socket) => {
      const bytes = Buffer.from("한글 출력\r\nprompt# ");
      socket.send(bytes.subarray(0, 2));
      socket.send(bytes.subarray(2, 5));
      socket.send(bytes.subarray(5));
      socket.send("\x1b[?1h\x1b=");
      socket.onMessage((message) =>
        received.push(
          typeof message === "string"
            ? message
            : Buffer.from(message).toString("utf8"),
        ),
      );
    });
    try {
      await page.goto(`${fixture.url}/#/console/vm-terminal`);
      await expect(page.locator(".xterm-rows")).toContainText("한글 출력");
      await page.locator(".xterm-helper-textarea").focus();
      await page.keyboard.type("echo test");
      await page.keyboard.press("Enter");
      await expect.poll(() => received.join("")).toContain("echo test\r");
      await page.keyboard.press("ArrowUp");
      await expect.poll(() => received.join("")).toContain("\x1bOA");
    } finally {
      await page.close();
      await browser.close();
      await fixture.close();
    }
  },
);

test(
  "console negotiates shared guest geometry while preserving terminal data",
  { timeout: 30000 },
  async () => {
    const browserType = { chromium, firefox, webkit }[
      process.env.FIRECRAB_TERMINAL_BROWSER || "chromium"
    ];
    const browser = await browserType.launch({
      headless: true,
      timeout: 15000,
    });
    const fixture = await createFixtureServer(
      fileURLToPath(new URL("../dist", import.meta.url)),
      0,
    );
    const machine = vm(1, { id: "vm-resize", state: "running" });
    fixture.state.vms = [machine];
    fixture.handlers.set("GET /api/vms/vm-resize", ({ send }) =>
      send(200, machine),
    );
    const transport = consoleResizeFixture(fixture);
    const page = await browser.newPage({
      viewport: { width: 1400, height: 950 },
    });
    try {
      await page.goto(`${fixture.url}/#/console/vm-resize`);
      const geometry = page.locator(".console-geometry");
      await expect(geometry).toHaveAttribute("data-status", "applied");
      const original = await geometry.textContent();
      await expect(page.locator(".terminal-output")).toContainText(
        "guest resize ready",
      );
      await expect(page.locator(".xterm-rows")).not.toContainText(
        '"type":"resize"',
      );
      const second = await browser.newPage({
        viewport: { width: 800, height: 600 },
      });
      await second.goto(`${fixture.url}/#/console/vm-resize`);
      await expect(second.locator(".console-geometry")).toHaveAttribute(
        "data-status",
        "applied",
      );
      await expect(geometry).not.toHaveText(original);
      await expect
        .poll(
          async () =>
            (await geometry.textContent()) ===
            (await second.locator(".console-geometry").textContent()),
        )
        .toBe(true);
      const shared = await geometry.textContent();
      await second.close();
      await expect(geometry).not.toHaveText(shared);
      transport.unavailable();
      await expect(geometry).toHaveAttribute("data-status", "unavailable");
      await page.locator(".xterm-helper-textarea").focus();
      await page.keyboard.type("echo live");
      await page.keyboard.press("Enter");
      await expect
        .poll(() =>
          transport.received.filter((v) => typeof v === "string").join(""),
        )
        .toContain("echo live\r");
      expect(
        transport.received.some(
          (v) => v.type === "resize" && v.cols >= 2 && v.rows >= 2,
        ),
      ).toBe(true);
    } finally {
      await browser.close();
      transport.close();
      await fixture.close();
    }
  },
);
