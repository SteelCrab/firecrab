import { useEffect, useLayoutEffect, useRef, useState } from "react";
import type { VmResponse } from "../bindings";
import type { VmAction } from "../model";
import { availableActions } from "../model";
import { consolePageUrl } from "../navigation";
import { useI18n } from "../i18n";
import ConsoleSshTab from "./ConsoleSshTab";

interface VmTableProps {
  vms: VmResponse[];
  /** VMs with an in-flight request; their actions are locked. */
  busy: Set<string>;
  onAction: (id: string, action: VmAction) => void;
  /** Opens the VM detail modal (stepper + log) — always available. */
  onOpenDetail: (id: string) => void;
}

export default function VmTable({ vms, busy, onAction, onOpenDetail }: VmTableProps) {
  const { t } = useI18n();
  /** Row whose actions menu is open — only ever one. */
  const [openMenu, setOpenMenu] = useState<string | null>(null);
  /** VM whose SSH dialog is open. */
  const [sshVm, setSshVm] = useState<VmResponse | null>(null);

  // Esc closes the menu first, then the dialog, so one key never does both.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      if (openMenu) setOpenMenu(null);
      else setSshVm(null);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [openMenu]);

  // A row that disappears (deleted elsewhere) must not leave a dialog behind.
  const openSshVm = sshVm ? (vms.find((vm) => vm.id === sshVm.id) ?? null) : null;

  if (vms.length === 0) {
    return <div className="empty">{t("No VMs yet — click Create.", "VM이 없습니다 — 생성을 누르세요")}</div>;
  }

  // The table has more columns than a narrow shell can show; it scrolls
  // inside its own box so the page itself never scrolls sideways.
  return (
    <div className="table-scroll">
      <table className="vm-table">
        <thead>
          <tr>
            <th>{t("Name", "이름")}</th>
            <th>{t("State", "상태")}</th>
            <th>{t("Image", "이미지")}</th>
            <th>cpu</th>
            <th>ram</th>
            <th>{t("Disk", "디스크")}</th>
            <th>{t("CPU use", "CPU 사용")}</th>
            <th>{t("Memory use", "메모리 사용")}</th>
            <th>id</th>
            <th className="actions">{t("Actions", "작업")}</th>
          </tr>
        </thead>
        <tbody>
          {vms.map((vm) => (
            <Row
              key={vm.id}
              vm={vm}
              busy={busy.has(vm.id)}
              menuOpen={openMenu === vm.id}
              onToggleMenu={() => setOpenMenu((open) => (open === vm.id ? null : vm.id))}
              onCloseMenu={() => setOpenMenu(null)}
              onOpenSsh={() => {
                setOpenMenu(null);
                setSshVm(vm);
              }}
              onAction={onAction}
              onOpenDetail={onOpenDetail}
            />
          ))}
        </tbody>
      </table>

      {openSshVm && (
        <div className="console-overlay" role="presentation" onClick={() => setSshVm(null)}>
          <div
            className="console-panel"
            role="dialog"
            aria-modal="true"
            aria-label={t(`SSH — ${openSshVm.name}`, `SSH — ${openSshVm.name}`)}
            onClick={(event) => event.stopPropagation()}
          >
            <div className="console-bar">
              <span className="console-title">{t(`SSH — ${openSshVm.name}`, `SSH — ${openSshVm.name}`)}</span>
              <button className="btn console-close" onClick={() => setSshVm(null)} title={t("Close", "닫기")}>
                ✕
              </button>
            </div>
            <ConsoleSshTab vm={openSshVm} />
          </div>
        </div>
      )}
    </div>
  );
}

interface RowProps {
  vm: VmResponse;
  busy: boolean;
  menuOpen: boolean;
  onToggleMenu: () => void;
  onCloseMenu: () => void;
  onOpenSsh: () => void;
  onAction: (id: string, action: VmAction) => void;
  onOpenDetail: (id: string) => void;
}

function formatCpuPercent(value: number): string {
  const rounded = value >= 10 ? Math.round(value) : Math.round(value * 10) / 10;
  return `${rounded}%`;
}

function Row({
  vm,
  busy,
  menuOpen,
  onToggleMenu,
  onCloseMenu,
  onOpenSsh,
  onAction,
  onOpenDetail,
}: RowProps) {
  const { t } = useI18n();
  const shortId = vm.id.split("-")[0] ?? "";
  const anchorRef = useRef<HTMLButtonElement>(null);
  // The table scrolls in its own box, so an absolutely positioned menu would
  // be clipped by it. The menu is fixed to the viewport and anchored here.
  const [anchor, setAnchor] = useState<{ top: number; right: number } | null>(null);

  useLayoutEffect(() => {
    if (!menuOpen) {
      setAnchor(null);
      return;
    }
    const place = () => {
      const rect = anchorRef.current?.getBoundingClientRect();
      if (rect) {
        setAnchor({ top: rect.bottom + 6, right: Math.max(8, window.innerWidth - rect.right) });
      }
    };
    place();
    // Anything that moves the button leaves the menu pointing at nothing.
    window.addEventListener("resize", onCloseMenu);
    window.addEventListener("scroll", onCloseMenu, true);
    return () => {
      window.removeEventListener("resize", onCloseMenu);
      window.removeEventListener("scroll", onCloseMenu, true);
    };
  }, [menuOpen, onCloseMenu]);

  return (
    <tr>
      <td className="name">
        <button type="button" className="link-button" onClick={() => onOpenDetail(vm.id)}>
          {vm.name}
        </button>
      </td>
      <td>
        <span className={`state-badge ${vm.state}`}>{vm.state}</span>
      </td>
      <td className="mono">{vm.template}</td>
      <td className="mono">{vm.cpu}</td>
      <td className="mono">{vm.ram} MiB</td>
      <td className="mono">{vm.diskGb} GiB</td>
      <td className="mono usage-cell">
        {vm.state === "running" && vm.cpuUsagePercent != null
          ? formatCpuPercent(vm.cpuUsagePercent)
          : "—"}
      </td>
      <td className="mono usage-cell">
        {vm.state === "running" && vm.memoryUsedMib != null
          ? `${vm.memoryUsedMib} / ${vm.memoryTotalMib ?? vm.ram} MiB${
              vm.memoryUsedPercent != null
                ? ` (${formatCpuPercent(vm.memoryUsedPercent)})`
                : ""
            }`
          : "—"}
      </td>
      <td className="mono" title={vm.id}>
        {shortId}
      </td>
      <td className="actions">
        <div className="options-menu vm-actions">
          <button
            type="button"
            ref={anchorRef}
            className="options-menu-trigger"
            aria-label={t("Actions", "작업")}
            title={t("Actions", "작업")}
            aria-expanded={menuOpen}
            aria-haspopup="menu"
            onClick={onToggleMenu}
          >
            ⋯
          </button>
          {busy && <span className="mono">…</span>}
          {menuOpen && anchor && (
            <ul
              className="options-menu-list vm-actions-menu"
              role="menu"
              style={{ top: `${anchor.top}px`, right: `${anchor.right}px` }}
            >
              {vm.state === "running" && (
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
              {availableActions(vm.state).map((action) => (
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
                    {action}
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
      </td>
    </tr>
  );
}
