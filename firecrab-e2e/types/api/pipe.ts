// API fixtures for browser tests. No application runtime code.
export interface PipeRoute { host: string; pathPrefix: string; upstreams: string[]; network?: { micro_network_id: string; gateway: string; prefix: number } | null; owner?: { serviceId: string; deploymentId: string } | null }

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

export type PipeService = "ingress" | "tunnel";

export type PipeServiceAction = "start" | "stop" | "restart";

export type QuickTunnelTarget = { type: "apps"; upstreams: string[] } | { type: "routes"; host: string };

export interface QuickTunnelEntry { id: string; name: string; target: QuickTunnelTarget }

export interface QuickTunnelSnapshot extends QuickTunnelEntry {
  unit: PipeSnapshot["units"][number];
  publicUrl: string | null;
  error?: string | null;
}

export interface CloudflareZone { id: string; name: string; status: string; account: { id: string; name?: string } }
