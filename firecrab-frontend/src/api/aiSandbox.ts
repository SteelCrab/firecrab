import { fetchJson } from "./client";

export interface ModelSelection { connectionId: string; model: string }
export interface SandboxModel { id: string; name: string; verified: boolean; toolsVerified: boolean }
export interface ModelCatalog { models: SandboxModel[]; discoveryAvailable: boolean }
export interface ProjectSpec { name: string; sourcePath: string | null; vmId: string | null; selection: ModelSelection | null; instructions: string }
export interface Project { id: string; spec: ProjectSpec; createdAt: number }
export type TaskState = "waiting" | "accepted" | "in_progress" | "result" | "merged" | "failed" | "cancelled" | "interrupted";
export interface TaskInput { kind: "chat" | "code"; input: string; selection: ModelSelection | null; retryOf: string | null }
export interface ToolEvent { sequence: number; at: number; name: string; input: string; output: string; stderr: string; exitCode: number | null }
export interface FileChange { path: string; before: string | null; after: string | null; beforeMode: number | null; afterMode: number | null; applied: boolean }
export interface SandboxTask { id: string; projectId: string; attemptId: string; request: TaskInput; selection: ModelSelection; state: TaskState; createdAt: number; updatedAt: number; result: string; errorCode: string | null; error: string | null; events: ToolEvent[]; changes: FileChange[] }
export type TaskSummary = Omit<SandboxTask, "events" | "changes"> & { eventCount: number; changeCount: number; lastTool: string | null };
export const summarizeTask = (task: SandboxTask): TaskSummary => ({ ...task, eventCount: task.events.length, changeCount: task.changes.length, lastTool: task.events.at(-1)?.name ?? null });
const base = "/api/ai-sandbox";
const write = (method: string, body?: unknown): RequestInit => ({ method, headers: { "Content-Type": "application/json" }, ...(body === undefined ? {} : { body: JSON.stringify(body) }) });
export const activeTask = (task: Pick<SandboxTask, "state">) => ["waiting", "accepted", "in_progress"].includes(task.state);
export const selectionKey = (selection: ModelSelection) => `${selection.connectionId}|${selection.model}`;
export const selectionFromKey = (key: string): ModelSelection | null => { const [connectionId, model] = key.split("|"); return connectionId && model ? { connectionId, model } : null; };
export const listProjects = (signal?: AbortSignal) => fetchJson<Project[]>(`${base}/projects`, { signal });
export const saveProject = (id: string, spec: ProjectSpec) => fetchJson<Project>(`${base}/projects/${id}`, write("PUT", spec));
export const deleteProject = (id: string) => fetchJson<{ deleted: boolean }>(`${base}/projects/${id}`, write("DELETE"));
export const listModels = (connection: string, discover = false, signal?: AbortSignal) => fetchJson<ModelCatalog>(`${base}/connections/${connection}/models?discover=${discover}`, { signal });
export const verifyModel = (connection: string, model: string, tools: boolean) => fetchJson<SandboxModel>(`${base}/connections/${connection}/verify`, write("POST", { model, tools }));
export const listTasks = (project: string, signal?: AbortSignal, since?: number, before?: string) => fetchJson<TaskSummary[]>(`${base}/projects/${project}/tasks?${new URLSearchParams({ ...(since === undefined ? {} : { since: String(since) }), ...(before ? { before } : {}) })}`, { signal });
export const getTask = (id: string, signal?: AbortSignal) => fetchJson<SandboxTask>(`${base}/tasks/${id}`, { signal });
export const createTask = (project: string, id: string, input: TaskInput) => fetchJson<SandboxTask>(`${base}/projects/${project}/tasks/${id}`, write("PUT", input));
export const cancelTask = (id: string) => fetchJson<SandboxTask>(`${base}/tasks/${id}/cancel`, write("POST"));
export const reconcileTask = (id: string) => fetchJson<SandboxTask>(`${base}/tasks/${id}/reconcile`, write("POST"));
export const applyTask = (id: string, paths: string[]) => fetchJson<SandboxTask>(`${base}/tasks/${id}/apply`, write("POST", { paths }));
