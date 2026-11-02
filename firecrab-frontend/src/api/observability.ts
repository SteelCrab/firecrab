import { fetchJson } from "./client";

export type DeliveryState =
  | "pending"
  | "delivered"
  | "retrying"
  | "partial"
  | "failed"
  | "disabled"
  | "paused";
export interface DeliveryBatch {
  timestampMs: number;
  records: number;
  delivered: number;
  rejected: number;
  status: DeliveryState;
  httpStatus: number | null;
}
export interface ExportStatus {
  state: DeliveryState;
  paused: boolean;
  queued: number;
  inFlight: number;
  delivered: number;
  dropped: number;
  rejected: number;
  failedBatches: number;
  lastDeliveredAtMs: number | null;
  history: DeliveryBatch[];
}
export interface SignozConfig {
  endpoint: string;
  serviceIds: string[];
  paused: boolean;
}
export interface ObservationSource {
  id: string;
  name: string;
  selected: boolean;
  available: boolean;
  status: ExportStatus | null;
}
export interface ObservationSnapshot {
  observedAtMs: number;
  provider: "signoz";
  config: SignozConfig | null;
  sources: ObservationSource[];
  history: (DeliveryBatch & { serviceId: string; serviceName: string })[];
  historyLimitPerService: number;
  displayHistoryLimit: number;
  queueLimitPerService: number;
  queuePersistent: boolean;
}
const base = "/api/observability/signoz";
export function getObservability(
  signal?: AbortSignal,
): Promise<ObservationSnapshot> {
  return fetchJson(base, { signal });
}
export function saveSignoz(config: SignozConfig): Promise<ObservationSnapshot> {
  return fetchJson(base, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(config),
  });
}
export function disconnectSignoz(): Promise<ObservationSnapshot> {
  return fetchJson(base, { method: "DELETE" });
}
export function testSignoz(
  endpoint: string,
  signal?: AbortSignal,
): Promise<DeliveryBatch> {
  return fetchJson(`${base}/test`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ endpoint }),
    signal,
  });
}
export function sendSignozTest(serviceId: string): Promise<DeliveryBatch> {
  return fetchJson(`${base}/test-log`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ serviceId }),
  });
}
