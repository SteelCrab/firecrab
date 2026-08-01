import type { ImageInstallStatus } from "./ImageInstallStatus";

export type ImageInstallResponse = {
  alias: string;
  status: ImageInstallStatus;
  log: string;
  startedAtMs?: number;
  endedAtMs?: number;
};
