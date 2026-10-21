import { useEffect, useRef, useState } from "react";
import type { CSSProperties, KeyboardEvent as ReactKeyboardEvent, PointerEvent as ReactPointerEvent, ReactNode } from "react";
import { X } from "lucide-react";
import { useI18n } from "../../i18n";
import { PANEL_WIDTH, clampPanelWidth } from "../../lib/detailPanel";
import { editorOpen, isTypingTarget, overlayOpen } from "../../lib/shortcuts";

/** Keyboard step of the resize handle; Shift moves four times as far. */
const RESIZE_STEP = 16;

interface DetailPanelProps {
  /** Accessible name of the panel, e.g. "MicroVM detail". */
  label: string;
  /** Small heading in the top row, e.g. an icon and "MicroVM". */
  kind?: ReactNode;
  /** Extra icon buttons placed before the close button. */
  tools?: ReactNode;
  onClose: () => void;
  /** Panel width in CSS pixels, already inside this screen's limits. */
  width?: number;
  /** Widest the panel may get on this screen. */
  maxWidth?: number;
  /** Called with the new width after a drag or a key press; leave out for a fixed panel. */
  onResize?: (width: number) => void;
  children: ReactNode;
}

/** Right-hand column of the three-column frame (spec §6.3). Not a modal. */
export default function DetailPanel({
  label,
  kind,
  tools,
  onClose,
  width = PANEL_WIDTH.initial,
  maxWidth = PANEL_WIDTH.max,
  onResize,
  children,
}: DetailPanelProps) {
  const { t } = useI18n();
  // Width shown while the pointer is down; the screen hears it once, on release.
  const [dragWidth, setDragWidth] = useState<number | null>(null);
  const drag = useRef<{ startX: number; startWidth: number; width: number } | null>(null);
  const shownWidth = dragWidth ?? width;

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

  const endDrag = () => {
    const current = drag.current;
    drag.current = null;
    delete document.body.dataset.panelResizing;
    setDragWidth(null);
    if (current && current.width !== current.startWidth) onResize?.(current.width);
  };

  const onPointerDown = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return;
    event.preventDefault();
    event.currentTarget.setPointerCapture(event.pointerId);
    drag.current = { startX: event.clientX, startWidth: width, width };
    // The cursor and text selection follow the drag across the whole page.
    document.body.dataset.panelResizing = "";
  };

  const onPointerMove = (event: ReactPointerEvent<HTMLDivElement>) => {
    const current = drag.current;
    if (!current) return;
    // The handle sits on the panel's left edge: moving left widens it.
    current.width = clampPanelWidth(current.startWidth + current.startX - event.clientX, maxWidth);
    setDragWidth(current.width);
  };

  const onKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    const step = event.shiftKey ? RESIZE_STEP * 4 : RESIZE_STEP;
    const next =
      event.key === "ArrowLeft" ? width + step
      : event.key === "ArrowRight" ? width - step
      : event.key === "Home" ? PANEL_WIDTH.min
      : event.key === "End" ? maxWidth
      : null;
    if (next === null) return;
    event.preventDefault();
    onResize?.(clampPanelWidth(next, maxWidth));
  };

  const resizeLabel = t("Resize detail panel", "상세 패널 폭 조절");

  return (
    <aside
      className="detail-panel"
      aria-label={label}
      style={{ "--panel-width": `${shownWidth}px` } as CSSProperties}
    >
      {onResize && (
        <div
          className="detail-panel-resize"
          role="separator"
          aria-orientation="vertical"
          aria-label={resizeLabel}
          aria-valuemin={PANEL_WIDTH.min}
          aria-valuemax={clampPanelWidth(maxWidth)}
          aria-valuenow={shownWidth}
          tabIndex={0}
          title={t("Drag to resize · double-click to reset", "끌어서 폭 조절 · 두 번 클릭하면 기본 폭")}
          data-dragging={dragWidth !== null ? "" : undefined}
          onPointerDown={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={endDrag}
          onPointerCancel={endDrag}
          onDoubleClick={() => onResize(PANEL_WIDTH.initial)}
          onKeyDown={onKeyDown}
        />
      )}
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
