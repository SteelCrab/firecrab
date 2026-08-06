// Mirrors firecrab_api_types::BootstrapResponse (camelCase wire shape).

import type { BootstrapStatus } from "./BootstrapStatus";
import type { BootstrapStep } from "./BootstrapStep";
import type { BootstrapStepRun } from "./BootstrapStepRun";

export type BootstrapResponse = {
  bootstrapId: string;
  alias: string;
  sourceAlias: string;
  vmId: string;
  status: BootstrapStatus;
  currentStep: BootstrapStep | null;
  stepTimeline: BootstrapStepRun[];
  log: string;
  startedAtMs: number;
  endedAtMs?: number;
};
