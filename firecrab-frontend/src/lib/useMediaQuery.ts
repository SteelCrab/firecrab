import { useCallback, useSyncExternalStore } from "react";

/** Whether `query` currently matches, updated when the viewport crosses it. */
export function useMediaQuery(query: string): boolean {
  const subscribe = useCallback((notify: () => void) => {
    const list = window.matchMedia(query);
    list.addEventListener("change", notify);
    return () => list.removeEventListener("change", notify);
  }, [query]);
  return useSyncExternalStore(subscribe, () => window.matchMedia(query).matches);
}

/** Wide enough for the three-column layout (spec §6.4). */
export const WIDE_LAYOUT = "(min-width: 1280px)";
