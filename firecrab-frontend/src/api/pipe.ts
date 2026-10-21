import { fetchJson } from "./client";

/** Shared with firecrab-helper-protocol::pipe; credentials never appear in GET. */
export interface PipeRoute { host: string; pathPrefix: string; upstreams: string[] }
export interface PipeConfig {
  listen: string;
  routes: PipeRoute[];
  tunnel: { id: string; credentialsFile: string; hostname?: string | null } | null;
  quickTunnel?: { upstreams: string[] } | null;
  quickTunnels?: QuickTunnelEntry[];
}
export interface PipeSnapshot {
  firecrabTunnels?: FirecrabAddress[];
  config: PipeConfig | null;
  units: { unit: string; loadState: string; activeState: string; subState: string; mainPid: number }[];
  nginxAvailable: boolean;
  cloudflaredAvailable: boolean;
  quickTunnel?: { publicUrl: string | null } | null;
  quickTunnels?: QuickTunnelSnapshot[];
}

export type FirecrabTarget = { type: "apps"; upstreams: string[] } | { type: "nginx"; host: string };
export interface FirecrabAddress {
  subdomain: string;
  publicUrl: string;
  target: FirecrabTarget | { type: "quick_tunnel"; id: string };
  connectorId: string;
  connected: boolean;
  error: string | null;
}
export function saveFirecrabAddress(ownership: { token: string } | { subdomain: string }, target: FirecrabTarget): Promise<PipeSnapshot> {
  return fetchJson("/api/pipe/firecrab-tunnels", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ ...ownership, target }) });
}
export function deleteFirecrabAddress(subdomain: string): Promise<PipeSnapshot> {
  return fetchJson("/api/pipe/firecrab-tunnels/" + encodeURIComponent(subdomain), { method: "DELETE" });
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

export type PipeService = "ingress" | "tunnel";
export type PipeServiceAction = "start" | "stop" | "restart";
export function controlPipeService(service: PipeService, action: PipeServiceAction): Promise<PipeSnapshot> {
  return fetchJson("/api/pipe/service", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ service, action }) });
}

export function connectQuickTunnel(config: PipeConfig): Promise<PipeSnapshot> {
  return fetchJson("/api/pipe/quick-tunnel", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ config }) });
}

export type QuickTunnelTarget = { type: "apps"; upstreams: string[] } | { type: "routes"; host: string };
export interface QuickTunnelEntry { id: string; name: string; target: QuickTunnelTarget }
export interface QuickTunnelSnapshot extends QuickTunnelEntry {
  unit: PipeSnapshot["units"][number];
  publicUrl: string | null;
  error?: string | null;
}
export function saveQuickTunnel(tunnel: { id: string | null; name: string; target: QuickTunnelTarget }): Promise<PipeSnapshot> {
  return fetchJson("/api/pipe/quick-tunnels", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(tunnel) });
}
export function deleteQuickTunnel(id: string): Promise<PipeSnapshot> {
  return fetchJson("/api/pipe/quick-tunnels/" + encodeURIComponent(id), { method: "DELETE" });
}
export function controlQuickTunnel(id: string, action: PipeServiceAction): Promise<PipeSnapshot> {
  return fetchJson("/api/pipe/quick-tunnels/" + encodeURIComponent(id) + "/service", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ action }) });
}

export interface CloudflareZone { id: string; name: string; status: string; account: { id: string; name?: string } }
export function getCloudflareZones(apiToken: string): Promise<CloudflareZone[]> {
  return fetchJson("/api/pipe/cloudflare/zones", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ apiToken }) });
}
export function connectCloudflare(apiToken: string, zoneId: string, hostname: string, config: PipeConfig): Promise<PipeSnapshot> {
  return fetchJson("/api/pipe/cloudflare/connect", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ apiToken, zoneId, hostname, config }) });
}
export function disconnectCloudflare(): Promise<PipeSnapshot> {
  return fetchJson("/api/pipe/cloudflare/disconnect", { method: "POST" });
}
