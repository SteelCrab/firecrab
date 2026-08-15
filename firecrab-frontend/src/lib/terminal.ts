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
