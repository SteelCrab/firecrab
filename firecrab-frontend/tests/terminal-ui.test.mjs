import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { chromium, firefox, webkit } from "playwright-core";
import { expect } from "@playwright/test";
import { createFixtureServer, vm } from "./fixture-server.mjs";

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
