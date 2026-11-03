import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { enableTerminalIme } from "../lib/terminal";
import "@xterm/xterm/css/xterm.css";

export async function createCanvasTerminal(
  container: HTMLElement,
  options: { cols?: number; rows?: number },
  signal: AbortSignal,
  onOutput: (value: string) => void,
  _unused: () => void,
  onInput: (value: Uint8Array) => void,
  onResize: (value: { cols: number; rows: number }) => void,
) {
  const term = new Terminal({
    fontFamily: "ui-monospace, monospace",
    fontSize: 13,
    cursorBlink: true,
    convertEol: false,
    scrollback: 5000,
    cols: options.cols || 80,
    rows: options.rows || 24,
  });
  const fit = new FitAddon();
  term.loadAddon(fit);
  term.open(container);
  enableTerminalIme(term, container);
  let shared = false;
  let disposed = false;
  const report = () => {
    const size = fit.proposeDimensions();
    if (size) onResize({ cols: size.cols, rows: size.rows });
    if (!shared) fit.fit();
  };
  const resize = new ResizeObserver(report);
  resize.observe(container);
  const input = term.onData((data) => onInput(new TextEncoder().encode(data)));
  const binary = term.onBinary((data) =>
    onInput(Uint8Array.from(data, (c) => c.charCodeAt(0))),
  );
  const mirror = () => {
    const buffer = term.buffer.active;
    const lines: string[] = [];
    for (let row = buffer.baseY; row < buffer.baseY + term.rows; row++)
      lines.push(buffer.getLine(row)?.translateToString(true) || "");
    onOutput(lines.join("\n"));
  };
  const dispose = () => {
    if (disposed) return;
    disposed = true;
    resize.disconnect();
    input.dispose();
    binary.dispose();
    term.dispose();
  };
  signal.addEventListener("abort", dispose, { once: true });
  if (signal.aborted) {
    dispose();
    throw new Error("Terminal detached");
  }
  report();
  return {
    write: (bytes: Uint8Array) => term.write(bytes, mirror),
    fit: report,
    reportDimensions: report,
    setSharedDimensions: (size: { cols: number; rows: number }) => {
      shared = true;
      if (size.cols >= 20 && size.rows >= 5) term.resize(size.cols, size.rows);
    },
    dispose,
  };
}
