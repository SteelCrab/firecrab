import { fetchJson } from "./client";

export type WorkspaceRequest = {
  id: string;
  name: string;
  cpu: number;
  ram: number;
  diskGb: number;
  microNetworkId: string | null;
  storageRoot: string | null;
};
export type WorkspaceSetup = Omit<WorkspaceRequest, "id"> & {
  id: string;
  vmId: string;
  state: "queued" | "running" | "ready" | "failed";
  phase: "image" | "network" | "create" | "start" | "complete";
  error: string | null;
  log?: string;
};
export const workspaceDefaults = () => ({
  name: "my-workspace",
  cpu: 2,
  ram: 4096,
  diskGb: 8,
  microNetworkId: null as string | null,
  storageRoot: null as string | null,
});
export function createWorkspace(
  request: WorkspaceRequest,
): Promise<WorkspaceSetup> {
  return fetchJson("/api/workspace-setups", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(request),
  });
}
export function getWorkspace(id: string): Promise<WorkspaceSetup> {
  return fetchJson(`/api/workspace-setups/${encodeURIComponent(id)}`);
}
export function listWorkspaces(): Promise<WorkspaceSetup[]> {
  return fetchJson("/api/workspace-setups");
}

export type AppConfig = {
  testCommand: string;
  buildCommand: string;
  startCommand: string;
  port: number;
  security: boolean;
};
export type AppVerification = {
  vmId: string;
  agentId: string;
  config: AppConfig;
  run: null | {
    id: string;
    state: string;
    phase: string;
    appReady: boolean;
    appRunning?: boolean;
    log: string;
    exitCode: number | null;
    config: AppConfig | null;
    findings: null | {
      vulnerabilities: number;
      secrets: number;
      misconfigurations: number;
    };
  };
  preview: null | {
    running: boolean;
    publicUrl: string | null;
    error: string | null;
  };
};
export type AddressProof = {
  url: string;
  status: number;
  ok: boolean;
  latencyMs: number;
};
export function checkWorkspaceAddress(
  subdomain: string,
): Promise<AddressProof> {
  return fetchJson(
    `/api/pipe/firecrab-tunnels/${encodeURIComponent(subdomain)}/check`,
    { method: "POST" },
  );
}
