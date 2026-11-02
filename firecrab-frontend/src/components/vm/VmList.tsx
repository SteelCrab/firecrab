import { useEffect, useRef, useState } from "react";
import { Ellipsis, Loader2, Play, Square, SquareTerminal } from "lucide-react";
import type { VmResponse, VmState } from "../../bindings";
import { useI18n } from "../../i18n";
import type { VmAction } from "../../model";
import {
  VM_STATE_LABELS,
  vmActionLabels,
  availableActions,
  isInstanceVm,
} from "../../model";
import { consolePageUrl } from "../../navigation";
import Sparkline from "../ui/Sparkline";
import StatusBadge from "../ui/StatusBadge";
import { startupFailure, startupStepLabel } from "./startup";
import {
  HIGH_USAGE_PERCENT,
  formatPercent,
  usageSeries,
  vmSpec,
} from "./vmFormat";

interface VmListProps {
  vms: VmResponse[];
  busy: Set<string>;
  selectedId: string | null;
  onSelect: (id: string) => void;
  onAction: (id: string, action: VmAction) => void;
  /** Opens the SSH dialog for `vm`. */
  onOpenSsh: (vm: VmResponse) => void;
}

const DESCRIPTIONS: Record<VmState, [string, string]> = {
  created: [
    "The VM is created and has not been started yet.",
    "VM을 생성했습니다. 아직 시작하지 않았습니다.",
  ],
  starting: [
    "The VM is starting. Open its details to follow startup progress.",
    "VM을 시작하는 중입니다. 상세 정보에서 진행 상황을 확인할 수 있습니다.",
  ],
  running: ["The VM is running.", "VM이 실행 중입니다."],
  pausing: ["The VM is pausing.", "VM을 일시정지하는 중입니다."],
  paused: [
    "Guest execution is paused. Memory remains allocated.",
    "VM 실행이 일시정지되었습니다. 메모리는 계속 사용합니다.",
  ],
  resuming: ["The VM is resuming.", "VM 실행을 재개하는 중입니다."],
  stopping: ["The VM is stopping.", "VM이 종료 중입니다."],
  stopped: ["The VM is stopped.", "VM이 중지되어 있습니다."],
  error: [
    "The VM is in an error state. Open its details to inspect the logs.",
    "VM에 오류가 발생했습니다. 상세 정보에서 로그를 확인하세요.",
  ],
};

