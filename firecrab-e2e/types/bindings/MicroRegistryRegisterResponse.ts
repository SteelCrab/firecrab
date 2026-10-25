/** Snapshot of `POST /api/microregistry/register` and `GET /api/microregistry/jobs/{jobId}`. */

import type { ImageInstallResponse } from "./ImageInstallResponse";

export type MicroRegistryRegisterResponse = ImageInstallResponse & {
  jobId: string;
};
