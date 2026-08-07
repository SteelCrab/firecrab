// Mirrors firecrab_api_types::BootstrapStepRun.
import type { BootstrapStep } from "./BootstrapStep";
import type { BootstrapStepOutcome } from "./BootstrapStepOutcome";

export type BootstrapStepRun = {
  step: BootstrapStep;
  startedAtMs: number;
  endedAtMs: number | null;
  outcome: BootstrapStepOutcome;
  detail: string | null;
};
