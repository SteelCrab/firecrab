import { useState } from "react";
import { KeyRound, Loader2, Pause, Play, Square, SquareTerminal, Trash2 } from "lucide-react";
import type { VmResponse } from "../../bindings";
import { useI18n } from "../../i18n";
import type { VmAction } from "../../model";
import { availableActions } from "../../model";
import { consolePageUrl } from "../../navigation";
import StatusBadge from "../ui/StatusBadge";
import EnvTab from "./detail/EnvTab";
import LogsTab from "./detail/LogsTab";
import NetworkTab from "./detail/NetworkTab";
import OverviewTab from "./detail/OverviewTab";
import { useSectionEdit } from "./detail/useSectionEdit";
import { useVmDetail } from "./detail/useVmDetail";

type Tab = "overview" | "network" | "env" | "logs";

interface VmDetailPanelProps {
  /** The VM as the list last saw it; the panel polls its own fresher copy. */
  vm: VmResponse;
  busy: boolean;
  onAction: (id: string, action: VmAction) => void;
  /** Opens the SSH dialog for this VM. */
  onOpenSsh: () => void;
}

/**
 * MicroVM detail in the side panel (spec §8.2). Mount it keyed by VM id:
 * choosing another VM starts again on the overview with no section in edit.
 */
export default function VmDetailPanel({ vm: listed, busy, onAction, onOpenSsh }: VmDetailPanelProps) {
  const { t } = useI18n();
  const [tab, setTab] = useState<Tab>("overview");
  const transitioning = ["starting", "stopping", "pausing", "resuming"].includes(listed.state);
  const detail = useVmDetail(listed.id, { fast: transitioning, withLog: tab === "logs" });
  const vm = detail.vm ?? listed;
  const edit = useSectionEdit(vm, detail.setVm, detail.reloadShells);
  const envCount = Object.keys(vm.env ?? {}).length;

  const tabs: { id: Tab; label: string; count?: number }[] = [
    { id: "overview", label: t("Overview", "개요") },
    { id: "network", label: t("Network", "네트워크") },
    { id: "env", label: t("Environment", "환경 변수"), count: envCount || undefined },
    { id: "logs", label: t("Log", "로그") },
  ];
  const current = tabs.find((entry) => entry.id === tab)!;

  return (
    <>
      <div className="detail-head">
        <h2 className="detail-title">{vm.name}</h2>
        <div className="detail-meta">
          <StatusBadge state={vm.state} />
          {vm.ipv4 && <span className="mono">{vm.ipv4}</span>}
          <span>{vm.template}</span>
        </div>
        <div className="detail-actions">
          <PanelActions
            vm={vm}
            busy={busy}
            onAction={onAction}
            onSsh={onOpenSsh}
          />
        </div>
      </div>
      {vm.state === "paused" && <p className="detail-note">{t("Guest execution is paused. Memory stays allocated. Resume to serve requests again.", "실행을 일시정지했습니다. 메모리는 계속 점유합니다. 요청을 처리하려면 재개합니다.")}</p>}
      <div className="tabs detail-tabs" role="tablist" aria-label={t("MicroVM detail sections", "MicroVM 상세 구역")}>
        {tabs.map((entry) => (
          <button
            key={entry.id}
            type="button"
            role="tab"
            className="tab"
            id={`vm-tab-${entry.id}`}
            aria-selected={entry.id === tab}
            aria-controls="vm-tab-panel"
            onClick={() => setTab(entry.id)}
          >
            {entry.label}
            {entry.count !== undefined && <span className="tab-count">{entry.count}</span>}
          </button>
        ))}
      </div>
      <div className="detail-body" role="tabpanel" id="vm-tab-panel" aria-labelledby={`vm-tab-${current.id}`}>
        {tab === "overview" && (
          <OverviewTab
            vm={vm}
            detail={detail}
            edit={edit}
            onShowLog={() => setTab("logs")}
          />
        )}
        {tab === "network" && <NetworkTab vm={vm} detail={detail} edit={edit} />}
        {tab === "env" && <EnvTab vm={vm} detail={detail} edit={edit} />}
        {tab === "logs" && <LogsTab vm={vm} detail={detail} />}
      </div>
    </>
  );
}

interface PanelActionsProps {
  vm: VmResponse;
  busy: boolean;
  onAction: (id: string, action: VmAction) => void;
  onSsh: () => void;
}

/** What the VM can do in its current state (spec §8.2). */
function PanelActions({ vm, busy, onAction, onSsh }: PanelActionsProps) {
  const { t } = useI18n();
  if (vm.state === "running") {
    return (
      <>
        <button type="button" className="btn" disabled={busy} onClick={() => onAction(vm.id, "pause")}>
          <Pause aria-hidden="true" />{t("Pause", "일시정지")}
        </button>
        <button type="button" className="btn" disabled={busy} onClick={() => onAction(vm.id, "stop")}>
          {busy ? <Loader2 className="spin" aria-hidden="true" /> : <Square aria-hidden="true" />}
          {t("Stop", "중지")}
        </button>
        <a className="btn" href={consolePageUrl(vm.id)} target="_blank" rel="noopener noreferrer" title={t("Serial console (new tab)", "시리얼 콘솔 (새 탭)")}>
          <SquareTerminal aria-hidden="true" />
          {t("Console", "콘솔")}
        </a>
        <button type="button" className="btn" onClick={onSsh}>
          <KeyRound aria-hidden="true" />
          SSH
        </button>
      </>
    );
  }
  if (["paused", "pausing", "resuming"].includes(vm.state)) {
    const uncertain = vm.state !== "paused";
    return <>
      {uncertain && <button type="button" className="btn" disabled={busy} onClick={() => onAction(vm.id, "pause")}><Pause aria-hidden="true" />{t("Retry pause", "일시정지 재시도")}</button>}
      <button type="button" className="btn primary" disabled={busy} onClick={() => onAction(vm.id, "resume")}><Play aria-hidden="true" />{uncertain ? t("Retry resume", "재개 재시도") : t("Resume", "재개")}</button>
      <button type="button" className="btn" disabled={busy} onClick={() => onAction(vm.id, "stop")}><Square aria-hidden="true" />{t("Stop", "중지")}</button>
    </>;
  }
  if (vm.state === "starting" || vm.state === "stopping") {
    return (
      <button type="button" className="btn" disabled>
        <Loader2 className="spin" aria-hidden="true" />
        {vm.state === "starting" ? t("Starting…", "시작 중…") : t("Stopping…", "중지 중…")}
      </button>
    );
  }
  const actions = availableActions(vm.state);
  return (
    <>
      {actions.includes("start") && (
        <button type="button" className="btn primary" disabled={busy} onClick={() => onAction(vm.id, "start")}>
          {busy ? <Loader2 className="spin" aria-hidden="true" /> : <Play aria-hidden="true" />}
          {t("Start", "시작")}
        </button>
      )}
      {actions.includes("delete") && (
        <button type="button" className="btn danger" disabled={busy} onClick={() => onAction(vm.id, "delete")}>
          <Trash2 aria-hidden="true" />
          {t("Delete", "삭제")}
        </button>
      )}
    </>
  );
}
