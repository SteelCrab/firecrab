import { expect, test } from "@playwright/test";
import { openDashboard, vm } from "./support/mockApi";

const VM = vm("terminal-test", "7f3a9c2e-1b4d-4e8a-9c3f-2d1e0a5b6c7d", "running");

test.describe("WASM terminal", () => {
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
        vms: [VM], hash: `#/console/${VM.id}`,
        routes: { [`/api/vms/${VM.id}`]: VM, [`/api/vms/${VM.id}/log`]: { consoleLog: "server log" } },
      });
      await expect(page.locator(".terminal-output")).toContainText("한글 출력 é 👩‍💻 😀");
      await expect(page.locator("[data-terminal-engine=ghostty]")).toHaveCount(1);
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
    WebAssembly.instantiate = () => Promise.reject<never>(new Error("WASM initialization unavailable"));
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


test("uses WASM even with the retired xterm query and accepts mobile editing", async ({ page }) => {
  const received: string[] = [];
  const resources: string[] = [];
  page.on("request", request => resources.push(request.url()));
  await page.routeWebSocket(`**/ws/vms/${VM.id}/console`, socket => {
    socket.send("mobile input ready");
    socket.onMessage(message => received.push(message.toString()));
  });
  await openDashboard(page, { vms: [VM], hash: `?terminalEngine=xterm#/console/${VM.id}`, routes: { [`/api/vms/${VM.id}`]: VM } });
  await expect(page.locator("[data-terminal-engine=ghostty] canvas")).toBeVisible();
  await expect(page.locator(".terminal-output")).toContainText("mobile input ready");
  const input = page.getByRole("textbox", { name: "Terminal input" });
  await input.focus();
  await page.keyboard.insertText("모바일 é 👩‍💻");
  await input.evaluate(textarea => {
    textarea.dispatchEvent(new InputEvent("beforeinput", { inputType: "deleteContentBackward", bubbles: true, cancelable: true }));
    textarea.dispatchEvent(new InputEvent("beforeinput", { inputType: "insertParagraph", bubbles: true, cancelable: true }));
    (textarea as HTMLTextAreaElement).value = "fallback";
    textarea.dispatchEvent(new InputEvent("input", { inputType: "insertText", data: "fallback", bubbles: true }));
  });
  await expect.poll(() => received.join("")).toBe("모바일 é 👩‍💻\x7f\rfallback");
  expect(resources.some(url => /xterm(?:[/-]|\.)/.test(url))).toBe(false);
});

test("IME trailing input does not duplicate the committed syllables", async ({ page }) => {
  const received: string[] = [];
  await page.routeWebSocket(`**/ws/vms/${VM.id}/console`, socket => {
    socket.send("composition ready");
    socket.onMessage(message => received.push(message.toString()));
  });
  await openDashboard(page, { vms: [VM], hash: `#/console/${VM.id}`, routes: { [`/api/vms/${VM.id}`]: VM } });
  await expect(page.locator(".terminal-output")).toContainText("composition ready");
  await page.locator(".terminal-emulator textarea").evaluate(textarea => {
    textarea.dispatchEvent(new CompositionEvent("compositionstart", { bubbles: true }));
    textarea.dispatchEvent(new InputEvent("input", { inputType: "insertCompositionText", data: "한", isComposing: true, bubbles: true }));
    textarea.dispatchEvent(new CompositionEvent("compositionend", { data: "한글", bubbles: true }));
    textarea.dispatchEvent(new InputEvent("input", { inputType: "insertText", data: "한글", bubbles: true }));
  });
  await expect.poll(() => received.join("")).toBe("한글");
});

