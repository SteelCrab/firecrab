import type { ITerminalInitOnlyOptions, ITerminalOptions } from "@xterm/xterm";
import type { InteractiveTerminal, TerminalEngine } from "./terminalEngine";

type TerminalCtorOptions = ITerminalOptions & ITerminalInitOnlyOptions;

/**
 * The dashboard's mono token first (index.html loads it), then system CJK so
 * Hangul/Kana/Han measure and render. JetBrains Mono has no CJK glyphs;
 * without these fallbacks composed syllables are tofu and xterm's cell width
 * is wrong.
 */
export const TERMINAL_FONT_FAMILY =
  '"JetBrains Mono", "Noto Sans KR", "Noto Sans JP", "Noto Sans SC", "D2Coding", "NanumGothicCoding", "Malgun Gothic", "Apple SD Gothic Neo", "Yu Gothic", "Microsoft YaHei", ui-monospace, monospace';

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
 * Point the hidden terminal textarea at the page language so the OS IME
 * (Hangul, Kana, Pinyin, …) attaches. Skip window shortcuts while a
 * composition is in progress so Escape does not cancel Hangul mid-syllable
 * and then also leave terminal-only mode.
 */
export function enableTerminalIme(term: InteractiveTerminal, container: HTMLElement, engine: TerminalEngine): void {
  const textarea = term.textarea ?? container.querySelector("textarea");
  if (textarea instanceof HTMLTextAreaElement) {
    const lang = document.documentElement.lang || navigator.language || "ko";
    textarea.lang = lang;
    textarea.setAttribute("inputmode", "text");
    textarea.setAttribute("autocomplete", "off");
    textarea.setAttribute("autocapitalize", "off");
    textarea.setAttribute("spellcheck", "false");
  }
  term.attachCustomKeyEventHandler((event) => {
    const composing = event.isComposing || event.key === "Process" || event.keyCode === 229;
    // Ghostty 0.4 consumes true; xterm consumes false. Both own composition.
    return engine === "ghostty" ? composing : !composing;
  });
}

/** True when a CJK/IME composition is live — window listeners must stand down. */
export function isImeComposing(event: KeyboardEvent): boolean {
  return event.isComposing || event.key === "Process" || event.keyCode === 229;
}
