import type { Node } from "./types.ts";
export type Selection = { connectionId: string; model: string };
export type AgentRequest = {
  kind: "code";
  projectShell?: true;
  input: string;
  projectDirectory: string;
  instructionSnapshotId: string;
  selection: Selection;
  coordination: { assignedAgent: string };
};
export type AgentDocument = {
  schema: 1;
  project: string;
  directory: string;
  draft: string;
  selection?: Selection;
  shell?: boolean;
  pending?: { id: string; snapshot: string; request: AgentRequest };
  taskId?: string;
};
export type AgentTask = {
  id: string;
  projectId: string;
  request: AgentRequest;
  selection: Selection;
  state: string;
  result: string;
  error?: string;
  errorCode?: string;
  createdAt: number;
  folderMutations?: {
    operationId: string;
    path: string;
    beforeVersion: number;
    afterVersion: number;
    sha256: string;
  }[];
  events?: { name: string; input: string; output: string; stderr: string }[];
};
export type Choice = { selection: Selection; label: string; verified: boolean };
export type Context = {
  id: string;
  files: { path: string; version: number }[];
};
export type AgentState = {
  scope: string;
  models: Choice[];
  rows: AgentTask[];
  task?: AgentTask;
  context?: Context;
  loaded: boolean;
  refreshing: boolean;
  busy: boolean;
  more: boolean;
  error: string;
};
export const agentLabel = (node: Node) => "Canvas " + node.id;
export const selectionKey = (s?: Selection) =>
  s ? JSON.stringify([s.connectionId, s.model]) : "";
export const activeTask = (t?: AgentTask) =>
  !!t && ["waiting", "accepted", "in_progress"].includes(t.state);
const uuid = (s: unknown): s is string =>
  typeof s === "string" &&
  /^[\da-f]{8}(-[\da-f]{4}){3}-[\da-f]{12}$/i.test(s) &&
  !/^0{8}-0{4}-0{4}-0{4}-0{12}$/.test(s);
const selection = (s: Selection) =>
  s &&
  uuid(s.connectionId) &&
  typeof s.model === "string" &&
  !!s.model &&
  s.model.length <= 256;
export function agentDocument(
  node: Node,
  project: string,
): AgentDocument | undefined {
  try {
    const d: AgentDocument = JSON.parse(node.content);
    if (
      d.schema !== 1 ||
      d.project !== project ||
      typeof d.directory !== "string" ||
      typeof d.draft !== "string" ||
      d.draft.length > 4096 ||
      (d.shell !== undefined && typeof d.shell !== "boolean") ||
      (d.selection && !selection(d.selection)) ||
      (d.taskId && !uuid(d.taskId))
    )
      return;
    if (d.pending) {
      const p = d.pending,
        r = p.request;
      if (
        !uuid(p.id) ||
        !uuid(p.snapshot) ||
        !r ||
        r.kind !== "code" ||
        (r.projectShell !== undefined && r.projectShell !== true) ||
        (r.projectShell === true && d.shell !== true) ||
        r.projectDirectory !== d.directory ||
        r.instructionSnapshotId !== p.snapshot ||
        !selection(r.selection) ||
        typeof r.input !== "string" ||
        r.input.length > 4096 ||
        r.coordination?.assignedAgent !== agentLabel(node)
      )
        return;
    }
    return d;
  } catch {
    return;
  }
}
export function assertTask(
  task: AgentTask,
  project: string,
  directory: string,
  label: string,
  id?: string,
) {
  if (
    !task ||
    !uuid(task.id) ||
    (id && task.id !== id) ||
    task.projectId !== project ||
    task.request?.kind !== "code" ||
    task.request.projectDirectory !== directory ||
    task.request.coordination?.assignedAgent !== label ||
    ![
      "waiting",
      "accepted",
      "in_progress",
      "result",
      "merged",
      "failed",
      "cancelled",
      "interrupted",
    ].includes(task.state)
  )
    throw new Error("Folder task response has a different session identity");
}