test("WASM modes control application arrows and bracketed paste", async ({ page }) => {
  const received: string[] = [];
  await page.routeWebSocket(`**/ws/vms/${VM.id}/console`, socket => {
    socket.send("\x1b[?1h\x1b[?2004happlication ready");
    socket.onMessage(message => received.push(message.toString()));
  });
  await openDashboard(page, { vms: [VM], hash: `#/console/${VM.id}`, routes: { [`/api/vms/${VM.id}`]: VM } });
  await expect(page.locator(".terminal-output")).toContainText("application ready");
  const input = page.locator(".terminal-emulator textarea");
  await input.focus();
  await page.keyboard.press("ArrowUp");
  await page.keyboard.press("Control+c");
  await input.evaluate(textarea => {
    const event = new ClipboardEvent("paste", { clipboardData: new DataTransfer(), bubbles: true, cancelable: true });
    // Firefox gives synthetic events their own clipboard store.
    event.clipboardData!.setData("text/plain", "한글 é 👩‍💻\ncommand");
    textarea.dispatchEvent(event);
  });
  await expect.poll(() => received.join("")).toBe("\x1bOA\x03\x1b[200~한글 é 👩‍💻\ncommand\x1b[201~");
});

test("restores the normal screen after a split alternate-screen TUI sequence", async ({ page }) => {
  let send: (text: string) => void = () => {};
  const received: string[] = [];
  await page.routeWebSocket(`**/ws/vms/${VM.id}/console`, socket => {
    send = text => socket.send(text);
    socket.send("normal screen\r\nprompt# ");
    socket.onMessage(message => received.push(message.toString()));
  });
  await openDashboard(page, { vms: [VM], hash: `#/console/${VM.id}`, routes: { [`/api/vms/${VM.id}`]: VM } });
  await expect(page.locator(".terminal-output")).toContainText("normal screen");
  send("\x1b[?104");
  send("9h\x1b[2J\x1b[H\x1b[31mTUI 한글 é 👩‍💻\x1b[0m");
  await expect(page.locator(".terminal-output")).toHaveText("TUI 한글 é 👩‍💻");
  send("\x1b[2;5H\x1b[6n");
  send("\x1b[?1049l");
  await expect(page.locator(".terminal-output")).toContainText("normal screen");
  await expect(page.locator(".terminal-output")).not.toContainText("TUI");
  await expect.poll(() => received.join("")).toBe("\x1b[2;5R");
});

test("drains chunked Unicode output before exporting the complete session", async ({ page }) => {
  const payload = Array.from({ length: 1200 }, (_, i) => `line-${i} 한글 é 👩‍💻\r\n`).join("") + "END_OUTPUT\r\n";
  await page.routeWebSocket(`**/ws/vms/${VM.id}/console`, socket => socket.send(Buffer.from(payload)));
  await openDashboard(page, { vms: [VM], hash: `#/console/${VM.id}`, routes: { [`/api/vms/${VM.id}`]: VM, [`/api/vms/${VM.id}/log`]: { consoleLog: "server log" } } });
  await expect(page.locator(".terminal-output")).toContainText("END_OUTPUT");
  const pending = page.waitForEvent("download");
  await page.getByRole("button", { name: "Save log", exact: true }).click();
  const stream = await (await pending).createReadStream();
  const chunks: Buffer[] = [];
  for await (const chunk of stream!) chunks.push(Buffer.from(chunk));
  const text = Buffer.concat(chunks).toString();
  expect(text).toContain("line-0 한글 é 👩‍💻");
  expect(text).toContain("line-1199 한글 é 👩‍💻");
  expect(text).toContain("END_OUTPUT");
  expect(text).not.toContain("�");
});

test("shows an output overflow instead of dropping bytes silently", async ({ page }) => {
  await page.routeWebSocket(`**/ws/vms/${VM.id}/console`, socket => socket.send(Buffer.alloc(8 * 1024 * 1024 + 1, 65)));
  await openDashboard(page, { vms: [VM], hash: `#/console/${VM.id}`, routes: { [`/api/vms/${VM.id}`]: VM } });
  await expect(page.getByRole("alert")).toContainText("8 MiB");
  await expect(page.locator(".console-status")).toHaveText("Connection failed");
  await expect(page.locator(".terminal-emulator")).toHaveCount(0);
});

