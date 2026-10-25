import { Search } from "lucide-react";
import { useHostStatus } from "../../hostStatusContext";
import { useI18n } from "../../i18n";
import { PALETTE_SHORTCUT } from "../../lib/shortcuts";
import { VIEWS, viewGroupLabel, viewLabel } from "../../navigation";
import type { ViewGroup, ViewId } from "../../navigation";
import { VIEW_ICONS } from "../../viewIcons";
import UpdateIndicator from "../UpdateIndicator";
import HostCard from "./HostCard";

interface SidebarNavProps {
  view: ViewId;
  onSelectView: (view: ViewId) => void;
  onOpenPalette: () => void;
  vmCount: number;
  vmErrorCount: number;
}

const GROUPS: ViewGroup[] = ["compute", "network", "data", "system", "ai"];

/** Left column: brand, palette entry, grouped destinations, host card, update and language (spec §6.1). */
export default function SidebarNav({ view, onSelectView, onOpenPalette, vmCount, vmErrorCount }: SidebarNavProps) {
  const { locale, setLocale, t } = useI18n();
  const host = useHostStatus();
  const searchLabel = t("Search and go", "검색 및 이동");

  return (
    <nav className="sidebar" aria-label={t("Main navigation", "주요 메뉴")}>
      <div className="sidebar-brand">
        <span className="sidebar-logo"><img src="/firecrab.png" alt="" width={20} height={20} /></span>
        <span className="sidebar-brand-text">
          <span className="sidebar-name">firecrab</span>
          <span className="sidebar-host">
            {host?.platform ? `${host.platform.name} · ${host.platform.architecture}` : "private microvm cloud"}
          </span>
        </span>
      </div>

      <button type="button" className="sidebar-search" aria-label={searchLabel} title={`${searchLabel} (${PALETTE_SHORTCUT})`} onClick={onOpenPalette}>
        <Search aria-hidden="true" />
        <span className="sidebar-label">{searchLabel}</span>
        <kbd>{PALETTE_SHORTCUT}</kbd>
      </button>

      <div className="sidebar-groups">
        {GROUPS.map((group) => (
          <div key={group} className="sidebar-group">
            <div className="sidebar-group-label">{viewGroupLabel(group, locale)}</div>
            <ul>
              {VIEWS.filter((item) => item.group === group).map((item) => {
                const label = viewLabel(item, locale);
                const Icon = VIEW_ICONS[item.id];
                const active = item.id === view;
                return (
                  <li key={item.id}>
                    <button
                      type="button"
                      className={active ? "sidebar-item active" : "sidebar-item"}
                      aria-current={active ? "page" : undefined}
                      // The icon rail hides the label, so the name comes from here.
                      aria-label={label}
                      title={label}
                      onClick={() => onSelectView(item.id)}
                    >
                      <Icon aria-hidden="true" />
                      <span className="sidebar-label">{label}</span>
                      {item.id === "vms" && (
                        <>
                          <span className="sidebar-count">{vmCount}</span>
                          {vmErrorCount > 0 && (
                            <span className="sidebar-alert" title={t(`${vmErrorCount} in error`, `오류 ${vmErrorCount}개`)}>{vmErrorCount}</span>
                          )}
                        </>
                      )}
                    </button>
                  </li>
                );
              })}
            </ul>
          </div>
        ))}
      </div>

      <HostCard />

      <div className="sidebar-foot">
        <UpdateIndicator />
        <label className="sidebar-language">
          <span className="sr-only">{t("Language", "언어")}</span>
          <select value={locale} onChange={(event) => setLocale(event.target.value as "en" | "ko")} aria-label={t("Language", "언어")}>
            <option value="en">English</option>
            <option value="ko">한국어</option>
          </select>
        </label>
      </div>
    </nav>
  );
}
