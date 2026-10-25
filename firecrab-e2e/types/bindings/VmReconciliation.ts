import type { VmReconciliationOutcome } from "./VmReconciliationOutcome";

/** Snapshot from API startup or operator network retry; cleared on a new VM start. */
export type VmReconciliation = {
  outcome: VmReconciliationOutcome;
  checkedAtMs: number;
  detail: string | null;
};
