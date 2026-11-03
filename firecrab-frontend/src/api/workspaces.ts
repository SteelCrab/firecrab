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
  cpu: 4,
  ram: 8192,
  diskGb: 16,
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
/** Renames the workspace and its VM together. */
export function renameWorkspace(
  id: string,
  name: string,
): Promise<WorkspaceSetup> {
  return fetchJson(`/api/workspace-setups/${encodeURIComponent(id)}`, {
    method: "PATCH",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ name }),
  });
}
/** Same rule as the API: 1–64 letters, numbers, ".", "_" or "-", starting with a letter or number. */
export const validWorkspaceName = (name: string) =>
  /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/.test(name);

export type AppConfig = {
  testCommand: string;
  buildCommand: string;
  startCommand: string;
  port: number;
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
  };
  /** The workspace's tunnel. It outlives app restarts while the app port stays the same. */
  preview: null | {
    running: boolean;
    /** Set once cloudflared registers a connection; null while the tunnel starts. */
    publicUrl: string | null;
    /** The app port the tunnel serves. Older services omit it. */
    port?: number | null;
    error: string | null;
  };
  /** The app on this computer's loopback through SSH. Older services omit it. */
  local?: null | {
    url: string;
    hostPort: number;
    appPort: number;
    check: null | {
      reachable: boolean;
      status?: number;
      latencyMs?: number;
      error?: string;
    };
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
