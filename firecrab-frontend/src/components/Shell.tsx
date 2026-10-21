import { useState } from "react";
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
export default function Shell({ view, onSelectView, onOpenPalette, vmCount, vmErrorCount, children }: ShellProps) {
  const { t } = useI18n();
  const [navOpen, setNavOpen] = useState(false);

  return (
    <div className="app-frame" data-nav-open={navOpen ? "" : undefined}>
      <button
        type="button"
        className="icon-btn nav-toggle"
        aria-label={t("Open menu", "메뉴 열기")}
        aria-expanded={navOpen}
        onClick={() => setNavOpen(true)}
      >
        <Menu aria-hidden="true" />
      </button>
      {navOpen && <div className="nav-backdrop" onClick={() => setNavOpen(false)} />}
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
      <div className="app-workspace">{children}</div>
    </div>
  );
}
