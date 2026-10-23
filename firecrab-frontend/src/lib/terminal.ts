import type { ITerminalOptions, Terminal } from "ghostty-web";

/** System CJK fallbacks supplement the dashboard's JetBrains Mono font. */
export const TERMINAL_FONT_FAMILY =
  '"JetBrains Mono", "Noto Sans KR", "Noto Sans JP", "Noto Sans SC", "D2Coding", "NanumGothicCoding", "Malgun Gothic", "Apple SD Gothic Neo", "Yu Gothic", "Microsoft YaHei", ui-monospace, monospace';

// ghostty-web The pinned package passes scrollback through as bytes, not lines (upstream #140).
export function defaultInteractiveTerminalOptions(overrides: ITerminalOptions = {}): ITerminalOptions {
  return { convertEol: false, fontFamily: TERMINAL_FONT_FAMILY, fontSize: 13,
    scrollback: 8 * 1024 * 1024, cursorBlink: true, ...overrides };
}

/** Connect browser composition, mobile editing and clipboard events to WASM input. */
export function enableTerminalInput(term: Terminal, signal: AbortSignal): void {
  const textarea = term.textarea;
  if (!textarea) throw new Error("The WASM terminal did not create its input field");
  textarea.lang = document.documentElement.lang || navigator.language || "ko";
  textarea.setAttribute("inputmode", "text");
  textarea.setAttribute("autocomplete", "off");
  textarea.setAttribute("autocapitalize", "off");
  textarea.setAttribute("spellcheck", "false");
  textarea.setAttribute("aria-label", document.documentElement.lang === "ko" ? "터미널 입력" : "Terminal input");
  let composing = false;
  let committed = "";
  let commitTimer: ReturnType<typeof setTimeout> | undefined;
  textarea.addEventListener("compositionstart", () => {
    composing = true;
    committed = "";
    clearTimeout(commitTimer);
  }, { signal });
  textarea.addEventListener("compositionend", event => {
    composing = false;
    committed = event.data;
    // Ghostty sends compositionend. Ignore the following native input event.
    commitTimer = setTimeout(() => { committed = ""; textarea.value = ""; }, 0);
  }, { signal });
  signal.addEventListener("abort", () => clearTimeout(commitTimer), { once: true });

  const mobileInput = (event: InputEvent): boolean => {
    if (composing || event.isComposing) return false;
    if (event.inputType.includes("Composition") || (committed && event.data === committed)) {
      committed = "";
      textarea.value = "";
      return true;
    }
    let text: string | null = event.data;
    if (event.inputType === "deleteContentBackward") text = "\x7f";
    else if (event.inputType === "deleteContentForward") text = "\x1b[3~";
    else if (event.inputType === "insertLineBreak" || event.inputType === "insertParagraph") text = "\r";
    else if (event.inputType !== "insertText" && event.inputType !== "insertFromPaste") return false;
    text ??= textarea.value;
    if (text) {
      if (event.inputType === "insertFromPaste") term.paste(text);
      else term.input(text, true);
    }
    textarea.value = "";
    return true;
  };
  textarea.addEventListener("beforeinput", event => {
    if (event.cancelable && mobileInput(event)) { event.preventDefault(); event.stopImmediatePropagation(); }
  }, { capture: true, signal });
  textarea.addEventListener("input", event => {
    if (mobileInput(event as InputEvent)) event.stopImmediatePropagation();
  }, { capture: true, signal });
  textarea.addEventListener("copy", event => {
    if (!term.hasSelection() || !event.clipboardData) return;
    event.clipboardData.setData("text/plain", term.getSelection());
    event.preventDefault();
  }, { signal });
  term.attachCustomKeyEventHandler(event => {
    if (isImeComposing(event)) return true;
    if (event.shiftKey && !event.ctrlKey && !event.altKey && !event.metaKey && (event.code === "PageUp" || event.code === "PageDown")) {
      term.scrollPages(event.code === "PageUp" ? -1 : 1);
      return true;
    }
    if (event.ctrlKey && !event.altKey && event.code === "KeyC") return term.copySelection();
    return false;
  });
}

export function isImeComposing(event: KeyboardEvent): boolean {
  return event.isComposing || event.key === "Process" || event.keyCode === 229;
}
