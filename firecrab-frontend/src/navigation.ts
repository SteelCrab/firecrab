import { useCallback, useEffect, useState } from "react";
import type { Locale } from "./i18n";

/**
 * Dashboard destinations. Rendered as a `NavIcon` (see `NavIcons.tsx`) in the
 * rail, except `shells`, whose `icon` is a small image (the Shell repository
 * bash cube) shown instead.
 *
 * Kept out of `components/Shell.tsx` so that file exports only its component
 * — mixing constants and hooks in there breaks Vite's fast refresh.
 */
export const VIEWS = [
  { id: "vms", labels: { en: "MicroVM", ko: "MicroVM" } },
  { id: "networks", labels: { en: "Networks", ko: "네트워크" } },
  { id: "storages", labels: { en: "Storage", ko: "스토리지" } },
  { id: "images", labels: { en: "Images", ko: "이미지" } },
  { id: "kernels", labels: { en: "Kernels", ko: "커널" } },
  { id: "shells", labels: { en: "Shells", ko: "Shell" }, icon: "/bash.png" },
  { id: "host", labels: { en: "Host", ko: "호스트" } },
] as const;

export type ViewId = (typeof VIEWS)[number]["id"];

export function viewLabel(view: (typeof VIEWS)[number], locale: Locale): string {
  return view.labels[locale];
}

const DEFAULT_VIEW: ViewId = "vms";

/**
 * Full-page serial console (not a modal). Hash form so the API host still
 * serves `index.html` and a reload keeps the session URL:
 * `#/console/<vmId>`.
 */
const CONSOLE_RE = /^console\/([^/?#]+)/;

function stripHash(hash: string): string {
  return hash.replace(/^#\/?/, "");
}

function parseViewId(path: string): ViewId | null {
  return VIEWS.some((view) => view.id === path) ? (path as ViewId) : null;
}

/** VM id when the hash is a console page, otherwise `null`. */
export function parseConsoleVmId(hash: string = window.location.hash): string | null {
  const match = stripHash(hash).match(CONSOLE_RE);
  if (!match) return null;
  try {
    return decodeURIComponent(match[1] ?? "");
  } catch {
    return match[1] ?? null;
  }
}

/** Hash URL for the dedicated console page (same-tab or `window.open`). */
export function consoleHash(vmId: string): string {
  return `#/console/${encodeURIComponent(vmId)}`;
}

/** Absolute URL for opening the console in a new browser tab/window. */
export function consolePageUrl(vmId: string): string {
  return `${window.location.origin}${window.location.pathname}${consoleHash(vmId)}`;
}

/**
 * Open the serial console in a **new browser tab** (not a sized popup).
 * Sized `window.open(..., "width=…")` windows are easy to miss / get blocked;
 * `_blank` keeps the console in the tab strip.
 *
 * If the browser blocks the new tab, fall back to same-tab navigation.
 */
export function openConsoleWindow(vmId: string): Window | null {
  const url = consolePageUrl(vmId);
  const win = window.open(url, "_blank", "noopener,noreferrer");
  if (!win) {
    window.location.assign(url);
    return null;
  }
  try {
    win.focus();
  } catch {
    /* ignore */
  }
  return win;
}

export function viewHash(view: ViewId = DEFAULT_VIEW): string {
  return `#/${view}`;
}

/** Dedicated New MicroVM page. Not a `VIEWS` id — `#/vms` stays the list. */
const NEW_VM_PATH = "vms/new";

export function newVmHash(): string {
  return `#/${NEW_VM_PATH}`;
}

/**
 * Shell views live under `#/vms` etc.; the terminal is a separate full-page
 * route (`#/console/<id>`) so it never shares the modal overlay layout that
 * clipped xterm after leaving "fullscreen".
 */
export type AppRoute =
  | { kind: "shell"; view: ViewId }
  | { kind: "console"; vmId: string }
  | { kind: "vm-new" };

export function parseAppRoute(hash: string = window.location.hash): AppRoute {
  const path = stripHash(hash);
  const consoleMatch = path.match(CONSOLE_RE);
  if (consoleMatch?.[1]) {
    try {
      return { kind: "console", vmId: decodeURIComponent(consoleMatch[1]) };
    } catch {
      return { kind: "console", vmId: consoleMatch[1] };
    }
  }
  if (path === NEW_VM_PATH) return { kind: "vm-new" };
  return { kind: "shell", view: parseViewId(path) ?? DEFAULT_VIEW };
}

/**
 * Current route from `location.hash` so reload and back/forward work. Hash
 * (not path routes) because the API serves the dashboard SPA — a deep path
 * would hit the server; `#/…` never leaves the client.
 */
export function useAppRoute(): {
  route: AppRoute;
  selectView: (view: ViewId) => void;
  openConsole: (vmId: string) => void;
  closeConsole: () => void;
} {
  const [route, setRoute] = useState<AppRoute>(() => parseAppRoute());

  useEffect(() => {
    const onHashChange = () => setRoute(parseAppRoute());
    window.addEventListener("hashchange", onHashChange);
    return () => window.removeEventListener("hashchange", onHashChange);
  }, []);

  // Empty / unknown shell hashes normalise to `#/vms`. Use `location.hash`
  // (not only replaceState) so the URL bar and any hash-dependent code see
  // the same route immediately on first paint after `npm run dev`.
  useEffect(() => {
    if (route.kind !== "shell") return;
    const path = stripHash(window.location.hash);
    // `#/vms/new` is not a ViewId; do not rewrite it to the list.
    if (path === NEW_VM_PATH) return;
    if (parseViewId(path) === null && parseConsoleVmId(window.location.hash) === null) {
      const next = viewHash(DEFAULT_VIEW);
      if (window.location.hash !== next) {
        window.history.replaceState(null, "", next);
        setRoute({ kind: "shell", view: DEFAULT_VIEW });
      }
    }
  }, [route]);

  const selectView = useCallback((next: ViewId) => {
    window.location.hash = viewHash(next);
  }, []);

  const openConsole = useCallback((vmId: string) => {
    window.location.hash = consoleHash(vmId);
  }, []);

  const closeConsole = useCallback(() => {
    window.location.hash = viewHash("vms");
  }, []);

  return { route, selectView, openConsole, closeConsole };
}

/** @deprecated Prefer `useAppRoute` — kept name for older call sites if any. */
export function useHashView(): [ViewId, (view: ViewId) => void] {
  const { route, selectView } = useAppRoute();
  // Console and `#/vms/new` still highlight MicroVM; `selectView("vms")` is the list.
  const view = route.kind === "shell" ? route.view : DEFAULT_VIEW;
  return [view, selectView];
}
