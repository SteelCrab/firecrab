import type { ITerminalInitOnlyOptions, ITerminalOptions, Terminal } from "@xterm/xterm";
import { copyText } from "./textExport";

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

export const TERMINAL_PAN_THRESHOLD_PX = 10;
export const TERMINAL_HOLD_MS = 500;

export type TerminalGesture = "pending" | "pan" | "hold";

/** Quick drag pans. A still press that lasts `holdMs` becomes hold (select/paste). */
export function classifyTerminalGesture(
  elapsedMs: number,
  distancePx: number,
  already: TerminalGesture,
  panThresholdPx = TERMINAL_PAN_THRESHOLD_PX,
  holdMs = TERMINAL_HOLD_MS,
): TerminalGesture {
  if (already === "pan" || already === "hold") return already;
  if (distancePx >= panThresholdPx && elapsedMs < holdMs) return "pan";
  if (elapsedMs >= holdMs) return "hold";
  return "pending";
}

/** Inclusive linear range for `Terminal.select`. Rows are buffer rows. */
export function linearSelect(
  cols: number,
  startCol: number,
  startRow: number,
  endCol: number,
  endRow: number,
): { column: number; row: number; length: number } {
  const width = cols > 0 ? cols : 1;
  const a = startRow * width + startCol;
  const b = endRow * width + endCol;
  const from = Math.min(a, b);
  const to = Math.max(a, b);
  return {
    column: ((from % width) + width) % width,
    row: Math.floor(from / width),
    length: Math.max(1, to - from + 1),
  };
}

function parkHelperTextarea(
  container: HTMLElement,
  clientX: number,
  clientY: number,
  text: string,
): HTMLTextAreaElement | null {
  const textarea = container.querySelector("textarea.xterm-helper-textarea");
  if (!(textarea instanceof HTMLTextAreaElement)) return null;
  const screen = container.querySelector(".xterm-screen");
  const origin = screen instanceof HTMLElement ? screen : container;
  const pos = origin.getBoundingClientRect();
  textarea.style.width = "44px";
  textarea.style.height = "44px";
  textarea.style.left = `${clientX - pos.left - 22}px`;
  textarea.style.top = `${clientY - pos.top - 22}px`;
  textarea.style.opacity = "0.01";
  textarea.style.zIndex = "1000";
  textarea.readOnly = false;
  textarea.value = text;
  textarea.focus();
  if (text) textarea.select();
  return textarea;
}

function bufferCellAt(
  term: Terminal,
  element: HTMLElement,
  clientX: number,
  clientY: number,
): { col: number; row: number } | null {
  const screen = element.querySelector(".xterm-screen");
  if (!(screen instanceof HTMLElement)) return null;
  const rect = screen.getBoundingClientRect();
  if (rect.width < 1 || rect.height < 1 || term.cols < 1 || term.rows < 1) return null;
  const col = Math.max(
    0,
    Math.min(term.cols - 1, Math.floor((clientX - rect.left) / (rect.width / term.cols))),
  );
  const viewRow = Math.max(
    0,
    Math.min(term.rows - 1, Math.floor((clientY - rect.top) / (rect.height / term.rows))),
  );
  return { col, row: term.buffer.active.viewportY + viewRow };
}

/**
 * xterm 6 has no native overflow, so a quick finger drag must call
 * `scrollLines`. Do not steal the gesture on pointerdown: a still hold is
 * paste (OS menu on the helper textarea) and hold-then-drag selects for copy.
 */
