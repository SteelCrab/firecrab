// Mirrors firecrab_api_types::HostStatusResponse (camelCase wire shape).
import type { HostPlatformResponse } from "./HostPlatformResponse";

export type HostStatusResponse = {
  loadAverage1m: number;
  memoryTotalMib: number;
  memoryAvailableMib: number;
  diskTotalGib: number;
  diskAvailableGib: number;
  uptimeSeconds: number;
  /** Absent from APIs that predate it. */
  platform?: HostPlatformResponse;
};
