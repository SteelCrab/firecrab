import type { VmResponse } from "../bindings";
import type { VmAction } from "../model";
import { availableActions } from "../model";
import { consolePageUrl } from "../navigation";
import { useI18n } from "../i18n";

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
  if (vms.length === 0) {
    return <div className="empty">{t("No VMs yet — create one above.", "VM이 없습니다 — 위에서 생성하세요")}</div>;
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
              onAction={onAction}
              onOpenDetail={onOpenDetail}
            />
          ))}
        </tbody>
      </table>
    </div>
  );
}

interface RowProps {
  vm: VmResponse;
  busy: boolean;
  onAction: (id: string, action: VmAction) => void;
  onOpenDetail: (id: string) => void;
}

function Row({ vm, busy, onAction, onOpenDetail }: RowProps) {
  const { t } = useI18n();
  const shortId = vm.id.split("-")[0] ?? "";

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
      <td className="mono" title={vm.id}>
        {shortId}
      </td>
      <td className="actions">
        {vm.state === "running" && (
          // Native new-tab link — visible in the tab bar (no detached popup).
          <a
            className="btn"
            href={consolePageUrl(vm.id)}
            target="_blank"
            rel="noopener noreferrer"
            title={t("Serial console (new tab)", "시리얼 콘솔 (새 탭)")}
          >
            {t("Terminal", "터미널")}
          </a>
        )}
        {availableActions(vm.state).map((action) => (
          <button
            key={action}
            className={actionClass(action)}
            disabled={busy}
            onClick={() => onAction(vm.id, action)}
          >
            {action}
          </button>
        ))}
        {busy && <span className="mono">…</span>}
      </td>
    </tr>
  );
}

function actionClass(action: VmAction): string {
  switch (action) {
    case "start":
      return "btn primary";
    case "stop":
      return "btn";
    case "delete":
      return "btn danger";
  }
}
