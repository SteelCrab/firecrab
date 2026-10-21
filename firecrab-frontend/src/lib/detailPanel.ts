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
export function useDetailPanelOpen(): [open: boolean, setOpen: (open: boolean) => void] {
  const [open, setOpenState] = useState(storedOpen);
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
