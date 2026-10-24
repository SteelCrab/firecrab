import type { ImageInstallStatus } from "./ImageInstallStatus";

export type ImageInstallResponse = {
  alias: string;
  status: ImageInstallStatus;
  log: string;
  startedAtMs?: number;
  endedAtMs?: number;
  /** Present for streamed registry package downloads. */
  downloadedBytes?: number;
  /** Package size from Content-Length, when the registry provides it. */
  totalBytes?: number;
};
