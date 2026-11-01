export type VmReconciliationOutcome =
  | "reconnected"
  | "gone"
  | "mismatched"
  | "networkFailed"
  | "interrupted"
  | "exited";
