// Mirrors firecrab_api_types::UpdatePhase (camelCase wire shape).

/** Where a `firecrab update --apply` run stands. */
export type UpdatePhase =
  | "idle"
  | "checking"
  | "downloading"
  | "verifying"
  | "applying"
  | "restarting"
  | "done"
  | "failed";
