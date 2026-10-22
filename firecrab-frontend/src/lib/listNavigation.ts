import { useEffect } from "react";
import { editorOpen, isTypingTarget, overlayOpen } from "./shortcuts";

interface ListNavigationOptions {
  /** Row ids in display order. */
  ids: string[];
  selectedId: string | null;
  onSelect: (id: string) => void;
  enabled: boolean;
}

/**
 * ↑/↓ (and j/k) move the selection through a list. Quiet while typing in a
 * field, while a modal, menu or the palette is open, while a form is
 * mid-edit, and for keys pressed inside the detail panel (tabs, log, buttons
 * there use the arrows themselves). `Esc` belongs to the detail panel.
 */
export function useListNavigation({ ids, selectedId, onSelect, enabled }: ListNavigationOptions): void {
  useEffect(() => {
    if (!enabled) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.metaKey || event.ctrlKey || event.altKey || event.defaultPrevented) return;
      if (isTypingTarget(event.target) || overlayOpen() || editorOpen()) return;
      if (event.target instanceof Element && event.target.closest(".detail-panel")) return;
      const step = event.key === "ArrowDown" || event.key === "j" ? 1 : event.key === "ArrowUp" || event.key === "k" ? -1 : 0;
      if (step === 0 || ids.length === 0) return;
      event.preventDefault();
      const index = selectedId === null ? -1 : ids.indexOf(selectedId);
      const next = index === -1
        ? (step > 0 ? 0 : ids.length - 1)
        : Math.min(ids.length - 1, Math.max(0, index + step));
      onSelect(ids[next]);
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [ids, selectedId, onSelect, enabled]);
}