export function enableTerminalTouchScroll(term: Terminal, container: HTMLElement): () => void {
  const element = term.element ?? container;
  let pointerId: number | null = null;
  let lastY = 0;
  let startX = 0;
  let startY = 0;
  let startAt = 0;
  let acc = 0;
  let mode: TerminalGesture = "pending";
  let holdTimer: ReturnType<typeof setTimeout> | null = null;
  let anchor: { col: number; row: number } | null = null;
  let lastClientX = 0;
  let lastClientY = 0;

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

  const clearHoldTimer = () => {
    if (holdTimer !== null) {
      clearTimeout(holdTimer);
      holdTimer = null;
    }
  };

  const applyHold = (clientX: number, clientY: number) => {
    mode = "hold";
    anchor = bufferCellAt(term, element, clientX, clientY);
    if (anchor) {
      term.select(anchor.col, anchor.row, 1);
    }
    parkHelperTextarea(element, clientX, clientY, term.getSelection());
    element.dispatchEvent(
      new MouseEvent("contextmenu", {
        bubbles: true,
        cancelable: true,
        clientX,
        clientY,
        view: window,
      }),
    );
  };

  const applySelect = (clientX: number, clientY: number) => {
    const end = bufferCellAt(term, element, clientX, clientY);
    if (!anchor || !end) return;
    const range = linearSelect(term.cols, anchor.col, anchor.row, end.col, end.row);
    term.select(range.column, range.row, range.length);
    parkHelperTextarea(element, clientX, clientY, term.getSelection());
  };

  const onPointerDown = (event: PointerEvent) => {
    if (!shouldPanTerminalPointer(event, hoverNone())) return;
    pointerId = event.pointerId;
    lastY = event.clientY;
    startX = event.clientX;
    startY = event.clientY;
    lastClientX = event.clientX;
    lastClientY = event.clientY;
    startAt = event.timeStamp;
    acc = 0;
    mode = "pending";
    anchor = null;
    term.focus();
    clearHoldTimer();
    holdTimer = setTimeout(() => {
      holdTimer = null;
      if (pointerId !== event.pointerId || mode !== "pending") return;
      applyHold(lastClientX, lastClientY);
    }, TERMINAL_HOLD_MS);
  };

  const onPointerMove = (event: PointerEvent) => {
    if (pointerId !== event.pointerId) return;
    lastClientX = event.clientX;
    lastClientY = event.clientY;
    const distance = Math.hypot(event.clientX - startX, event.clientY - startY);
    const nextMode = classifyTerminalGesture(event.timeStamp - startAt, distance, mode);
    if (nextMode === "pan" && mode !== "pan") {
      clearHoldTimer();
      term.clearSelection();
      try {
        element.setPointerCapture(event.pointerId);
      } catch {
        /* capture is best-effort */
      }
    }
    mode = nextMode;
    if (mode === "hold") {
      applySelect(event.clientX, event.clientY);
      event.preventDefault();
      return;
    }
    if (mode !== "pan") return;
    const y = event.clientY;
    const dy = y - lastY;
    lastY = y;
    const next = linesFromPointerDelta(dy, cellHeight(), acc);
    acc = next.acc;
    if (next.lines !== 0) term.scrollLines(next.lines);
    event.preventDefault();
  };

  const onPointerUp = (event: PointerEvent) => {
    if (pointerId !== event.pointerId) return;
    clearHoldTimer();
    const ended = mode;
    pointerId = null;
    mode = "pending";
    acc = 0;
    try {
      element.releasePointerCapture(event.pointerId);
    } catch {
      /* already released */
    }
    if (ended === "hold") {
      const selected = term.getSelection();
      if (selected) {
        void copyText(selected);
        parkHelperTextarea(element, event.clientX, event.clientY, selected);
      } else {
        parkHelperTextarea(element, event.clientX, event.clientY, "");
      }
    }
  };

  const opts: AddEventListenerOptions = { capture: true, passive: false };
  element.addEventListener("pointerdown", onPointerDown, opts);
  element.addEventListener("pointermove", onPointerMove, opts);
  element.addEventListener("pointerup", onPointerUp, opts);
  element.addEventListener("pointercancel", onPointerUp, opts);
  element.addEventListener("lostpointercapture", onPointerUp, opts);

  return () => {
    clearHoldTimer();
    element.removeEventListener("pointerdown", onPointerDown, opts);
    element.removeEventListener("pointermove", onPointerMove, opts);
    element.removeEventListener("pointerup", onPointerUp, opts);
    element.removeEventListener("pointercancel", onPointerUp, opts);
    element.removeEventListener("lostpointercapture", onPointerUp, opts);
  };
}
