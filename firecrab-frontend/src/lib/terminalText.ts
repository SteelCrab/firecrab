import type { GhosttyCell, Terminal } from "ghostty-web";

function lineText(cells: GhosttyCell[], grapheme: (column: number) => string): string {
  let text = "";
  for (let x = 0; x < cells.length; x++) {
    const cell = cells[x];
    if (cell.width === 0) continue;
    // grapheme_len counts the codepoints after the base character.
    text += cell.grapheme_len > 0 ? grapheme(x) : cell.codepoint ? String.fromCodePoint(cell.codepoint) : " ";
  }
  return text.trimEnd();
}

/** Read native cells; the compatibility buffer truncates complex graphemes. */
export function terminalText(terminal: Terminal, includeHistory = false): string {
  const core = terminal.wasmTerm;
  if (!core) return "";
  core.update();
  const history = core.isAlternateScreen() ? 0 : core.getScrollbackLength();
  const start = includeHistory ? 0 : Math.max(0, history - Math.floor(terminal.getViewportY()));
  const end = includeHistory ? history + terminal.rows : Math.min(history + terminal.rows, start + terminal.rows);
  const cells = core.getViewport();
  const lines: string[] = [];
  for (let y = start; y < end; y++) {
    const row = y - history;
    lines.push(y < history
      ? lineText(core.getScrollbackLine(y) ?? [], x => core.getScrollbackGraphemeString(y, x))
      : lineText(cells.slice(row * terminal.cols, (row + 1) * terminal.cols), x => core.getGraphemeString(row, x)));
  }
  return lines.join("\n").trimEnd();
}
