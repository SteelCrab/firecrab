import { ApiClientError, fetchJson } from "./client";

export const CODE = "/api/code/workspaces";
export type Agent = { agent: string; connectionId: string };
export interface Spec {
  name: string;
  github: {
    account: string;
    owner: string;
    repository: string;
    branch: string;
  };
  runtime: string;
  resources: { cpu: number; ram: number; diskGb: number };
  agent: Agent | null;
}
export interface Workspace {
  id: string;
  revision: number;
  spec: Spec;
  connections: { github: string; ai: string };
}
export interface Runtime {
  workspaceId: string;
  configurationRevision: number;
  vmId: string;
  runtimeId: string | null;
  operationId: string;
  action: string;
  phase: string;
  vmState: string | null;
  projectPath: string;
  initialCommit: string | null;
  error: string | null;
  placement: {
    template: { name: string; version: string };
    microNetworkId: string;
    storageRoot: string;
  };
}
export interface Account {
  id: string;
  name: string;
  provider: string;
  authentication: string;
}
export interface Repository {
  owner: string;
  repository: string;
  defaultBranch: string;
  language?: string;
}
export interface Adapter {
  id: string;
  agent: string;
  provider: string;
  authentication: string;
  cliVersion: string;
  supported: boolean;
}
export interface Editor {
  workspaceId: string;
  revision: number;
  vmId: string;
  runtimeId: string;
  operationId: string;
  phase: string;
  version: string;
}
export interface Run {
  id: string;
  workspaceId: string;
  revision: number;
  connectionId: string;
  agent: string;
  adapterId: string;
  cliVersion: string;
  model: string;
  modelsUsed: string[];
  vmId: string;
  runtimeId: string;
  operationId: string;
  phase: string;
  output: string;
  truncated: boolean;
  providerRequests: number;
  error: string | null;
}
export interface RunBody {
  expectedRevision: number;
  vmId: string;
  runtimeId: string;
  operationId: string;
  model: string;
  prompt: string;
}
export const uuid = (v: unknown): v is string =>
  typeof v === "string" &&
  /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(v) &&
  /[1-9a-f]/i.test(v.replaceAll("-", ""));
export const equal = (a: unknown, b: unknown) =>
  JSON.stringify(a) === JSON.stringify(b);
export const active = (phase: string) =>
  ["queued", "starting", "running", "cancelling"].includes(phase);
export const preferred = (a: Account) =>
  a.authentication !== "api_key"
    ? ""
    : a.provider === "openai"
      ? "codex"
      : a.provider === "anthropic"
        ? "claude_code"
        : "";
export const compatible = (agent: string, a: Account) =>
  uuid(a.id) &&
  (agent === "codex"
    ? a.provider === "openai" &&
      ["api_key", "chatgpt"].includes(a.authentication)
    : ["claude_code", "opencode"].includes(agent) &&
      a.authentication === "api_key" &&
      (a.provider === "anthropic" ||
        (agent === "opencode" && a.provider === "openai")));
export const agentName = (name: string) =>
  ({ codex: "Codex", claude_code: "Claude Code", opencode: "OpenCode" })[
    name
  ] ?? name;
export function ready(r: Runtime | null): r is Runtime & { runtimeId: string } {
  return (
    !!r &&
    r.phase === "ready" &&
    r.action === "start" &&
    r.vmState === "running" &&
    !r.error &&
    uuid(r.vmId) &&
    uuid(r.runtimeId) &&
    uuid(r.operationId) &&
    r.projectPath === "/home/fc-code/project" &&
    /^[a-f0-9]{40}$/i.test(r.initialCommit ?? "")
  );
}
export function editorMatches(
  e: Editor | null,
  w: Workspace,
  r: Runtime | null,
) {
  return (
    !!e &&
    ready(r) &&
    e.workspaceId === w.id &&
    e.revision === w.revision &&
    e.vmId === r.vmId &&
    e.runtimeId === r.runtimeId &&
    e.operationId === r.operationId &&
    e.phase === "ready" &&
    !!e.version
  );
}
export const write = <T>(path: string, body: unknown, method = "POST") =>
  fetchJson<T>(path, {
    method,
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
export function codeError(error: unknown) {
  return error instanceof ApiClientError
    ? (error.apiError?.code ??
        (error.status === 403 ? "access_denied" : "unavailable"))
    : "unavailable";
}
export async function workspaces(signal?: AbortSignal) {
  const rows = await fetchJson<Workspace[]>(CODE, { signal });
  if (
    !Array.isArray(rows) ||
    rows.some(
      (w, i) =>
        !uuid(w.id) ||
        !Number.isSafeInteger(w.revision) ||
        w.revision < 1 ||
        !w.spec?.github ||
        !w.spec.resources ||
        !w.connections ||
        rows.findIndex((v) => v.id === w.id) !== i,
    )
  )
    throw new Error("invalid");
  return rows;
}
export async function runtime(
  id: string,
  signal?: AbortSignal,
): Promise<Runtime | null> {
  try {
    const r = await fetchJson<Runtime>(`${CODE}/${id}/runtime`, { signal });
    if (
      r.workspaceId !== id ||
      !uuid(r.vmId) ||
      !uuid(r.operationId) ||
      !Number.isSafeInteger(r.configurationRevision)
    )
      throw new Error("invalid");
    return r;
  } catch (e) {
    if (
      !(e instanceof ApiClientError) ||
      e.status !== 404 ||
      e.apiError?.code !== "not_found"
    )
      throw e;
    const w = await fetchJson<Workspace>(`${CODE}/${id}`, { signal });
    if (w.id !== id) throw new Error("invalid");
    return null;
  }
}
export function remember(key: string, value: unknown) {
  sessionStorage.setItem(key, JSON.stringify(value));
}
export function restore<T>(key: string): T | null {
  try {
    return JSON.parse(sessionStorage.getItem(key) ?? "null") as T | null;
  } catch {
    return null;
  }
}
