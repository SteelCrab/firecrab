import type { ReactNode } from "react";
import { VIEWS } from "../navigation";
import type { ViewId } from "../navigation";

interface ShellProps {
  view: ViewId;
  onSelectView: (view: ViewId) => void;
  /** Header-level controls (still modal buttons until the pages land). */
  actions?: ReactNode;
  children: ReactNode;
}

/**
 * Header + left nav + content frame every screen renders inside. The nav
 * collapses to an icon rail and then to a horizontal strip via CSS alone —
 * no breakpoint state to keep in sync with the stylesheet.
 */
export default function Shell({ view, onSelectView, actions, children }: ShellProps) {
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
        {actions && <div className="shell-actions">{actions}</div>}
      </header>
      <div className="shell-body">
        <nav className="shell-nav" aria-label="주요 메뉴">
          <ul>
            {VIEWS.map((item) => (
              <li key={item.id}>
                <button
                  type="button"
                  className={item.id === view ? "shell-nav-item active" : "shell-nav-item"}
                  aria-current={item.id === view ? "page" : undefined}
                  // The rail hides the label, so the accessible name has to
                  // come from here; `title` gives the same text on hover.
                  aria-label={item.label}
                  title={item.label}
                  onClick={() => onSelectView(item.id)}
                >
                  <span className="shell-nav-glyph" aria-hidden="true">
                    {item.glyph}
                  </span>
                  <span className="shell-nav-label">{item.label}</span>
                </button>
              </li>
            ))}
          </ul>
        </nav>
        <main className="shell-content">{children}</main>
      </div>
    </div>
  );
}
