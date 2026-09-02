// Mirrors firecrab_api_types::KernelInstallResponse (camelCase wire shape).

import type { ImageInstallStatus } from "./ImageInstallStatus";

export type KernelInstallResponse = {
  version: string;
  status: ImageInstallStatus;
  log: string;
  startedAtMs?: number;
  endedAtMs?: number;
  downloadedBytes?: number;
  totalBytes?: number;
};
