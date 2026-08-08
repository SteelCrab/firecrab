import type { ReactNode } from "react";
import { VIEWS, viewLabel } from "../navigation";
import type { ViewId } from "../navigation";
import { useI18n } from "../i18n";

interface ShellProps {
  view: ViewId;
  onSelectView: (view: ViewId) => void;
  children: ReactNode;
}

/**
 * Header + left nav + content frame every screen renders inside. The nav
 * collapses to an icon rail and then to a horizontal strip via CSS alone —
 * no breakpoint state to keep in sync with the stylesheet.
 */
export default function Shell({ view, onSelectView, children }: ShellProps) {
  const { locale, setLocale, t } = useI18n();
  return (
    <div className="shell">
      <header className="shell-header">
        <div className="shell-brand">
          <p className="eyebrow">private microvm cloud</p>
          <h1 className="wordmark">
            firecrab
            <span className="cursor">_</span>
          </h1>
        </div>
        <label className="language-select">
          <span className="sr-only">{t("Language", "언어")}</span>
          <select
            value={locale}
            onChange={(event) => setLocale(event.target.value as "en" | "ko")}
            aria-label={t("Language", "언어")}
          >
            <option value="en">English</option>
            <option value="ko">한국어</option>
          </select>
        </label>
      </header>
      <div className="shell-body">
        <nav className="shell-nav" aria-label={t("Main navigation", "주요 메뉴")}>
          <ul>
            {VIEWS.map((item) => {
              const label = viewLabel(item, locale);
              return (
              <li key={item.id}>
                <button
                  type="button"
                  className={item.id === view ? "shell-nav-item active" : "shell-nav-item"}
                  aria-current={item.id === view ? "page" : undefined}
                  // The rail hides the label, so the accessible name has to
                  // come from here; `title` gives the same text on hover.
                  aria-label={label}
                  title={label}
                  onClick={() => onSelectView(item.id)}
                >
                  {"icon" in item && item.icon ? (
                    <img
                      className="shell-nav-icon"
                      src={item.icon}
                      alt=""
                      width={18}
                      height={18}
                      aria-hidden="true"
                    />
                  ) : (
                    <span className="shell-nav-glyph" aria-hidden="true">
                      {item.glyph}
                    </span>
                  )}
                  <span className="shell-nav-label">{label}</span>
                </button>
              </li>
              );
            })}
          </ul>
        </nav>
        <main className="shell-content">{children}</main>
      </div>
    </div>
  );
}
