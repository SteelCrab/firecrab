import type { Terminal } from "ghostty-web";

/** Encode browser pointer events using the modes negotiated by the WASM core.
 * Protocol: https://invisible-island.net/xterm/ctlseqs/ctlseqs.html#h2-Mouse-Tracking
 */
export function enableTerminalMouse(terminal: Terminal, signal: AbortSignal, send: (bytes: Uint8Array) => void): void {
  const canvas = terminal.element?.querySelector("canvas");
  if (!canvas) return;
  const encoder = new TextEncoder();
  const mode = (number: number) => terminal.wasmTerm?.getMode(number) ?? false;
  const enabled = () => !terminal.options.disableStdin && Boolean(terminal.wasmTerm?.hasMouseTracking());
  const onCanvas = (event: MouseEvent) => event.target instanceof Node && canvas.contains(event.target);
  let pressed = -1;
  let lastCell = "";

  function report(event: MouseEvent, button: number, release = false): void {
    const bounds = canvas!.getBoundingClientRect();
    const metrics = terminal.renderer?.getMetrics();
    if (!metrics?.width || !metrics.height) return;
    const pixel = mode(1016);
    const x = Math.max(1, Math.min(pixel ? Math.floor(bounds.width) : terminal.cols, Math.floor((event.clientX - bounds.left) / (pixel ? 1 : metrics.width)) + 1));
    const y = Math.max(1, Math.min(pixel ? Math.floor(bounds.height) : terminal.rows, Math.floor((event.clientY - bounds.top) / (pixel ? 1 : metrics.height)) + 1));
    if (button & 32) {
      if (lastCell === `${x}:${y}`) return;
    }
    lastCell = `${x}:${y}`;
    const mods = (event.shiftKey ? 4 : 0) | (event.altKey || event.metaKey ? 8 : 0) | (event.ctrlKey ? 16 : 0);
    if (pixel || mode(1006)) {
      send(encoder.encode(`\x1b[<${button | mods};${x};${y}${release ? "m" : "M"}`));
      return;
    }
    const code = (release ? 3 : button) | mods;
    if (mode(1015)) send(encoder.encode(`\x1b[${code + 32};${x};${y}M`));
    else if (mode(1005)) {
      if (x <= 2015 && y <= 2015) send(encoder.encode(`\x1b[M${String.fromCharCode(code + 32, x + 32, y + 32)}`));
    } else if (x <= 223 && y <= 223) {
      // Legacy coordinates are raw bytes, not UTF-8 text.
      send(new Uint8Array([27, 91, 77, code + 32, x + 32, y + 32]));
    }
  }

  const consume = (event: MouseEvent) => { event.preventDefault(); event.stopImmediatePropagation(); };
  document.addEventListener("mousedown", event => {
    if (!enabled() || event.shiftKey || !onCanvas(event) || event.button > 2) return;
    consume(event);
    terminal.focus();
    pressed = event.button;
    lastCell = "";
    report(event, pressed);
  }, { capture: true, signal });
  document.addEventListener("mouseup", event => {
    if (pressed < 0) return;
    if (enabled()) {
      consume(event);
      if (!mode(9)) report(event, event.button, true);
    }
    pressed = -1;
    lastCell = "";
  }, { capture: true, signal });
  document.addEventListener("mousemove", event => {
    if (!enabled() || event.shiftKey || (!onCanvas(event) && pressed < 0)) return;
    if (!mode(1003) && !(mode(1002) && pressed >= 0)) return;
    consume(event);
    const button = event.buttons & 1 ? 0 : event.buttons & 4 ? 1 : event.buttons & 2 ? 2 : 3;
    report(event, button | 32);
  }, { capture: true, signal });
  document.addEventListener("wheel", event => {
    if (!enabled() || event.shiftKey || !onCanvas(event)) return;
    consume(event);
    const vertical = Math.abs(event.deltaY) >= Math.abs(event.deltaX);
    const delta = vertical ? event.deltaY : event.deltaX;
    if (!delta) return;
    const button = vertical ? delta < 0 ? 64 : 65 : delta < 0 ? 66 : 67;
    const count = Math.max(1, Math.min(5, Math.round(Math.abs(delta) / (event.deltaMode === 0 ? 100 : 1))));
    for (let i = 0; i < count; i++) report(event, button);
  }, { capture: true, passive: false, signal });
  document.addEventListener("contextmenu", event => {
    if (enabled() && !event.shiftKey && onCanvas(event)) consume(event);
  }, { capture: true, signal });
  terminal.textarea?.addEventListener("focus", () => {
    if (!terminal.options.disableStdin && mode(1004)) send(encoder.encode("\x1b[I"));
  }, { signal });
  terminal.textarea?.addEventListener("blur", () => {
    pressed = -1;
    if (!terminal.options.disableStdin && mode(1004)) send(encoder.encode("\x1b[O"));
  }, { signal });
}
