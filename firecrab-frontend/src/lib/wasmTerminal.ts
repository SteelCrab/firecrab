import type { ITerminalOptions, Terminal } from "ghostty-web";
import wasmUrl from "ghostty-web/ghostty-vt.wasm?url";
import { defaultInteractiveTerminalOptions, enableTerminalInput } from "./terminal";
import { terminalText } from "./terminalText";
import { enableTerminalMouse } from "./terminalMouse";

const OUTPUT_CHUNK_BYTES = 16 * 1024;
const MAX_PENDING_BYTES = 8 * 1024 * 1024;
const FRAME_BUDGET_MS = 4;

let compiled: Promise<WebAssembly.Module> | undefined;

function loadModule(): Promise<WebAssembly.Module> {
  // Compiled code can be shared; terminal memory and handles cannot.
  compiled ??= fetch(wasmUrl).then(async response => {
    if (!response.ok) throw new Error(`Cannot load terminal WASM (${response.status})`);
    return WebAssembly.compile(await response.arrayBuffer());
  }).catch(error => {
    compiled = undefined;
    throw error;
  });
  return compiled;
}

export interface WasmTerminalSession {
  terminal: Terminal;
  fit: () => void;
  write: (data: string | Uint8Array) => void;
  readText: () => Promise<string>;
  dispose: () => void;
}

