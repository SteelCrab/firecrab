import type { VmResponse } from "../bindings";
import type { VmAction } from "../model";
import { availableActions } from "../model";

interface VmTableProps {
  vms: VmResponse[];
  /** VMs with an in-flight request; their actions are locked. */
  busy: Set<string>;
  onAction: (id: string, action: VmAction) => void;
  /** A console is only attachable while the VM has a live process. */
  onOpenConsole: (id: string) => void;
}

export default function VmTable({ vms, busy, onAction, onOpenConsole }: VmTableProps) {
  if (vms.length === 0) {
    return <div className="empty">VM이 없습니다 — 위에서 생성하세요</div>;
  }

  return (
    <table className="vm-table">
      <thead>
        <tr>
          <th>name</th>
          <th>state</th>
          <th>template</th>
          <th>cpu</th>
          <th>ram</th>
          <th>id</th>
          <th className="actions">actions</th>
        </tr>
      </thead>
      <tbody>
        {vms.map((vm) => (
          <Row key={vm.id} vm={vm} busy={busy.has(vm.id)} onAction={onAction} onOpenConsole={onOpenConsole} />
        ))}
      </tbody>
    </table>
  );
}

interface RowProps {
  vm: VmResponse;
  busy: boolean;
  onAction: (id: string, action: VmAction) => void;
  onOpenConsole: (id: string) => void;
}

function Row({ vm, busy, onAction, onOpenConsole }: RowProps) {
  const shortId = vm.id.split("-")[0] ?? "";

  return (
    <tr>
      <td className="name">{vm.name}</td>
      <td>
        <span className={`state-badge ${vm.state}`}>{vm.state}</span>
      </td>
      <td className="mono">{vm.templateVersion}</td>
      <td className="mono">{vm.cpu}</td>
      <td className="mono">{vm.ram} MiB</td>
      <td className="mono" title={vm.id}>
        {shortId}
      </td>
      <td className="actions">
        {vm.state === "running" && (
          <button className="btn" onClick={() => onOpenConsole(vm.id)}>
            terminal
          </button>
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
