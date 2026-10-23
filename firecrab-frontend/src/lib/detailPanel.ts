import { useCallback, useState } from "react";

const STORAGE_KEY = "firecrab.detailPanel";

function storedOpen(): boolean {
  try {
    return localStorage.getItem(STORAGE_KEY) !== "closed";
  } catch {
    return true;
  }
}

/** Wide-screen detail column, open unless the user closed it; remembered per browser. */
export function useDetailPanelOpen(initialSelection = false): [open: boolean, setOpen: (open: boolean) => void] {
  const [open, setOpenState] = useState(() => initialSelection || storedOpen());
  const setOpen = useCallback((next: boolean) => {
    setOpenState(next);
    try {
      localStorage.setItem(STORAGE_KEY, next ? "open" : "closed");
    } catch {
      // Remembering the choice is optional; private browsing can reject storage.
    }
  }, []);
  return [open, setOpen];
}

const WIDTH_KEY = "firecrab.detailPanelWidth";

/** Width limits of the detail column, in CSS pixels. */
export const PANEL_WIDTH = { min: 320, initial: 384, max: 720 } as const;

/** Room the main column keeps beside the panel on wide screens. */
export const MAIN_MIN_WIDTH = 480;

/** `width` inside the panel limits and never past `max` (the room left on this screen). */
export function clampPanelWidth(width: number, max: number = PANEL_WIDTH.max): number {
  return Math.round(Math.min(Math.max(width, PANEL_WIDTH.min), Math.max(PANEL_WIDTH.min, Math.min(max, PANEL_WIDTH.max))));
}

function storedWidth(): number {
  try {
    const value = Number(localStorage.getItem(WIDTH_KEY));
    return Number.isFinite(value) && value > 0 ? clampPanelWidth(value) : PANEL_WIDTH.initial;
  } catch {
    return PANEL_WIDTH.initial;
  }
}

/** Width the user dragged the detail panel to; shared by every screen, remembered per browser. */
export function useDetailPanelWidth(): [width: number, setWidth: (width: number) => void] {
  const [width, setWidthState] = useState(storedWidth);
  const setWidth = useCallback((next: number) => {
    const clamped = clampPanelWidth(next);
    setWidthState(clamped);
    try {
      localStorage.setItem(WIDTH_KEY, String(clamped));
    } catch {
      // Remembering the width is optional; private browsing can reject storage.
    }
  }, []);
  return [width, setWidth];
}