/** MicroVM table (spec §8.1): one selectable row per VM, row actions on hover or selection. */
export default function VmList({
  vms,
  busy,
  selectedId,
  onSelect,
  onAction,
  onOpenSsh,
}: VmListProps) {
  const { t } = useI18n();
  /** Row whose actions menu is open — only ever one. */
  const [menu, setMenu] = useState<{
    id: string;
    top: number;
    right: number;
  } | null>(null);
  const tbody = useRef<HTMLTableSectionElement>(null);
  const virtual = vms.length > 256;
  const [windowRange, setWindowRange] = useState({ start: 0, end: 40 });
  useEffect(() => {
    if (!virtual) return;
    const scroll = tbody.current?.closest("table")?.parentElement;
    if (!scroll) return;
    const update = () => {
      const start = Math.max(0, Math.floor((scroll.scrollTop - 51) / 75) - 6),
        end = Math.min(
          vms.length,
          start + Math.ceil(scroll.clientHeight / 75) + 13,
        );
      setWindowRange((previous) =>
        previous.start === start && previous.end === end
          ? previous
          : { start, end },
      );
    };
    const observer = new ResizeObserver(update);
    observer.observe(scroll);
    scroll.addEventListener("scroll", update);
    update();
    return () => {
      observer.disconnect();
      scroll.removeEventListener("scroll", update);
    };
  }, [virtual, vms.length]);
  const start = virtual
    ? Math.min(windowRange.start, Math.max(0, vms.length - 1))
    : 0;
  const end = virtual
    ? Math.max(start + 1, Math.min(windowRange.end, vms.length))
    : vms.length;

  useEffect(() => {
    if (!menu) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setMenu(null);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [menu]);

  // The menu is fixed to the viewport; anything that moves its button leaves it pointing at nothing.
  useEffect(() => {
    if (!menu) return;
    const close = () => setMenu(null);
    window.addEventListener("resize", close);
    window.addEventListener("scroll", close, true);
    return () => {
      window.removeEventListener("resize", close);
      window.removeEventListener("scroll", close, true);
    };
  }, [menu]);

  // Keyboard moves can select a row outside the scrolled view.
  useEffect(() => {
    const selected = tbody.current?.querySelector<HTMLElement>(
      'tr[aria-selected="true"]',
    );
    if (selected)
      selected.scrollIntoView({ block: "nearest", inline: "start" });
    else if (virtual && selectedId) {
      const index = vms.findIndex((vm) => vm.id === selectedId),
        scroll = tbody.current?.closest("table")?.parentElement;
      if (index >= 0 && scroll) {
        scroll.scrollTop = index * 75;
        scroll.dispatchEvent(new Event("scroll"));
      }
    }
  }, [selectedId]);

  return (
    <>
      <table
        className="vm-list"
        data-visible-count={vms.length}
        aria-label={t("MicroVMs", "MicroVM 목록")}
      >
        <colgroup>
          <col />
          <col className="vm-col-state" />
          <col className="vm-col-ip" />
          <col className="vm-col-cpu" />
          <col className="vm-col-mem" />
          <col className="vm-col-actions" />
        </colgroup>
        <thead>
          <tr>
            <th>{t("Name", "이름")}</th>
            <th>{t("State", "상태")}</th>
            <th className="vm-col-ip">IPv4</th>
            <th className="vm-col-cpu">CPU</th>
            <th className="vm-col-mem">{t("Memory", "메모리")}</th>
            <th>
              <span className="sr-only">{t("Actions", "작업")}</span>
            </th>
          </tr>
        </thead>
        <tbody id="vm-rows" ref={tbody}>
          {virtual && start > 0 && (
            <tr aria-hidden="true" style={{ height: start * 75 }}>
              <td
                colSpan={6}
                style={{ padding: 0, border: 0, height: start * 75 }}
              />
            </tr>
          )}
          {vms.slice(start, end).map((vm) => (
            <Row
              key={vm.id}
              vm={vm}
              busy={busy.has(vm.id)}
              selected={vm.id === selectedId}
              menu={menu?.id === vm.id ? menu : null}
              onSelect={onSelect}
              onToggleMenu={(button) => {
                if (menu?.id === vm.id) return setMenu(null);
                const rect = button.getBoundingClientRect();
                setMenu({
                  id: vm.id,
                  top: rect.bottom + 6,
                  right: Math.max(8, window.innerWidth - rect.right),
                });
              }}
              onCloseMenu={() => setMenu(null)}
              onOpenSsh={() => {
                setMenu(null);
                onOpenSsh(vm);
              }}
              onAction={onAction}
            />
          ))}
          {virtual && end < vms.length && (
            <tr aria-hidden="true" style={{ height: (vms.length - end) * 75 }}>
              <td
                colSpan={6}
                style={{
                  padding: 0,
                  border: 0,
                  height: (vms.length - end) * 75,
                }}
              />
            </tr>
          )}
        </tbody>
      </table>
    </>
  );
}

interface RowProps {
  vm: VmResponse;
  busy: boolean;
  selected: boolean;
  /** Where the open actions menu sits; `null` when it is closed. */
  menu: { top: number; right: number } | null;
  onSelect: (id: string) => void;
  onToggleMenu: (button: HTMLButtonElement) => void;
  onCloseMenu: () => void;
  onOpenSsh: () => void;
  onAction: (id: string, action: VmAction) => void;
}

function Row({
  vm,
  busy,
  selected,
  menu,
  onSelect,
  onToggleMenu,
  onCloseMenu,
  onOpenSsh,
  onAction,
}: RowProps) {
  const { t } = useI18n();
  const running = vm.state === "running";
  const failure = vm.state === "error" ? startupFailure(vm) : null;
  const sub =
    vm.state === "error"
      ? { text: failure ?? t("Start failed", "시작 실패"), tone: "error" }
      : vm.state === "starting" && vm.startupStep
        ? { text: `${startupStepLabel(vm.startupStep, t)}…`, tone: "busy" }
        : { text: `${vm.template} · ${vmSpec(vm)}`, tone: "" };
  const cpuHistory = usageSeries(
    vm.usageHistory,
    (sample) => sample.cpuUsagePercent,
  );
  const owned = isInstanceVm(vm);
  const lifecycle: {
    action: VmAction;
    label: string;
    icon: typeof Play;
  } | null = !owned
    ? null
    : running
      ? { action: "stop", label: t("Stop", "중지"), icon: Square }
      : vm.state === "paused"
        ? { action: "resume", label: t("Resume", "재개"), icon: Play }
        : availableActions(vm.state).includes("start")
          ? { action: "start", label: t("Start", "시작"), icon: Play }
          : null;

  return (
    <tr
      aria-selected={selected}
      className={selected ? "selected" : undefined}
      onClick={() => onSelect(vm.id)}
    >
      <td>
        <button
          type="button"
          className="vm-row-name"
          onClick={() => onSelect(vm.id)}
        >
          {vm.name}
        </button>
        <div className={`vm-row-sub ${sub.tone}`}>{sub.text}</div>
      </td>
      <td>
        <StatusBadge
          state={vm.state}
          tooltip={
            <>
              <h3>VM-STATUS</h3>
              <dl>
                <dt>NAME</dt>
                <dd>{vm.name}</dd>
                <dt>ID</dt>
                <dd className="mono">{vm.id}</dd>
                <dt>{t("Status", "상태")}</dt>
                <dd>{t(...VM_STATE_LABELS[vm.state])}</dd>
                <dt>{t("Info", "정보")}</dt>
                <dd>{t(...DESCRIPTIONS[vm.state])}</dd>
              </dl>
            </>
          }
        />
      </td>
      <td className="vm-col-ip">
        {vm.ipv4 ? (
          <span className="mono">{vm.ipv4}</span>
        ) : (
          <span className="vm-nil">—</span>
        )}
      </td>
      <td className="vm-col-cpu">
        {running && vm.cpuUsagePercent != null ? (
          <span className="vm-usage">
            <Sparkline
              values={cpuHistory}
              width={56}
              height={18}
              tone={
                vm.cpuUsagePercent >= HIGH_USAGE_PERCENT ? "warn" : "neutral"
              }
            />
            <span className="vm-usage-value">
              {formatPercent(vm.cpuUsagePercent)}
            </span>
          </span>
        ) : (
          <span className="vm-nil">—</span>
        )}
      </td>
      <td className="vm-col-mem">
        {running && vm.memoryUsedPercent != null ? (
          <span className="vm-usage">
            <span
              className={
                vm.memoryUsedPercent >= HIGH_USAGE_PERCENT
                  ? "vm-meter warn"
                  : "vm-meter"
              }
            >
              <i style={{ width: `${Math.min(100, vm.memoryUsedPercent)}%` }} />
            </span>
            <span className="vm-usage-value">
              {formatPercent(vm.memoryUsedPercent)}
            </span>
          </span>
        ) : (
          <span className="vm-nil">—</span>
        )}
      </td>
      <td
        className="vm-row-actions"
        onClick={(event) => event.stopPropagation()}
      >
        {lifecycle && (
          <button
            type="button"
            className="icon-btn"
            aria-label={lifecycle.label}
            title={lifecycle.label}
            disabled={busy}
            onClick={() => onAction(vm.id, lifecycle.action)}
          >
            {busy ? (
              <Loader2 className="spin" aria-hidden="true" />
            ) : (
              <lifecycle.icon aria-hidden="true" />
            )}
          </button>
        )}
        {running ? (
          <a
            className="icon-btn"
            href={consolePageUrl(vm.id)}
            target="_blank"
            rel="noopener noreferrer"
            aria-label={t("Terminal (new tab)", "터미널 (새 탭)")}
            title={t("Terminal (new tab)", "터미널 (새 탭)")}
          >
            <SquareTerminal aria-hidden="true" />
          </a>
        ) : (
          <button
            type="button"
            className="icon-btn"
            disabled
            aria-label={t("Terminal (new tab)", "터미널 (새 탭)")}
          >
            <SquareTerminal aria-hidden="true" />
          </button>
        )}
        <button
          type="button"
          className="icon-btn"
          aria-label={t("Actions", "작업")}
          title={t("Actions", "작업")}
          aria-expanded={menu !== null}
          aria-haspopup="menu"
          onClick={(event) => onToggleMenu(event.currentTarget)}
        >
          <Ellipsis aria-hidden="true" />
        </button>
        {menu && (
          // Fixed to the viewport: the list scrolls in its own box and would clip it.
          <ul
            className="options-menu-list vm-actions-menu"
            role="menu"
            style={{ top: `${menu.top}px`, right: `${menu.right}px` }}
          >
            {running && (
              <li role="none">
                {/* Native new-tab link — visible in the tab bar (no detached popup). */}
                <a
                  role="menuitem"
                  className="options-menu-item"
                  href={consolePageUrl(vm.id)}
                  target="_blank"
                  rel="noopener noreferrer"
                  title={t("Serial console (new tab)", "시리얼 콘솔 (새 탭)")}
                  onClick={onCloseMenu}
                >
                  {t("Terminal", "터미널")}
                </a>
              </li>
            )}
            <li role="none">
              <button
                type="button"
                role="menuitem"
                className="options-menu-item"
                onClick={onOpenSsh}
              >
                {t("SSH connect", "SSH 연결")}
              </button>
            </li>
            {(owned ? availableActions(vm.state) : []).map((action) => (
              <li key={action} role="none">
                <button
                  type="button"
                  role="menuitem"
                  className={`options-menu-item${action === "delete" ? " danger" : ""}`}
                  disabled={busy}
                  onClick={() => {
                    onCloseMenu();
                    onAction(vm.id, action);
                  }}
                >
                  {t(...vmActionLabels(action, vm.state))}
                </button>
              </li>
            ))}
          </ul>
        )}
      </td>
    </tr>
  );
}
