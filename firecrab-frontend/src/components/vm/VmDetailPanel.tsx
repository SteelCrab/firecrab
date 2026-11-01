import { useEffect, useState } from "react";
import {
  KeyRound,
  Pause,
  Loader2,
  Play,
  Square,
  SquareTerminal,
  Trash2,
} from "lucide-react";
import type { VmResponse } from "../../bindings";
import { useI18n } from "../../i18n";
import type { VmAction } from "../../model";
import { availableActions, vmActionLabels, isInstanceVm } from "../../model";
import { consolePageUrl } from "../../navigation";
import StatusBadge from "../ui/StatusBadge";
import { VmVolumes } from "../Volumes";
import VmSchedules from "../schedules/VmSchedules";
import EnvTab from "./detail/EnvTab";
import LogsTab from "./detail/LogsTab";
import NetworkTab from "./detail/NetworkTab";
import OverviewTab from "./detail/OverviewTab";
import { useSectionEdit } from "./detail/useSectionEdit";
import { useVmDetail } from "./detail/useVmDetail";

type Tab = "overview" | "network" | "volumes" | "env" | "logs";

interface VmDetailPanelProps {
  /** The VM as the list last saw it; the panel polls its own fresher copy. */
  vm: VmResponse;
  busy: boolean;
  onUnsavedChange: (dirty: boolean) => void;
  onAction: (id: string, action: VmAction) => void;
  /** Opens the SSH dialog for this VM. */
  onOpenSsh: () => void;
}

/**
 * MicroVM detail in the side panel (spec §8.2). Mount it keyed by VM id:
 * choosing another VM starts again on the overview with no section in edit.
 */
export default function VmDetailPanel({
  vm: listed,
  busy,
  onUnsavedChange,
  onAction,
  onOpenSsh,
}: VmDetailPanelProps) {
  const { t } = useI18n();
  const [tab, setTab] = useState<Tab>("overview");
  const transitioning =
    listed.state === "starting" ||
    listed.state === "stopping" ||
    listed.state === "pausing" ||
    listed.state === "resuming";
  const detail = useVmDetail(listed.id, {
    fast: transitioning,
    withLog: tab === "logs",
  });
  const vm = detail.vm ?? listed;
  const edit = useSectionEdit(vm, detail.setVm, detail.reloadShells);
  useEffect(() => {
    onUnsavedChange(edit.draft !== null);
    return () => onUnsavedChange(false);
  }, [edit.draft, onUnsavedChange]);
  const envCount = Object.keys(vm.env ?? {}).length;

  const tabs: { id: Tab; label: string; count?: number }[] = [
    { id: "overview", label: t("Overview", "개요") },
    { id: "network", label: t("Network", "네트워크") },
    { id: "volumes", label: t("Volumes", "볼륨") },
    {
      id: "env",
      label: t("Environment", "환경 변수"),
      count: envCount || undefined,
    },
    { id: "logs", label: t("Log", "로그") },
  ];
  const current = tabs.find((entry) => entry.id === tab)!;

  return (
    <>
      <div className="detail-head">
        <h2 className="detail-title">
          {vm.name}
          {vm.purpose === "queue"
            ? " · Queue"
            : vm.purpose === "pool"
              ? " · Pool"
              : ""}
        </h2>
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
      {vm.state === "paused" && (
        <p className="detail-note">
          {t(
            "Memory stays allocated while guest execution is paused.",
            "일시정지 중에도 메모리는 계속 점유합니다.",
          )}
        </p>
      )}
      <div
        className="tabs detail-tabs"
        role="tablist"
        aria-label={t("MicroVM detail sections", "MicroVM 상세 구역")}
        onKeyDown={(event) => {
          if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key))
            return;
          event.preventDefault();
          const next =
            event.key === "Home"
              ? 0
              : event.key === "End"
                ? tabs.length - 1
                : (tabs.findIndex((entry) => entry.id === tab) +
                    (event.key === "ArrowRight" ? 1 : -1) +
                    tabs.length) %
                  tabs.length;
          setTab(tabs[next].id);
          event.currentTarget
            .querySelectorAll<HTMLButtonElement>('[role="tab"]')
            [next]?.focus();
        }}
      >
        {tabs.map((entry) => (
          <button
            key={entry.id}
            type="button"
            role="tab"
            className="tab"
            id={`vm-tab-${entry.id}`}
            aria-selected={entry.id === tab}
            tabIndex={entry.id === tab ? 0 : -1}
            aria-controls="vm-tab-panel"
            onClick={() => setTab(entry.id)}
          >
            {entry.label}
            {entry.count !== undefined && (
              <span className="tab-count">{entry.count}</span>
            )}
          </button>
        ))}
      </div>
      <div
        className="detail-body"
        role="tabpanel"
        id="vm-tab-panel"
        aria-labelledby={`vm-tab-${current.id}`}
      >
        {tab === "overview" && (
          <>
            <OverviewTab
              vm={vm}
              detail={detail}
              edit={edit}
              onShowLog={() => setTab("logs")}
            />
            {isInstanceVm(vm) && <VmSchedules vm={vm} />}
          </>
        )}
        {tab === "network" && (
          <NetworkTab vm={vm} detail={detail} edit={edit} />
        )}
        {tab === "volumes" && <VmVolumes vm={vm} />}
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
  const icons = {
    start: Play,
    pause: Pause,
    resume: Play,
    stop: Square,
    delete: Trash2,
  };
  const actions = isInstanceVm(vm) ? availableActions(vm.state) : [];
  const transitioning = [
    "starting",
    "stopping",
    "pausing",
    "resuming",
  ].includes(vm.state);
  return (
    <>
      {!isInstanceVm(vm) && (
        <a
          className="btn"
          href={vm.purpose === "queue" ? "#/queues" : "#/pools"}
        >
          {vm.purpose === "queue" ? "Queue" : "Pool"}
        </a>
      )}
      {actions.map((action) => {
        const Icon = icons[action];
        return (
          <button
            key={action}
            type="button"
            className={`btn${action === "delete" ? " danger" : ""}`}
            disabled={busy}
            title={t(...vmActionLabels(action, vm.state))}
            onClick={() => onAction(vm.id, action)}
          >
            {busy ? (
              <Loader2 className="spin" aria-hidden="true" />
            ) : (
              <Icon aria-hidden="true" />
            )}
            {t(...vmActionLabels(action, vm.state))}
          </button>
        );
      })}
      {transitioning && (
        <button className="btn" disabled>
          <Loader2 className="spin" aria-hidden="true" />
          {t("Updating execution state…", "실행 상태 확인 중…")}
        </button>
      )}
      {vm.state === "running" && (
        <>
          <a
            className="btn"
            href={consolePageUrl(vm.id)}
            aria-label={t("Open console", "콘솔 열기")}
            target="_blank"
            rel="noopener noreferrer"
            title={t("Serial console (new tab)", "시리얼 콘솔 (새 탭)")}
          >
            <SquareTerminal aria-hidden="true" />
            {t("Console", "콘솔")}
          </a>
          <button type="button" className="btn" onClick={onSsh}>
            <KeyRound aria-hidden="true" />
            SSH
          </button>
        </>
      )}
    </>
  );
}
