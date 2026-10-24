import type { Terminal } from "ghostty-web";

/** A tap opens input; a drag moves local history without focusing the textarea. */
export function enableTerminalTouch(terminal: Terminal, signal: AbortSignal): void {
  const canvas = terminal.element?.querySelector("canvas");
  if (!canvas) return;
  let touch: { id: number; x: number; y: number; lastY: number; dragged: boolean } | undefined;
  let multiple = false;
  canvas.addEventListener("touchstart", event => {
    if (event.touches.length !== 1) { multiple = true; touch = undefined; return; }
    const point = event.touches[0];
    touch = { id: point.identifier, x: point.clientX, y: point.clientY, lastY: point.clientY, dragged: false };
    multiple = false;
  }, { passive: true, signal });
  canvas.addEventListener("touchmove", event => {
    if (event.touches.length !== 1) { multiple = true; touch = undefined; return; }
    if (!touch) return;
    const point = event.touches[0];
    if (point.identifier !== touch.id) return;
    const dx = point.clientX - touch.x, dy = point.clientY - touch.y;
    if (!touch.dragged && Math.max(Math.abs(dx), Math.abs(dy)) < 8) return;
    touch.dragged = true;
    if (Math.abs(dx) > Math.abs(dy) || terminal.wasmTerm?.isAlternateScreen() || terminal.wasmTerm?.hasMouseTracking()) return;
    const height = terminal.renderer?.getMetrics().height;
    if (!height) return;
    event.preventDefault();
    event.stopImmediatePropagation();
    const lines = Math.trunc((touch.lastY - point.clientY) / height);
    if (lines) { terminal.scrollLines(lines); touch.lastY -= lines * height; }
  }, { passive: false, signal });
  canvas.addEventListener("touchend", event => {
    if (!touch && !multiple) return;
    // The upstream touchend listener focuses input even after a drag or pinch.
    event.preventDefault();
    event.stopImmediatePropagation();
    if (!multiple && touch && !touch.dragged) terminal.focus();
    if (!event.touches.length) { touch = undefined; multiple = false; }
  }, { capture: true, signal });
  canvas.addEventListener("touchcancel", () => { touch = undefined; multiple = false; }, { signal });
}
