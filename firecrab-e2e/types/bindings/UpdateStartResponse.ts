// Mirrors firecrab_api_types::UpdateStartResponse (camelCase wire shape).

export type UpdateStartResponse = {
  /** Version this host was running when the updater was launched. */
  current: string;
  /** PID of the spawned `firecrab update --apply`, for journal correlation. */
  pid: number;
};
