// Mirrors firecrab_api_types::UpdateCheckResponse (camelCase wire shape).

export type UpdateCheckResponse = {
  /** Version of the build that answered. */
  current: string;
  /** Newest release tag with any leading `v` stripped; omitted when the check failed. */
  latest?: string;
  /** True only when `latest` parsed and is strictly newer than `current`. */
  updateAvailable: boolean;
  /** One-line reason there is no `latest`; omitted on a successful check. */
  error?: string;
};
