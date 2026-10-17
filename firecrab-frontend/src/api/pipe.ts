import { fetchJson } from "./client";

/** Shared with firecrab-helper-protocol::pipe; credentials never appear in GET. */
export interface PipeRoute { host: string; pathPrefix: string; upstreams: string[] }
export interface PipeConfig {
  listen: string;
  routes: PipeRoute[];
  tunnel: { id: string; credentialsFile: string } | null;
}
export interface PipeSnapshot {
  config: PipeConfig | null;
  units: { unit: string; loadState: string; activeState: string; subState: string; mainPid: number }[];
  nginxAvailable: boolean;
  cloudflaredAvailable: boolean;
}
export function getPipe(signal?: AbortSignal): Promise<PipeSnapshot> {
  return fetchJson("/api/pipe", { signal });
}
export function applyPipe(config: PipeConfig, credentials: unknown): Promise<PipeSnapshot> {
  return fetchJson("/api/pipe", { method: "PUT", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ config, credentials }) });
}
export function disablePipe(): Promise<PipeSnapshot> {
  return fetchJson("/api/pipe/disable", { method: "POST" });
}

export interface CloudflareZone { id: string; name: string; status: string; account: { id: string; name: string } }
export function getCloudflareZones(apiToken: string): Promise<CloudflareZone[]> {
  return fetchJson("/api/pipe/cloudflare/zones", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ apiToken }) });
}
export function connectCloudflare(apiToken: string, zoneId: string, hostname: string, config: PipeConfig): Promise<PipeSnapshot> {
  return fetchJson("/api/pipe/cloudflare/connect", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ apiToken, zoneId, hostname, config }) });
}
