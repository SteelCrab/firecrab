import { managerEndpoint } from "./manager";

export type AgentSession = {
  id: string;
  name: string;
  role: string;
  kind: string;
  cwd: string;
  model: string;
  workspaceId: string;
  paneId: string;
  closed: boolean;
  launchError: string | null;
  hasPendingTask?: boolean;
  taskDeliveryUncertain?: boolean;
  state:
    | "idle"
    | "working"
    | "blocked"
    | "done"
    | "unknown"
    | "closed"
    | "disconnected"
    | "failed";
};
export type GitHubAuth = {
  state: "unavailable" | "disconnected" | "pending" | "connected" | "failed";
  userCode?: string | null;
  verificationUri?: string;
  error?: string;
};
export type AgentStatus = {
  github?: GitHubAuth;
  project?: VmRepository | null;
  available: boolean;
  running: boolean;
  reason: string | null;
  controllerId: string;
  drivers: {
    id: string;
    name: string;
    available: boolean;
    authenticated?: boolean;
  }[];
  vmId?: string;
  githubConnected?: boolean;
  githubAvailable?: boolean;
  preparation?: {
    state: string;
    cli: string;
    step?: number;
    log?: string;
    error?: string | null;
  } | null;
  agents: AgentSession[];
};
export type AgentTask = {
  id: string;
  input: string;
  cli: string;
  model: string;
};
export type VmRepository = {
  vmId: string;
  repository: string | null;
  branch: string;
  path: string;
  state: "preparing" | "ready" | "failed" | "interrupted";
  error?: string | null;
};
export type AgentTerminal = { agent: AgentSession; text: string };

export class AgentRequestError extends Error {
  readonly status: number;
  constructor(message: string, status: number) {
    super(message);
    this.name = "AgentRequestError";
    this.status = status;
  }
}

/** Retry only read-only status checks while first-boot SSH becomes available. */
export function isAgentConnectionPending(failure: unknown): boolean {
  if (failure instanceof TypeError) return true;
  if (
    failure instanceof DOMException &&
    ["TimeoutError", "AbortError"].includes(failure.name)
  )
    return true;
  if (
    failure instanceof AgentRequestError &&
    [502, 503, 504].includes(failure.status)
  )
    return true;
  if (!(failure instanceof Error)) return false;
  const message = failure.message;
  if (
    /host key verification failed|host identification has changed|permission denied|another controller|pinned SSH host key/i.test(
      message,
    )
  )
    return false;
  return (
    (/^MicroVM:/.test(message) &&
      /connection refused|connection timed out|connection closed|connection reset|no route to host|stdio forwarding request failed|stdio forwarding failed/i.test(
        message,
      )) ||
    message.startsWith("The MicroVM command timed out.")
  );
}

export function agentTerminalUrl(
  vmId: string,
  agentId: string,
  controllerId: string,
  cols: number,
  rows: number,
): string {
  const resource = managerEndpoint();
  if (!resource || !vmId || !agentId || !controllerId)
    throw Error("Select this host's MicroVM before connecting its terminal.");
  const url = new URL(
    resource.replace(/\/resources$/, "/agents") +
      `/microvms/${encodeURIComponent(vmId)}/${encodeURIComponent(agentId)}/terminal`,
    window.location.href,
  );
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  url.search = new URLSearchParams({
    controller: controllerId,
    cols: String(cols),
    rows: String(rows),
  }).toString();
  return url.href;
}

export async function agentRequest<T>(
  path = "",
  body?: object,
  controllerId?: string,
  vmId?: string,
): Promise<T> {
  const resource = managerEndpoint();
  if (!resource)
    throw Error(
      "CLI agent management requires this host’s localhost dashboard. Start firecrab service dev on this host.",
    );
  if (!vmId) throw Error("Select a MicroVM before opening its CLI workspace.");
  const endpoint =
    resource.replace(/\/resources$/, "/agents") +
    `/microvms/${encodeURIComponent(vmId)}`;
  const response = await fetch(endpoint + path, {
    method: body ? "POST" : "GET",
    headers: body
      ? {
          "Content-Type": "application/json",
          "X-Firecrab-Controller-Id": controllerId || "",
        }
      : undefined,
    body: body ? JSON.stringify(body) : undefined,
    signal: AbortSignal.timeout(body ? 160000 : 30000),
  });
  const value = await response.json().catch(() => null);
  if (!response.ok)
    throw new AgentRequestError(
      typeof value?.error === "string"
        ? value.error
        : response.status === 404
          ? "Update the native Firecrab service to enable CLI agent management."
          : `Agent runtime returned HTTP ${response.status}`,
      response.status,
    );
  return value as T;
}

export function agentEditorUrl(
  vmId: string,
  agentId: string,
  controllerId: string,
): string {
  const params = new URLSearchParams({
    editor: agentId,
    vm: vmId,
    controller: controllerId,
  });
  return `${window.location.origin}${window.location.pathname}#/ai-sandbox?${params}`;
}