test("retries a failed WASM asset load without a stale terminal or socket", async ({ page }) => {
  let allow = false;
  let connections = 0;
  await page.route("**/*.wasm", route => allow ? route.continue() : route.fulfill({ status: 503, body: "unavailable" }));
  await page.routeWebSocket(`**/ws/vms/${VM.id}/console`, socket => { connections++; socket.send("recovered"); });
  await openDashboard(page, { vms: [VM], hash: `#/console/${VM.id}`, routes: { [`/api/vms/${VM.id}`]: VM } });
  await expect(page.getByRole("alert")).toBeVisible();
  expect(connections).toBe(0);
  allow = true;
  await page.getByRole("button", { name: "Reconnect", exact: true }).click();
  await expect(page.locator(".terminal-output")).toContainText("recovered");
  await expect(page.locator(".terminal-emulator canvas")).toHaveCount(1);
  expect(connections).toBe(1);
});

test("fits the final box after rapid resizes and an SSH tab round trip", async ({ page }) => {
  await page.routeWebSocket(`**/ws/vms/${VM.id}/console`, socket => socket.send("resize ready"));
  await openDashboard(page, { vms: [VM], hash: `#/console/${VM.id}`, routes: { [`/api/vms/${VM.id}`]: VM } });
  await expect(page.locator(".terminal-output")).toContainText("resize ready");
  for (const width of [620, 1400, 760, 1050]) await page.setViewportSize({ width, height: 740 });
  await page.getByRole("tab", { name: "SSH", exact: true }).click();
  await page.getByRole("tab", { name: "Serial", exact: true }).click();
  await expect.poll(() => page.locator(".terminal-emulator").evaluate(element => {
    const canvas = element.querySelector("canvas")!;
    const gapX = element.clientWidth - parseFloat(canvas.style.width);
    const gapY = element.clientHeight - parseFloat(canvas.style.height);
    return gapX >= 12 && gapX < 30 && gapY >= 0 && gapY < 30;
  })).toBe(true);
});


