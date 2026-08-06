// Mirrors firecrab_api_types::BootstrapResponse (camelCase wire shape).

import type { BootstrapStatus } from "./BootstrapStatus";

export type BootstrapResponse = {
  bootstrapId: string;
  alias: string;
  sourceAlias: string;
  vmId: string;
  status: BootstrapStatus;
  log: string;
  startedAtMs: number;
  endedAtMs?: number;
};