/** One session owns one WASM memory, its DOM, and every scheduled callback. */
export async function createWasmTerminal(
  container: HTMLElement,
  options: ITerminalOptions,
  signal: AbortSignal,
  onOutput: (text: string) => void,
  onError: (error: unknown) => void,
  onInput: (bytes: string | Uint8Array) => void,
): Promise<WasmTerminalSession> {
  signal.throwIfAborted();
  const { Ghostty, Terminal } = await import("ghostty-web");
  signal.throwIfAborted();
  // A freed complex grapheme can corrupt shared memory (upstream #141).
  const module = await loadModule();
  signal.throwIfAborted();
  const ghostty = new Ghostty(await WebAssembly.instantiate(module, { env: { log: () => {} } }));
  signal.throwIfAborted();
  const terminal = new Terminal({ ...defaultInteractiveTerminalOptions(options), ghostty });
  const mount = document.createElement("div");
  mount.className = "terminal-emulator";
  mount.dataset.terminalEngine = "ghostty";
  mount.style.fontFamily = terminal.options.fontFamily;
  mount.style.fontSize = `${terminal.options.fontSize}px`;
  container.appendChild(mount);
  const listeners = new AbortController();
  let disposed = false;
  let outputFrame = 0;
  let fitFrame = 0;
  let mirrorTimer: ReturnType<typeof setTimeout> | undefined;
  let pending: Uint8Array[] = [];
  let pendingBytes = 0;
  let head = 0;
  let offset = 0;
  const idle: (() => void)[] = [];
  const encoder = new TextEncoder();
  const observer = new ResizeObserver(() => fit());

  function dispose() {
    if (disposed) return;
    disposed = true;
    signal.removeEventListener("abort", dispose);
    listeners.abort();
    observer.disconnect();
    cancelAnimationFrame(outputFrame);
    cancelAnimationFrame(fitFrame);
    clearTimeout(mirrorTimer);
    pending = [];
    pendingBytes = 0;
    for (const resolve of idle.splice(0)) resolve();
    terminal.dispose();
    mount.remove();
  }

  function fit() {
    if (disposed || fitFrame) return;
    fitFrame = requestAnimationFrame(() => {
      fitFrame = 0;
      if (disposed || mount.clientWidth < 16 || mount.clientHeight < 16) return;
      const metrics = terminal.renderer?.getMetrics();
      if (!metrics?.width || !metrics.height) return;
      // The bundled FitAddon drops resizes while locked for 50 ms. Use the final box.
      const viewingHistory = terminal.getViewportY() > 0;
      const top = terminal.getScrollbackLength() - terminal.getViewportY();
      terminal.resize(Math.max(2, Math.floor((mount.clientWidth - 12) / metrics.width)), Math.max(2, Math.floor(mount.clientHeight / metrics.height)));
      if (viewingHistory) terminal.scrollToLine(terminal.getScrollbackLength() - top);
      scheduleMirror();
    });
  }

  function scheduleMirror() {
    if (disposed || mirrorTimer !== undefined) return;
    mirrorTimer = setTimeout(() => {
      mirrorTimer = undefined;
      if (!disposed) onOutput(terminalText(terminal));
    }, 75);
  }

  function drain() {
    outputFrame = 0;
    if (disposed) return;
    const started = performance.now();
    try {
      do {
        const chunk = pending[head];
        const end = Math.min(chunk.length, offset + OUTPUT_CHUNK_BYTES);
        const viewingHistory = terminal.getViewportY() > 0;
        const top = terminal.getScrollbackLength() - terminal.getViewportY();
        terminal.write(chunk.subarray(offset, end));
        if (viewingHistory) terminal.scrollToLine(terminal.getScrollbackLength() - top);
        pendingBytes -= end - offset;
        offset = end;
        if (offset === chunk.length) { head++; offset = 0; }
      } while (head < pending.length && performance.now() - started < FRAME_BUDGET_MS);
      scheduleMirror();
      if (head < pending.length) {
        // Release consumed socket buffers even during sustained output.
        if (head) { pending = pending.slice(head); head = 0; }
        outputFrame = requestAnimationFrame(drain);
      } else {
        pending = [];
        head = 0;
        for (const resolve of idle.splice(0)) resolve();
      }
    } catch (error) {
      dispose();
      onError(error);
    }
  }

  // The upstream renderer focuses its contenteditable parent. Mobile input needs the textarea.
  terminal.focus = () => { if (!disposed) terminal.textarea?.focus({ preventScroll: true }); };
  terminal.blur = () => { if (!disposed) terminal.textarea?.blur(); };
  try {
    terminal.open(mount);
    // The textarea owns editing; the canvas has an accessible text mirror.
    mount.removeAttribute("contenteditable");
    mount.removeAttribute("role");
    mount.removeAttribute("aria-label");
    mount.removeAttribute("aria-multiline");
    mount.tabIndex = -1;
    mount.querySelector("canvas")?.setAttribute("aria-hidden", "true");
    enableTerminalInput(terminal, listeners.signal);
    terminal.onData(onInput);
    enableTerminalMouse(terminal, listeners.signal, onInput);
    terminal.onScroll(scheduleMirror);
    observer.observe(container);
    window.addEventListener("resize", fit, { signal: listeners.signal });
    const refreshFont = () => {
      if (disposed) return;
      terminal.renderer?.remeasureFont();
      fit();
      if (terminal.wasmTerm) terminal.renderer?.render(terminal.wasmTerm, true, terminal.getViewportY(), terminal);
    };
    document.fonts.addEventListener("loadingdone", refreshFont, { signal: listeners.signal });
    void document.fonts.ready.then(refreshFont);
    signal.addEventListener("abort", dispose, { once: true });
    fit();
  } catch (error) {
    dispose();
    throw error;
  }

  return {
    terminal, fit, dispose,
    write(data) {
      if (disposed) return;
      const bytes = typeof data === "string" ? encoder.encode(data) : data;
      if (!bytes.length) return;
      if (pendingBytes + bytes.length > MAX_PENDING_BYTES) {
        dispose();
        onError(new Error("Terminal output exceeded the 8 MiB pending limit. Reconnect to resume."));
        return;
      }
      pending.push(bytes);
      pendingBytes += bytes.length;
      if (!outputFrame) outputFrame = requestAnimationFrame(drain);
    },
    async readText() {
      if (!disposed && pendingBytes) await new Promise<void>(resolve => idle.push(resolve));
      return disposed ? "" : terminalText(terminal, true);
    },
  };
}
