// Mirrors firecrab_api_types::UpdateProgressResponse (camelCase wire shape).

import type { UpdatePhase } from "./UpdatePhase";

export type UpdateProgressResponse = {
  phase: UpdatePhase;
  /** Overall progress from 0 to 100, not the current stage's. */
  percent: number;
  /** Version being installed, without a leading `v`. */
  target?: string;
  /** Bundle bytes received so far, while downloading. */
  downloadedBytes?: number;
  /** Size of the bundle, when the server said. */
  totalBytes?: number;
  /** Why the run failed. */
  error?: string;
  /** PID of the updater, matching `UpdateStartResponse.pid`, to tell this run from an earlier one. */
  pid?: number;
  /** When the record was last written, in milliseconds since the Unix epoch. */
  updatedAtMs: number;
};
