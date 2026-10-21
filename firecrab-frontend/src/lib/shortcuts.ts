const APPLE_PLATFORM = /Mac|iPhone|iPad/.test(navigator.userAgent);

/** How the command palette shortcut is written on this keyboard. */
export const PALETTE_SHORTCUT = APPLE_PLATFORM ? "⌘K" : "Ctrl K";

/** ⌘K or Ctrl+K, with no other modifier. Both work on every platform. */
export function isPaletteShortcut(event: KeyboardEvent): boolean {
  return (event.metaKey || event.ctrlKey) && !event.altKey && !event.shiftKey && event.key.toLowerCase() === "k";
}
