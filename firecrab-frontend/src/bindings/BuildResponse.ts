// Mirrors firecrab_api_types::BuildResponse (camelCase wire shape).

import type { BuildStatus } from "./BuildStatus";

export type BuildResponse = {
  buildId: string;
  sourceAlias: string;
  targetAlias?: string;
  vmId: string;
  status: BuildStatus;
  log: string;
  startedAtMs: number;
  endedAtMs?: number;
  hadPackageAction: boolean;
};
