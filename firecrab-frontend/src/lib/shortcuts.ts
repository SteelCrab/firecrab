const APPLE_PLATFORM = /Mac|iPhone|iPad/.test(navigator.userAgent);

/** How the command palette shortcut is written on this keyboard. */
export const PALETTE_SHORTCUT = APPLE_PLATFORM ? "⌘K" : "Ctrl K";

/** ⌘K or Ctrl+K, with no other modifier. Both work on every platform. */
export function isPaletteShortcut(event: KeyboardEvent): boolean {
  return (event.metaKey || event.ctrlKey) && !event.altKey && !event.shiftKey && event.key.toLowerCase() === "k";
}

/** Typing into a field, where single-key shortcuts must not fire. */
export function isTypingTarget(target: EventTarget | null): boolean {
  return target instanceof HTMLElement && (target.isContentEditable || target.matches("input, textarea, select"));
}

/** A modal, menu, tooltip or the palette is open; page-level shortcuts stay quiet. */
export function overlayOpen(): boolean {
  return document.querySelector('[aria-modal="true"], [role="menu"], [role="tooltip"]') !== null;
}

/** Some form is mid-edit (marked `data-editing`); keys must not throw its inputs away. */
export function editorOpen(): boolean {
  return document.querySelector("[data-editing]") !== null;
}
