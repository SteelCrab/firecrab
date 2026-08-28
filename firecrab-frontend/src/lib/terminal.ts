import type { ITerminalInitOnlyOptions, ITerminalOptions, Terminal } from "@xterm/xterm";

type TerminalCtorOptions = ITerminalOptions & ITerminalInitOnlyOptions;

/**
 * Mono first, then system CJK so Hangul/Kana/Han measure and render.
 * IBM Plex Mono has no CJK glyphs; without these fallbacks composed
 * syllables are tofu and xterm's cell width is wrong.
 */
export const TERMINAL_FONT_FAMILY =
  '"IBM Plex Mono", "Noto Sans KR", "Noto Sans JP", "Noto Sans SC", "D2Coding", "NanumGothicCoding", "Malgun Gothic", "Apple SD Gothic Neo", "Yu Gothic", "Microsoft YaHei", ui-monospace, monospace';

export function defaultInteractiveTerminalOptions(
  overrides: Partial<TerminalCtorOptions> = {},
): TerminalCtorOptions {
  return {
    convertEol: true,
    fontFamily: TERMINAL_FONT_FAMILY,
    fontSize: 13,
    scrollback: 5000,
    cursorBlink: true,
    rescaleOverlappingGlyphs: true,
    macOptionIsMeta: false,
    ...overrides,
  };
}

/**
 * Point the hidden xterm textarea at the page language so the OS IME
 * (Hangul, Kana, Pinyin, …) attaches. Skip window shortcuts while a
 * composition is in progress so Escape does not cancel 한글 mid-syllable
 * and then also leave terminal-only mode.
 */
export function enableTerminalIme(term: Terminal, container: HTMLElement): void {
  const textarea = container.querySelector("textarea.xterm-helper-textarea");
  if (textarea instanceof HTMLTextAreaElement) {
    const lang = document.documentElement.lang || navigator.language || "ko";
    textarea.lang = lang;
    textarea.setAttribute("inputmode", "text");
    textarea.setAttribute("autocomplete", "off");
    textarea.setAttribute("autocapitalize", "off");
    textarea.setAttribute("spellcheck", "false");
  }
  term.attachCustomKeyEventHandler((event) => {
    if (event.isComposing || event.key === "Process" || event.keyCode === 229) {
      return false;
    }
    return true;
  });
}

/** True when a CJK/IME composition is live — window listeners must stand down. */
export function isImeComposing(event: KeyboardEvent): boolean {
  return event.isComposing || event.key === "Process" || event.keyCode === 229;
}

/** Finger/pen always pan. Mouse pans only on phones (`hover: none`). */
export function shouldPanTerminalPointer(
  event: { isPrimary: boolean; pointerType: string },
  hoverNone: boolean,
): boolean {
  if (!event.isPrimary) return false;
  if (event.pointerType === "touch" || event.pointerType === "pen") return true;
  return hoverNone && event.pointerType === "mouse";
}

/** Convert a vertical pointer delta into whole xterm rows, keeping the remainder. */
export function linesFromPointerDelta(
  deltaY: number,
  cellHeight: number,
  accumulated: number,
): { lines: number; acc: number } {
  const height = cellHeight > 0 ? cellHeight : 16;
  let acc = accumulated + -deltaY / height;
  const raw = acc < 0 ? Math.ceil(acc) : Math.floor(acc);
  const lines = raw === 0 ? 0 : raw;
  return { lines, acc: acc - lines };
}

/**
 * xterm 6 paints a fixed viewport and scrolls with a custom wheel handler —
 * the `.xterm-viewport` box is not a native scroller (`scrollHeight ===
 * clientHeight`), so a finger drag does nothing. Capture non-mouse pointers
 * (and mouse on `hover: none` devices) and map pans to `scrollLines`.
 */
export function enableTerminalTouchScroll(term: Terminal, container: HTMLElement): () => void {
  const element = term.element ?? container;
  const PAN_THRESHOLD_PX = 8;
  let pointerId: number | null = null;
  let lastY = 0;
  let startY = 0;
  let acc = 0;
  let panning = false;

  const cellHeight = (): number => {
    const screen = element.querySelector(".xterm-screen");
    const height =
      screen instanceof HTMLElement && screen.clientHeight > 0
        ? screen.clientHeight
        : element.clientHeight;
    const rows = term.rows;
    if (rows < 1 || height < 1) return 16;
    return height / rows;
  };

  const hoverNone = (): boolean =>
    typeof window.matchMedia === "function" && window.matchMedia("(hover: none)").matches;

  const onPointerDown = (event: PointerEvent) => {
    if (!shouldPanTerminalPointer(event, hoverNone())) return;
    pointerId = event.pointerId;
    lastY = event.clientY;
    startY = event.clientY;
    acc = 0;
    panning = false;
    term.focus();
    event.preventDefault();
    try {
      element.setPointerCapture(event.pointerId);
    } catch {
      /* capture is best-effort: the node may not be connected */
    }
  };

  const onPointerMove = (event: PointerEvent) => {
    if (pointerId !== event.pointerId) return;
    const y = event.clientY;
    const dy = y - lastY;
    lastY = y;
    if (!panning) {
      if (Math.abs(y - startY) < PAN_THRESHOLD_PX) return;
      panning = true;
    }
    const next = linesFromPointerDelta(dy, cellHeight(), acc);
    acc = next.acc;
    if (next.lines !== 0) term.scrollLines(next.lines);
    event.preventDefault();
  };

  const onPointerUp = (event: PointerEvent) => {
    if (pointerId !== event.pointerId) return;
    pointerId = null;
    panning = false;
    acc = 0;
    try {
      element.releasePointerCapture(event.pointerId);
    } catch {
      /* already released */
    }
  };

  const opts: AddEventListenerOptions = { capture: true, passive: false };
  element.addEventListener("pointerdown", onPointerDown, opts);
  element.addEventListener("pointermove", onPointerMove, opts);
  element.addEventListener("pointerup", onPointerUp, opts);
  element.addEventListener("pointercancel", onPointerUp, opts);
  element.addEventListener("lostpointercapture", onPointerUp, opts);

  return () => {
    element.removeEventListener("pointerdown", onPointerDown, opts);
    element.removeEventListener("pointermove", onPointerMove, opts);
    element.removeEventListener("pointerup", onPointerUp, opts);
    element.removeEventListener("pointercancel", onPointerUp, opts);
    element.removeEventListener("lostpointercapture", onPointerUp, opts);
  };
}