test("reports SGR clicks, drag, wheel and focus using WASM modes", async ({ page }) => {
  const received: string[] = [];
  let send: (text: string) => void = () => {};
  await page.routeWebSocket(`**/ws/vms/${VM.id}/console`, socket => {
    send = text => socket.send(text);
    socket.send("\x1b[?1002h\x1b[?1006hmouse ready");
    socket.onMessage(message => received.push(message.toString()));
  });
  await openDashboard(page, { vms: [VM], hash: `#/console/${VM.id}`, routes: { [`/api/vms/${VM.id}`]: VM } });
  await expect(page.locator(".terminal-output")).toContainText("mouse ready");
  await page.evaluate(async () => {
    await document.fonts.ready;
    await new Promise<void>(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
  });
  await page.locator(".terminal-emulator canvas").evaluate(canvas => {
    const style = getComputedStyle(canvas.parentElement!);
    const context = document.createElement("canvas").getContext("2d")!;
    context.font = `${style.fontSize} ${style.fontFamily}`;
    const metrics = context.measureText("M");
    const w = Math.ceil(metrics.width), h = Math.ceil(metrics.actualBoundingBoxAscent + metrics.actualBoundingBoxDescent) + 2;
    const box = canvas.getBoundingClientRect();
    const event = (name: string, column: number, extra = {}) => canvas.dispatchEvent(new MouseEvent(name, { bubbles: true, cancelable: true, clientX: box.left + (column - 0.5) * w, clientY: box.top + 1.5 * h, button: 0, ...extra }));
    event("mousedown", 3, { buttons: 1 });
    event("mousemove", 4, { buttons: 1 });
    event("mousemove", 4, { buttons: 1 });
    event("mouseup", 4);
    canvas.dispatchEvent(new WheelEvent("wheel", { bubbles: true, cancelable: true, clientX: box.left + 3.5 * w, clientY: box.top + 1.5 * h, deltaY: 100 }));
  });
  await expect.poll(() => received.join("")).toBe("\x1b[<0;3;2M\x1b[<32;4;2M\x1b[<0;4;2m\x1b[<65;4;2M");
  send("\x1b[?1004h\r\nfocus ready");
  await expect(page.locator(".terminal-output")).toContainText("focus ready");
  await page.getByRole("button", { name: "Display settings", exact: true }).focus();
  await page.locator(".terminal-emulator textarea").focus();
  await expect.poll(() => received.slice(-2)).toEqual(["\x1b[O", "\x1b[I"]);
});

test("sends legacy mouse coordinates as raw bytes above ASCII", async ({ page }) => {
  const received: Buffer[] = [];
  await page.setViewportSize({ width: 1800, height: 850 });
  await page.routeWebSocket(`**/ws/vms/${VM.id}/console`, socket => {
    socket.send("\x1b[?1000hlegacy ready");
    socket.onMessage(message => received.push(Buffer.from(message)));
  });
  await openDashboard(page, { vms: [VM], hash: `#/console/${VM.id}`, routes: { [`/api/vms/${VM.id}`]: VM } });
  await expect(page.locator(".terminal-output")).toContainText("legacy ready");
  await page.getByRole("button", { name: "Terminal only", exact: true }).click();
  await page.evaluate(async () => {
    await document.fonts.ready;
    await new Promise<void>(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
  });
  await page.locator(".terminal-emulator canvas").evaluate(canvas => {
    const style = getComputedStyle(canvas.parentElement!);
    const context = document.createElement("canvas").getContext("2d")!;
    context.font = `${style.fontSize} ${style.fontFamily}`;
    const metrics = context.measureText("M");
    const w = Math.ceil(metrics.width), h = Math.ceil(metrics.actualBoundingBoxAscent + metrics.actualBoundingBoxDescent) + 2;
    const box = canvas.getBoundingClientRect();
    for (const type of ["mousedown", "mouseup"]) canvas.dispatchEvent(new MouseEvent(type, { bubbles: true, cancelable: true, clientX: box.left + 119.5 * w, clientY: box.top + 1.5 * h, button: 0 }));
  });
  await expect.poll(() => [...Buffer.concat(received)]).toEqual([27, 91, 77, 32, 152, 34, 27, 91, 77, 35, 152, 34]);
});

test("copies selected Korean and graphemes without wide-cell padding", async ({ page }) => {
  const expected = "한글 é 👩‍💻 end";
  const received: string[] = [];
  await page.routeWebSocket(`**/ws/vms/${VM.id}/console`, socket => {
    socket.send(expected);
    socket.onMessage(message => received.push(message.toString()));
  });
  await openDashboard(page, { vms: [VM], hash: `#/console/${VM.id}`, routes: { [`/api/vms/${VM.id}`]: VM } });
  await expect(page.locator(".terminal-output")).toContainText(expected);
  const box = await page.locator(".terminal-emulator canvas").boundingBox();
  await page.mouse.move(box!.x + 1, box!.y + 5);
  await page.mouse.down();
  await page.mouse.move(box!.x + 220, box!.y + 5, { steps: 5 });
  await page.mouse.up();
  const copied = await page.locator(".terminal-emulator textarea").evaluate(textarea => {
    const event = new ClipboardEvent("copy", { clipboardData: new DataTransfer(), bubbles: true, cancelable: true });
    textarea.dispatchEvent(event);
    return { text: event.clipboardData!.getData("text/plain"), prevented: event.defaultPrevented };
  });
  expect(copied).toEqual({ text: expected, prevented: true });
  await page.locator(".terminal-emulator textarea").focus();
  await page.keyboard.press("Control+c");
  await expect(page.locator(".terminal-emulator textarea")).toBeFocused();
  expect(received).toEqual([]);
});

test("cancels a pending WASM load when navigating away", async ({ page }) => {
  let release: () => void = () => {};
  const gate = new Promise<void>(resolve => { release = resolve; });
  let started = false;
  let connections = 0;
  const errors: string[] = [];
  page.on("pageerror", error => errors.push(error.message));
  await page.route("**/*.wasm", async route => { started = true; await gate; await route.continue(); });
  await page.routeWebSocket(`**/ws/vms/${VM.id}/console`, socket => { connections++; socket.send("loaded again"); });
  await openDashboard(page, { vms: [VM], hash: `#/console/${VM.id}`, routes: { [`/api/vms/${VM.id}`]: VM } });
  await expect.poll(() => started).toBe(true);
  await page.evaluate(() => { location.hash = "#/vms"; });
  await expect(page.locator(".console-page")).toHaveCount(0);
  release();
  await page.evaluate(() => new Promise<void>(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
  expect(connections).toBe(0);
  await page.evaluate(id => { location.hash = `#/console/${id}`; }, VM.id);
  await expect(page.locator(".terminal-output")).toContainText("loaded again");
  await expect(page.locator(".terminal-emulator canvas")).toHaveCount(1);
  expect(connections).toBe(1);
  expect(errors).toEqual([]);
});

test("reuses compiled code across sessions without reusing WASM memory", async ({ page }) => {
  let requests = 0;
  let connections = 0;
  await page.route("**/*.wasm", route => { requests++; return route.continue(); });
  await page.routeWebSocket(`**/ws/vms/${VM.id}/console`, socket => {
    connections++;
    socket.send(`session ${connections} 한글 é 👩‍💻`);
    socket.onMessage(() => socket.close({ code: 4000, reason: "session_ended" }));
  });
  await openDashboard(page, { vms: [VM], hash: `#/console/${VM.id}`, routes: { [`/api/vms/${VM.id}`]: VM } });
  for (let i = 1; i <= 3; i++) {
    await expect(page.locator(".terminal-output")).toContainText(`session ${i} 한글 é 👩‍💻`);
    await page.locator(".terminal-emulator textarea").focus();
    await page.keyboard.press("Enter");
    await expect(page.locator(".console-status")).toHaveText("Session ended");
    if (i < 3) await page.getByRole("button", { name: "New session", exact: true }).click();
  }
  expect(requests).toBe(1);
});

test("updates the Canvas backing scale after a live device-pixel-ratio change", async ({ page, browserName }) => {
  test.skip(browserName !== "chromium", "Live DPR emulation uses Chromium CDP");
  await page.routeWebSocket(`**/ws/vms/${VM.id}/console`, socket => socket.send("DPI 한글 é 👩‍💻"));
  await openDashboard(page, { vms: [VM], hash: `#/console/${VM.id}`, routes: { [`/api/vms/${VM.id}`]: VM } });
  await expect(page.locator(".terminal-output")).toContainText("DPI 한글 é 👩‍💻");
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("Emulation.setDeviceMetricsOverride", { width: 1100, height: 850, deviceScaleFactor: 2, mobile: false });
  await expect.poll(() => page.locator(".terminal-emulator canvas").evaluate(canvas => {
    const element = canvas as HTMLCanvasElement;
    return { ratio: element.width / parseFloat(element.style.width), transform: element.getContext("2d")!.getTransform().a };
  })).toEqual({ ratio: 2, transform: 2 });
  await cdp.send("Emulation.setDeviceMetricsOverride", { width: 1100, height: 850, deviceScaleFactor: 1, mobile: false });
  await expect.poll(() => page.locator(".terminal-emulator canvas").evaluate(canvas => {
    const element = canvas as HTMLCanvasElement;
    return { ratio: element.width / parseFloat(element.style.width), transform: element.getContext("2d")!.getTransform().a };
  })).toEqual({ ratio: 1, transform: 1 });
});


test("keeps the history viewport while output arrives and returns to live output", async ({ page }) => {
  const received: string[] = [];
  let send: (text: string) => void = () => {};
  await page.routeWebSocket(`**/ws/vms/${VM.id}/console`, socket => {
    send = text => socket.send(text);
    socket.send(Array.from({ length: 160 }, (_, i) => `history-${i}\r\n`).join("") + "LIVE_OUTPUT");
    socket.onMessage(message => received.push(message.toString()));
  });
  await openDashboard(page, { vms: [VM], hash: `#/console/${VM.id}`, routes: { [`/api/vms/${VM.id}`]: VM } });
  await expect(page.locator(".terminal-output")).toContainText("LIVE_OUTPUT");
  await page.locator(".terminal-emulator textarea").focus();
  await page.keyboard.press("Shift+PageUp");
  await expect(page.locator(".terminal-output")).not.toContainText("LIVE_OUTPUT");
  const firstLine = (await page.locator(".terminal-output").textContent())!.split("\n")[0];
  send("\r\nNEW_OUTPUT");
  const pendingDownload = page.waitForEvent("download");
  await page.getByRole("button", { name: "Save log", exact: true }).click();
  const stream = await (await pendingDownload).createReadStream();
  const chunks: Buffer[] = [];
  for await (const chunk of stream!) chunks.push(Buffer.from(chunk));
  expect(Buffer.concat(chunks).toString()).toContain("NEW_OUTPUT");
  await expect.poll(async () => (await page.locator(".terminal-output").textContent())!.split("\n")[0]).toBe(firstLine);
  await page.locator(".terminal-emulator textarea").focus();
  await page.keyboard.press("Shift+PageDown");
  await page.keyboard.press("Shift+PageDown");
  await expect(page.locator(".terminal-output")).toContainText("NEW_OUTPUT");
  expect(received).toEqual([]);
});


test("returns live cursor and device status replies from the WASM core", async ({ page }) => {
  const received: string[] = [];
  await page.routeWebSocket(`**/ws/vms/${VM.id}/console`, socket => {
    socket.send("ready#\x1b[6n\x1b[5n");
    socket.onMessage(message => received.push(message.toString()));
  });
  await openDashboard(page, { vms: [VM], hash: `#/console/${VM.id}`, routes: { [`/api/vms/${VM.id}`]: VM } });
  await expect(page.locator(".terminal-output")).toHaveText("ready#");
  await expect.poll(() => received.join("")).toBe("\x1b[1;7R\x1b[0n");
});


test("applies color themes on explicit reconnect while font changes stay live", async ({ page }) => {
  let connections = 0;
  await page.routeWebSocket(`**/ws/vms/${VM.id}/console`, socket => {
    connections++;
    socket.send(`session ${connections} Theme text`);
  });
  await openDashboard(page, { vms: [VM], hash: `#/console/${VM.id}`, routes: { [`/api/vms/${VM.id}`]: VM } });
  await expect(page.locator(".terminal-output")).toContainText("session 1 Theme text");
  await expect(page.locator(".terminal-emulator textarea")).toBeFocused();
  await page.getByRole("button", { name: "Display settings", exact: true }).click();
  await page.getByRole("combobox", { name: "Font size", exact: true }).selectOption("16");
  await expect(page.locator(".terminal-emulator")).toHaveCSS("font-size", "16px");
  expect(connections).toBe(1);
  await page.getByRole("combobox", { name: "Color theme", exact: true }).selectOption("light");
  await expect(page.locator(".console-surface")).toHaveCSS("background-color", "rgb(24, 24, 27)");
  expect(connections).toBe(1);
  await page.getByRole("button", { name: "Apply theme and reconnect", exact: true }).click();
  await expect(page.locator(".terminal-output")).toContainText("session 2 Theme text");
  await expect(page.locator(".console-surface")).toHaveCSS("background-color", "rgb(250, 250, 251)");
  await expect(page.locator(".terminal-emulator canvas")).toHaveCount(1);
  await expect.poll(() => page.locator(".terminal-emulator canvas").evaluate(canvas => {
    const pixels = (canvas as HTMLCanvasElement).getContext("2d")!.getImageData(0, 0, 180, 30).data;
    let light = 0, dark = 0;
    for (let i = 0; i < pixels.length; i += 4) {
      if (pixels[i] > 230 && pixels[i + 1] > 230 && pixels[i + 2] > 230) light++;
      if (pixels[i] < 60 && pixels[i + 1] < 60 && pixels[i + 2] < 60) dark++;
    }
    return light > 1000 && dark > 30;
  })).toBe(true);
  expect(connections).toBe(2);
});

test("synchronizes the final viewport and font size through control frames", async ({ page }) => {
  const sizes: { rows: number; cols: number }[] = [];
  const input: string[] = [];
  await page.routeWebSocket(`**/ws/vms/${VM.id}/console`, socket => {
    socket.send(JSON.stringify({ type: "resize", status: "pending", size: null, viewers: 1 }));
    socket.onMessage(message => {
      if (typeof message !== "string") { input.push(message.toString()); return; }
      const { rows, cols } = JSON.parse(message);
      sizes.push({ rows, cols });
      socket.send(JSON.stringify({ type: "resize", status: "applied", size: { rows, cols }, viewers: 1 }));
    });
  });
  await openDashboard(page, { vms: [VM], hash: `#/console/${VM.id}`, routes: { [`/api/vms/${VM.id}`]: VM } });
  await expect(page.locator(".console-geometry")).toHaveAttribute("data-status", "applied");
  const first = sizes.at(-1)!;
  await page.setViewportSize({ width: 1400, height: 950 });
  await page.setViewportSize({ width: 1200, height: 800 });
  await page.setViewportSize({ width: 900, height: 650 });
  await expect.poll(() => sizes.at(-1)!.cols).toBeLessThan(first.cols);
  await page.getByRole("button", { name: "Display settings", exact: true }).click();
  const beforeFont = sizes.at(-1)!;
  await page.getByRole("combobox", { name: "Font size", exact: true }).selectOption("18");
  await expect.poll(() => sizes.at(-1)!.cols).toBeLessThan(beforeFont.cols);
  await expect.poll(async () => (await page.locator(".console-geometry").textContent())?.trim()).toBe(`Guest size: ${sizes.at(-1)!.cols} × ${sizes.at(-1)!.rows}`);
  await page.locator(".terminal-emulator textarea").focus();
  await page.keyboard.press("Enter");
  await expect.poll(() => input.join("")).toBe("\r");
  expect(input.join("")).not.toContain("resize");
});

test("renders a shared guest grid without reporting it as the local viewport", async ({ page }) => {
  const sizes: { rows: number; cols: number }[] = [];
  await page.routeWebSocket(`**/ws/vms/${VM.id}/console`, socket => {
    socket.send(JSON.stringify({ type: "resize", status: "pending", size: null, viewers: 2 }));
    socket.onMessage(message => {
      if (typeof message !== "string") return;
      sizes.push(JSON.parse(message));
      socket.send(JSON.stringify({ type: "resize", status: "applied", size: { rows: 10, cols: 20 }, viewers: 2 }));
      socket.send(Buffer.from("\x1b[2J\x1b[H12345678901234567890ABCDE"));
    });
  });
  await openDashboard(page, { vms: [VM], hash: `#/console/${VM.id}`, routes: { [`/api/vms/${VM.id}`]: VM } });
  await expect(page.locator(".console-geometry")).toHaveText("Guest size: 20 × 10");
  await expect(page.locator(".terminal-output")).toHaveText("12345678901234567890\nABCDE");
  expect(sizes.every(size => size.cols > 20 && size.rows > 10)).toBe(true);
  await page.setViewportSize({ width: 1500, height: 1000 });
  await expect.poll(() => sizes.at(-1)!.cols).toBeGreaterThan(sizes[0].cols);
  await expect(page.locator(".terminal-output")).toHaveText("12345678901234567890\nABCDE");
});

test("reconnects and reports dimensions again when guest resizing is unavailable", async ({ page }) => {
  let connections = 0;
  const reports: number[] = [];
  const received: string[] = [];
  await page.routeWebSocket(`**/ws/vms/${VM.id}/console`, socket => {
    const connection = ++connections;
    socket.send(JSON.stringify({ type: "resize", status: "pending", size: null, viewers: 1 }));
    socket.onMessage(message => {
      if (typeof message !== "string") { received.push(message.toString()); return; }
      reports.push(connection);
      socket.send(JSON.stringify({ type: "resize", status: "unavailable", size: null, viewers: 1 }));
      if (connection === 1) socket.close({ code: 1011, reason: "retry" });
    });
  });
  await openDashboard(page, { vms: [VM], hash: `#/console/${VM.id}`, routes: { [`/api/vms/${VM.id}`]: VM } });
  await expect.poll(() => reports.includes(2)).toBe(true);
  await expect(page.locator(".console-geometry")).toHaveText("Guest size not synchronized");
  await expect(page.locator(".console-status")).toHaveText("Connected");
  await page.locator(".terminal-emulator textarea").focus();
  await page.keyboard.type("still usable");
  await expect.poll(() => received.join("")).toBe("still usable");
  expect(connections).toBe(2);
});
