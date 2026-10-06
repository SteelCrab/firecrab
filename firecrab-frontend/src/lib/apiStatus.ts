import type { VmReconciliation, VmReconciliationOutcome, VmState } from "../bindings";

/**
 * What the API label shows: the outcome the API recorded for the VM, or else
 * what the VM's own state implies. A running VM with no recorded outcome was
 * started by the API that is answering, which controls it directly, so it is
 * connected. Any other VM has nothing to report.
 */
export type ApiStatus = VmReconciliationOutcome | "connected" | "unchecked";

export function apiStatus(result: VmReconciliation | null | undefined, state: VmState): ApiStatus {
  return result?.outcome ?? (state === "running" ? "connected" : "unchecked");
}
