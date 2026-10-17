import type { ManagerResourceOperation, ManagerResourceStatus, ManagerResources } from "../bindings";

export class ManagerRequestError extends Error {
  readonly status: number;
  constructor(message: string, status: number) { super(message); this.status = status; }
}

/** This endpoint runs on the physical host, alongside the CLI and VM launcher. */
export function managerEndpoint(): string | null {
  if (window.location.protocol !== "http:" || !["localhost", "127.0.0.1"].includes(window.location.hostname)) return null;
  return `http://${window.location.hostname}:5524/api/service/resources`;
}

async function request<T>(path = "", init?: RequestInit): Promise<T> {
  const endpoint = managerEndpoint();
  if (!endpoint) throw new Error("Open the dashboard on this host's localhost to manage microManager resources.");
  const response = await fetch(endpoint + path, { ...init, signal: AbortSignal.timeout(15000) });
  if (!response.ok) {
    const body = await response.json().catch(() => null) as { error?: string } | null;
    throw new ManagerRequestError(body?.error ?? `Resource controller returned HTTP ${response.status}`, response.status);
  }
  return response.json() as Promise<T>;
}

export function getManagerResources(): Promise<ManagerResourceStatus> { return request(); }

export function saveManagerResources(settings: ManagerResources, controllerId: string): Promise<ManagerResourceStatus> {
  return request("", { method: "PUT", headers: { "Content-Type": "application/json", "X-Firecrab-Controller-Id": controllerId }, body: JSON.stringify(settings) });
}

export function applyManagerResources(controllerId: string): Promise<ManagerResourceOperation> {
  return request("/apply", { method: "POST", headers: { "Content-Type": "application/json", "X-Firecrab-Controller-Id": controllerId }, body: "{}" });
}
