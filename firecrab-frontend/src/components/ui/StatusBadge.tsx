import type { ReactNode } from "react";
import type { VmState } from "../../bindings";
import { useI18n } from "../../i18n";
import { VM_STATE_LABELS } from "../../model";
import StatusLabel from "../StatusLabel";

interface StatusBadgeProps {
  state: VmState;
  /** Hover/focus card with more about the state; omit for a plain badge. */
  tooltip?: ReactNode;
}

/** Dot + state name (spec §5.6). With a tooltip it keeps `StatusLabel`'s hover/focus/Esc behaviour. */
export default function StatusBadge({ state, tooltip }: StatusBadgeProps) {
  const { t } = useI18n();
  const label = t(...VM_STATE_LABELS[state]);
  if (!tooltip) return <span className={`badge ${state}`}>{label}</span>;
  return (
    <StatusLabel className={`badge ${state} vm-status-label`} accessibleLabel={`VM-STATUS: ${label}`} state={state} tooltip={tooltip}>
      {label}
    </StatusLabel>
  );
}
