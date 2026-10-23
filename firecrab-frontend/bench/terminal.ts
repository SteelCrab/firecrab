import { createInteractiveTerminal, terminalText, type InteractiveTerminal, type TerminalEngine } from "../src/lib/terminalEngine";
import { enableTerminalIme } from "../src/lib/terminal";
import { terminalWorkload, type Workload } from "./workloads";

const container = document.getElementById("terminal")!;
let terminal: InteractiveTerminal | undefined;
const frame = () => new Promise<void>(resolve => requestAnimationFrame(() => resolve()));

// The write callbacks have different semantics. Two following frames give both
// renderers a chance to draw. This is a frame-completion proxy, not GPU timing.
async function afterFrames() {
  await frame();
  await frame();
}

function currentTerminal() {
  if (!terminal) throw new Error("Mount a terminal before measuring");
  return terminal;
}

const benchmark = {
  async mount(engine: TerminalEngine, cols: number, rows: number) {
    terminal?.dispose();
    container.replaceChildren();
    const started = performance.now();
    const instance = await createInteractiveTerminal(engine, { cols, rows, cursorBlink: false, convertEol: false });
    terminal = instance.terminal;
    terminal.open(container);
    enableTerminalIme(terminal, container, engine);
    await afterFrames();
    document.getElementById("status")!.textContent = `${engine} · ${cols} × ${rows}`;
    return { readyMs: performance.now() - started };
  },

  async output(name: Workload, chunkSize = 4093) {
    const term = currentTerminal();
    const { bytes, expected } = terminalWorkload(name, term.cols, term.rows);
    const longTasks: PerformanceEntry[] = [];
    const observer = new PerformanceObserver(list => longTasks.push(...list.getEntries()));
    observer.observe({ type: "longtask" });
    // Align each sample to a frame. Workload construction is outside the timer.
    await frame();
    const started = performance.now();
    for (let offset = 0; offset < bytes.length; offset += chunkSize) {
      term.write(bytes.subarray(offset, Math.min(offset + chunkSize, bytes.length)));
    }
    const enqueueMs = performance.now() - started;
    // Do not use write callbacks as a timing boundary: Ghostty schedules its
    // callback on a frame while xterm calls it after asynchronous parsing.
    do {
      await frame();
    } while (term.buffer.active.getLine(term.buffer.active.length - 1)?.translateToString(true) !== "BENCH_DONE"
      && performance.now() - started < 10_000);
    await frame();
    const elapsedMs = performance.now() - started;
    longTasks.push(...observer.takeRecords());
    observer.disconnect();
    const pauses = longTasks.filter(entry => entry.startTime >= started && entry.startTime < started + elapsedMs);
    const actual = {
      text: terminalText(term),
      cursorX: term.buffer.active.cursorX,
      cursorY: term.buffer.active.cursorY,
    };
    return {
      elapsedMs,
      enqueueMs,
      longTaskCount: pauses.length,
      longestTaskMs: Math.max(0, ...pauses.map(entry => entry.duration)),
      bytes: bytes.length,
      mibPerSecond: bytes.length / 1048576 / (elapsedMs / 1000),
      correct: JSON.stringify(actual) === JSON.stringify(expected),
      actual,
      expected,
    };
  },

  /** Local echo excludes WebSocket, server, and guest latency. */
  async input(samples = 24) {
    const term = currentTerminal();
    await new Promise<void>(resolve => term.write("\x1bc", resolve));
    await afterFrames();
    const results: number[] = [];
    const subscription = term.onData(data => term.write(data));
    try {
      for (let index = 0; index < samples; index++) {
        await frame();
        const started = performance.now();
        const target = term.textarea ?? container;
        target.dispatchEvent(new KeyboardEvent("keydown", { key: "x", code: "KeyX", keyCode: 88, bubbles: true, cancelable: true }));
        await afterFrames();
        if (term.buffer.active.cursorX !== index + 1) throw new Error("Input was lost or duplicated");
        results.push(performance.now() - started);
      }
    } finally {
      subscription.dispose();
    }
    return results;
  },

  async resize() {
    const term = currentTerminal();
    await frame();
    const started = performance.now();
    for (let index = 0; index < 40; index++) term.resize(index % 2 ? 80 : 160, index % 2 ? 24 : 48);
    await afterFrames();
    if (term.cols !== 80 || term.rows !== 24) throw new Error("Resize did not reach the requested grid");
    return performance.now() - started;
  },

  async resizePaced() {
    const term = currentTerminal();
    const handlers: number[] = [];
    const started = performance.now();
    for (let index = 0; index < 40; index++) {
      await frame();
      const step = index < 20 ? index + 1 : 39 - index;
      const resizeStarted = performance.now();
      term.resize(80 + step * 4, 24 + Math.floor(step * 1.2));
      handlers.push(performance.now() - resizeStarted);
    }
    await afterFrames();
    if (term.cols !== 80 || term.rows !== 24) throw new Error("Paced resize did not reach the requested grid");
    return { elapsedMs: performance.now() - started, handlers };
  },

  /** Instrument synchronous calls only; never use these timings as benchmark samples. */
  async profileResize() {
    const term = currentTerminal();
    if (!("wasmTerm" in term) || !term.wasmTerm || !term.renderer) throw new Error("Profile the Ghostty engine");
    const core = term.wasmTerm;
    const renderer = term.renderer;
    const canvas = container.querySelector("canvas")!;
    await afterFrames();
    const result = {
      core: { calls: 0, milliseconds: 0 },
      canvas: { calls: 0, milliseconds: 0 },
      render: { calls: 0, milliseconds: 0 },
      widthAssignments: 0, heightAssignments: 0, synchronousMs: 0,
      devicePixelRatio, backingWidth: 0, cssWidth: 0,
    };
    const coreResize = core.resize;
    const canvasResize = renderer.resize;
    const render = renderer.render;
    const width = Object.getOwnPropertyDescriptor(HTMLCanvasElement.prototype, "width")!;
    const height = Object.getOwnPropertyDescriptor(HTMLCanvasElement.prototype, "height")!;
    core.resize = (...args) => {
      const started = performance.now();
      try { return coreResize.apply(core, args); }
      finally { result.core.calls++; result.core.milliseconds += performance.now() - started; }
    };
    renderer.resize = (...args) => {
      const started = performance.now();
      try { return canvasResize.apply(renderer, args); }
      finally { result.canvas.calls++; result.canvas.milliseconds += performance.now() - started; }
    };
    renderer.render = (...args) => {
      const started = performance.now();
      try { return render.apply(renderer, args); }
      finally { result.render.calls++; result.render.milliseconds += performance.now() - started; }
    };
    Object.defineProperty(HTMLCanvasElement.prototype, "width", {
      ...width, set(value: number) { if (this === canvas) result.widthAssignments++; width.set!.call(this, value); },
    });
    Object.defineProperty(HTMLCanvasElement.prototype, "height", {
      ...height, set(value: number) { if (this === canvas) result.heightAssignments++; height.set!.call(this, value); },
    });
    try {
      const started = performance.now();
      for (let index = 0; index < 40; index++) term.resize(index % 2 ? 80 : 160, index % 2 ? 24 : 48);
      result.synchronousMs = performance.now() - started;
    } finally {
      core.resize = coreResize;
      renderer.resize = canvasResize;
      renderer.render = render;
      Object.defineProperty(HTMLCanvasElement.prototype, "width", width);
      Object.defineProperty(HTMLCanvasElement.prototype, "height", height);
    }
    await afterFrames();
    result.backingWidth = canvas.width;
    result.cssWidth = parseFloat(canvas.style.width);
    return result;
  },

  dispose() {
    terminal?.dispose();
    terminal = undefined;
    container.replaceChildren();
  },
};

declare global {
  interface Window { terminalBenchmark: typeof benchmark }
}
window.terminalBenchmark = benchmark;
document.getElementById("status")!.textContent = "Ready";
