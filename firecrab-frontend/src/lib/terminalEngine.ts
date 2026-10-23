import type { Terminal as XtermTerminal } from "@xterm/xterm";
import type { FitAddon as XtermFitAddon } from "@xterm/addon-fit";
import type { Terminal as GhosttyTerminal, FitAddon as GhosttyFitAddon, GhosttyCell } from "ghostty-web";
import { defaultInteractiveTerminalOptions } from "./terminal";

export type TerminalEngine = "ghostty" | "xterm";
export type InteractiveTerminal = XtermTerminal | GhosttyTerminal;
export type TerminalFit = XtermFitAddon | GhosttyFitAddon;
type Options = Parameters<typeof defaultInteractiveTerminalOptions>[0];

/** The query override supports diagnostics and the xterm comparison baseline. */
export function selectedTerminalEngine(): TerminalEngine {
  return new URLSearchParams(window.location.search).get("terminalEngine") === "xterm"
    ? "xterm"
    : "ghostty";
}

export async function createInteractiveTerminal(
  engine: TerminalEngine,
  overrides: Options = {},
): Promise<{ terminal: InteractiveTerminal; fit: TerminalFit }> {
  const options = defaultInteractiveTerminalOptions(overrides);
  if (engine === "xterm") {
    const [{ Terminal }, { FitAddon }] = await Promise.all([
      import("@xterm/xterm"),
      import("@xterm/addon-fit"),
      import("@xterm/xterm/css/xterm.css"),
    ]);
    const terminal = new Terminal(options);
    const fit = new FitAddon();
    terminal.loadAddon(fit);
    return { terminal, fit };
  }

  const { Ghostty, Terminal, FitAddon } = await import("ghostty-web");
  // Isolate memory per session: 0.4.0 can corrupt shared memory after freeing
  // complex graphemes (coder/ghostty-web#141). Never reuse that WASM instance.
  const ghostty = await Ghostty.load();
  const terminal = new Terminal({ ...options, ghostty });
  const fit = new FitAddon();
  terminal.loadAddon(fit);
  return { terminal, fit };
}

function ghosttyLine(cells: GhosttyCell[], grapheme: (column: number) => string): string {
  let text = "";
  for (let x = 0; x < cells.length; x++) {
    const cell = cells[x];
    if (cell.width === 0) continue;
    // grapheme_len counts additional codepoints, excluding the base character.
    text += cell.grapheme_len > 0 ? grapheme(x) : cell.codepoint ? String.fromCodePoint(cell.codepoint) : " ";
  }
  return text.trimEnd();
}

/** Read the active screen, optionally including history, through public APIs. */
export function terminalText(terminal: InteractiveTerminal, includeHistory = false): string {
  if ("wasmTerm" in terminal && terminal.wasmTerm) {
    const core = terminal.wasmTerm;
    core.update();
    const lines: string[] = [];
    // 0.4.0's compatibility buffer truncates complex graphemes (#200).
    // The native text APIs preserve combining marks and emoji sequences.
    if (includeHistory && !core.isAlternateScreen()) {
      for (let y = 0; y < core.getScrollbackLength(); y++) {
        lines.push(ghosttyLine(core.getScrollbackLine(y) ?? [], x => core.getScrollbackGraphemeString(y, x)));
      }
    }
    const cells = core.getViewport();
    for (let y = 0; y < terminal.rows; y++) {
      lines.push(ghosttyLine(cells.slice(y * terminal.cols, (y + 1) * terminal.cols), x => core.getGraphemeString(y, x)));
    }
    return lines.join("\n").trimEnd();
  }
  const buffer = terminal.buffer.active;
  const lines: string[] = [];
  const start = includeHistory ? 0 : Math.max(0, buffer.length - terminal.rows);
  for (let y = start; y < buffer.length; y++) {
    lines.push(buffer.getLine(y)?.translateToString(true) ?? "");
  }
  return lines.join("\n").trimEnd();
}
