import { useEffect } from "react";
import type { ReactNode } from "react";
import { X } from "lucide-react";
import { useI18n } from "../../i18n";
import { editorOpen, isTypingTarget, overlayOpen } from "../../lib/shortcuts";

interface DetailPanelProps {
  /** Accessible name of the panel, e.g. "MicroVM detail". */
  label: string;
  /** Small heading in the top row, e.g. an icon and "MicroVM". */
  kind?: ReactNode;
  /** Extra icon buttons placed before the close button. */
  tools?: ReactNode;
  onClose: () => void;
  children: ReactNode;
}

/** Right-hand column of the three-column frame (spec §6.3). Not a modal. */
export default function DetailPanel({ label, kind, tools, onClose, children }: DetailPanelProps) {
  const { t } = useI18n();

  // Capture phase: menus, tooltips and dialogs close themselves on Escape in
  // later listeners, so this one must look before they are gone. Escape
  // then belongs to them, and never discards a form that is mid-edit.
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || event.defaultPrevented) return;
      if (isTypingTarget(event.target) || overlayOpen() || editorOpen()) return;
      onClose();
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [onClose]);

  return (
    <aside className="detail-panel" aria-label={label}>
      <div className="detail-panel-top">
        <span className="detail-panel-kind">{kind}</span>
        {tools}
        <button
          type="button"
          className="icon-btn"
          aria-label={t("Close detail", "상세 닫기")}
          title={`${t("Close detail", "상세 닫기")} (esc)`}
          onClick={onClose}
        >
          <X aria-hidden="true" />
        </button>
      </div>
      {children}
    </aside>
  );
}
