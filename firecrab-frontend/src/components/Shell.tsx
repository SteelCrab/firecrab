import { useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import { Menu } from "lucide-react";
import type { ViewId } from "../navigation";
import { useI18n } from "../i18n";
import SidebarNav from "./layout/SidebarNav";

interface ShellProps {
  view: ViewId;
  onSelectView: (view: ViewId) => void;
  onOpenPalette: () => void;
  vmCount: number;
  vmErrorCount: number;
  children: ReactNode;
}

/**
 * Sidebar + workspace frame every screen renders inside. Below 768px the
 * sidebar becomes a drawer opened from the menu button; the icon rail between
 * 768px and 1024px is CSS alone.
 */
export default function Shell({
  view,
  onSelectView,
  onOpenPalette,
  vmCount,
  vmErrorCount,
  children,
}: ShellProps) {
  const { t } = useI18n();
  const [navOpen, setNavOpen] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!navOpen) return;
    const nav = root.current?.querySelector("nav.sidebar"),
      toggle = root.current?.querySelector<HTMLButtonElement>(".nav-toggle");
    const targets = () =>
      [
        ...(toggle ? [toggle] : []),
        ...(nav?.querySelectorAll<HTMLElement>(
          "a[href],button:not([disabled]),select",
        ) || []),
      ].filter((node) => node.getClientRects().length > 0);
    const viewport = window.matchMedia("(min-width: 768px)");
    const resize = () => {
      if (viewport.matches) setNavOpen(false);
    };
    viewport.addEventListener("change", resize);
    const previous = document.activeElement;
    const first =
      targets().find((node) => node.getAttribute("aria-current") === "page") ||
      targets()[0];
    first?.focus();
    const handle = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopImmediatePropagation();
        setNavOpen(false);
      } else if (event.key === "Tab") {
        const list = targets(),
          index = list.indexOf(document.activeElement as HTMLElement);
        if (
          index === -1 ||
          (event.shiftKey && index === 0) ||
          (!event.shiftKey && index === list.length - 1)
        ) {
          event.preventDefault();
          (event.shiftKey ? list.at(-1) : list[0])?.focus();
        }
      }
    };
    window.addEventListener("keydown", handle, true);
    return () => {
      window.removeEventListener("keydown", handle, true);
      viewport.removeEventListener("change", resize);
      if (previous instanceof HTMLElement && previous.isConnected)
        previous.focus();
      else toggle?.focus();
    };
  }, [navOpen]);

  return (
    <div
      ref={root}
      className="app-frame"
      data-nav-open={navOpen ? "" : undefined}
    >
      <button
        type="button"
        id="nav-toggle"
        className="icon-btn nav-toggle"
        aria-label={t("Open menu", "메뉴 열기")}
        aria-expanded={navOpen}
        onClick={() => setNavOpen(true)}
      >
        <Menu aria-hidden="true" />
      </button>
      {navOpen && (
        <div className="nav-backdrop" onClick={() => setNavOpen(false)} />
      )}
      <SidebarNav
        view={view}
        onSelectView={(next) => {
          setNavOpen(false);
          onSelectView(next);
        }}
        onOpenPalette={() => {
          setNavOpen(false);
          onOpenPalette();
        }}
        vmCount={vmCount}
        vmErrorCount={vmErrorCount}
      />
      <main className="app-workspace" inert={navOpen}>
        {children}
      </main>
    </div>
  );
}
