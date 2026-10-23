import { expect, test } from "@playwright/test";
import { openDashboard, vm } from "./support/mockApi";

const VM = vm("terminal-test", "7f3a9c2e-1b4d-4e8a-9c3f-2d1e0a5b6c7d", "running");

for (const engine of ["ghostty", "xterm"]) {
  test.describe(`${engine} terminal`, () => {
    test("preserves split UTF-8 output, input and log export", async ({ page }) => {
      const received: string[] = [];
      await page.routeWebSocket(`**/ws/vms/${VM.id}/console`, socket => {
        const bytes = Buffer.from("한글 출력 é 👩‍💻 😀\r\nprompt# ");
        socket.send(bytes.subarray(0, 2));
        socket.send(bytes.subarray(2, 5));
        socket.send(bytes.subarray(5));
        socket.onMessage(message => {
          received.push(typeof message === "string" ? message : message.toString("utf8"));
        });
      });
      await openDashboard(page, {
        vms: [VM], hash: `?terminalEngine=${engine}#/console/${VM.id}`,
        routes: { [`/api/vms/${VM.id}`]: VM, [`/api/vms/${VM.id}/log`]: { consoleLog: "server log" } },
      });
      await expect(page.locator(".terminal-output")).toContainText("한글 출력 é 👩‍💻 😀");
      await expect(page.locator(`[data-terminal-engine=${engine}]`)).toHaveCount(1);
      await page.locator(".terminal-emulator textarea").focus();
      await page.keyboard.type("echo test");
      await page.keyboard.press("Enter");
      await expect.poll(() => received.join("")).toBe("echo test\r");

      const download = page.waitForEvent("download");
      await page.getByRole("group", { name: "Export log" }).getByRole("button", { name: "Save log", exact: true }).click();
      const file = await download;
      const stream = await file.createReadStream();
      const chunks: Buffer[] = [];
      for await (const chunk of stream!) chunks.push(Buffer.from(chunk));
      const text = Buffer.concat(chunks).toString("utf8");
      expect(text).toContain("server log");
      expect(text).toContain("한글 출력 é 👩‍💻 😀");
    });
  });
}

test("Ghostty composition commits once and keeps terminal shortcuts local", async ({ page }) => {
  const received: string[] = [];
  await page.routeWebSocket(`**/ws/vms/${VM.id}/console`, socket => {
    socket.send("prompt# ");
    socket.onMessage(message => received.push(typeof message === "string" ? message : message.toString("utf8")));
  });
  await openDashboard(page, { vms: [VM], hash: `#/console/${VM.id}`, routes: { [`/api/vms/${VM.id}`]: VM } });
  await expect(page.locator(".console-status")).toHaveText("Connected");
  await page.getByRole("button", { name: "Terminal only", exact: true }).click();
  await page.locator(".terminal-emulator textarea").evaluate(textarea => {
    textarea.dispatchEvent(new CompositionEvent("compositionstart", { bubbles: true }));
    textarea.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", isComposing: true, bubbles: true, cancelable: true }));
    textarea.dispatchEvent(new CompositionEvent("compositionend", { data: "한글", bubbles: true }));
  });
  await expect.poll(() => received.join("")).toBe("한글");
  await expect(page.locator(".console-header")).toHaveCount(0);
  await page.locator(".terminal-emulator textarea").focus();
  await page.keyboard.press("Control+k");
  await expect(page.getByRole("dialog", { name: /command/i })).toHaveCount(0);
  await expect.poll(() => received.join("")).toBe("한글\x0b");
});

test("a failed WASM initialization is visible and does not connect the socket", async ({ page }) => {
  await page.addInitScript(() => {
    WebAssembly.instantiate = (() => Promise.reject(new Error("WASM initialization unavailable"))) as typeof WebAssembly.instantiate;
  });
  let connections = 0;
  await page.routeWebSocket(`**/ws/vms/${VM.id}/console`, () => { connections++; });
  await openDashboard(page, { vms: [VM], hash: `#/console/${VM.id}`, routes: { [`/api/vms/${VM.id}`]: VM } });
  await expect(page.getByRole("alert")).toBeVisible();
  await expect(page.locator(".console-status")).toHaveText("Connection failed");
  expect(connections).toBe(0);
});

test("Ghostty can create a new session after displaying complex graphemes", async ({ page }) => {
  let connections = 0;
  await page.routeWebSocket(`**/ws/vms/${VM.id}/console`, socket => {
    connections++;
    socket.send(`session ${connections} é 👩‍💻\r\nprompt# `);
    socket.onMessage(() => socket.close({ code: 4000, reason: "session_ended" }));
  });
  await openDashboard(page, { vms: [VM], hash: `#/console/${VM.id}`, routes: { [`/api/vms/${VM.id}`]: VM } });
  for (let session = 1; session <= 3; session++) {
    await expect(page.locator(".terminal-output")).toContainText(`session ${session} é 👩‍💻`);
    await page.locator(".terminal-emulator textarea").focus();
    await page.keyboard.press("Enter");
    await expect(page.locator(".console-status")).toHaveText("Session ended");
    if (session < 3) await page.getByRole("button", { name: "New session", exact: true }).click();
  }
  expect(connections).toBe(3);
});

test.describe("Ghostty canvas resizing", () => {
  test.use({ deviceScaleFactor: 2 });

  test("loads external WASM and draws text after resizing at device scale 2", async ({ page }) => {
    const wasmResponses: { status: number; type: string }[] = [];
    page.on("response", response => {
      if (new URL(response.url()).pathname.endsWith(".wasm")) {
        wasmResponses.push({ status: response.status(), type: response.headers()["content-type"] });
      }
    });
    await page.routeWebSocket(`**/ws/vms/${VM.id}/console`, socket => socket.send("Firecrab resize 한글 é 👩‍💻\r\nprompt# "));
    await openDashboard(page, { vms: [VM], hash: `#/console/${VM.id}`, routes: { [`/api/vms/${VM.id}`]: VM } });
    await expect(page.locator(".terminal-output")).toContainText("Firecrab resize 한글 é 👩‍💻");
    await page.setViewportSize({ width: 1100, height: 800 });
    await page.getByRole("button", { name: "Terminal only", exact: true }).click();
    await expect.poll(async () => page.locator(".terminal-emulator canvas").evaluate(canvas => {
      const element = canvas as HTMLCanvasElement;
      const width = parseFloat(element.style.width);
      const context = element.getContext("2d")!;
      const pixels = context.getImageData(0, 0, Math.min(element.width, 300), 32).data;
      let bright = 0;
      for (let index = 0; index < pixels.length; index += 4) {
        if (pixels[index] > 100 && pixels[index + 1] > 100 && pixels[index + 2] > 100) bright++;
      }
      return { scale: element.width / width, transform: context.getTransform().a, hasText: bright > 100 };
    })).toEqual({ scale: 2, transform: 2, hasText: true });
    expect(wasmResponses.some(response => response.status === 200 && response.type.includes("application/wasm"))).toBe(true);
  });
});
