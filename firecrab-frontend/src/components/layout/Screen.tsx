import { useRef } from "react";
import type { ReactNode } from "react";
import { ChevronRight, MousePointerClick, PanelRightClose, PanelRightOpen } from "lucide-react";
import { useI18n } from "../../i18n";
import { MAIN_MIN_WIDTH, PANEL_WIDTH, clampPanelWidth } from "../../lib/detailPanel";
import { useElementWidth } from "../../lib/useElementWidth";
import { PHONE_LAYOUT, WIDE_LAYOUT, useMediaQuery } from "../../lib/useMediaQuery";
import { VIEWS, viewGroupLabel, viewLabel } from "../../navigation";
import type { ViewId } from "../../navigation";
import DetailPanel from "./DetailPanel";
import { useScreenStatus } from "./screenStatus";

interface ScreenProps {
  view: ViewId;
  /** Buttons at the right end of the header, e.g. "New MicroVM". */
  actions?: ReactNode;
  /** Row under the header and banner: filters, search. */
  toolbar?: ReactNode;
  /**
   * Detail panel content. Leave out for a two-column screen; `null` means the
   * screen has a panel but nothing is selected.
   */
  detail?: ReactNode | null;
  detailLabel?: string;
  /** Small heading in the panel's top row, e.g. an icon and "MicroVM". */
  detailKind?: ReactNode;
  /** Clears the selection or closes the open form when the panel closes. */
  onCloseDetail?: () => void;
  /** Show the VM list's poll state in the header. */
  live?: boolean;
  /** Body without padding, for edge-to-edge lists. */
  flush?: boolean;
  children: ReactNode;
}

/** One screen inside the frame: header, banner, body, and the optional detail panel. */
export default function Screen({ view, actions, toolbar, detail, detailLabel, detailKind, onCloseDetail, live, flush, children }: ScreenProps) {
  const { locale, t } = useI18n();
  const { banner, pollNote, detailOpen, setDetailOpen, detailWidth, setDetailWidth } = useScreenStatus();
  const wide = useMediaQuery(WIDE_LAYOUT);
  const phone = useMediaQuery(PHONE_LAYOUT);
  const screenRef = useRef<HTMLDivElement>(null);
  const screenWidth = useElementWidth(screenRef);
  const item = VIEWS.find((entry) => entry.id === view)!;

  const hasPanel = detail !== undefined;
  // Wide screens keep the column unless the user closed it; narrow screens
  // overlay the panel only while something is selected.
  const panelVisible = hasPanel && (wide ? detailOpen : detail !== null);
  // Closing also drops the selection (or the open form): reopening the
  // column starts from the empty state instead of a stale item.
  const closePanel = () => {
    if (wide) setDetailOpen(false);
    onCloseDetail?.();
  };
  // A wide screen keeps room for the list beside the column; the overlay
  // only keeps a strip of the workspace visible. Unmeasured: no limit yet.
  const room = screenWidth === 0 ? PANEL_WIDTH.max : screenWidth - (wide ? MAIN_MIN_WIDTH : 48);
  const panelMax = clampPanelWidth(room);
  const panelWidth = clampPanelWidth(detailWidth, panelMax);
  const toggleLabel = detailOpen ? t("Close detail panel", "상세 패널 닫기") : t("Open detail panel", "상세 패널 열기");

  return (
    <div ref={screenRef} className={panelVisible ? "screen has-panel" : "screen"}>
      <div className="screen-main">
        <header className="screen-header">
          <div className="screen-crumbs">
            <span>{viewGroupLabel(item.group, locale)}</span>
            <ChevronRight aria-hidden="true" />
            <h1 className="screen-title">{viewLabel(item, locale)}</h1>
          </div>
          <div className="screen-header-end">
            {live && pollNote && (
              <span className="screen-live">
                <span className="screen-live-dot" aria-hidden="true" />
                {pollNote}
              </span>
            )}
            {hasPanel && wide && (
              <button
                type="button"
                className="icon-btn"
                aria-label={toggleLabel}
                aria-pressed={detailOpen}
                title={toggleLabel}
                onClick={() => setDetailOpen(!detailOpen)}
              >
                {detailOpen ? <PanelRightClose aria-hidden="true" /> : <PanelRightOpen aria-hidden="true" />}
              </button>
            )}
            {actions}
          </div>
        </header>
        {banner}
        {toolbar}
        <div className={flush ? "screen-body flush" : "screen-body"}>{children}</div>
      </div>
      {panelVisible && (
        <DetailPanel
          label={detailLabel ?? ""}
          kind={detail ? detailKind : undefined}
          onClose={closePanel}
          width={panelWidth}
          maxWidth={panelMax}
          onResize={phone ? undefined : setDetailWidth}
        >
          {detail ?? (
            <div className="empty-state detail-panel-empty">
              <div className="empty-state-icon"><MousePointerClick aria-hidden="true" /></div>
              <div>{t("Select an item to see its details.", "항목을 고르면 상세 정보가 보입니다.")}</div>
            </div>
          )}
        </DetailPanel>
      )}
    </div>
  );
}
