import { useCallback, useEffect, useState } from "react";

/**
 * The console's five destinations. All but `images` have a page today; that
 * one is a placeholder until the image catalog lands. The glyph is what the
 * nav shows once it collapses to a rail.
 *
 * Kept out of `components/Shell.tsx` so that file exports only its component
 * — mixing constants and hooks in there breaks Vite's fast refresh.
 */
export const VIEWS = [
  { id: "vms", label: "MicroVM", glyph: "▣" },
  { id: "networks", label: "네트워크", glyph: "◇" },
  { id: "storages", label: "스토리지", glyph: "▤" },
  { id: "images", label: "이미지", glyph: "◈" },
  { id: "host", label: "호스트", glyph: "◉" },
] as const;

export type ViewId = (typeof VIEWS)[number]["id"];

const DEFAULT_VIEW: ViewId = "vms";

function parseHash(hash: string): ViewId | null {
  const id = hash.replace(/^#\/?/, "");
  return VIEWS.some((view) => view.id === id) ? (id as ViewId) : null;
}

/**
 * Current view, kept in `location.hash` so a reload (and back/forward) lands
 * on the same screen. The hash — not `history.pushState` paths — because the
 * API serves the dashboard itself: a deep path would be a real request the
 * SPA fallback has to absorb, while `#/vms` never leaves the client. Swapping
 * this for react-router later is a drop-in (`HashRouter` reads the same URLs).
 */
export function useHashView(): [ViewId, (view: ViewId) => void] {
  const [view, setView] = useState<ViewId>(() => parseHash(window.location.hash) ?? DEFAULT_VIEW);

  useEffect(() => {
    const onHashChange = () => setView(parseHash(window.location.hash) ?? DEFAULT_VIEW);
    window.addEventListener("hashchange", onHashChange);
    return () => window.removeEventListener("hashchange", onHashChange);
  }, []);

  // An empty or unknown hash is normalised without a history entry, so the
  // back button still leaves the app instead of bouncing off a rewrite.
  useEffect(() => {
    if (parseHash(window.location.hash) === null) {
      window.history.replaceState(null, "", `#/${DEFAULT_VIEW}`);
    }
  }, [view]);

  const select = useCallback((next: ViewId) => {
    window.location.hash = `#/${next}`;
  }, []);

  return [view, select];
}
